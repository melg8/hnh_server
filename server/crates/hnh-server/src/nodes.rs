//! Node-link mesh for the multi-process grid-owner cluster (session 27).
//!
//! The world partitions into VisIndex cells; `grid_owner::owner_of` picks
//! each cell's owning node from the cluster membership. With more than one
//! node process the partition becomes REAL: every node simulates only the
//! gobs standing in its own cells and exchanges the results with its peers
//! over this TCP mesh:
//!
//! - players are always authored by their HOME node (the node the client's
//!   UDP session landed on); the home node publishes a player to the owner
//!   of whatever foreign cell the player currently stands in;
//! - animals and world gobs are authored by their cell's owner; when a gob
//!   crosses a cell boundary the old owner TRANSFERS the full gob state to
//!   the new owner and demotes its local copy to a guest, so the gob id
//!   (globally unique by per-node slot stride, see `Gobs`) never changes;
//! - sessions see foreign-authority gobs as guests: a node subscribes its
//!   peers to the view cells its sessions actually look at, and the owning
//!   peer streams GuestAnnounce/GuestUpdate/GuestRetract for those cells.
//!   Guests run through the same dirty-cell visibility machinery as local
//!   gobs, so spawn, LINSTEP progress, pose change and retract render
//!   identically.
//!
//! The wire format is length-prefixed bincode. Frames are capped at 1 MiB:
//! a garbage peer (or a port scanner) must not be able to force an
//! allocation (the length prefix is checked before any read-into buffer).
//!
//! Both link sides send `Hello { proto, node }` first and validate the
//! peer's reply; an unexpected index or a stale protocol version drops the
//! connection. A dialer retries forever with a 500 ms backoff; messages
//! queued while the link is down flush on reconnect (dev-cluster
//! semantics: brief partitions lose nothing).
//!
//! Single-node mode (the default, `--cluster` absent) never constructs the
//! mesh: every cell is owned by node 0 and no socket is opened.

use std::net::SocketAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// Protocol version of the node link; mismatching peers are rejected so a
/// stale process cannot corrupt a newer cluster's state.
pub const NODE_PROTO: u16 = 1;

/// Hard cap on one wire frame (length-prefix value). The largest legitimate
/// message is a chat line or a player guest state (a few KiB worst case);
/// 1 MiB leaves orders of magnitude of headroom without allowing a bogus
/// length prefix to reserve unbounded memory.
pub const MAX_FRAME: usize = 1024 * 1024;

/// Wire states of one foreign-authority gob, as published by its owner.
/// Everything the subscriber needs to render and progress the gob locally
/// (position/linmove arithmetic is deterministic and identical on every
/// node, so movement progress is NOT streamed — only start/retarget/finish
/// facts are).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuestState {
    pub id: i32,
    pub pos: (i32, i32),
    pub mv: Option<GuestLinMove>,
    /// Pose state: moving = walking layer set, else standing.
    pub moving: bool,
    /// Movement octant (0..8, see `game::move_dir`).
    pub facing: u8,
    pub kind: GuestKind,
    pub hp: i32,
    pub max_hp: i32,
    /// Authoritative gait speed in subtiles/second (transfer needs it to
    /// keep retarget math identical on the new owner).
    pub speed: i32,
}

/// Copy of `state::LinMove` (that type is game-internal; the node link
/// carries its own serializable mirror).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct GuestLinMove {
    pub sx: i32,
    pub sy: i32,
    pub tx: i32,
    pub ty: i32,
    pub steps: i32,
    pub step: i32,
    pub started_ms: u64,
    pub total_ms: u32,
}

/// What the subscriber must render. Species and equipment are carried as
/// plain data (the resource NAMES resolve deterministically on every node
/// from the same served pack, so ids are interned locally at ingest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GuestKind {
    /// `species` is the `state::Species` discriminant.
    Animal { species: u8 },
    Player {
        name: String,
        /// Equipped borka piece prefixes (equip.rs table), render-relevant.
        equip: Vec<String>,
    },
    /// Drops/structures: rendered from one concrete resource. `class` is
    /// the STABLE interaction class (session 30): it never changes during
    /// the gob's lifetime, so the subscriber can pick the relay act
    /// without any extra state sync. The authority still re-validates
    /// every act against its own Kind.
    ///
    /// `crop` is `Some((spec, stage))` for crops only (session 31): the
    /// home node needs both to decide whether a click may open the
    /// harvest flower menu, and the subscriber renders the growth stage
    /// from it (sdt byte). Bincode over the node link is positional -
    /// every node in a cluster runs the SAME binary, so additive struct
    /// changes are safe (rolling upgrades are not supported anywhere
    /// else either, see SavedPlayer in persist.rs).
    ///
    /// `stage` is `Some(stage)` for construction plans only (session
    /// 34): a peer watching a build re-renders the plan sprite as the
    /// credited materials advance the stage, exactly like the local
    /// restage path. Plans and structures publish the Structure class
    /// (no relay act); on completion the gob re-publishes as its real
    /// class (Station/Structure) and the plan stage drops away.
    Static {
        res_name: String,
        class: StaticClass,
        #[serde(default)]
        crop: Option<(u8, u8)>,
        #[serde(default)]
        station: Option<StationView>,
        #[serde(default)]
        stage: Option<u8>,
        #[serde(default)]
        drop: Option<DropView>,
    },
}

/// Full drop payload for authority transfer (session 35): a node that
/// spawns a `Kind::Drop` onto a cell it does not own (e.g. a station
/// output drop whose spawn jitter crossed a cell boundary) hands the
/// WHOLE drop state to the cell's owner so it can claim it into its
/// sim tables with the same id. The world render shape is NOT carried:
/// it is derived deterministically from the inventory resource name on
/// the receiving side (`game::drop_world_res`), the same derivation the
/// spawner used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DropView {
    /// The INVENTORY resource name (gfx/invobjs/...): the stack the
    /// picker receives, and the seed for the deterministic world shape.
    pub inv_res: String,
    /// Stack quality.
    pub ql: u8,
    /// Display label (food fep.conf identity).
    pub label: String,
}

/// Snapshot of a production station's authority state, piggybacked on
/// the guest static payload (session 33). The home node opens the
/// Light/Extinguish flower menu from `lit` without a round trip, and
/// the subscriber re-renders the lit sprite from the sdt byte on every
/// re-published GuestUpdate. `fuel`/`has_input` ride along so future
/// widgets can render readiness without extra messages.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct StationView {
    /// Index into `build::BUILDABLES` (which station this is).
    pub spec: u8,
    /// Whether a job is running (authority state at publish time).
    pub lit: bool,
    /// Stored fuel units.
    pub fuel: u32,
    /// Whether the single input slot is loaded.
    pub has_input: bool,
}

/// Stable interaction class carried by GuestKind::Static (session 30).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StaticClass {
    /// Item drop: the relay act is Pickup.
    Drop,
    /// Harvestable tree: the relay act is Chop.
    Tree,
    /// Stone: the relay act is Mine.
    Stone,
    /// Growing crop: the home node opens the stage-appropriate harvest
    /// menu locally; the chosen act (HarvestCrop) is re-validated by the
    /// authority against its own crop state (session 31).
    Crop,
    /// Production station (oven): the home node opens the
    /// Light/Extinguish menu locally from the piggybacked StationView;
    /// the chosen act is relayed and re-validated by the authority
    /// (session 33). Fuel/input deliveries relay the held stack.
    Station,
    /// Plans/structures: no relay act today (flavor only).
    Structure,
}

/// One pick-up-able stack as removed by the authority (session 30 relay).
/// Resource NAME (not index) crosses the wire: names resolve on every
/// node against the same served pack, the same rule every other Guest
/// string follows. `label` keeps the food fep.conf identity alive from
/// the authority's side to the picker's inventory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StaticStack {
    pub res: String,
    pub count: u32,
    pub ql: u8,
    pub label: String,
}

/// Player-side interaction choice for a relayed static act (session 30).
/// Picked by the home node from the guest view's StaticClass; validated
/// by the authority against its authoritative Kind. A mismatch (stale
/// guest view) is dropped, never trusted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StaticAct {
    Pickup,
    Chop,
    Mine,
    /// Harvest a growing crop (session 31). The authority decides
    /// mature vs unripe from ITS crop state, rolls the yield table and
    /// answers one StaticAck per yielded stack (crops never grant LP).
    HarvestCrop,
}

/// Player-side station menu choice (session 33). The home node picks
/// the act from the piggybacked StationView snapshot (`lit` decides
/// which option the flower menu shows); the authority re-validates it
/// against its own StationState - a stale act changes nothing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StationAct {
    /// Start a job (fuel + input required, station must be unlit).
    Light,
    /// Cancel the lit state (progress resets; the input is preserved -
    /// this server's documented policy).
    Extinguish,
}

/// Outcome of a relayed station act, authority -> home (session 33).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StationResult {
    /// The job started; the authority re-published the lit sprite.
    Lit,
    /// The job was cancelled; progress reset, input preserved.
    Extinguished,
    /// Refusal: no fuel stored (same system line as the local path).
    NeedsFuel,
    /// Refusal: the input slot is empty (same system line as the local
    /// path).
    NeedsInput,
    /// The view was stale (act did not match the authority's lit
    /// state) or the station vanished. Silent on the home side.
    Stale,
}

/// Outcome of a relayed fuel/input delivery, authority -> home
/// (session 33). `FuelAdded`/`InputLoaded` consume ONE unit from the
/// cursor stack on the home node; every refusal keeps the whole stack
/// and maps to the exact system line the local refusal emits.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StationItemResult {
    FuelAdded,
    InputLoaded,
    /// Refusal: the station is lit (input refused only; fuel still
    /// loads while lit, matching the local path's order).
    BusyLit,
    /// Refusal: the input slot is already loaded.
    InputFull,
    /// Refusal: the station cannot process this label.
    NotProcessable,
    /// The station vanished mid-flight. Silent, stack kept.
    Gone,
}

/// One node-link message. Sub/Unsub flow viewer -> owner; guest messages
/// flow owner -> viewers (and owner -> cell owner for players).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeMsg {
    /// Handshake: sender's node index in the shared membership list.
    Hello { proto: u16, node: usize },
    /// Liveness probe; also exercises the codec in unit tests.
    Ping,
    /// Viewer -> owner: start streaming gobs in these cells. `from` is
    /// the sender's node index (the shared membership identity).
    Sub { from: usize, cells: Vec<(i32, i32)> },
    /// Viewer -> owner: stop streaming these cells.
    Unsub { from: usize, cells: Vec<(i32, i32)> },
    /// Area chat broadcast with the same radius semantics: the receiver
    /// filters its sessions by the SENDER's position (guest or local).
    Chat {
        from: String,
        at: (i32, i32),
        text: String,
    },
    /// Full state of a gob this node now owns / viewers should render.
    GuestAnnounce(GuestState),
    /// Movement/pose delta of an already-announced gob.
    GuestUpdate(GuestState),
    /// The gob died or left every subscribed cell.
    GuestRetract { id: i32 },
    /// Authority handoff: the receiver inserts the gob with the SAME id
    /// and becomes its simulation authority.
    GuestTransfer(GuestState),
    /// Cross-node interaction relay (session 28): home node -> authority
    /// node. The player gob `attacker` swung at the guest animal `target`.
    /// `chip` (defence-bar damage) and `dmg` (HP damage on an opening) are
    /// computed on the home node with the exact formulas the local path
    /// uses — the attacker's str/armor live there. The authority applies
    /// them to its authoritative bars/HP and answers with FightBars.
    RelayAttack {
        attacker: i32,
        target: i32,
        chip: i32,
        dmg: i32,
    },
    /// Authority -> home: the authoritative defence bar of a relay-fought
    /// animal after one applied swing; the home mirror re-syncs from it
    /// (the fightview reads the mirror, so a missed message self-heals on
    /// the next swing).
    FightBars { id: i32, def: i32 },
    /// Authority -> home: an animal bit the guest player `player_gob` (a
    /// session player homed on the receiver). Armor absorption, HP,
    /// stamina and the knockout path all live on the home node.
    PlayerHurt {
        player_gob: i32,
        dmg: i32,
        from: i32,
    },
    /// Authority -> home: the guest attacker dealt the killing blow; the
    /// receiver grants the learning points (the LP wallet lives there).
    KillCredit { player_gob: i32, lp: i32 },
    /// Cross-node static interaction (session 30), home -> authority.
    /// The clicking player (a session player homed on the sender) acted on
    /// the static gob `target`; the target's LIFECYCLE (drop contents, tree
    /// harvests, stone) is authoritative on the receiver. The receiver
    /// applies its local logic and answers StaticAck.
    RelayStaticAct {
        player: i32,
        target: i32,
        act: StaticAct,
    },
    /// Authority -> home: the result of a RelayStaticAct. `stack` carries
    /// the removed drop's contents for Pickup (the home node pushes it
    /// through grant_pickup); `lp` grants Chop/Mine learning points. An
    /// empty ack means the target was gone or the act mismatched its kind
    /// - the home side shows nothing and the retract has cleaned the view.
    StaticAck {
        player: i32,
        stack: Option<StaticStack>,
        lp: i32,
    },
    /// Cross-node planting (session 31), home -> authority. The player
    /// clicked a plowed tile whose cell is owned by the receiver; tilth
    /// and occupancy are authoritative THERE. `seed_ql` rides the act
    /// because the seed physically lives on the home node's cursor. The
    /// receiver spawns the crop and answers PlantAck (ok = true) on
    /// success; on any refusal it stays silent - the home node keeps the
    /// seed on the cursor, exactly like a local refusal keeps it.
    RelayPlantAct {
        player: i32,
        tx: i32,
        ty: i32,
        spec: u8,
        seed_ql: u8,
    },
    /// Authority -> home: the crop was planted. The home node now
    /// consumes one seed unit from the cursor (never before the ack -
    /// a rejected or lost relay must not destroy the seed).
    PlantAck { player: i32, ok: bool },
    /// Cross-node plowing (session 32), home -> authority. The player
    /// aimed a plow at a grass tile whose cell is owned by the receiver;
    /// the TILE state is authoritative THERE (the home node never mutates
    /// its own grid while relaying, so no shadow furrow can desync the
    /// views). The receiver validates against its own grid, mutates,
    /// answers PlowAck and broadcasts TileMutation to every peer.
    RelayPlowAct { player: i32, tx: i32, ty: i32 },
    /// Authority -> home: the result of a RelayPlowAct. `ok = false`
    /// (not grass / occupied by a crop) is silent on the home side: no
    /// stamina drain, parity with a local refusal. A lost ack costs the
    /// player nothing (retry works; the act is idempotent on the
    /// authority: a second plow of a now-PLOWED tile is refused by the
    /// not-grass check).
    PlowAck { player: i32, ok: bool },
    /// Authority -> every peer: one tile mutation landed (plow or the
    /// tilth-decay revert to grass). Every node holding that grid
    /// applies the tile to its local copy, re-sends MAPDATA to its local
    /// holders, and records the override so a later materialization
    /// replays it. Non-resident grids only record the override (no
    /// materialization just to shadow a mutation nobody looks at).
    TileMutation { tx: i32, ty: i32, tile: u8 },
    /// Cross-node station act (session 33), home -> authority. The
    /// player chose Light/Extinguish in the flower menu the HOME node
    /// opened from the piggybacked StationView; the station's fuel,
    /// input and lit state are authoritative on the receiver, which
    /// validates the act against its own StationState and answers
    /// StationAck. A stale act (the view said unlit but the authority
    /// already lit it, or vice versa) answers Stale and changes nothing.
    RelayStationAct {
        player: i32,
        target: i32,
        act: StationAct,
    },
    /// Authority -> home: the outcome of a RelayStationAct. Refusals map
    /// to the exact system lines the LOCAL refusal path emits, so the
    /// cross-node UX matches; Stale/Gone stay silent (parity with the
    /// crop relay's stale-view drop).
    StationAck { player: i32, result: StationResult },
    /// Cross-node fuel/input delivery (session 33), home -> authority.
    /// The stack physically lives on the home node's cursor - it is NOT
    /// consumed until StationItemAck ok (the seed-safe pattern from the
    /// plant relay). The receiver validates against its own StationState
    /// (fuel list, lit, input slot, roastable label) and answers with
    /// the same refusal lines the local path emits.
    RelayStationItem {
        player: i32,
        target: i32,
        stack: StaticStack,
    },
    /// Authority -> home: the outcome of a RelayStationItem. `ok = true`
    /// consumes ONE unit from the cursor stack on the home node (a
    /// multi-unit stack keeps the rest, exactly like the local path);
    /// refusals keep the whole stack (parity with a local refusal).
    StationItemAck {
        player: i32,
        result: StationItemResult,
    },
    /// Cluster character migration (session 29). A node about to enter a
    /// player whose save key it does not hold broadcasts this query. A
    /// peer holding the snapshot offline answers CharData (re-serving it
    /// on every retry until acknowledged); a peer without it answers
    /// CharNack. `from` lets a node ignore its own broadcast echo.
    CharQuery { from: usize, name: String },
    /// Unicast answer to CharQuery: the full character snapshot, keyed by
    /// the save key it was stored under. The receiver adopts it, acks
    /// back (CharAck) and proceeds with the world entry. The holder keeps
    /// its copy until the ack - a lost reply can always be re-served.
    CharData {
        to: usize,
        from: usize,
        name: String,
        snap: crate::persist::SavedPlayer,
    },
    /// Unicast confirmation that the CharData snapshot was adopted. The
    /// holder drops its copy only on receipt.
    CharAck { name: String },
    /// Unicast answer to CharQuery from a peer that does not hold the key
    /// (or holds it online). Lets the requester enter immediately once
    /// every peer answered - fresh logins must not wait out the deadline.
    /// `to` routes to the requester, `from` identifies the answering peer.
    CharNack {
        to: usize,
        from: usize,
        name: String,
    },
}

/// Length-prefix + bincode encode of one node message.
pub fn encode_frame(msg: &NodeMsg) -> Vec<u8> {
    let body = bincode::serialize(msg).expect("bincode serialize of NodeMsg is total");
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Incremental frame decoder over a TCP read half: buffers partial reads
/// and yields whole frames.
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        FrameReader { buf: Vec::new() }
    }

    /// Read from the stream, then drain every complete frame. Returns
    /// None on EOF; a length prefix over MAX_FRAME is a protocol error
    /// (the connection must be dropped by the caller). Generic over the
    /// reader so unit tests can drive it with in-memory duplex streams.
    pub async fn next_frame<R: tokio::io::AsyncRead + Unpin>(
        &mut self,
        stream: &mut R,
    ) -> std::io::Result<Option<NodeMsg>> {
        loop {
            if self.buf.len() >= 4 {
                let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]])
                    as usize;
                if len > MAX_FRAME {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "frame length exceeds MAX_FRAME",
                    ));
                }
                if self.buf.len() >= 4 + len {
                    let body: Vec<u8> = self.buf.drain(..4 + len).skip(4).collect();
                    let msg: NodeMsg = bincode::deserialize(&body).map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                    })?;
                    return Ok(Some(msg));
                }
            }
            let mut chunk = [0u8; 8192];
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Ok(None);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

/// Static cluster membership: the shared listen-address list plus which
/// entry is me. Parsed from `--cluster "host:port,host:port" --node N`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterConfig {
    /// Every node's listen address, indexed by node id.
    pub addrs: Vec<SocketAddr>,
    /// This process's node id.
    pub me: usize,
}

impl ClusterConfig {
    pub fn parse(list: &str, me: usize) -> Result<Self, String> {
        if list.trim().is_empty() {
            return Err("empty cluster list".into());
        }
        let mut addrs = Vec::new();
        for part in list.split(',') {
            let addr: SocketAddr = part
                .trim()
                .parse()
                .map_err(|_| format!("bad node address {part:?} (want host:port)"))?;
            addrs.push(addr);
        }
        if addrs.len() < 2 {
            return Err("a cluster needs at least 2 nodes".into());
        }
        if me >= addrs.len() {
            return Err(format!("--node {me} out of range (0..{})", addrs.len()));
        }
        let mut seen = std::collections::HashSet::new();
        for a in &addrs {
            if !seen.insert(*a) {
                return Err(format!("duplicate node address {a}"));
            }
        }
        Ok(ClusterConfig { addrs, me })
    }

    /// The peers to dial: every address except mine.
    pub fn peers(&self) -> impl Iterator<Item = (usize, SocketAddr)> + '_ {
        self.addrs
            .iter()
            .enumerate()
            .filter(move |&(i, _)| i != self.me)
            .map(|(i, a)| (i, *a))
    }

    pub fn listen(&self) -> SocketAddr {
        self.addrs[self.me]
    }

    pub fn node_count(&self) -> usize {
        self.addrs.len()
    }
}

/// Live mesh handle: fan messages out to peers, receive inbound ones.
#[derive(Clone)]
pub struct Mesh {
    /// Send one message to one peer (buffered while the peer reconnects).
    pub out_tx: UnboundedSender<(usize, NodeMsg)>,
}

impl Mesh {
    /// Fire-and-forget send to one peer (loss only if the runtime is gone).
    pub fn send(&self, peer: usize, msg: NodeMsg) {
        let _ = self.out_tx.send((peer, msg));
    }

    /// Broadcast to every peer EXCEPT `me` (self-addressed frames would
    /// otherwise sit in an undrained queue and leak).
    pub fn broadcast_except(&self, count: usize, me: usize, msg: NodeMsg) {
        for peer in 0..count {
            if peer != me {
                let _ = self.out_tx.send((peer, msg.clone()));
            }
        }
    }
}

/// Spawn the mesh: one acceptor on my listen address, one dial+maintain
/// task per peer. Inbound frames go to `in_tx`.
pub async fn run(cfg: ClusterConfig, in_tx: UnboundedSender<NodeMsg>) -> Mesh {
    // Per-peer outbound queues created up front so senders never block on
    // a reconnect cycle; the link task owns the receiver for its whole
    // life and resumes draining it across reconnects.
    let mut peer_tx: Vec<UnboundedSender<NodeMsg>> = Vec::new();
    let mut peer_rx: Vec<UnboundedReceiver<NodeMsg>> = Vec::new();
    for _ in 0..cfg.node_count() {
        let (tx, rx) = unbounded_channel();
        peer_tx.push(tx);
        peer_rx.push(rx);
    }
    // Public handle routes through a dispatcher task into the per-peer
    // queues, so senders never need to know about reconnect cycles.
    let (route_tx, mut route_rx) = unbounded_channel::<(usize, NodeMsg)>();
    let mesh = Mesh { out_tx: route_tx };
    {
        let peer_tx = peer_tx.clone();
        tokio::spawn(async move {
            while let Some((peer, msg)) = route_rx.recv().await {
                if let Some(tx) = peer_tx.get(peer) {
                    let _ = tx.send(msg);
                }
            }
        });
    }
    // Acceptor: inbound connections identify themselves by their Hello
    // node index; only known peers are accepted.
    {
        let listen = cfg.listen();
        let in_tx = in_tx.clone();
        let me = cfg.me;
        tokio::spawn(async move {
            let listener = match TcpListener::bind(listen).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(%listen, error = %e, "cluster listen failed");
                    return;
                }
            };
            loop {
                let Ok((stream, _addr)) = listener.accept().await else {
                    continue;
                };
                let in_tx = in_tx.clone();
                tokio::spawn(serve_inbound(stream, me, in_tx));
            }
        });
    }
    // Dialer per peer: connect, handshake, pump both directions until the
    // link breaks, then retry after a backoff. The per-peer OUTBOUND
    // receiver is owned by the dial loop and lent to each link attempt:
    // messages queued while the link was down survive the reconnect.
    for (peer, addr) in cfg.peers() {
        let in_tx = in_tx.clone();
        // Take this peer's receiver out (a throwaway goes in its slot so
        // the vec indexing stays valid for the remaining dial loops).
        let mut rx = {
            let (_dead_tx, dead_rx) = unbounded_channel();
            std::mem::replace(&mut peer_rx[peer], dead_rx)
        };
        let me = cfg.me;
        tokio::spawn(async move {
            loop {
                match TcpStream::connect(addr).await {
                    Ok(stream) => {
                        if let Err(e) = pump_link(me, peer, stream, &in_tx, &mut rx).await {
                            tracing::warn!(peer, %addr, error = %e, "cluster link down");
                        }
                    }
                    Err(e) => {
                        tracing::debug!(peer, %addr, error = %e, "cluster connect failed");
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    }
    mesh
}

/// Serve one inbound connection: validate the dialer's Hello, reply with
/// mine, then pump inbound frames until the link breaks.
async fn serve_inbound(stream: TcpStream, me: usize, in_tx: UnboundedSender<NodeMsg>) {
    let (mut rd, mut wr) = stream.into_split();
    let mut fr = FrameReader::new();
    let peer = match fr.next_frame(&mut rd).await {
        Ok(Some(NodeMsg::Hello { proto, node })) if proto == NODE_PROTO => node,
        Ok(Some(other)) => {
            tracing::debug!(?other, "unexpected cluster first frame");
            return;
        }
        Ok(None) => return,
        Err(e) => {
            tracing::debug!(error = %e, "bad cluster handshake");
            return;
        }
    };
    if let Err(e) = wr
        .write_all(&encode_frame(&NodeMsg::Hello {
            proto: NODE_PROTO,
            node: me,
        }))
        .await
    {
        tracing::debug!(error = %e, "cluster hello reply failed");
        return;
    }
    tracing::debug!(peer, "cluster acceptor link up");
    pump_read(peer, rd, fr, in_tx).await;
    tracing::debug!(peer, "cluster acceptor link closed");
}

/// Dialer-side link: send my Hello, validate the acceptor's reply, then
/// pump inbound frames and the outbound queue until the connection dies.
/// The outbound receiver moves into the link task for its whole life; on
/// reconnect the dial loop's ORIGINAL receiver resumes, preserving
/// messages buffered during the outage.
async fn pump_link(
    me: usize,
    peer: usize,
    stream: TcpStream,
    in_tx: &UnboundedSender<NodeMsg>,
    out_rx: &mut UnboundedReceiver<NodeMsg>,
) -> anyhow::Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    wr.write_all(&encode_frame(&NodeMsg::Hello {
        proto: NODE_PROTO,
        node: me,
    }))
    .await?;
    let mut fr = FrameReader::new();
    match fr.next_frame(&mut rd).await? {
        Some(NodeMsg::Hello { proto, .. }) if proto == NODE_PROTO => {}
        Some(other) => anyhow::bail!("unexpected cluster reply {other:?}"),
        None => anyhow::bail!("peer closed during handshake"),
    }
    tracing::info!(peer, "cluster dial link up");
    pump_both(peer, rd, wr, fr, in_tx, out_rx).await
}

/// Full-duplex pump: outbound queue -> wire, wire -> in_tx.
async fn pump_both(
    peer: usize,
    mut rd: OwnedReadHalf,
    mut wr: OwnedWriteHalf,
    mut fr: FrameReader,
    in_tx: &UnboundedSender<NodeMsg>,
    out_rx: &mut UnboundedReceiver<NodeMsg>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            frame = out_rx.recv() => {
                match frame {
                    Some(msg) => wr.write_all(&encode_frame(&msg)).await?,
                    None => return Ok(()), // mesh shutting down
                }
            }
            read = fr.next_frame(&mut rd) => {
                match read {
                    Ok(Some(msg)) => {
                        tracing::trace!(peer, kind = msg_name(&msg), "cluster rx");
                        let _ = in_tx.send(msg);
                    }
                    Ok(None) => {
                        tracing::debug!(peer, "cluster peer closed");
                        anyhow::bail!("peer closed");
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
}

/// Acceptor-side one-way pump (the acceptor never writes after Hello).
async fn pump_read(
    peer: usize,
    mut rd: OwnedReadHalf,
    mut fr: FrameReader,
    in_tx: UnboundedSender<NodeMsg>,
) {
    loop {
        match fr.next_frame(&mut rd).await {
            Ok(Some(msg)) => {
                let _ = in_tx.send(msg);
            }
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(peer, error = %e, "cluster read error");
                return;
            }
        }
    }
}

/// Stable short name for trace logs (NodeMsg has no Display).
fn msg_name(msg: &NodeMsg) -> &'static str {
    match msg {
        NodeMsg::Hello { .. } => "hello",
        NodeMsg::Ping => "ping",
        NodeMsg::Sub { .. } => "sub",
        NodeMsg::Unsub { .. } => "unsub",
        NodeMsg::Chat { .. } => "chat",
        NodeMsg::GuestAnnounce(_) => "guest_announce",
        NodeMsg::GuestUpdate(_) => "guest_update",
        NodeMsg::GuestRetract { .. } => "guest_retract",
        NodeMsg::GuestTransfer(_) => "guest_transfer",
        NodeMsg::RelayAttack { .. } => "relay_attack",
        NodeMsg::RelayStaticAct { .. } => "relay_static_act",
        NodeMsg::StaticAck { .. } => "static_ack",
        NodeMsg::RelayPlantAct { .. } => "relay_plant_act",
        NodeMsg::PlantAck { .. } => "plant_ack",
        NodeMsg::RelayPlowAct { .. } => "relay_plow_act",
        NodeMsg::PlowAck { .. } => "plow_ack",
        NodeMsg::TileMutation { .. } => "tile_mutation",
        NodeMsg::RelayStationAct { .. } => "relay_station_act",
        NodeMsg::StationAck { .. } => "station_ack",
        NodeMsg::RelayStationItem { .. } => "relay_station_item",
        NodeMsg::StationItemAck { .. } => "station_item_ack",
        NodeMsg::FightBars { .. } => "fight_bars",
        NodeMsg::CharQuery { .. } => "char_query",
        NodeMsg::CharData { .. } => "char_data",
        NodeMsg::CharAck { .. } => "char_ack",
        NodeMsg::CharNack { .. } => "char_nack",
        NodeMsg::PlayerHurt { .. } => "player_hurt",
        NodeMsg::KillCredit { .. } => "kill_credit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg_roundtrip(msg: NodeMsg) {
        let frame = encode_frame(&msg);
        let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        assert_eq!(frame.len(), 4 + len);
        let mut fr = FrameReader::new();
        fr.buf.extend_from_slice(&frame);
        let mut null = tokio::io::duplex(64).0;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let got = rt.block_on(async { fr.next_frame(&mut null).await });
        match got {
            Ok(Some(m)) => assert_eq!(m, msg),
            other => panic!("roundtrip failed for {msg:?}: {other:?}"),
        }
    }

    #[test]
    fn codec_roundtrips_every_message_kind() {
        msg_roundtrip(NodeMsg::Hello {
            proto: NODE_PROTO,
            node: 1,
        });
        msg_roundtrip(NodeMsg::Ping);
        msg_roundtrip(NodeMsg::Sub {
            from: 1,
            cells: vec![(0, 0), (-3, 7), (i32::MAX / 250, i32::MIN / 250)],
        });
        msg_roundtrip(NodeMsg::Unsub {
            from: 0,
            cells: vec![(1, 1)],
        });
        msg_roundtrip(NodeMsg::Chat {
            from: "developer".into(),
            at: (-1200, 900),
            text: "привет from the mesh".into(),
        });
        msg_roundtrip(NodeMsg::GuestAnnounce(GuestState {
            id: 0x0001_0003,
            pos: (1234, -5678),
            mv: Some(GuestLinMove {
                sx: 1,
                sy: 2,
                tx: 3,
                ty: 4,
                steps: 10,
                step: 3,
                started_ms: 42,
                total_ms: 5000,
            }),
            moving: true,
            facing: 7,
            kind: GuestKind::Animal { species: 2 },
            hp: 50,
            max_hp: 50,
            speed: 33,
        }));
        msg_roundtrip(NodeMsg::GuestUpdate(GuestState {
            id: 7,
            pos: (10, 10),
            mv: None,
            moving: false,
            facing: 3,
            kind: GuestKind::Player {
                name: "tester".into(),
                equip: vec!["pants-linen".into()],
            },
            hp: 100,
            max_hp: 100,
            speed: 50,
        }));
        msg_roundtrip(NodeMsg::GuestRetract { id: 0x0002_0004 });
        msg_roundtrip(NodeMsg::GuestTransfer(GuestState {
            id: 0x0003_0005,
            pos: (0, 0),
            mv: None,
            moving: false,
            facing: 1,
            kind: GuestKind::Static {
                res_name: "gfx/terobjs/items/branch".into(),
                class: StaticClass::Drop,
                crop: None,
                station: None,
                stage: None,
                drop: None,
            },
            hp: 1,
            max_hp: 1,
            speed: 0,
        }));
        msg_roundtrip(NodeMsg::RelayAttack {
            attacker: 0x0001_0007,
            target: 0x0002_0003,
            chip: 42,
            dmg: 5,
        });
        msg_roundtrip(NodeMsg::FightBars {
            id: 0x0002_0003,
            def: 5000,
        });
        msg_roundtrip(NodeMsg::PlayerHurt {
            player_gob: 0x0001_0007,
            dmg: 2,
            from: 0x0002_0003,
        });
        msg_roundtrip(NodeMsg::KillCredit {
            player_gob: 0x0001_0007,
            lp: 10,
        });
        msg_roundtrip(NodeMsg::RelayStaticAct {
            player: 0x0002_0007,
            target: 0x0001_0009,
            act: StaticAct::Chop,
        });
        msg_roundtrip(NodeMsg::StaticAck {
            player: 0x0002_0007,
            stack: Some(StaticStack {
                res: "gfx/invobjs/wood".into(),
                count: 1,
                ql: 10,
                label: String::new(),
            }),
            lp: 0,
        });
    }

    #[test]
    fn frame_reader_reassembles_split_and_coalesced_writes() {
        let msgs = vec![
            NodeMsg::Ping,
            NodeMsg::GuestRetract { id: 9 },
            NodeMsg::Sub {
                from: 0,
                cells: vec![(1, 2)],
            },
        ];
        let mut wire = Vec::new();
        for m in &msgs {
            wire.extend_from_slice(&encode_frame(m));
        }
        // Feed the whole stream in one read (coalesced frames).
        let mut fr = FrameReader::new();
        fr.buf.extend_from_slice(&wire);
        let (mut a, _b) = tokio::io::duplex(64);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let got = rt.block_on(async {
            let mut out = Vec::new();
            for _ in 0..3 {
                out.push(fr.next_frame(&mut a).await.unwrap().unwrap());
            }
            out
        });
        assert_eq!(got, msgs);
    }

    #[test]
    fn frame_length_cap_rejects_bogus_prefix() {
        let mut fr = FrameReader::new();
        fr.buf
            .extend_from_slice(&(MAX_FRAME as u32 + 1).to_le_bytes());
        let (mut a, _b) = tokio::io::duplex(64);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let got = rt.block_on(async { fr.next_frame(&mut a).await });
        assert!(got.is_err(), "oversized frame must be a protocol error");
    }

    #[test]
    fn membership_parses_and_rejects_bad_lists() {
        let cfg =
            ClusterConfig::parse("127.0.0.1:7100,127.0.0.1:7101", 1).expect("valid two-node list");
        assert_eq!(cfg.node_count(), 2);
        assert_eq!(
            cfg.listen(),
            "127.0.0.1:7101".parse::<SocketAddr>().unwrap()
        );
        let peers: Vec<(usize, SocketAddr)> = cfg.peers().collect();
        assert_eq!(peers, vec![(0, "127.0.0.1:7100".parse().unwrap())]);

        assert!(ClusterConfig::parse("", 0).is_err());
        assert!(
            ClusterConfig::parse("127.0.0.1:7100", 0).is_err(),
            "one node is not a cluster"
        );
        assert!(ClusterConfig::parse("127.0.0.1:7100,127.0.0.1:7101", 2).is_err());
        assert!(ClusterConfig::parse("127.0.0.1:7100,nonsense", 0).is_err());
        assert!(ClusterConfig::parse("127.0.0.1:7100,127.0.0.1:7100", 0).is_err());
    }
}
