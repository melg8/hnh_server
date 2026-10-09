//! Movement timing, batched LINBEG/LINSTEP fan-out, FX patches,
//! gait/dir/pose layer contracts.
use super::super::*;
use super::common::*;

/// The bootstrap stream must announce avatar RESIDs before the
/// charlist add (session-lifecycle.md 3.1).
#[tokio::test]
async fn bootstrap_announces_resids_first() {
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut g = Game::new(
        42,
        cmd_rx,
        net_rx,
        false,
        std::env::temp_dir().join("hnh-game-test-save.json"),
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel(512);
    g.session_connected(1, "acct".to_owned(), tx, raw_tx);
    // Inspect the wire table state after registration.
    {
        let out = g.sessions.get(&1).unwrap();
        eprintln!(
            "WIRE: {:?}",
            (0..out.res.wire_count())
                .map(|w| out.res.pending_announce(w as u16))
                .collect::<Vec<_>>()
        );
    }
    drop(g); // close channels to end the recv loop
    let mut types = Vec::new();
    while let Ok(p) = rx.try_recv() {
        types.push(p[0]);
    }
    {
        eprintln!("TYPES: {:?}", types);
        assert_eq!(&types[..3], &[RMSG_RESID, RMSG_RESID, RMSG_RESID]);
    }
    assert!(types.contains(&RMSG_NEWWDG));
}

// ------------------------------------------------------------------
// Session 20: movement fidelity (timing, retargeting, gaits, poses)
// ------------------------------------------------------------------

/// The client covers a move in c * 66.67 ms (LinMove.ctick: a +=
/// (dt/1000)/(c*0.06) * 0.9). client_steps must round-trip the planned
/// duration within one tick so the client and the server agree on when
/// the gob arrives.
#[test]
fn movement_timing_client_steps_match_planned_duration() {
    for total_ms in [
        100u32, 250, 500, 1000, 1667, 3333, 5000, 10000, 30000, 60000, 120000,
    ] {
        let c = LinMove::client_steps(total_ms);
        let client_ms = i64::from(c) * 200; // c * 200/3 ms exact
        let err = (client_ms - i64::from(total_ms) * 3).abs();
        assert!(
            err <= 300,
            "client_steps({total_ms}) = {c} => client time {client_ms}/3 ms, err {err} ms"
        );
    }
}

/// A walk of 30 tiles at walk gait (33 subtile/s) must take 10 s and
/// produce the client-consistent step count; the server-side logical
/// position must track the interpolated path (not jump to the
/// destination) and land exactly on the target when the move ends.
#[tokio::test]
async fn movement_timing_walk_duration_and_interpolated_pos() {
    let (mut g, _rx, _raw) = entered_game("walktiming");
    let pgob = g.sessions[&1].player_gob.expect("player gob");
    let slot = g.world.gobs.get(pgob).expect("slot");
    let (sx, sy) = g.world.gobs.pos[slot];
    let target = (sx + 330, sy); // 30 tiles
    assert!(g.start_move(slot, target), "walk must be accepted");
    let lm = g.world.gobs.mv[slot].expect("mv");
    assert_eq!(lm.total_ms, 10_000, "30 tiles at 33 subtile/s = 10 s");
    assert_eq!(lm.steps, 150, "client steps for 10 s at 66.67 ms/step");
    assert_eq!(g.world.gobs.speed[slot], GAIT_SPEEDS[GAIT_WALK]);

    // Halfway through, the logical position is on the path...
    for _ in 0..50 {
        g.tick();
    }
    let (mx, my) = g.world.gobs.pos[slot];
    assert!(
        (mx - (sx + 165)).abs() <= 2 && (my - sy).abs() <= 2,
        "mid-move logical pos ({mx},{my}) must be ~midpath ({},{})",
        sx + 165,
        sy
    );
    // ...and a viewer saw LINSTEP progress ~halfway.
    assert!(g.world.gobs.mv[slot].is_some(), "still moving halfway");

    // Completion: exactly on the target, movement cleared.
    for _ in 0..55 {
        g.tick();
    }
    assert!(g.world.gobs.mv[slot].is_none(), "move finished");
    assert_eq!(g.world.gobs.pos[slot], target);
}

/// Rapid re-clicks must NOT teleport: a second walk order while moving
/// starts from the interpolated on-path position, never from the old
/// destination.
#[tokio::test]
async fn movement_reclick_starts_from_interpolated_position() {
    let (mut g, _rx, _raw) = entered_game("reclick");
    let pgob = g.sessions[&1].player_gob.expect("player gob");
    let slot = g.world.gobs.get(pgob).expect("slot");
    let (sx, sy) = g.world.gobs.pos[slot];
    let far = (sx + 330, sy);
    assert!(g.start_move(slot, far));
    // Walk ~2 s (20 ticks), then click somewhere else.
    for _ in 0..20 {
        g.tick();
    }
    let (cx, cy) = g.world.gobs.pos[slot];
    let back = (sx, sy);
    assert!(g.start_move(slot, back), "retarget accepted");
    let lm = g.world.gobs.mv[slot].expect("mv after retarget");
    assert_eq!(
        (lm.sx, lm.sy),
        (cx, cy),
        "new move must start from the interpolated position, not the old destination"
    );
    assert_ne!(
        lm.sx,
        lm.tx + 330,
        "sanity: not teleporting from destination"
    );
    // The move completes back at the start point without a position jump
    // larger than one path leg.
    for _ in 0..(lm.total_ms as u64 / TICK_MS) + 2 {
        g.tick();
    }
    assert!(g.world.gobs.mv[slot].is_none());
    assert_eq!(g.world.gobs.pos[slot], back);
}

/// Session 44: a LINBEG start encodes once into the packed start
/// batch and ships as ONE datagram to each viewer at tick end; the
/// authoritative frame lands in `unacked` for OBJACK retransmission.
#[tokio::test]
async fn batch_linbeg_fans_out_once_per_tick() {
    let (mut g, _rx, mut raw) = entered_game("batchwalk");
    let pgob = g.sessions[&1].player_gob.expect("player gob");
    let slot = g.world.gobs.get(pgob).expect("slot");
    let (sx, sy) = g.world.gobs.pos[slot];
    assert!(g.start_move(slot, (sx + 330, sy)), "walk accepted");
    assert!(
        !g.start_scratch.is_empty(),
        "the LINBEG block queues in the batch (fan-out at tick end)"
    );
    let frame = g.world.gobs.frame[slot]; // start_move bumped it
    g.tick();
    assert!(
        g.world.perf.start_blocks >= 1,
        "start batch counter recorded"
    );
    // Exactly one combined OBJDATA datagram carrying the LINBEG block.
    // Headerless block layout: [fl][id i32][frame i32][OD ops..][ff].
    let mut linbeg_n = 0u8;
    let mut saw_block = false;
    while let Ok(p) = raw.try_recv() {
        assert_eq!(p[0], MSG_OBJDATA, "one type byte opens the datagram");
        let mut off = 1usize;
        while off + 9 <= p.len() {
            if p[off + 9] == hnh_proto::consts::OD_LINBEG {
                let id = i32::from_le_bytes(p[off + 1..off + 5].try_into().unwrap());
                let fr = i32::from_le_bytes(p[off + 5..off + 9].try_into().unwrap());
                if id == pgob && fr == frame as i32 {
                    linbeg_n += 1;
                    saw_block = true;
                }
            }
            // Advance to the next block: each block ends with OD_END.
            off = match p[off..]
                .iter()
                .position(|&b| b == hnh_proto::consts::OD_END)
            {
                Some(rel) => off + rel + 1,
                None => p.len(),
            };
        }
    }
    if !saw_block {
        panic!("LINBEG block lost on the wire");
    }
    assert_eq!(linbeg_n, 1, "one LINBEG per tick, not per viewer");
    // The frame is retransmittable: recorded in `unacked`.
    let out = g.sessions.get(&1).unwrap();
    assert!(
        out.unacked
            .get(&pgob)
            .is_some_and(|m| m.contains_frame(frame)),
        "LINBEG frame {frame} recorded for OBJACK"
    );
}

/// Session 44: a one-shot FX overlay encodes once with the global
/// index as the wire placeholder; the fan-out rewrites the 2-byte
/// session-local wire id, first-announces the resource to the session
/// and records the patched block in `unacked`.
#[tokio::test]
async fn batch_fx_patches_session_wire_id() {
    let (mut g, mut rx, mut raw) = entered_game("batchfx");
    let pgob = g.sessions[&1].player_gob.expect("player gob");
    let slot = g.world.gobs.get(pgob).expect("slot");
    let frame = g.world.gobs.frame[slot];
    g.fx_overlay_broadcast(pgob, "gfx/fx/hit");
    g.tick();
    // The session wire id allocated for the resource after fan-out.
    let gi = g.world.res.intern("gfx/fx/hit");
    let w = g
        .sessions
        .get_mut(&1)
        .unwrap()
        .res
        .wire_named(gi, "gfx/fx/hit");
    let out = g.sessions.get(&1).unwrap();
    // The reliable channel carried the RMSG_RESID announcement.
    let needle = b"gfx/fx/hit";
    let mut announced = false;
    while let Ok(msg) = rx.try_recv() {
        if msg.windows(needle.len()).any(|w2| w2 == needle) {
            announced = true;
        }
    }
    assert!(announced, "first-use RESID announcement was queued");
    // The OBJDATA datagram carries the patched wire id at block
    // offset 14 ([fl][id 4][frame 4][OD_OVERLAY][olid 4] -> wire).
    let mut found = false;
    while let Ok(p) = raw.try_recv() {
        assert_eq!(p[0], MSG_OBJDATA);
        let mut off = 1usize;
        while off + 16 <= p.len() {
            if p[off + 9] == hnh_proto::consts::OD_OVERLAY {
                let id = i32::from_le_bytes(p[off + 1..off + 5].try_into().unwrap());
                if id == pgob {
                    let wire = u16::from_le_bytes(p[off + 14..off + 16].try_into().unwrap());
                    assert_eq!(
                        wire, w,
                        "overlay wire id patched to the session-local value"
                    );
                    found = true;
                }
            }
            off = match p[off..]
                .iter()
                .position(|&b| b == hnh_proto::consts::OD_END)
            {
                Some(rel) => off + rel + 1,
                None => p.len(),
            };
        }
    }
    assert!(found, "FX overlay block reached the viewer");
    // The patched block is retransmittable (the FX block carries the
    // CURRENT frame - it does not open a new one).
    let rec = out
        .unacked
        .get(&pgob)
        .and_then(|m| m.block(frame))
        .map(|b| b.bytes.as_slice());
    assert!(rec.is_some(), "FX block recorded for OBJACK");
    assert_eq!(
        rec.unwrap()[14..16],
        w.to_le_bytes(),
        "unacked copy carries the PATCHED wire id"
    );
}

/// Session 44: the pose (OD_LAYERS) block encodes once with every
/// wire slot as a global-index placeholder (Patch::Many); the fan-out
/// rewrites ALL of them to the session's wire ids and lands the
/// patched block in `unacked`.
#[tokio::test]
async fn batch_pose_patches_all_wire_ids() {
    let (mut g, _rx, mut raw) = entered_game("batchpose");
    let pgob = g.sessions[&1].player_gob.expect("player gob");
    let slot = g.world.gobs.get(pgob).expect("slot");
    g.stream_pose(slot);
    g.tick();
    // Drain the datagrams; find the LAYERS block for the player gob.
    let mut layers: Option<Vec<u16>> = None;
    while let Ok(p) = raw.try_recv() {
        assert_eq!(p[0], MSG_OBJDATA);
        let mut off = 1usize;
        while off + 11 <= p.len() {
            let id = i32::from_le_bytes(p[off + 1..off + 5].try_into().unwrap());
            let od = p[off + 9];
            if id == pgob && od == hnh_proto::consts::OD_LAYERS {
                // Collect the wire ids up to the 65535 terminator.
                let mut ids: Vec<u16> = Vec::new();
                let mut q = off + 10;
                loop {
                    let w = u16::from_le_bytes(p[q..q + 2].try_into().unwrap());
                    q += 2;
                    if w == 65535 {
                        break;
                    }
                    ids.push(w);
                }
                layers = Some(ids);
            }
            off = match p[off..]
                .iter()
                .position(|&b| b == hnh_proto::consts::OD_END)
            {
                Some(rel) => off + rel + 1,
                None => p.len(),
            };
        }
    }
    let ids = layers.expect("LAYERS block reached the viewer");
    assert!(!ids.is_empty(), "pose layers present");
    // Every wire id must be the session's own allocation for its
    // resource (a placeholder global index would be garbage here).
    let out = g.sessions.get(&1).unwrap();
    for w in &ids {
        assert!(
            out.res.wire_is_local(*w),
            "wire id {w} must be session-local"
        );
    }
    // The patched block is retransmittable.
    assert!(
        out.unacked.contains_key(&pgob),
        "pose block recorded for OBJACK"
    );
}

/// Gait speeds must match docs/mechanics/character/attributes-and-vitals.md
/// (RoB Glossary "Speed"): crawl 1.5, walk 3.0, run 4.5, sprint 6.0
/// tiles/s = 16/33/50/66 subtile/s, and the speedget "set" message must
/// apply the picked gait to the mover speed.
#[tokio::test]
async fn gait_speeds_match_docs_and_speedget_set_applies() {
    assert_eq!(GAIT_SPEEDS, [16, 33, 50, 66]);
    assert_eq!(GAIT_SPEEDS[GAIT_WALK], 33, "walk = 3 tiles/s");
    let (mut g, _rx, _raw) = entered_game("gaits");
    let pgob = g.sessions[&1].player_gob.expect("player gob");
    let slot = g.world.gobs.get(pgob).expect("slot");
    assert_eq!(g.world.gobs.speed[slot], 33, "default gait is walk");
    let wid = g
        .sessions
        .get(&1)
        .unwrap()
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "speedget")
        .map(|(k, _)| *k)
        .expect("speedget widget");
    g.on_wdgmsg(1, wid, "set", vec![hnh_proto::ListArg::Int(2)]);
    assert_eq!(g.world.gobs.speed[slot], 50, "run gait applied");
    g.on_wdgmsg(1, wid, "set", vec![hnh_proto::ListArg::Int(9)]);
    assert_eq!(
        g.world.gobs.speed[slot], 66,
        "out-of-range clamps to sprint"
    );
}

/// Direction quantization: the 8 octants map to the pack's
/// directional pose index (dir 0 = +x, dir 2 = +y, dir 4 = -x,
/// dir 6 = -y, diagonals on odd dirs; wraparound exact at 180 deg).
#[test]
fn move_dir_quantizes_octants() {
    assert_eq!(move_dir((0, 0), (10, 0)), 0, "+x");
    assert_eq!(move_dir((0, 0), (10, 10)), 1, "+x+y diagonal");
    assert_eq!(move_dir((0, 0), (0, 10)), 2, "+y");
    assert_eq!(move_dir((0, 0), (-10, 10)), 3, "-x+y");
    assert_eq!(move_dir((0, 0), (-10, 0)), 4, "-x");
    assert_eq!(move_dir((0, 0), (-10, -10)), 5, "-x-y");
    assert_eq!(move_dir((0, 0), (0, -10)), 6, "-y");
    assert_eq!(move_dir((0, 0), (10, -10)), 7, "+x-y");
    assert_eq!(move_dir((0, 0), (-10, -1)), 4, "steep -x wraparound");
    assert_eq!(move_dir((0, 0), (-1, -10)), 6, "steep -y wraparound");
    assert_eq!(move_dir((5, 5), (5, 5)), 0, "zero vector defaults to dir 0");
}

/// The art ring is rotated one octant against the movement ring:
/// sprite = (octant - 1) mod 8. Anchors: front octant 1 -> sprite 0,
/// back octant 5 -> sprite 4, pure left octant 3 -> sprite 2, pure
/// right octant 7 -> sprite 6. The two user-reported defect cases:
/// walking up (octant 5) must show the BACK set (sprite 4), not the
/// up-right set (sprite 5); walking left (octant 3) must show the
/// pure LEFT profile (sprite 2), not the up-left set (sprite 3).
#[test]
fn art_dir_offsets_the_sprite_ring() {
    for octant in 0u8..8 {
        assert_eq!(art_dir(octant), (octant + 7) & 7, "octant {octant}");
    }
    assert_eq!(art_dir(1), 0, "camera-facing front is sprite 0");
    assert_eq!(art_dir(5), 4, "walking up shows the back set");
    assert_eq!(art_dir(3), 2, "walking left shows the left profile");
    assert_eq!(art_dir(7), 6, "walking right shows the right profile");
    assert_eq!(art_dir(0), 7, "east shows the down-right 3/4 set");
}

/// Pose layer composition: walking vs standing sets at a direction,
/// plus the fixed banzai doll set and the kritter pose part. Layer
/// names carry the ART sprite index (art_dir(octant)), so octant 3
/// composes legs-2 and octant 6 composes legs-5.
#[test]
fn pose_layers_compose_direction_and_kind() {
    let walk = avatar_pose_layers(true, 3);
    assert!(walk[0].ends_with("walking/legs-2"), "{}", walk[0]);
    assert!(walk[1].ends_with("walking/torso/male-2"), "{}", walk[1]);
    assert!(
        walk[5].starts_with("gfx/borka/hair-karin/walking/"),
        "{}",
        walk[5]
    );
    let stand = avatar_pose_layers(false, 6);
    assert!(stand[0].ends_with("standing/legs-5"), "{}", stand[0]);
    assert!(stand[3].contains("arm/idle/left-5"), "{}", stand[3]);
    let doll = avatar_doll_layers();
    assert!(
        doll.iter().all(|n| n.contains("/standing/")),
        "doll is a standing pose"
    );
    assert!(
        doll.iter().any(|n| n.contains("arm/banzai/left-0")),
        "doll arms are banzai (spread), front view sprite 0"
    );
    assert!(
        !doll.iter().any(|n| n.contains("arm/idle")),
        "doll never uses idle arms"
    );
    let wolf = kritter_pose_layer(Species::Wolf, true, 5);
    assert_eq!(wolf, "gfx/kritter/wolf/body/walking/walking-4");
    let hare = kritter_pose_layer(Species::Hare, false, 0);
    assert_eq!(hare, "gfx/kritter/hare/body/standing/standing-7");
    assert_eq!(kritter_base(Species::Fox), "gfx/kritter/fox/body");
}

/// While a player avatar moves, the streamed pose is the walking set
/// of the travel direction (pose_streamed = 8+dir); when the move ends
/// the standing set of the same direction returns (pose_streamed =
/// dir). No frame streaming ever happens: the pose streams fire only
/// on pose/direction changes.
#[tokio::test]
async fn walk_layers_swap_between_walking_and_standing() {
    let (mut g, _rx, _raw) = entered_game("walkpose");
    let pgob = g.sessions[&1].player_gob.expect("player gob");
    let slot = g.world.gobs.get(pgob).expect("slot");
    let (sx, sy) = g.world.gobs.pos[slot];
    // Long walk east: 60 tiles at walk speed ~ 20 s.
    assert!(g.start_move(slot, (sx + 660, sy)));
    assert_eq!(g.world.gobs.facing[slot], 0, "east is dir 0");
    assert_eq!(
        g.world.gobs.pose_streamed[slot], 8,
        "walking set of dir 0 streamed on start"
    );
    // 2 s in: still the walking pose (the client cycles the frames
    // natively; the server must not re-stream anything mid-walk).
    let before = g.world.gobs.pose_streamed[slot];
    for _ in 0..20 {
        g.tick();
    }
    assert_eq!(
        g.world.gobs.pose_streamed[slot], before,
        "no frame streaming mid-walk"
    );
    // Drain the remaining move.
    for _ in 0..210 {
        g.tick();
    }
    assert!(g.world.gobs.mv[slot].is_none(), "move finished");
    assert_eq!(
        g.world.gobs.pose_streamed[slot], 0,
        "standing set of dir 0 restored after arrival"
    );
}

// ------------------------------------------------------------------
// Session 27: multi-node cluster (authority, guests, transfer, chat)
// ------------------------------------------------------------------
