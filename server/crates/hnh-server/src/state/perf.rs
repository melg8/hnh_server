//! Tick attribution counters (the perf report's fields).

#[derive(Default)]
pub struct Perf {
    pub last_tick_us: u128,
    pub max_tick_us: u128,
    /// Max tick cost since the LAST perf report (the 5 s reporter window):
    /// attributes ramp-up spikes to their window. The lifetime maximum
    /// never resets, so a single early spike made every later report read
    /// 197-210 ms regardless of the current steady state.
    pub window_max_tick_us: u128,
    /// Exponential moving average of tick cost (stable steady-state number
    /// for load reports; 50-tick half-life).
    pub mean_tick_us: u64,
    pub active_sessions: usize,
    pub visible_total: usize,
    pub spawned_objects: usize,
    /// Per-phase microseconds of the last tick: [movement, ai, combat,
    /// vitals, visibility]. Load-test hot-loop attribution.
    pub phase_us: [u128; 9],
    /// Visibility optimization counters (cumulative): total gob distance
    /// checks issued by scans, session-ticks skipped entirely, and the
    /// live cell count. Log-time proof the index is active.
    pub vis_gob_scans: u64,
    pub vis_skipped: u64,
    /// Session-ticks served from the cached scan result (patch or clean
    /// skip) instead of a full view rescan (session 30).
    pub vis_cached: u64,
    /// Session-ticks served by the DELTA scan (session 85): the viewer
    /// moved within one VIEW_RADIUS of the last scan, the cache was
    /// re-filtered and the strip cells added instead of a full square
    /// walk. vis_skipped + vis_cached + vis_delta = scans issued.
    pub vis_delta: u64,
    pub vis_cells: usize,
    /// Node-link publishes sent (cumulative) and guest rows ingested
    /// (cumulative) - cluster-mode counters for the perf report.
    pub guest_pub: u64,
    pub guest_ingests: u64,
    /// Last-tick visibility sub-phase attribution (microseconds):
    /// candidate scan (phase A/A2), spawn application, retract sweep.
    /// Load-test hot-loop attribution inside the visibility phase.
    pub vis_scan_us: u64,
    pub vis_spawn_us: u64,
    /// Gobs spawned into session views this tick (spawn-cost attribution:
    /// `vis_spawn_us / max(vis_spawns, 1)` is the per-spawn price).
    pub vis_spawns: u64,
    pub vis_retract_us: u64,
    /// Last-tick guest-phase sub-attribution (microseconds): guest row
    /// progress + block encode, per-session fan-out, rest-pose streaming.
    /// Load-test attribution inside the guests phase.
    pub guests_encode_us: u64,
    pub guests_fanout_us: u64,
    pub guests_pose_us: u64,
    /// Last-tick combat-phase sub-attribution (microseconds, session 43):
    /// the once-per-tick lookup-index build, the player-side melee loop,
    /// the animal retaliation loop, and the guest relay loop. Attribution
    /// before optimization (perf-profile-first): the session-42 combat
    /// p95 spikes (39 ms) were unattributed; these four counters decide
    /// whether the next cut targets PvP, animals or relays.
    pub combat_index_us: u64,
    pub combat_players_us: u64,
    pub combat_animals_us: u64,
    pub combat_relay_us: u64,
    /// Combat player-phase event counters (last tick): chase `start_move`
    /// calls (count + accumulated microseconds), in-reach swing-path
    /// entries, and landed PvP hits (count + accumulated hit-tail
    /// microseconds: hurt + chat + FX broadcast + log). Splits the 18 ms
    /// player-phase mean the session-43 load run measured.
    pub combat_chase_n: u64,
    pub combat_chase_us: u64,
    pub combat_swing_n: u64,
    pub combat_hit_n: u64,
    pub combat_hit_us: u64,
    /// Last-tick `start_move` sub-attribution (microseconds, cumulative
    /// over the tick's calls): path check (interpolated_pos, path_clear,
    /// terrain read), viewer fan-out (LINBEG scan, send, unacked) and the
    /// pose layer stream. `mv_calls` splits per-call averages.
    pub mv_path_us: u64,
    pub mv_viewers_us: u64,
    pub mv_pose_us: u64,
    pub mv_calls: u64,
    /// Last-tick movement-tick sub-attribution (microseconds): the O(alive)
    /// mover scan (interpolated pos + dirty marks, no encoding), the packed
    /// block encode + batch push, and the per-session packed fan-out
    /// (`broadcast_batch`, both the mid-tick movement batch and the tick-end
    /// start/FX batch). Splits the phase_mv_us top-line the way
    /// combat_index_us et al. split phase_combat_us.
    pub mvbat_scan_us: u64,
    pub mvbat_encode_us: u64,
    pub mvbat_fanout_us: u64,
    /// Last-tick mover count (alive authority gobs carrying a LinMove) and
    /// the packed-block count they produced - per-mover cost attribution:
    /// `mvbat_encode_us / max(movers,1)` is the per-mover encode price.
    pub mvbat_movers: u64,
    /// Last-tick viewer-index diagnostics: mean visible set size across
    /// sessions (visible_total / sessions) and the total candidate count
    /// the cell-index fan-outs probed (`ix_cand_n`). Decides whether the
    /// fan-out cost is the candidate walk or the pair work.
    pub ix_cand_n: u64,
    /// Last-tick packed movement batch: block count and distinct cell
    /// count (the fan-out probe width). Zero with no movers.
    pub move_blocks: u64,
    pub move_cells: u64,
    /// Last-tick fan-out pair attribution (both batches): (session, block)
    /// pairs the dense cell walk probed, pairs that passed the visible
    /// filter (bytes appended), finalizer `record_unacked` calls (the
    /// per-pair block copy), and raw datagrams sent. Decides where the
    /// pair cost lives: probe count vs hit count vs finalizer copies.
    pub fanout_pairs: u64,
    pub fanout_hits: u64,
    pub fanout_fin: u64,
    pub fanout_msgs: u64,
    /// Phase snapshot taken when a tick set the current window maximum
    /// (`window_max_tick_us`): answers "which phase owns the spike"
    /// without a second profiling run. Reset with the window.
    pub wmax_phase_us: [u128; 9],
    pub wmax_mvbat_scan_us: u64,
    pub wmax_mvbat_encode_us: u64,
    pub wmax_mvbat_fanout_us: u64,
    pub wmax_retx_sweep_us: u64,
    /// Last-tick packed start/FX batch (session 44): LINBEG starts + FX
    /// overlays encoded once and fanned out at tick end. Block count (mv
    /// + fx attribution: `fx_batch_n` counts the FX subset).
    pub start_blocks: u64,
    pub fx_batch_n: u64,
    /// Last-sweep OBJDATA retransmission attribution (session 65; the
    /// sweep runs every 3rd tick, so these are per-sweep, not per-tick):
    /// wall time of the sweep, pending blocks walked, blocks actually
    /// resent and raw-queue-full refusals. The load-bot cohort never
    /// echoes OBJACK, so at bot scale the pending table is NOT empty -
    /// these numbers decide whether the sweep is free or needs a fast
    /// path.
    pub retx_sweep_us: u64,
    pub retx_pending: u64,
    pub retx_resent: u64,
    pub retx_queue_full: u64,
    /// Sessions whose unacked table was non-empty during the last sweep
    /// (the real walk depth; total sessions is a misleading denominator).
    pub retx_busy_sessions: u64,
    /// Last-tick tail attribution (session 82): wall time of everything
    /// AFTER the nine phase timers - the retransmit sweep, the criminal/
    /// leash/production sweeps, the dirty-index clear, the tick-end
    /// start/FX batch fan-out, and the perf bookkeeping. Closes the
    /// attribution gap between sum(phase_us) + mvbat_fanout_us +
    /// retx_sweep_us and last_tick_us; the S82 baseline measured that
    /// gap at 45-70 ms on burst ticks (wmax 105-181 ms over budget).
    pub tail_us: u64,
    /// Last-tick fan-out share of the tick-end start/FX batch alone
    /// (`broadcast_batch` called from `tick`, not from `tick_movement`).
    /// mvbat_fanout_us accumulates BOTH batch fan-outs, so this field
    /// splits the movement-batch cost from the start/FX-batch cost.
    pub startbat_fanout_us: u64,
    /// Retire pass share of the retransmit sweep (session 82): the
    /// expired-block retention walk runs AFTER `retx_sweep_us` is
    /// recorded, so it previously landed in the unattributed tail.
    /// `retx_retire_us + sweeps_us + startbat_fanout_us + retx_sweep_us`
    /// now account for the whole `tail_us`.
    pub retx_retire_us: u64,
    /// Gob entries dropped by the targeted retire pass (session 82):
    /// confirms the pass touches only the swept sessions instead of the
    /// whole table (compare against retx_busy_sessions).
    pub retx_retired_gobs: u64,
    /// Last-tick rare-event sweeps + dirty-index clear share of the tick
    /// tail: criminal expiry, leash break, tamed production, vis dirty
    /// clear. Expected near zero in the load steady state; a spike here
    /// names the sweep that owns it.
    pub sweeps_us: u64,
}
