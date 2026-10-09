//! Shared test helpers (setup builders, wire drains, cluster
//! rigs). pub(super): every sibling theme module uses these.
use super::super::*;
use std::num::NonZeroUsize;

/// Test harness: game + widget-msg channel + raw-block channel + mesh
/// publish sink (what this node would send to its peers).
type ClusterHarness = (
    Game,
    tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    tokio::sync::mpsc::Receiver<crate::state::BlockBytes>,
    tokio::sync::mpsc::UnboundedReceiver<(usize, crate::nodes::NodeMsg)>,
);

/// Enter the world as `name` on a fresh single-session game and return
/// the game plus the outgoing message receivers (shared setup for the
/// equipment tests).
pub(super) fn entered_game(
    name: &str,
) -> (
    Game,
    tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    tokio::sync::mpsc::Receiver<crate::state::BlockBytes>,
) {
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut g = Game::new(
        42,
        cmd_rx,
        net_rx,
        false,
        std::env::temp_dir().join(format!("hnh-equip-test-{}.json", name)),
    );
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, raw_rx) = tokio::sync::mpsc::channel::<crate::state::BlockBytes>(512);
    g.session_connected(1, "acct".to_owned(), tx, raw_tx);
    let wid = g
        .sessions
        .get(&1)
        .unwrap()
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "charlist")
        .map(|(k, _)| *k)
        .expect("charlist widget");
    g.on_wdgmsg(
        1,
        wid,
        "play",
        vec![hnh_proto::ListArg::Str(name.to_owned())],
    );
    for _ in 0..3 {
        g.tick();
    }
    (g, rx, raw_rx)
}

/// Extract the (op, layer wire ids) pairs from one raw OBJDATA block
/// (the same layout the spawn-block test walks).
pub(super) fn objdata_layer_lists(block: &[u8]) -> Vec<(u8, Vec<u16>)> {
    if block.len() < 10 || block[0] != MSG_OBJDATA {
        return Vec::new();
    }
    let mut off = 10;
    let mut out = Vec::new();
    while off < block.len() {
        let code = block[off];
        off += 1;
        match code {
            OD_END => break,
            OD_RES => {
                let wire = u16::from_le_bytes([block[off], block[off + 1]]);
                off += 2;
                if wire & 0x8000 != 0 {
                    let n = block[off] as usize;
                    off += 1 + n;
                }
            }
            OD_MOVE => off += 8,
            OD_LINBEG => off += 20,
            OD_LINSTEP => off += 4,
            OD_LAYERS | OD_AVATAR => {
                let mut ids = Vec::new();
                while off + 2 <= block.len() {
                    let id = u16::from_le_bytes([block[off], block[off + 1]]);
                    off += 2;
                    if id == 65535 {
                        break;
                    }
                    ids.push(id);
                }
                out.push((code, ids));
            }
            OD_HEALTH => off += 1,
            OD_BUDDY => break,
            _ => break,
        }
    }
    out
}

pub(super) fn predator_slot(g: &Game) -> usize {
    for &id in &g.world.animal_gobs {
        if let Some(slot) = g.world.gobs.get(id) {
            let aggro = matches!(
                g.world.gobs.kind[slot],
                Kind::Animal { species } if species.aggressive()
            );
            if aggro {
                return slot;
            }
        }
    }
    panic!("BUG: no predator");
}

/// A game wired for node `me` of a `nodes`-node cluster WITHOUT a real
/// mesh: guest publishes land in a drainable channel (mesh_rx), which
/// lets tests assert exactly what this node would send to its peers.
pub(super) fn clustered_game(name: &str, me: usize, nodes: usize) -> ClusterHarness {
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut g = Game::new(
        42,
        cmd_rx,
        net_rx,
        false,
        std::env::temp_dir().join(format!("hnh-cluster-test-{}.json", name)),
    );
    let (mesh_tx, mut mesh_rx) = tokio::sync::mpsc::unbounded_channel();
    let nz = NonZeroUsize::new(nodes).expect("nodes");
    g.world = World::with_layout(42, nz, me);
    g.cluster = Some(Cluster {
        me,
        nodes: nz,
        mesh: crate::nodes::Mesh { out_tx: mesh_tx },
        peer_subs: HashMap::new(),
        my_subs: HashMap::new(),
        player_abroad: crate::fxhash::FxHashMap::default(),
    });
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, raw_rx) = tokio::sync::mpsc::channel::<crate::state::BlockBytes>(4096);
    g.session_connected(1, "acct".to_owned(), tx, raw_tx);
    let wid = g
        .sessions
        .get(&1)
        .unwrap()
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "charlist")
        .map(|(k, _)| *k)
        .expect("charlist widget");
    g.on_wdgmsg(
        1,
        wid,
        "play",
        vec![hnh_proto::ListArg::Str(name.to_owned())],
    );
    // Simulate the cluster answering the character-migration query:
    // every peer nacks a save key it does not hold, which completes
    // the world entry in one mesh round trip (no 2 s deadline wait).
    if g.cluster.is_some() {
        let nodes = g.cluster.as_ref().map(|c| c.nodes.get()).unwrap_or(0);
        let me = g.cluster.as_ref().map(|c| c.me).unwrap_or(0);
        let mut names: Vec<String> = Vec::new();
        while let Ok((_, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::CharQuery { name, .. } = msg {
                names.push(name);
            }
        }
        for name in names {
            for peer in 0..nodes {
                if peer != me {
                    g.on_node_msg(crate::nodes::NodeMsg::CharNack {
                        to: me,
                        from: peer,
                        name: name.clone(),
                    });
                }
            }
        }
    }
    for _ in 0..3 {
        g.tick();
    }
    (g, rx, raw_rx, mesh_rx)
}

/// First position around (px, py) whose VisIndex cell belongs to a node
/// other than `me` (scans outward; cells are 250 subtiles).
pub(super) fn foreign_cell_pos(g: &Game, me: usize) -> (i32, i32) {
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let slot = g.world.gobs.get(pgob).expect("player gob");
    let (px, py) = g.world.gobs.pos[slot];
    for r in [260, 400, 600, 900, 1300, 1800] {
        for (dx, dy) in [(r, 0), (0, r), (-r, 0), (0, -r), (r, r), (-r, -r)] {
            let c = crate::visidx::cell_of(px + dx, py + dy);
            let owner = match &g.cluster {
                Some(cl) => crate::grid_owner::owner_of(c, cl.nodes),
                None => 0,
            };
            if owner != me {
                return (px + dx, py + dy);
            }
        }
    }
    panic!("no foreign cell found near spawn");
}

/// First position around the player whose VisIndex cell belongs to
/// `me` (mirror of foreign_cell_pos for home-ground fixtures).
pub(super) fn home_cell_pos(g: &Game, me: usize) -> (i32, i32) {
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let slot = g.world.gobs.get(pgob).expect("player gob");
    let (px, py) = g.world.gobs.pos[slot];
    for r in [11, 130, 260, 400, 600, 900, 1300, 1800] {
        for (dx, dy) in [(r, r), (0, r), (r, 0), (-r, -r), (0, -r), (-r, 0)] {
            let c = crate::visidx::cell_of(px + dx, py + dy);
            let owner = match &g.cluster {
                Some(cl) => crate::grid_owner::owner_of(c, cl.nodes),
                None => 0,
            };
            if owner == me {
                return (px + dx, py + dy);
            }
        }
    }
    panic!("no home cell found near spawn");
}

/// A gob id from another node's allocation range (slot stride).
pub(super) fn foreign_node_gob_id(me: usize, nodes: usize, seq: usize) -> GobId {
    let per = (MAX_SLOT + 1) / nodes;
    let slot = me * per + seq; // my own range is NOT foreign; pick another
    let slot = if slot == me * per + seq {
        (nodes - me - 1) * per + seq + 3
    } else {
        slot
    };
    gob_id_from_slot(slot % (MAX_SLOT + 1), 1)
}

/// Ingest a wolf guest standing `dx` subtiles right of the local
/// player (in view, same cell so node 0 is its authority stand-in for
/// wire tests) and return its id.
pub(super) fn relay_wolf_guest(g: &mut Game, dx: i32, hp: i32) -> GobId {
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let gid = foreign_node_gob_id(0, 2, 7);
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(
        crate::nodes::GuestState {
            id: gid,
            pos: (px + dx, py),
            mv: None,
            moving: false,
            facing: 0,
            kind: crate::nodes::GuestKind::Animal {
                species: Species::Wolf.index(),
            },
            hp,
            max_hp: hp,
            speed: 33,
        },
    ));
    g.tick();
    assert!(
        g.sessions[&1].visible.contains(&gid),
        "relay test precondition: the guest wolf must spawn in view"
    );
    gid
}

/// A clustered game with NO session: the raw fixture for save-store
/// and node-msg level tests. Returns the mesh sink receiver so tests
/// can assert exactly what this node sends to its peers.
pub(super) fn bare_clustered(
    tag: &str,
    me: usize,
    nodes: usize,
) -> (
    Game,
    tokio::sync::mpsc::UnboundedReceiver<(usize, crate::nodes::NodeMsg)>,
) {
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut g = Game::new(
        42,
        cmd_rx,
        net_rx,
        false,
        std::env::temp_dir().join(format!("hnh-save-test-{}.json", tag)),
    );
    let (mesh_tx, mesh_rx) = tokio::sync::mpsc::unbounded_channel();
    let nz = NonZeroUsize::new(nodes).expect("nodes");
    g.world = World::with_layout(42, nz, me);
    g.cluster = Some(Cluster {
        me,
        nodes: nz,
        mesh: crate::nodes::Mesh { out_tx: mesh_tx },
        peer_subs: HashMap::new(),
        my_subs: HashMap::new(),
        player_abroad: crate::fxhash::FxHashMap::default(),
    });
    (g, mesh_rx)
}

/// Open one session on `g` and press play on the charlist (the full
/// login path; cluster nodes may defer the entry on a CharQuery).
pub(super) fn open_session_and_play(g: &mut Game, sid: SessionId, account: &str, chosen: &str) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, _raw) = tokio::sync::mpsc::channel::<crate::state::BlockBytes>(512);
    g.session_connected(sid, account.to_owned(), tx, raw_tx);
    let wid = g.sessions[&sid]
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "charlist")
        .map(|(k, _)| *k)
        .expect("charlist widget");
    g.on_wdgmsg(
        sid,
        wid,
        "play",
        vec![hnh_proto::ListArg::Str(chosen.to_owned())],
    );
}

pub(super) fn snapshot(key: &str, pos: (i32, i32)) -> crate::persist::SavedPlayer {
    crate::persist::SavedPlayer {
        name: key.to_owned(),
        pos,
        hp: 80,
        energy: 70,
        stamina: 60,
        lp: 42,
        attrs: HashMap::from([("str".to_owned(), 12)]),
        inv: Vec::new(),
        inv_labels: Vec::new(),
        skills: Vec::new(),
        equip: Vec::new(),
        criminal_until_ms: None,
        carried_trough: None,
    }
}

/// Test helper: find the only live ground Drop gob.
pub(super) fn only_drop_gob(g: &Game) -> GobId {
    let mut found = None;
    for slot in 0..g.world.gobs.alive.len() {
        if g.world.gobs.alive[slot] && matches!(g.world.gobs.kind[slot], Kind::Drop { .. }) {
            found = Some(gob_id_from_slot(slot, g.world.gobs.gen[slot]));
        }
    }
    found.expect("test precondition: exactly one live Drop gob")
}

pub(super) fn pgob_of(g: &Game) -> GobId {
    let pidx = *g.world.by_session.get(&1).unwrap();
    g.world.players[pidx].gob
}

/// Plant a crop gob directly (restore-path construction, no farming
/// skill gate) near the test player and return its id.
pub(super) fn planted_crop(g: &mut Game, spec: u8, stage: u8) -> GobId {
    let pslot = g.world.gobs.get(pgob_of(g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern("gfx/terobjs/crops/wheat");
    let gob = g
        .world
        .gobs
        .spawn(Kind::Crop { spec, stage }, (px + 30, py), res, 1, 0);
    g.world.crops.insert(
        gob,
        crate::farm::CropState {
            spec,
            stage,
            seed_ql: 10,
            soil_ql: 10,
            next_stage_at: u64::MAX,
        },
    );
    gob
}

/// Find a grass tile whose tile-center VisIndex cell is owned by `me`
/// in a `nodes`-node cluster, near `from`. Probes a widening strip of
/// tiles from `from` until both conditions hold (ownership is a per-
/// cell hash, grass a per-tile roll; both are deterministic).
pub(super) fn grass_tile_on_cell(
    g: &mut Game,
    from: (i32, i32),
    me: usize,
    nodes: usize,
) -> (i32, i32) {
    let nz = std::num::NonZeroUsize::new(nodes).expect("nodes");
    let base = (from.0.div_euclid(11), from.1.div_euclid(11));
    for dy in -40..=40i32 {
        for dx in -40..=40i32 {
            let (tx, ty) = (base.0 + dx, base.1 + dy);
            let cell = crate::visidx::cell_of(tx * 11 + 5, ty * 11 + 5);
            if crate::grid_owner::owner_of(cell, nz) != me {
                continue;
            }
            let gc = (tx.div_euclid(100), ty.div_euclid(100));
            let lx = tx.rem_euclid(100) as usize;
            let ly = ty.rem_euclid(100) as usize;
            if g.world.grids.grid(gc).tile(lx, ly) == tile::GRASS {
                return (tx, ty);
            }
        }
    }
    panic!("no grass tile on my cells near {from:?}");
}

/// A finished oven beside the player: the exact rows place_buildable
/// would have produced (Kind::Station + StationState), so tests read
/// like the real world state.
pub(super) fn built_oven(g: &mut Game, fuel: u32, input: Option<(&'static str, u8)>) -> GobId {
    let pslot = g.world.gobs.get(pgob_of(g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern("gfx/terobjs/oven");
    let gob = g.world.gobs.spawn(
        Kind::Station {
            spec: 0,
            lit: false,
        },
        (px + 30, py),
        res,
        1,
        0,
    );
    let input_row = input.map(|(label, ql)| {
        let idx = g.world.res.intern("gfx/invobjs/meat");
        (idx, ql, label)
    });
    g.world.stations.insert(
        gob,
        crate::build::StationState {
            spec: 0,
            fuel,
            fuel_ql_sum: 10 * fuel as u64,
            fuel_seen: fuel as u64,
            input: input_row,
            aux: None,
            lit: false,
            progress: 0,
            quality: 10,
        },
    );
    gob
}

/// Helper: replace a player's inventory with a synthetic stack list.
pub(super) fn set_inv(g: &mut Game, stacks: &[(&'static str, u32, u8)]) {
    let pidx = *g.world.by_session.get(&1).unwrap();
    g.world.players[pidx].inv = stacks
        .iter()
        .map(|(res, count, ql)| InvStack {
            res: g.world.res.intern(res),
            count: *count,
            ql: *ql,
            label: "",
        })
        .collect();
}

/// Session 77: the labeled variant - raw meats share ONE resource and
/// are told apart by the display label (Species::meat_label), so the
/// wurst tests need stacks that carry it.
pub(super) fn set_inv_labeled(g: &mut Game, stacks: &[(&'static str, u32, u8, &'static str)]) {
    let pidx = *g.world.by_session.get(&1).unwrap();
    g.world.players[pidx].inv = stacks
        .iter()
        .map(|(res, count, ql, label)| InvStack {
            res: g.world.res.intern(res),
            count: *count,
            ql: *ql,
            label,
        })
        .collect();
}

/// Equip a bow into slot 0 and put one stack of arrows into the
/// inventory.
pub(super) fn arm_bow(g: &mut Game, pidx: usize, arrows: u32, bow_ql: u8) {
    let bow_gidx = g.world.res.intern("gfx/invobjs/bow");
    g.world.players[pidx].equip[0] = Some(crate::state::InvStack {
        res: bow_gidx,
        count: 1,
        ql: bow_ql,
        label: "",
    });
    if arrows > 0 {
        let arr_gidx = g.world.res.intern("gfx/invobjs/arrow-stone");
        g.world.players[pidx].inv.push(crate::state::InvStack {
            res: arr_gidx,
            count: arrows,
            ql: 10,
            label: "",
        });
    }
}

/// Spawn a Deer at Chebyshev distance `d` (units) from the player
/// with explicit HP (lets hit/miss tests survive a 75-damage shot).
pub(super) fn spawn_deer_at(g: &mut Game, pidx: usize, d: i32, hp: i32) -> crate::state::GobId {
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).expect("player gob");
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern(Species::Deer.resname());
    let id = g.world.gobs.spawn(
        Kind::Animal {
            species: Species::Deer,
        },
        (px + d, py),
        res,
        hp,
        33,
    );
    g.world.animal_gobs.push(id);
    id
}

pub(super) fn arrow_count(g: &mut Game, pidx: usize) -> u32 {
    let arr_gidx = g.world.res.intern("gfx/invobjs/arrow-stone");
    g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == arr_gidx)
        .map(|s| s.count)
        .sum()
}

/// Extract the chat "log" text from one RMSG_WDGMSG frame (used by
/// the aim-progress assertions). Payload after the widget name is
/// a typed list: LIST_STR(2), NUL-terminated text, then the color.
pub(super) fn chat_log_text(msg: &[u8]) -> Option<String> {
    if msg.first() != Some(&RMSG_WDGMSG) || msg.len() < 5 {
        return None;
    }
    let nul = msg[3..].iter().position(|&b| b == 0)? + 3;
    if &msg[3..nul] != b"log" {
        return None;
    }
    let rest = &msg[nul + 1..];
    if rest.first() != Some(&2) {
        return None; // LIST_STR tag
    }
    let end = rest[1..].iter().position(|&b| b == 0)? + 1;
    String::from_utf8(rest[1..end].to_vec()).ok()
}

/// Enter a SECOND local session player (sid 2) and return
/// (player index, gob). Both players spawn at the world spawn.
/// In cluster mode pass the mesh receiver so the character-
/// migration query can be answered with nacks (the same trick
/// `clustered_game` plays for the FIRST player).
pub(super) fn second_player(
    g: &mut Game,
    name: &str,
    mut mesh: Option<&mut tokio::sync::mpsc::UnboundedReceiver<(usize, crate::nodes::NodeMsg)>>,
) -> (usize, GobId) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel::<crate::state::BlockBytes>(512);
    g.session_connected(2, "acct2".to_owned(), tx, raw_tx);
    let wid = g
        .sessions
        .get(&2)
        .unwrap()
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "charlist")
        .map(|(k, _)| *k)
        .expect("charlist widget");
    g.on_wdgmsg(
        2,
        wid,
        "play",
        vec![hnh_proto::ListArg::Str(name.to_owned())],
    );
    // Cluster: answer the character-migration query with nacks
    // from every peer so the world entry completes in one local
    // round (the clustered_game setup does the same for sid 1).
    if g.cluster.is_some() {
        if let Some(mesh) = mesh.as_mut() {
            let nodes = g.cluster.as_ref().map(|c| c.nodes.get()).unwrap_or(0);
            let me = g.cluster.as_ref().map(|c| c.me).unwrap_or(0);
            let mut names: Vec<String> = Vec::new();
            while let Ok((_, msg)) = mesh.try_recv() {
                if let crate::nodes::NodeMsg::CharQuery { name, .. } = msg {
                    names.push(name);
                }
            }
            for name in names {
                for peer in 0..nodes {
                    if peer != me {
                        g.on_node_msg(crate::nodes::NodeMsg::CharNack {
                            to: me,
                            from: peer,
                            name: name.clone(),
                        });
                    }
                }
            }
        }
    }
    g.tick();
    let pidx = *g.world.by_session.get(&2).expect("second player entered");
    (pidx, g.world.players[pidx].gob)
}

/// Announce a cross-node GUEST player at Chebyshev distance `dx`
/// from the local session player and return its guest gob id.
pub(super) fn guest_player(g: &mut Game, dx: i32) -> GobId {
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let gid = foreign_node_gob_id(0, 2, 7);
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(
        crate::nodes::GuestState {
            id: gid,
            pos: (px + dx, py),
            mv: None,
            moving: false,
            facing: 0,
            kind: crate::nodes::GuestKind::Player {
                name: "Rival".to_owned(),
                equip: Vec::new(),
            },
            hp: 100,
            max_hp: 100,
            speed: 33,
        },
    ));
    g.tick();
    gid
}

/// Collect every chat "log" line queued for the test session.
pub(super) fn drain_chat(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) -> Vec<String> {
    let mut seen = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let Some(text) = chat_log_text(&msg) {
            seen.push(text);
        }
    }
    seen
}

pub(super) fn equip_rope(g: &mut Game, pidx: usize) {
    let rope = g.world.res.intern("gfx/invobjs/rope");
    g.world.players[pidx].equip[0] = Some(crate::state::InvStack {
        res: rope,
        count: 1,
        ql: 10,
        label: "",
    });
}

/// Grant the Animal Husbandry skill (plus its documented Hunting
/// prerequisite) directly into the player's owned set.
pub(super) fn grant_ahusb(g: &mut Game, pidx: usize) {
    let p = &mut g.world.players[pidx];
    p.skills.insert("hunting");
    p.skills.insert("ahusb");
}

/// Spawn one animal of any species at a tile offset from the player.
pub(super) fn spawn_species_at(
    g: &mut Game,
    pidx: usize,
    d: i32,
    hp: i32,
    species: Species,
) -> crate::state::GobId {
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).expect("player gob");
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern(species.resname());
    let id = g
        .world
        .gobs
        .spawn(Kind::Animal { species }, (px + d, py), res, hp, 33);
    g.world.animal_gobs.push(id);
    id
}

/// Satisfy the static quell gates (2 IP in the pool, advantage in
/// the tamer's favor past 3) and queue the quell selection.
pub(super) fn arm_quell(g: &mut Game, sid: SessionId, target: GobId) {
    {
        let out = g.sessions.get_mut(&sid).unwrap();
        let rel = out.fight.rel_mut(target).unwrap();
        rel.ip_self = 5;
        rel.adv = 40;
        rel.sync_balance();
    }
    g.on_maneuver(sid, "quell");
    assert_eq!(
        g.sessions.get(&sid).unwrap().fight.atk_cur,
        Some("paginae/atk/quell"),
        "quell selection accepted"
    );
}

/// Force the tile under a gob to `tile` in the LIVE grid (mutate_tile
/// patches the resident grid and records the override), keeping the
/// test independent of the seed's terrain roll at the spawn spot.
pub(super) fn force_tile(g: &mut Game, sub: (i32, i32), tile: u8) {
    let tx = sub.0.div_euclid(11);
    let ty = sub.1.div_euclid(11);
    let gc = (tx.div_euclid(100), ty.div_euclid(100));
    let ix = tx.rem_euclid(100) as usize;
    let iy = ty.rem_euclid(100) as usize;
    g.world.grids.mutate_tile(gc, ix, iy, tile);
}

pub(super) fn full_tame(g: &mut Game, gob: crate::state::GobId, tamer: crate::state::GobId) {
    let mut tame = crate::state::TameState::new(tamer, 0);
    tame.tameness = crate::state::TAMENESS_FULL;
    g.world.tamed.insert(gob, tame);
}

/// Spawn a completed Food Trough at a subtile offset from the
/// player, the way `complete_plan` would leave it.
pub(super) fn built_trough(g: &mut Game, units: u32, ql_sum: u64, ql_seen: u64) -> GobId {
    let pslot = g.world.gobs.get(pgob_of(g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern("gfx/terobjs/trough");
    let gob = g.world.gobs.spawn(
        Kind::Structure {
            spec: crate::build::buildable_by_ad("trough").unwrap() as u8,
        },
        (px + 22, py),
        res,
        1,
        0,
    );
    let tile = ((px + 22).div_euclid(11), py.div_euclid(11));
    g.world.structure_at.insert(tile, gob);
    g.world.troughs.insert(
        gob,
        crate::state::TroughState {
            units,
            ql_sum,
            ql_seen,
        },
    );
    gob
}

/// Click helper: a gob click exactly the shape MapView.wdgmsg("click")
/// sends (c0, mc, button, modflags, gobid, gobrc).
pub(super) fn click_gob_args(gob: GobId, pos: (i32, i32)) -> Vec<hnh_proto::ListArg> {
    vec![
        hnh_proto::ListArg::Coord(0, 0),
        hnh_proto::ListArg::Coord(pos.0, pos.1),
        hnh_proto::ListArg::Int(1),
        hnh_proto::ListArg::Int(0),
        hnh_proto::ListArg::Int(gob),
        hnh_proto::ListArg::Coord(pos.0, pos.1),
    ]
}

/// A finished station beside the player for any BUILDABLES spec index
/// (the built_oven generalization; the same rows place_buildable's
/// completion would have produced).
pub(super) fn built_station(g: &mut Game, spec_index: usize) -> GobId {
    let pslot = g.world.gobs.get(pgob_of(g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern(crate::build::BUILDABLES[spec_index].res);
    let gob = g.world.gobs.spawn(
        Kind::Station {
            spec: spec_index as u8,
            lit: false,
        },
        (px + 30, py),
        res,
        1,
        0,
    );
    g.world.stations.insert(
        gob,
        crate::build::StationState {
            spec: spec_index as u8,
            fuel: 0,
            fuel_ql_sum: 0,
            fuel_seen: 0,
            input: None,
            aux: None,
            lit: false,
            progress: 0,
            quality: 10,
        },
    );
    gob
}

pub(super) fn station_spec_idx(id: &str) -> usize {
    crate::build::BUILDABLES
        .iter()
        .position(|b| b.id == id)
        .unwrap_or_else(|| panic!("{id} must be a BUILDABLES spec"))
}

/// The REAL station itemact path: the stack rides the session cursor
/// and the click targets the station gob (items.rs station dispatch -
/// the same entry the wire client's MapView.iteminteract produces).
pub(super) fn click_station_with_cursor(g: &mut Game, gob: GobId, stack: InvStack) {
    let slot = g.world.gobs.get(gob).unwrap();
    let pos = g.world.gobs.pos[slot];
    g.sessions.get_mut(&1).unwrap().cursor = Some(stack);
    let args = vec![
        hnh_proto::ListArg::Coord(0, 0),
        hnh_proto::ListArg::Coord(pos.0, pos.1),
        hnh_proto::ListArg::Int(0),
        hnh_proto::ListArg::Int(gob),
        hnh_proto::ListArg::Int(0),
    ];
    g.on_map_itemact(1, &args);
}
