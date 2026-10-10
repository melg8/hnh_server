//! A connected client's outbound sinks and per-session UI
//! bookkeeping.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use super::retrans::{BlockBytes, PerGobPending};
use super::{GobId, InvStack, SessionId};
use crate::resources::ResTable;

/// A connected, in-world client's outbound message sinks.
///
/// Two channels mirror the legacy split: widget/control traffic rides the
/// reliable MSG_REL stream; MAPDATA/OBJDATA travel as raw datagrams with
/// their own ack/retransmit regimes (client re-requests lost grids, and
/// OBJACK gates gob state retransmission).
pub struct SessionOut {
    #[allow(dead_code)] // echoed in session teardown bookkeeping
    pub sid: SessionId,
    /// Authenticated login account (save keys are account-scoped).
    pub account: String,
    pub queue: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// Bounded unreliable datagram fan-out (MAPDATA / OBJDATA); send_raw
    /// drops on a full queue exactly like a lost UDP datagram. Session
    /// 78: ships `BlockBytes` so retransmit resends enqueue the inline
    /// buffer with no allocation on the tick thread; the receiver sends
    /// `as_slice()` to the socket.
    pub raw: tokio::sync::mpsc::Sender<crate::state::BlockBytes>,
    pub player_gob: Option<GobId>,
    /// Gobs currently streamed to this client. Keys are server-allocated
    /// gob ids -> fxhash id hasher (hot per-candidate membership checks).
    pub visible: crate::fxhash::FxHashSet<GobId>,
    /// Slot-index bitset mirror of `visible` (fan-out fast path): bit
    /// `slot` set iff some visible gob occupies that slot. Gob ids pack
    /// the slot into the low 16 bits and a live gob owns its slot
    /// exclusively, so the mirror is EXACT as long as it is updated at
    /// the same three sites that mutate `visible` (spawn, retract,
    /// guest spawn) - asserted by the state tests. The fan-out pair loop
    /// probes one aligned word here (~1 L1 load) instead of hashing into
    /// the scattered set; the set stays authoritative and re-checks every
    /// bit-set pair, so a stale bit can only cost one probe, never a
    /// wrong send. Empty until the first insert grows it.
    pub visible_bits: Vec<u64>,
    /// Exponential moving average of the peer's ack lag in milliseconds
    /// (sampled when an OBJACK newly confirms a pending block's frame:
    /// `now - last_sent` of the highest newly-acked block). The retransmit
    /// sweep raises a session's first-retry delay to twice this average
    /// (capped): a client that acks in ~200 ms batches (the load-bot
    /// cohort mirrors the real client's batched echo) must not see its
    /// blocks retried at the 80 ms schedule first - the measured sweep
    /// resent 73K not-yet-lost blocks per pass because the schedule
    /// expired before the ack landed. 0 = no sample yet (legacy delays).
    pub ack_lag_ema_ms: u32,
    /// Unacked OBJDATA blocks per gob, ordered by frame ascending (the
    /// retransmit sweep walks frames ASCENDING so resent datagrams keep
    /// the wire order a spawn -> move -> retract sequence had on the
    /// socket; a resent stale frame must never overtake a newer one).
    /// Session 78: flat `Vec` per gob (was `BTreeMap` - a heap node per
    /// block with the per-gob cap of 4) over inline `BlockBytes` (was
    /// `Vec<u8>` deep-cloned on every resend). Capped per gob (see
    /// `record_unacked`).
    pub unacked: crate::fxhash::FxHashMap<GobId, PerGobPending>,
    /// Retransmit backpressure (session 65): while a raw-queue refusal is
    /// recent, the sweep skips this session entirely - retries never fire
    /// into a saturated channel and the walk spends its budget elsewhere.
    pub retx_throttle_until: Instant,
    /// Highest OBJACK-confirmed frame per gob (the client echoes the
    /// max frame it decoded). `None` = nothing confirmed yet. The
    /// Option matters: frame 0 is a VALID wire frame (an untouched
    /// static's first block) and must not be confused with "nothing
    /// acked".
    pub gob_acked: crate::fxhash::FxHashMap<GobId, Option<u32>>,
    /// Widget id counter (session-local uint16 space).
    pub next_wid: u16,
    pub widgets: HashMap<u16, String>,
    /// Pending map requests (grid coord -> requested tick).
    #[allow(dead_code)] // pending MAPREQ dedup, consumed by grid streaming
    pub mapreqs: HashSet<(i32, i32)>,
    /// Session-local resource id table.
    pub res: ResTable,
    /// Fightview window state (widget id + per-opponent relations).
    pub fight: crate::fight::FightState,
    /// Open makewindow recipe id (`craft.rs::Recipe::id`), if any.
    pub craft_recipe: Option<String>,
    /// Widget id of the open makewindow, paired with `craft_recipe`.
    pub craft_window: Option<u16>,
    /// Inventory stack index whose right-click opened the flower menu,
    /// paired with the `sm` widget id (eat flow).
    pub item_menu: Option<(u16, usize)>,
    /// Item widget id -> inventory stack index (iact routing).
    pub item_wids: HashMap<u16, usize>,
    /// Open harvest flower menu: `sm` widget id -> target crop gob.
    pub crop_menu: Option<(u16, GobId)>,
    /// Open production flower menu (session 47): `sm` widget id ->
    /// target tamed-animal gob (Milk on a cow, Shear on a sheep).
    pub animal_menu: Option<(u16, GobId)>,
    /// Widget id of the Area Chat window (`slenchat`), 0 = none.
    pub chat_wid: u16,
    /// Widget id of the party roster (`pv`), 0 = none.
    pub party_wid: u16,
    /// Open player flower menu and what it arms (party invite
    /// choreography; see `party::PlayerMenu`).
    pub player_menu: Option<(u16, crate::party::PlayerMenu)>,
    /// Plow Field pagina armed: next map click plows the tile.
    pub pending_plow: bool,
    /// Build pagina armed (`build::BUILDABLES` index): the mapview ghost
    /// is up and the next mapview `place` wdgmsg commits it.
    pub pending_build: Option<usize>,
    /// Open station flower menu: `sm` widget id + target station gob.
    /// The third element carries the GUEST act intent (session 33):
    /// `None` for a local station (the choice resolves against the
    /// authoritative local state), `Some(Light/Extinguish)` for a guest
    /// station (the act was picked from the piggybacked snapshot and is
    /// relayed to the authority, which re-validates it).
    pub station_menu: Option<(u16, GobId, Option<crate::nodes::StationAct>)>,
    /// Pending Food Trough flower menu (session 62): (sm widget id, the
    /// clicked trough gob). The Lift choice is resolved against the
    /// authority state at apply time, like the station menu.
    pub trough_menu: Option<(u16, GobId)>,
    /// Item stack currently held on the cursor (take -> itemact flow).
    pub cursor: Option<InvStack>,
    /// Widget id of the drag Item widget following the mouse (the
    /// cursor stack's on-screen body). Kept in sync by
    /// `game::sync_cursor_widget` after every cursor mutation.
    pub cursor_wid: Option<u16>,
    /// Map grids this client already holds (MAPDATA re-send targeting).
    pub grids_seen: HashSet<(i32, i32)>,
    /// Last visibility scan result (session 30 result caching). Valid
    /// only while the player position is unchanged; a session standing
    /// still then patches this list from the per-tick touched records
    /// instead of rescanning the whole view square.
    pub vis_cache: Option<Vec<GobId>>,
    /// The exact player position the cached list is valid for.
    pub vis_cache_pos: Option<(i32, i32)>,
    /// Last tick's player cell (dirty-cell skip decision; None = first
    /// tick, which always scans).
    pub vis_cell: Option<(i32, i32)>,
    /// Tick of the session's last retract sweep (session 42 spawn-debounce:
    /// a gob oscillating across the 2x-VIEW_RADIUS boundary must not be
    /// retracted and re-spawned every tick of a moving session; the sweep
    /// runs at most every RETRACT_SWEEP_EVERY ticks regardless of cell
    /// crossings, so a quick boundary return never sees a retract).
    pub last_retract_tick: u64,
}

impl SessionOut {
    /// Set the visible bit for `slot`, growing the mirror as needed.
    /// Called only from the three sites that insert into `visible`.
    #[inline]
    pub fn vis_bit_insert(&mut self, slot: usize) {
        let word = slot / 64;
        if word >= self.visible_bits.len() {
            self.visible_bits.resize(word + 1, 0);
        }
        self.visible_bits[word] |= 1u64 << (slot % 64);
    }

    /// Clear the visible bit for `slot` (retract path). A slot beyond the
    /// current mirror length was never set - nothing to do.
    #[inline]
    pub fn vis_bit_remove(&mut self, slot: usize) {
        let word = slot / 64;
        if let Some(w) = self.visible_bits.get_mut(word) {
            *w &= !(1u64 << (slot % 64));
        }
    }

    /// Fan-out fast path: whether the slot's bit is set. `false` means
    /// the gob id is DEFINITELY not visible; `true` still requires the
    /// exact `visible.contains` confirmation (the set is authoritative).
    #[inline]
    pub fn vis_bit_probe(&self, id: GobId) -> bool {
        let slot = (id & 0xFFFF) as usize;
        self.visible_bits
            .get(slot / 64)
            .is_some_and(|w| w & (1u64 << (slot % 64)) != 0)
    }

    /// Widget id of the character sheet window, if created.
    pub fn chr_window(&self) -> Option<u16> {
        self.widgets
            .iter()
            .find(|(_, t)| t.as_str() == "chr")
            .map(|(id, _)| *id)
    }

    pub fn new_wid(&mut self, tag: &str) -> u16 {
        let id = self.next_wid;
        self.next_wid = self.next_wid.wrapping_add(1).max(1);
        self.widgets.insert(id, tag.to_owned());
        id
    }

    /// Queue a reliable RMSG sub-message payload.
    pub fn send(&self, payload: Vec<u8>) {
        // The only error is a closed receiver (session gone); dropping the
        // payload then is correct.
        let _ = self.queue.send(payload);
    }

    /// Send a raw datagram (MSG_MAPDATA / MSG_OBJDATA), unreliable. A full
    /// bounded queue means the per-session sender task is starved; dropping
    /// the newest datagram matches the protocol's UDP semantics (the client
    /// re-requests lost grids; LINSTEP progress self-heals next tick).
    pub fn send_raw(&self, datagram: Vec<u8>) {
        let _ = self.raw.try_send(BlockBytes::from_vec(datagram));
    }
}
