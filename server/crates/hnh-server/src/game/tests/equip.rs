//! Equipment, cursor, inventory-window and gob-streaming entry
//! contracts.
use super::super::*;
use super::common::*;

/// The paperdoll must exist right after world entry with a full "set"
/// sync and the "ava" gob binding (the user-reported missing doll).
#[tokio::test]
async fn epry_is_bootstrapped_with_set_and_ava() {
    let (g, mut rx, _raw) = entered_game("dolluser");
    assert!(
        g.epry_window(1).is_some(),
        "epry widget must exist after world entry"
    );
    let mut saw_set = false;
    let mut saw_ava = false;
    while let Ok(msg) = rx.try_recv() {
        // RMSG_WDGMSG payload: type byte, u16 wid, NUL-terminated name.
        if msg.first() == Some(&RMSG_WDGMSG) && msg.len() > 4 {
            let nul = msg[3..]
                .iter()
                .position(|&b| b == 0)
                .map(|p| 3 + p)
                .unwrap_or(msg.len());
            let n = String::from_utf8_lossy(&msg[3..nul]);
            if n == "set" {
                saw_set = true;
            }
            if n == "ava" {
                saw_ava = true;
            }
        }
    }
    assert!(saw_set, "bootstrap must queue the epry \"set\" sync");
    assert!(saw_ava, "bootstrap must queue the epry \"ava\" gob id");
}

/// Equip via cursor ("drop"), unequip via "take"; occupied and invalid
/// slots are rejected without losing the held stack.
#[tokio::test]
async fn epry_equips_and_unequips_via_cursor() {
    let (mut g, _rx, _raw) = entered_game("equipuser");
    let pidx = *g.world.by_session.get(&1).unwrap();
    assert!(g.world.players[pidx].equip.iter().all(|s| s.is_none()));

    // Open the inventory and take the first stack onto the cursor.
    let slen = g
        .sessions
        .get(&1)
        .unwrap()
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "slen")
        .map(|(k, _)| *k)
        .unwrap();
    g.on_wdgmsg(1, slen, "inv", vec![]);
    let item_wid = g
        .sessions
        .get(&1)
        .unwrap()
        .item_wids
        .iter()
        .find(|(_, &idx)| idx == 0)
        .map(|(&w, _)| w)
        .expect("first inventory item widget");
    g.on_wdgmsg(1, item_wid, "take", vec![]);
    assert!(
        g.sessions.get(&1).unwrap().cursor.is_some(),
        "take -> cursor"
    );

    let epry = g.epry_window(1).unwrap();
    // Equip into slot 3.
    g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(3)]);
    assert!(
        g.world.players[pidx].equip[3].is_some(),
        "cursor item must land in slot 3"
    );
    assert!(g.sessions.get(&1).unwrap().cursor.is_none());

    // Occupied slot: the take has to refill the hand first.
    let held = g.world.players[pidx].equip[3];
    g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(3)]);
    assert_eq!(g.world.players[pidx].equip[3], held, "occupied slot kept");

    // Invalid slot: no panic, no state change.
    g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(16)]);
    g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(-1)]);

    // Unequip: back onto the cursor.
    g.on_wdgmsg(1, epry, "take", vec![hnh_proto::ListArg::Int(3)]);
    assert!(g.world.players[pidx].equip[3].is_none());
    assert!(g.sessions.get(&1).unwrap().cursor.is_some());

    // Empty slot take with a full hand: rejected.
    g.on_wdgmsg(1, epry, "take", vec![hnh_proto::ListArg::Int(4)]);
    assert!(g.world.players[pidx].equip[4].is_none());
    assert!(g.sessions.get(&1).unwrap().cursor.is_some(), "hand kept");
}

/// Equipping must re-stream the world drawable (OD_LAYERS) with the
/// piece's borka layers and push the updated doll attribute
/// (OD_AVATAR) to the owner; unequipping streams again without the
/// piece (the live doll/world update, not just a spawn-time view).
#[tokio::test]
async fn equip_change_streams_layers_and_avatar() {
    let (mut g, mut rx, mut raw) = entered_game("equipvisuser");
    let pidx = *g.world.by_session.get(&1).unwrap();
    // Hand the player a wearable (server-side grant, like a pickup).
    let pants = "gfx/invobjs/linenpants";
    let res = g.world.res.intern(pants);
    g.world.players[pidx].inv.push(InvStack {
        res,
        count: 1,
        ql: 10,
        label: "",
    });
    // Drain the bootstrap traffic: only the CHANGE may be asserted.
    while rx.try_recv().is_ok() {}
    while raw.try_recv().is_ok() {}

    // Reserve the wire id of the layer name the equip table emits
    // for the standing pants (wire_named allocates once per session,
    // so the later stream reuses this id). The world gob is idle, so
    // the standing set of the SPAWN facing applies (spawn faces
    // movement octant 1 - the camera-facing front).
    let gob = g.world.players[pidx].gob;
    let slot = g.world.gobs.get(gob).expect("player gob slot");
    let facing = g.world.gobs.facing[slot];
    let pants_le: &str = crate::equip::world_layers([&"gfx/invobjs/linenpants"], false, facing)[0];
    let pants_layer: &'static str = pants_le;
    let layer_res = g.world.res.intern(pants_layer);
    let pants_wire = g
        .sessions
        .get_mut(&1)
        .unwrap()
        .res
        .wire_named(layer_res, pants_layer);

    // Equip: take the stack onto the cursor, drop it into slot 2.
    let slen = g
        .sessions
        .get(&1)
        .unwrap()
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "slen")
        .map(|(k, _)| *k)
        .unwrap();
    g.on_wdgmsg(1, slen, "inv", vec![]);
    // The starting inventory already holds a branch: target the
    // granted stack's widget (the last inventory index) explicitly.
    let pants_idx = g.world.players[pidx].inv.len() - 1;
    let item_wid = g
        .sessions
        .get(&1)
        .unwrap()
        .item_wids
        .iter()
        .find(|(_, &idx)| idx == pants_idx)
        .map(|(&w, _)| w)
        .unwrap_or_else(|| panic!("granted stack widget at idx {pants_idx}"));
    g.on_wdgmsg(1, item_wid, "take", vec![]);
    let epry = g.epry_window(1).unwrap();
    g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(2)]);
    assert!(g.world.players[pidx].equip[2].is_some());
    // The pose re-stream ships through the packed start batch at tick
    // end (session 44).
    g.tick();

    // The OD_LAYERS re-stream must carry the pants wire id, and the
    // doll attribute (OD_AVATAR) must be pushed with it too.
    let mut saw_world = false;
    let mut saw_doll = false;
    while let Ok(block) = raw.try_recv() {
        for (op, ids) in objdata_layer_lists(&block) {
            if ids.contains(&pants_wire) {
                if op == OD_LAYERS {
                    saw_world = true;
                }
                if op == OD_AVATAR {
                    saw_doll = true;
                }
            }
        }
    }
    assert!(
        saw_world,
        "equip must re-stream OD_LAYERS carrying wire id {pants_le:?}"
    );
    assert!(saw_doll, "equip must push OD_AVATAR carrying the piece");

    // Unequip: another OD_LAYERS/OD_AVATAR pair streams (now without
    // the piece - the layer lists shrink back).
    while raw.try_recv().is_ok() {}
    g.on_wdgmsg(1, epry, "take", vec![hnh_proto::ListArg::Int(2)]);
    assert!(g.world.players[pidx].equip[2].is_none());
    g.tick();
    let mut streamed_after_take = 0;
    while let Ok(block) = raw.try_recv() {
        for (op, _) in objdata_layer_lists(&block) {
            if op == OD_LAYERS || op == OD_AVATAR {
                streamed_after_take += 1;
            }
        }
    }
    assert!(
        streamed_after_take >= 2,
        "unequip must re-stream the drawable and the doll"
    );
}

// ------------------------------------------------------------------
// Session 26: cursor drag widget + ground drops
// ------------------------------------------------------------------

/// Taking a stack onto the cursor must create the drag Item widget
/// with drag=1 + a grab Coord (the legacy held-item body at the
/// pointer); dropping it back must destroy that widget and leave no
/// table entry behind.
#[tokio::test]
async fn take_creates_and_drop_destroys_cursor_widget() {
    let (mut g, mut rx, _raw) = entered_game("cursoruser");
    g.open_inventory(1);
    // The starter kit ships Wheat Seeds; take that stack.
    let pidx = *g.world.by_session.get(&1).unwrap();
    let item_wid = g.sessions[&1]
        .item_wids
        .iter()
        .find(|(_, &idx)| {
            g.world.players[pidx].inv.get(idx).map(|s| s.label) == Some("Wheat Seeds")
        })
        .map(|(w, _)| *w)
        .expect("starter wheat seeds must have an item widget");
    g.inv_take(1, item_wid);

    let out = g.sessions.get(&1).unwrap();
    assert!(out.cursor.is_some(), "cursor holds the taken stack");
    let cw = out.cursor_wid.expect("drag widget must exist after take");
    assert_eq!(out.widgets.get(&cw).map(String::as_str), Some("item"));
    // Wire proof: the NEWWDG for the cursor wid carries drag=1 and a
    // grab Coord among its args (Item.java factory contract).
    let mut saw_drag_widget = false;
    while let Ok(msg) = rx.try_recv() {
        if msg.first() != Some(&RMSG_NEWWDG) {
            continue;
        }
        let mut m = hnh_proto::MessageBuf::from_slice(&msg[1..]);
        let id = m.u16().unwrap();
        let ty = m.str().unwrap();
        if id != cw || ty != "item" {
            continue;
        }
        let (_x, _y) = m.coord2().unwrap();
        let _parent = m.u16().unwrap();
        let mut args = Vec::new();
        while let Some(a) = m.list_arg().unwrap() {
            args.push(a);
        }
        // args: [res, q, dragFlag, (dragCoord), tooltip, num]
        assert!(args.len() >= 6, "drag item args: {args:?}");
        assert_eq!(args[2], hnh_proto::ListArg::Int(1), "drag flag");
        assert!(
            matches!(args[3], hnh_proto::ListArg::Coord(..)),
            "grab coord expected: {args:?}"
        );
        saw_drag_widget = true;
    }
    assert!(saw_drag_widget, "cursor drag widget must be on the wire");

    // Drop back into the inventory: widget destroyed, table clean.
    g.inv_drop(1, cw, &[]);
    let out = g.sessions.get(&1).unwrap();
    assert!(out.cursor.is_none(), "cursor empty after drop");
    assert!(out.cursor_wid.is_none(), "drag widget id cleared");
    assert!(
        !out.widgets.contains_key(&cw),
        "destroyed drag widget leaves the widget table"
    );
    let mut saw_dst = false;
    while let Ok(msg) = rx.try_recv() {
        if msg.first() == Some(&RMSG_DSTWDG) && msg.len() >= 3 {
            let id = u16::from_le_bytes([msg[1], msg[2]]);
            saw_dst |= id == cw;
        }
    }
    assert!(saw_dst, "the drag widget destruction must be on the wire");
}

/// MapView `drop` must land the held stack as a ground gob near the
/// player and clear the cursor (stack + drag widget).
#[tokio::test]
async fn map_drop_spawns_ground_gob_and_clears_cursor() {
    let (mut g, mut rx, mut raw) = entered_game("dropuser");
    g.open_inventory(1);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let item_wid = g.sessions[&1]
        .item_wids
        .iter()
        .find(|(_, &idx)| {
            g.world.players[pidx].inv.get(idx).map(|s| s.label) == Some("Wheat Seeds")
        })
        .map(|(w, _)| *w)
        .expect("starter wheat seeds must have an item widget");
    g.inv_take(1, item_wid);
    assert!(g.sessions[&1].cursor.is_some());
    // Drain bootstrap + take traffic.
    while rx.try_recv().is_ok() {}
    while raw.try_recv().is_ok() {}
    let count_drops = |g: &Game| {
        g.world
            .gobs
            .kind
            .iter()
            .zip(g.world.gobs.alive.iter())
            .filter(|(_, a)| **a)
            .filter(|(k, _)| matches!(k, Kind::Drop { .. }))
            .count()
    };
    let drops_before = count_drops(&g);

    g.on_map_drop(1);

    let out = g.sessions.get(&1).unwrap();
    assert!(out.cursor.is_none(), "cursor cleared by the drop");
    assert!(out.cursor_wid.is_none(), "drag widget gone after the drop");
    // A Drop gob appeared.
    let drops_after = count_drops(&g);
    assert!(
        drops_after > drops_before,
        "map drop must spawn a ground gob ({drops_before} -> {drops_after})"
    );
    // The drop gob must have been announced on the raw wire (OBJDATA).
    let mut saw_objdata = false;
    while let Ok(p) = raw.try_recv() {
        if p.first() == Some(&MSG_OBJDATA) {
            saw_objdata = true;
        }
    }
    assert!(saw_objdata, "the ground gob must be streamed to the client");
}

/// The take -> ground-drop round trip conserves the stack: what left
/// the inventory came back as the same resource/count on the ground.
#[tokio::test]
async fn map_drop_conserves_stack_contents() {
    let (mut g, _rx, _raw) = entered_game("dropq");
    g.open_inventory(1);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let item_wid = g.sessions[&1]
        .item_wids
        .iter()
        .find(|(_, &idx)| {
            g.world.players[pidx].inv.get(idx).map(|s| s.label) == Some("Wheat Seeds")
        })
        .map(|(w, _)| *w)
        .expect("item widget");
    let taken = {
        let inv = &g.world.players[pidx].inv;
        let idx = g.sessions[&1].item_wids[&item_wid];
        inv[idx]
    };
    g.inv_take(1, item_wid);
    g.on_map_drop(1);
    // The new Drop gob: inv_res_idx == the taken stack's resource
    // (pickup restores the exact icon), render res != inv res (the
    // world shape must be a terobjs resource with a neg layer).
    let mut hit_inv = false;
    let mut render_is_world_shape = false;
    for (k, _alive) in g
        .world
        .gobs
        .kind
        .iter()
        .zip(g.world.gobs.alive.iter())
        .filter(|(_, a)| **a)
    {
        if let Kind::Drop {
            resname_idx,
            inv_res_idx,
            ql,
            label,
        } = k
        {
            if *inv_res_idx == taken.res && *ql == taken.ql && *label == taken.label {
                hit_inv = true;
                let render_name = g.world.res.name(*resname_idx).unwrap_or("");
                render_is_world_shape |= render_name.starts_with("gfx/terobjs/items/");
            }
        }
    }
    assert!(
        hit_inv,
        "the drop must carry the taken stack's inventory resource (res {})",
        taken.res
    );
    assert!(
        render_is_world_shape,
        "the drop gob must render with a gfx/terobjs/items world shape"
    );
    let inv_name = g.world.res.name(taken.res).unwrap_or("");
    assert!(inv_name.starts_with("gfx/invobjs/"));
}

/// Regression test (avatar bug): the player's own gob must be streamed
/// to the session with OD_BUDDY naming the character. A double insert
/// into `visible` (scan phase + stream_spawn) used to suppress the
/// spawn block, so the client never received its own avatar gob.
#[tokio::test]
async fn player_gob_is_streamed_with_buddy() {
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut g = Game::new(
        42,
        cmd_rx,
        net_rx,
        false,
        std::env::temp_dir().join("hnh-game-test-save.json"),
    );
    let (tx, mut _rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, mut raw_rx) = tokio::sync::mpsc::channel(512);
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
        vec![hnh_proto::ListArg::Str("avatared".to_owned())],
    );
    // A few ticks: visibility scan must stream the player's own gob.
    for _ in 0..5 {
        g.tick();
    }
    let mut saw_buddy = false;
    while let Ok(block) = raw_rx.try_recv() {
        // Block layout: [MSG_OBJDATA][flags][gobid i32][frame i32][subs].
        if block.len() < 10 || block[0] != MSG_OBJDATA {
            continue;
        }
        let mut off = 10;
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
                    // base u16, then u16 layer ids until the 65535
                    // terminator (variable size since the layers are
                    // the concrete standing-pose frames).
                    off += 2;
                    loop {
                        let id = u16::from_le_bytes([block[off], block[off + 1]]);
                        off += 2;
                        if id == 65535 {
                            break;
                        }
                    }
                }
                OD_HEALTH => off += 1,
                OD_BUDDY => {
                    let end = block[off..]
                        .iter()
                        .position(|&b| b == 0)
                        .map(|p| off + p)
                        .unwrap_or(block.len());
                    if &block[off..end] == b"avatared" {
                        saw_buddy = true;
                    }
                    break;
                }
                _ => break,
            }
        }
        if saw_buddy {
            break;
        }
    }
    assert!(
        saw_buddy,
        "player gob spawn block with OD_BUDDY must be streamed"
    );
}

/// The visible bitset mirror must agree with the authoritative set at
/// every point of the spawn/retract lifecycle (S68): the fan-out fast
/// path only re-checks bit-set pairs, so a desync would either skip
/// real viewers (probe false, set contains) or waste probes. Direct
/// method contract first, then the live-path invariant after ticks.
#[tokio::test]
async fn visible_bitset_mirror_tracks_the_set() {
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut g = Game::new(
        42,
        cmd_rx,
        net_rx,
        false,
        std::env::temp_dir().join("hnh-bitset-unit.json"),
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, _raw) = tokio::sync::mpsc::channel(512);
    g.session_connected(1, "acct".to_owned(), tx, raw_tx);
    let out = g.sessions.get_mut(&1).unwrap();
    // Low slots and one high slot (forces the mirror to grow).
    for id in [1, 2, 0x0005_0003, 0x0002_0003] {
        out.visible.insert(id);
        out.vis_bit_insert(crate::state::split_gob_id(id).0);
    }
    for id in [1, 2, 0x0005_0003, 0x0002_0003] {
        assert!(out.vis_bit_probe(id), "bit set for {id:#x}");
        assert!(out.visible.contains(&id));
    }
    // Slot collision across generations: (slot 3, gen 5) removed must
    // clear the bit even though (slot 3, gen 2) shares the slot word.
    out.visible.remove(&0x0005_0003);
    out.vis_bit_remove(crate::state::split_gob_id(0x0005_0003).0);
    assert!(!out.vis_bit_probe(0x0005_0003));
    // Removing a slot beyond the mirror length is a no-op, not a panic.
    out.vis_bit_remove(0xFFFF);
    // The live path: after entering a world and ticking, every visible
    // gob id must have its bit set (spawn inserts keep the mirror warm).
    let (mut g, _rx, _raw2) = entered_game("s68bitset");
    for _ in 0..5 {
        g.tick();
    }
    let out = g.sessions.get(&1).unwrap();
    assert!(!out.visible.is_empty(), "the view scanned something");
    for &id in out.visible.iter() {
        assert!(
            out.vis_bit_probe(id),
            "mirror missing visible id {id:#010x}"
        );
    }
}

// ------------------------------------------------------------------
// Session 72: the station input/lit/output contract as white-box pins.
// Both S70/S71 live findings (the plan->station sink completion and
// the crafted-label gate) were only caught by python probes outside
// the cargo gate - these tests move the load-bearing halves in.
// ------------------------------------------------------------------
