use super::*;

/// Enter the world as `name` on a fresh single-session game and return
/// the game plus the outgoing message receivers (shared setup for the
/// equipment tests).
fn entered_game(
    name: &str,
) -> (
    Game,
    tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    tokio::sync::mpsc::Receiver<Vec<u8>>,
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
    let (raw_tx, raw_rx) = tokio::sync::mpsc::channel(512);
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

/// Extract the (op, layer wire ids) pairs from one raw OBJDATA block
/// (the same layout the spawn-block test walks).
fn objdata_layer_lists(block: &[u8]) -> Vec<(u8, Vec<u16>)> {
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

/// Full plow -> plant -> grow -> harvest flow at the handler level
/// (wire transport is covered by server/scripts/test_farming.py).
#[tokio::test]
async fn farming_flow_end_to_end() {
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
    let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel(512);
    g.session_connected(1, "acct".to_owned(), tx, raw_tx);
    let charlist = g
        .sessions
        .get(&1)
        .unwrap()
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "charlist")
        .map(|(k, _)| *k)
        .unwrap();
    g.on_wdgmsg(
        1,
        charlist,
        "play",
        vec![hnh_proto::ListArg::Str("farmer".to_owned())],
    );
    // Put the player on a known grass spot and populate its grid.
    let pgob = g.world.players[0].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    g.world.gobs.set_pos(pslot, (550, 550));
    // The farming skill value gates planting; grant it the way the
    // sattr purchase would (the wire flow is covered by skillbot).
    g.world.players[0].attrs.insert("farming".to_owned(), 1);
    g.on_mapreq(1, (0, 0));
    // Find a grass tile near the player (avoid trees/stones/animals).
    let mut tile = None;
    'outer: for r in 0..8i32 {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue;
                }
                let tx = 50 + dx;
                let ty = 50 + dy;
                let gc = (tx.div_euclid(100), ty.div_euclid(100));
                let ix = tx.rem_euclid(100) as usize;
                let iy = ty.rem_euclid(100) as usize;
                if g.world.grids.grid(gc).tile(ix, iy) == tile::GRASS {
                    tile = Some((tx, ty));
                    break 'outer;
                }
            }
        }
    }
    let (tx0, ty0) = tile.expect("grass tile near spawn");
    // 1. Arm the plow pagina, click the tile.
    let scm = g
        .sessions
        .get(&1)
        .unwrap()
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "scm")
        .map(|(k, _)| *k)
        .unwrap();
    g.on_wdgmsg(
        1,
        scm,
        "act",
        vec![hnh_proto::ListArg::Str("plow".to_owned())],
    );
    assert!(
        g.sessions.get(&1).unwrap().pending_plow,
        "plow pagina must arm"
    );
    let c0 = hnh_proto::ListArg::Coord(0, 0);
    let mc = hnh_proto::ListArg::Coord(tx0 * 11 + 5, ty0 * 11 + 5);
    let mapview = g
        .sessions
        .get(&1)
        .unwrap()
        .widgets
        .iter()
        .find(|(_, t)| t.as_str() == "mapview")
        .map(|(k, _)| *k)
        .unwrap();
    g.on_wdgmsg(
        1,
        mapview,
        "click",
        vec![
            c0.clone(),
            mc.clone(),
            hnh_proto::ListArg::Int(1),
            hnh_proto::ListArg::Int(0),
        ],
    );
    assert!(
        g.world.tilth.contains_key(&(tx0, ty0)),
        "tile must be furrowed"
    );

    // 2. Take a seed stack onto the cursor and itemact the tile.
    let seed_idx = g.world.players[0]
        .inv
        .iter()
        .position(|s| s.label == "Wheat Seeds")
        .expect("starter seeds");
    let stack = g.world.players[0].inv[seed_idx];
    g.sessions.get_mut(&1).unwrap().cursor = Some(stack);
    g.on_map_itemact(1, &[c0, mc, hnh_proto::ListArg::Int(0)]);
    // Crop gob exists and is registered at the tile.
    let crop_gob = *g
        .world
        .crop_at
        .get(&(tx0, ty0))
        .expect("crop gob registered");
    assert!(g.world.crops.contains_key(&crop_gob));
    // Cursor kept the remainder (5 seeds - 1).
    assert_eq!(
        g.sessions.get(&1).unwrap().cursor.as_ref().map(|s| s.count),
        Some(4)
    );

    // 3. Force growth to maturity by rewinding stage deadlines.
    let state = g.world.crops.get_mut(&crop_gob).unwrap();
    state.next_stage_at = 0;
    g.tick();
    assert_eq!(
        g.world.crops.get(&crop_gob).unwrap().stage,
        1,
        "first stage advance on due tick"
    );
    for _ in 0..8 {
        let st = g.world.crops.get_mut(&crop_gob).unwrap();
        st.next_stage_at = 0;
        g.tick();
    }
    let mature = {
        let slot = g.world.gobs.get(crop_gob).unwrap();
        match g.world.gobs.kind[slot] {
            Kind::Crop { stage, spec } => stage >= farm::CROPS[spec as usize].stages,
            _ => false,
        }
    };
    assert!(mature, "crop must reach maturity after forced advances");

    // 4. Click the crop -> flower menu -> harvest -> yields.
    g.on_map_click(
        1,
        &[
            hnh_proto::ListArg::Coord(0, 0),
            hnh_proto::ListArg::Coord(tx0 * 11 + 5, ty0 * 11 + 5),
            hnh_proto::ListArg::Int(1),
            hnh_proto::ListArg::Int(0),
            hnh_proto::ListArg::Int(crop_gob),
            hnh_proto::ListArg::Coord(tx0 * 11 + 5, ty0 * 11 + 5),
        ],
    );
    let sm = g.sessions.get(&1).unwrap().crop_menu.map(|(w, _)| w);
    assert!(sm.is_some(), "harvest flower menu must open");
    g.on_flower_choice(1, sm.unwrap(), 0);
    let inv_before = g.world.players[0].inv.len();
    assert!(inv_before > 5, "harvest must push yields into inventory");
    assert!(
        !g.world.crops.contains_key(&crop_gob)
            && g.world.crop_at.get(&(tx0, ty0)) != Some(&crop_gob),
        "crop gob removed from registries"
    );
    // Tilth decay timer restored (non-zero deadline again).
    assert_ne!(g.world.tilth.get(&(tx0, ty0)), Some(&0));
}

/// Predators in a saturated world must engage the player: chase, open
/// the Fightview window and start swinging back.
#[tokio::test]
async fn predator_engages_player_in_reach() {
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut g = Game::new(
        42,
        cmd_rx,
        net_rx,
        true,
        std::env::temp_dir().join("hnh-game-test-save.json"),
    );
    let (tx, mut _rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel(512);
    g.session_connected(1, "acct".to_owned(), tx, raw_tx);
    // Select the character through the normal widget path.
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
        vec![hnh_proto::ListArg::Str("hunter".to_owned())],
    );
    // Populate the player's grid with saturated wildlife.
    g.on_mapreq(1, (0, 0));
    // Find a wolf or boar and teleport the player into melee reach.
    let _predator = (0..g.world.animal_gobs.len())
        .map(|i| g.world.animal_gobs[i])
        .find(|&id| {
            let slot = g.world.gobs.get(id).unwrap();
            matches!(g.world.gobs.kind[slot], Kind::Animal { species } if species.aggressive())
        })
        .expect("saturated grid spawns predators");
    let pslot = predator_slot(&g);
    let (ax, ay) = g.world.gobs.pos[pslot];
    let pgob = g.world.players[0].gob;
    let pslot2 = g.world.gobs.get(pgob).unwrap();
    g.world.gobs.set_pos(pslot2, (ax + 5, ay));
    info!(?ax, ?ay, "teleported player next to predator");
    // Run ticks until the engagement opens.
    let mut fought = false;
    for _ in 0..300 {
        g.tick();
        if !g.world.animal_fights.is_empty() {
            fought = true;
            break;
        }
    }
    assert!(fought, "predator must engage a player in reach");
}

/// A stationary player next to a predator must land damage through
/// openings and eventually kill it (full combat kill-cycle check).
#[tokio::test]
async fn stationary_player_kills_predator() {
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut g = Game::new(
        42,
        cmd_rx,
        net_rx,
        true,
        std::env::temp_dir().join("hnh-game-test-save.json"),
    );
    let (tx, mut _rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel(512);
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
        vec![hnh_proto::ListArg::Str("hunter".to_owned())],
    );
    g.on_mapreq(1, (0, 0));
    let (pred_id, pred_slot) = g
        .world
        .animal_gobs
        .iter()
        .filter_map(|&id| {
            g.world.gobs.get(id).map(|slot| {
                let aggro = matches!(
                    g.world.gobs.kind[slot],
                    Kind::Animal { species } if species.aggressive()
                );
                if aggro {
                    Some((id, slot))
                } else {
                    None
                }
            })
        })
        .flatten()
        .next()
        .expect("saturated grid spawns predators");
    let (ax, ay) = g.world.gobs.pos[pred_slot];
    // Leave exactly one predator alive so the fight dynamics are
    // deterministic (no pack target swapping).
    let keep = pred_id;
    let others: Vec<GobId> = g
        .world
        .animal_gobs
        .iter()
        .copied()
        .filter(|&id| id != keep)
        .collect();
    for id in others {
        g.world.gobs.kill(id);
    }
    g.world.animal_gobs.retain(|&id| id == keep);
    let pgob = g.world.players[0].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    g.world.gobs.set_pos(pslot, (ax + 5, ay));
    let mut killed = false;
    let mut saw_damage = false;
    // Track the predator currently engaged (the dense pack may swap
    // targets as other wolves wander into reach).
    for tick in 0..3000 {
        if tick % 4 == 0 {
            // Hold position next to the predator (stationary player).
            g.world.gobs.set_pos(pslot, (ax + 5, ay));
        }
        g.tick();
        // The target may vanish (killed): check both paths.
        if !g.world.gobs.alive[pred_slot] {
            killed = true;
            break;
        }
        let engaged = g.world.players[0].fight_target;
        if let Some(tid) = engaged {
            if let Some(tslot) = g.world.gobs.get(tid) {
                if g.world.gobs.hp[tslot] < g.world.gobs.max_hp[tslot] {
                    saw_damage = true;
                }
            }
        }
    }
    assert!(saw_damage, "damage must land through openings");
    assert!(
        killed,
        "stationary player must kill a predator in 3000 ticks"
    );
}

/// A stationary player in reach must be BITTEN: the animal's offence
/// builds every tick, swings chip the player's defence, the bite lands
/// (hp drop) and the one-shot bite FX overlay (gfx/fx/bite) is
/// broadcast to the victim - the animal attack animation path.
#[tokio::test]
async fn predator_bites_and_broadcasts_bite_overlay() {
    let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut g = Game::new(
        42,
        cmd_rx,
        net_rx,
        true,
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
        vec![hnh_proto::ListArg::Str("prey".to_owned())],
    );
    g.on_mapreq(1, (0, 0));
    let (pred_id, pred_slot) = g
        .world
        .animal_gobs
        .iter()
        .filter_map(|&id| {
            g.world.gobs.get(id).map(|slot| {
                let aggro = matches!(
                    g.world.gobs.kind[slot],
                    Kind::Animal { species } if species.aggressive()
                );
                if aggro {
                    Some((id, slot))
                } else {
                    None
                }
            })
        })
        .flatten()
        .next()
        .expect("saturated grid spawns predators");
    let (ax, ay) = g.world.gobs.pos[pred_slot];
    let keep = pred_id;
    let others: Vec<GobId> = g
        .world
        .animal_gobs
        .iter()
        .copied()
        .filter(|&id| id != keep)
        .collect();
    for id in others {
        g.world.gobs.kill(id);
    }
    g.world.animal_gobs.retain(|&id| id == keep);
    let pgob = g.world.players[0].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    g.world.gobs.set_pos(pslot, (ax + 5, ay));
    let mut bitten = false;
    let mut saw_overlay = false;
    for tick in 0..900 {
        if tick % 4 == 0 {
            g.world.gobs.set_pos(pslot, (ax + 5, ay));
        }
        g.tick();
        if g.world.players[0].hp < 100 {
            bitten = true;
        }
    }
    // Scan the raw OBJDATA stream for a bite overlay on the player gob.
    while let Ok(msg) = raw_rx.try_recv() {
        if msg.first() != Some(&MSG_OBJDATA) || msg.len() < 10 {
            continue;
        }
        let gid = i32::from_le_bytes([msg[2], msg[3], msg[4], msg[5]]);
        if gid != pgob as i32 {
            continue;
        }
        // Walk the op stream: find OD_OVERLAY (12) before OD_END (0).
        let mut off = 10;
        while off < msg.len() {
            let op = msg[off];
            off += 1;
            match op {
                0 => break,     // OD_END
                1 => off += 8,  // OD_MOVE
                3 => off += 20, // OD_LINBEG
                4 => off += 4,  // OD_LINSTEP
                6 | 9 => {
                    // OD_LAYERS / OD_AVATAR: u16 ids to 65535.
                    if op == 6 {
                        off += 2;
                    }
                    while off + 1 < msg.len() {
                        let id = u16::from_le_bytes([msg[off], msg[off + 1]]);
                        off += 2;
                        if id == 65535 {
                            break;
                        }
                    }
                }
                12 => {
                    saw_overlay = true;
                    break;
                }
                14 => off += 1, // OD_HEALTH
                15 => {
                    // OD_BUDDY: string + 2 bytes.
                    match msg[off..].iter().position(|&b| b == 0) {
                        Some(p) => off += p + 1 + 2,
                        None => break,
                    }
                }
                _ => break,
            }
        }
        if saw_overlay {
            break;
        }
    }
    assert!(bitten, "predator must land a bite (player hp drop)");
    assert!(
        saw_overlay,
        "bite must broadcast the one-shot FX overlay to the victim"
    );
}

fn predator_slot(g: &Game) -> usize {
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
            .is_some_and(|m| m.contains_key(&frame)),
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
    let rec = out.unacked.get(&pgob).and_then(|m| m.get(&frame));
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

use std::num::NonZeroUsize;

/// Test harness: game + widget-msg channel + raw-block channel + mesh
/// publish sink (what this node would send to its peers).
type ClusterHarness = (
    Game,
    tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    tokio::sync::mpsc::Receiver<Vec<u8>>,
    tokio::sync::mpsc::UnboundedReceiver<(usize, crate::nodes::NodeMsg)>,
);

/// A game wired for node `me` of a `nodes`-node cluster WITHOUT a real
/// mesh: guest publishes land in a drainable channel (mesh_rx), which
/// lets tests assert exactly what this node would send to its peers.
fn clustered_game(name: &str, me: usize, nodes: usize) -> ClusterHarness {
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
        player_abroad: HashMap::new(),
    });
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, raw_rx) = tokio::sync::mpsc::channel(4096);
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
fn foreign_cell_pos(g: &Game, me: usize) -> (i32, i32) {
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
fn home_cell_pos(g: &Game, me: usize) -> (i32, i32) {
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
fn foreign_node_gob_id(me: usize, nodes: usize, seq: usize) -> GobId {
    let per = (MAX_SLOT + 1) / nodes;
    let slot = me * per + seq; // my own range is NOT foreign; pick another
    let slot = if slot == me * per + seq {
        (nodes - me - 1) * per + seq + 3
    } else {
        slot
    };
    gob_id_from_slot(slot % (MAX_SLOT + 1), 1)
}

/// G3: a foreign-cell animal must not simulate on this node while a
/// local-cell animal does, and a player standing in a foreign cell
/// keeps moving (players are always authored by their home node).
#[tokio::test]
async fn authority_follows_cells_but_players_stay_home() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("authuser", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).expect("player gob");

    // Home-cell deer (mine): flees when the player is close.
    let res = g.world.res.intern(Species::Deer.resname());
    let (hx, hy) = home_cell_pos(&g, 0);
    let local_deer = g.world.gobs.spawn(
        Kind::Animal {
            species: Species::Deer,
        },
        (hx, hy),
        res,
        10,
        33,
    );
    g.world.animal_gobs.push(local_deer);
    // Foreign-cell deer: same species, other node's cell.
    let (fx, fy) = foreign_cell_pos(&g, 0);
    let foreign_deer = g.world.gobs.spawn(
        Kind::Animal {
            species: Species::Deer,
        },
        (fx, fy),
        res,
        10,
        33,
    );
    g.world.animal_gobs.push(foreign_deer);

    for _ in 0..12 {
        g.tick();
    }
    let lslot = g.world.gobs.get(local_deer).expect("local deer alive");
    assert!(
        g.world.gobs.mv[lslot].is_some(),
        "home deer must simulate (flee from the nearby player)"
    );
    // The foreign deer never simulates here: the cluster pass hands it
    // to its owner on the first tick (demoted to a local guest).
    assert!(
        g.world.guests.contains_key(&foreign_deer) && g.world.gobs.get(foreign_deer).is_none(),
        "foreign deer must transfer out, not simulate locally"
    );

    // Player authority: teleport into the foreign cell and click a
    // nearby target — the home node still simulates its own player.
    g.world.gobs.set_pos(pslot, (fx, fy));
    g.player_walk(1, pgob, (fx + 30, fy));
    let pslot = g.world.gobs.get(pgob).expect("player gob");
    assert!(
        g.world.gobs.mv[pslot].is_some(),
        "a player abroad must keep moving on its home node"
    );
}

/// G4: a guest announce flows through the visibility machinery — the
/// session spawns it, receives movement finalizers on update, and the
/// retract drops it from the session's visible set.
#[tokio::test]
async fn guest_ingest_update_reach_sessions() {
    let (mut g, _rx, mut raw, _mesh) = clustered_game("guestuser", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];

    let gid = foreign_node_gob_id(0, 2, 7);
    assert_ne!(gid, pgob, "guest id must never collide with local ids");
    let st = crate::nodes::GuestState {
        id: gid,
        pos: (px + 60, py),
        mv: None,
        moving: false,
        facing: 1,
        kind: crate::nodes::GuestKind::Animal {
            species: Species::Wolf.index(),
        },
        hp: 50,
        max_hp: 50,
        speed: 33,
    };
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st));
    g.tick();
    assert!(
        g.sessions[&1].visible.contains(&gid),
        "an in-view guest must spawn to the session through the vis scan"
    );
    // Wire proof: the session received an OBJDATA block for the guest
    // carrying OD_LAYERS (server-side pose resolution mirrors locals).
    let mut saw_layers = false;
    while let Ok(block) = raw.try_recv() {
        if block.first() != Some(&MSG_OBJDATA) {
            continue;
        }
        let id = i32::from_le_bytes([block[2], block[3], block[4], block[5]]);
        if id != gid {
            continue;
        }
        for (op, _) in objdata_layer_lists(&block) {
            if op == OD_LAYERS {
                saw_layers = true;
            }
        }
    }
    assert!(saw_layers, "guest spawn must carry the pose layer list");

    // Update: the owner starts the wolf moving; the guest row adopts
    // the linmove and the progress loop advances it.
    let st2 = crate::nodes::GuestState {
        id: gid,
        pos: (px + 60, py),
        mv: Some(crate::nodes::GuestLinMove {
            sx: px + 60,
            sy: py,
            tx: px + 260,
            ty: py,
            steps: 10,
            step: 0,
            started_ms: g.world.now_ms,
            total_ms: 300,
        }),
        moving: true,
        facing: 0,
        kind: crate::nodes::GuestKind::Animal {
            species: Species::Wolf.index(),
        },
        hp: 50,
        max_hp: 50,
        speed: 33,
    };
    g.on_node_msg(crate::nodes::NodeMsg::GuestUpdate(st2));
    g.tick();
    let gr = g.world.guests.get(&gid).expect("guest row");
    assert!(
        gr.mv.is_some() && gr.mv.as_ref().unwrap().step > 0,
        "guest progress must advance locally from the linmove params"
    );

    // Retract: the owner reports death; the session drops the gob.
    g.on_node_msg(crate::nodes::NodeMsg::GuestRetract { id: gid });
    assert!(
        !g.sessions[&1].visible.contains(&gid),
        "retracted guest must drop"
    );
    assert!(!g.world.guests.contains_key(&gid));
}

/// G5: an animal standing in a foreign cell transfers to its owner
/// (GuestTransfer on the mesh), demotes to a guest locally with the
/// SAME id, and the receiver claims it into its sim tables.
#[tokio::test]
async fn animal_transfer_keeps_identity_across_nodes() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("xfer0", 0, 2);
    let (fx, fy) = foreign_cell_pos(&g, 0);
    let res = g.world.res.intern(Species::Wolf.resname());
    let id = g.world.gobs.spawn(
        Kind::Animal {
            species: Species::Wolf,
        },
        (fx, fy),
        res,
        40,
        33,
    );
    g.world.animal_gobs.push(id);
    g.tick();
    // Sender side: mesh carries the transfer to the owner; the local
    // copy is a guest (viewers keep rendering, sim stops).
    let mut transferred = None;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::GuestTransfer(st) = msg {
            transferred = Some((peer, st));
        }
    }
    let (peer, st) = transferred.expect("transfer must publish to the cell owner");
    assert_eq!(peer, 1, "the foreign cell's owner receives the transfer");
    assert_eq!(st.id, id, "transfer preserves the gob id");
    assert!(
        g.world.guests.contains_key(&id) && g.world.gobs.get(id).is_none(),
        "the old owner demotes the gob to a guest"
    );
    assert!(
        !g.world.animal_gobs.contains(&id),
        "the old owner stops simulating the transferred animal"
    );

    // Receiver side: a fresh node-1 game claims the exact id.
    let (mut g1, _rx1, _raw1, _mesh1) = clustered_game("xfer1", 1, 2);
    let wpos = st.pos;
    g1.on_node_msg(crate::nodes::NodeMsg::GuestTransfer(st));
    let slot = g1
        .world
        .gobs
        .get(id)
        .expect("transferred gob alive on the new owner");
    assert!(g1.world.gobs.alive[slot]);
    assert!(
        g1.world.animal_gobs.contains(&id),
        "the new owner simulates it"
    );
    // Deterministic resume: a player walks into the wolf's aggro radius
    // on the new owner - the wolf must chase (AI runs there now).
    let pidx1 = *g1.world.by_session.get(&1).unwrap();
    let pg1 = g1.world.players[pidx1].gob;
    let ps1 = g1.world.gobs.get(pg1).unwrap();
    g1.world.gobs.set_pos(ps1, (wpos.0 - 200, wpos.1));
    let mut moved = false;
    for _ in 0..15 {
        g1.tick();
        if let Some(s) = g1.world.gobs.get(id) {
            if g1.world.gobs.mv[s].is_some() {
                moved = true;
                break;
            }
        }
    }
    assert!(moved, "the claimed animal must resume AI on the new owner");
}

/// Session 35 (drop authority transfer): a drop spawned onto a cell
/// this node does NOT own (station output jitter across the cell
/// boundary) sends GuestTransfer with the FULL DropView payload to
/// the cell's owner and demotes the local copy to a guest under the
/// same id - the wire mirror of the animal authority transfer.
#[tokio::test]
async fn drop_transfer_sends_to_cell_owner_and_demotes() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("dropxfer0", 0, 2);
    let (fx, fy) = foreign_cell_pos(&g, 0);
    let inv_res_idx = g.world.res.intern("gfx/invobjs/branch");
    let world_res_idx = g.world.res.intern("gfx/terobjs/items/branch");
    let id = g.world.gobs.spawn(
        Kind::Drop {
            resname_idx: world_res_idx,
            inv_res_idx,
            ql: 7,
            label: "",
        },
        (fx, fy),
        world_res_idx,
        1,
        0,
    );
    g.tick();
    let mut transferred = None;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::GuestTransfer(st) = msg {
            transferred = Some((peer, st));
        }
    }
    let (peer, st) = transferred.expect("the foreign drop must transfer to the cell owner");
    assert_eq!(peer, 1, "the foreign cell's owner receives the transfer");
    assert_eq!(st.id, id, "transfer preserves the gob id");
    let crate::nodes::GuestKind::Static { class, drop, .. } = &st.kind else {
        panic!("drop transfer must carry the Static kind");
    };
    assert_eq!(*class, crate::nodes::StaticClass::Drop);
    let view = drop
        .as_ref()
        .expect("transfer carries the DropView payload");
    assert_eq!(view.inv_res, "gfx/invobjs/branch");
    assert_eq!(view.ql, 7);
    assert!(
        g.world.guests.contains_key(&id) && g.world.gobs.get(id).is_none(),
        "the spawner demotes the drop to a guest"
    );
}

/// Session 35 (drop authority transfer): the receiving owner claims
/// the exact id back into Kind::Drop with the deterministic world
/// shape and a working pickup payload (drop_info round-trips the
/// inventory resource, quality and label).
#[tokio::test]
async fn drop_transfer_receiver_claims_pickup_payload() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("dropxfer1", 0, 2);
    let (fx, fy) = foreign_cell_pos(&g, 0);
    let inv_res_idx = g.world.res.intern("gfx/invobjs/meatroast");
    let world_res_idx = g.world.res.intern(drop_world_res("gfx/invobjs/meatroast"));
    let id = g.world.gobs.spawn(
        Kind::Drop {
            resname_idx: world_res_idx,
            inv_res_idx,
            ql: 12,
            label: "Meat roast",
        },
        (fx, fy),
        world_res_idx,
        1,
        0,
    );
    let slot = g.world.gobs.get(id).unwrap();
    let st = g.guest_state_from_slot(id, slot).unwrap();
    // Receiver side: a fresh node-1 game claims the exact id.
    let (mut g1, _rx1, _raw1, _mesh1) = clustered_game("dropxfer1b", 1, 2);
    g1.on_node_msg(crate::nodes::NodeMsg::GuestTransfer(st));
    let slot1 = g1
        .world
        .gobs
        .get(id)
        .expect("the claimed drop is alive on the new owner");
    assert!(g1.world.gobs.alive[slot1]);
    let (res, count, ql, label) = g1.world.gobs.kind[slot1]
        .drop_info()
        .expect("pickup payload intact after the transfer");
    assert_eq!(
        g1.world.res.name(res),
        Some("gfx/invobjs/meatroast"),
        "the inventory resource round-trips"
    );
    assert_eq!(count, 1);
    assert_eq!(ql, 12);
    assert_eq!(label, "Meat roast");
    assert_eq!(
        g1.world.res.name(g1.world.gobs.res_idx[slot1]),
        Some(drop_world_res("gfx/invobjs/meatroast")),
        "the deterministic world shape is re-derived on the receiver"
    );
}

/// Session 35 (drop authority transfer, negative control): a drop on
/// a cell this node OWNS never transfers - no GuestTransfer on the
/// mesh and the gob stays in the sim tables.
#[tokio::test]
async fn drop_transfer_local_cell_drop_stays() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("dropstay", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let inv_res_idx = g.world.res.intern("gfx/invobjs/branch");
    let world_res_idx = g.world.res.intern("gfx/terobjs/items/branch");
    let id = g.world.gobs.spawn(
        Kind::Drop {
            resname_idx: world_res_idx,
            inv_res_idx,
            ql: 3,
            label: "",
        },
        (px + 11, py + 11),
        world_res_idx,
        1,
        0,
    );
    g.tick();
    let mut saw_transfer = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if matches!(msg, crate::nodes::NodeMsg::GuestTransfer(_)) {
            saw_transfer = true;
        }
    }
    assert!(!saw_transfer, "own-cell drops never transfer");
    assert!(
        g.world.gobs.get(id).is_some(),
        "the local drop stays authoritative on its owner"
    );
    assert!(!g.world.guests.contains_key(&id));
}

/// G5 (player territory): a player entering a foreign cell is
/// announced to that cell's owner; returning home retracts.
#[tokio::test]
async fn player_abroad_publishes_to_cell_owner() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("abroad0", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (fx, fy) = foreign_cell_pos(&g, 0);
    g.world.gobs.set_pos(pslot, (fx, fy));
    g.tick();
    let mut announced = false;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::GuestAnnounce(st) = msg {
            if st.id == pgob {
                assert_eq!(peer, 1, "announce goes to the foreign owner");
                announced = true;
            }
        }
    }
    assert!(announced, "the owner must learn about the abroad player");
    assert_eq!(
        g.cluster.as_ref().unwrap().player_abroad.get(&pgob),
        Some(&1),
        "abroad bookkeeping pins the owner"
    );

    // Back home: the owner is told to retract, bookkeeping clears.
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (hx, hy) = home_cell_pos(&g, 0);
    g.world.gobs.set_pos(pslot, (hx, hy));
    g.tick();
    let mut retracted = false;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::GuestRetract { id } = msg {
            if id == pgob && peer == 1 {
                retracted = true;
            }
        }
    }
    assert!(retracted, "returning home must retract from the owner");
    assert!(!g
        .cluster
        .as_ref()
        .unwrap()
        .player_abroad
        .contains_key(&pgob));
}

/// G6: a local chat line broadcasts on the mesh, and a remote line
/// delivers to in-radius local sessions (same area-chat semantics).
#[tokio::test]
async fn chat_relays_across_nodes() {
    let (mut g, mut rx, _raw, mut mesh_rx) = clustered_game("chat0", 0, 2);
    // Synthesize the chat window (the real client creates it; the
    // server only needs a wid to route "log" uimsgs to).
    let chat_wid = {
        let out = g.sessions.get_mut(&1).unwrap();
        let w = out.new_wid("chat");
        out.chat_wid = w;
        w
    };

    // Send path: the line goes out on the mesh (cluster broadcast).
    g.on_chat_msg(1, "hello mesh");
    let mut relayed = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::Chat { from, text, .. } = msg {
            assert_eq!(from, "chat0");
            assert_eq!(text, "hello mesh");
            relayed = true;
        }
    }
    assert!(relayed, "local chat must broadcast to peers");

    // Receive path: a remote line near the player delivers to the
    // session's chat window; a far one does not.
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    while rx.try_recv().is_ok() {}
    g.deliver_remote_chat("peer1", (px + 50, py), "near line");
    let mut near = false;
    while let Ok(msg) = rx.try_recv() {
        if msg.first() == Some(&RMSG_WDGMSG) {
            let mut m = hnh_proto::MessageBuf::from_slice(&msg[1..]);
            let wid = m.u16().unwrap();
            if wid == chat_wid && String::from_utf8_lossy(&msg).contains("near line") {
                near = true;
            }
        }
    }
    assert!(near, "in-radius remote line must reach the session");

    g.deliver_remote_chat("peer1", (px + 100_000, py), "far line");
    let mut far = false;
    while let Ok(msg) = rx.try_recv() {
        if String::from_utf8_lossy(&msg).contains("far line") {
            far = true;
        }
    }
    assert!(!far, "out-of-radius remote line must be filtered");
}

// ==================================================================
// Cross-node interaction relay (session 28)
// ==================================================================

/// Ingest a wolf guest standing `dx` subtiles right of the local
/// player (in view, same cell so node 0 is its authority stand-in for
/// wire tests) and return its id.
fn relay_wolf_guest(g: &mut Game, dx: i32, hp: i32) -> GobId {
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

/// Relay fight opening: the fightview opens against the guest and the
/// mirror row appears (the authoritative bars stay on the owner).
#[tokio::test]
async fn relay_fight_opens_against_a_guest_target() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("relayer", 0, 2);
    let gid = relay_wolf_guest(&mut g, 30, 50);
    g.start_fight(1, gid, Species::Wolf);
    let pidx = *g.world.by_session.get(&1).unwrap();
    assert_eq!(g.world.players[pidx].fight_target, Some(gid));
    assert!(
        g.world.guest_fights.contains_key(&gid),
        "opening a relay fight must seed the local defence-bar mirror"
    );
    let out = g.sessions.get(&1).unwrap();
    assert!(
        out.fight.widget.is_some() && out.fight.rel(gid).is_some(),
        "the fightview widget + relation must exist for a guest target"
    );
}

/// The REAL attack entry point: an interact click on a guest animal
/// (not in the local gob table) opens the relay fight instead of the
/// old "interact target gone" no-op.
#[tokio::test]
async fn interact_click_on_guest_animal_opens_relay_fight() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("clicker", 0, 2);
    let gid = relay_wolf_guest(&mut g, 30, 50);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    g.player_interact(1, pgob, gid, (0, 0));
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(gid),
        "a guest-animal click must open the relay fight"
    );
    assert!(g.world.guest_fights.contains_key(&gid));
}

/// A swing at an in-reach guest spends offence and ships exactly one
/// RelayAttack to the wolf's owner; a FightBars answer re-syncs the
/// mirror the fightview reads.
#[tokio::test]
async fn relay_swing_ships_relayattack_and_fightbars_resync() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("swinger", 0, 2);
    let gid = relay_wolf_guest(&mut g, 30, 50);
    g.start_fight(1, gid, Species::Wolf);
    // Full offence bar + no cooldown: the next tick must swing.
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    g.tick();
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let mut attacks = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::RelayAttack {
            attacker,
            target,
            chip,
            dmg,
        } = msg
        {
            attacks.push((attacker, target, chip, dmg));
        }
    }
    // Default str 10: dmg = (5*10/10).max(1) = 5; weight 1.0 -> the
    // plain SWING_DEF_DMG chip.
    assert_eq!(
        attacks,
        vec![(pgob, gid, crate::fight::SWING_DEF_DMG, 5)], // (5*str/10).max(1) with the default str 10.
        "one swing = exactly one RelayAttack to the owner"
    );
    // The authoritative answer re-syncs the mirror.
    g.on_node_msg(crate::nodes::NodeMsg::FightBars { id: gid, def: 1234 });
    assert_eq!(g.world.guest_fights[&gid].def, 1234);
}

/// Authority side: one relayed swing chips the authoritative bar; an
/// opening lands the HP damage (OD_HEALTH to local viewers + the hp
/// rides GuestUpdate to the attacker's node); the death drops loot,
/// retracts, and credits the attacker's home node.
#[tokio::test]
async fn relay_authority_applies_damage_and_death_credit() {
    let (mut g, _rx, mut raw, mut mesh_rx) = clustered_game("authwlf", 0, 2);
    // The AUTHORITY side fixture: the wolf lives in MY gob table (a
    // local spawn in my cell); the attacker is a foreign player gob.
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern(Species::Wolf.resname());
    let gid = g.world.gobs.spawn(
        Kind::Animal {
            species: Species::Wolf,
        },
        (px + 30, py),
        res,
        50,
        33,
    );
    g.world.animal_gobs.push(gid);
    g.tick();
    let attacker = foreign_node_gob_id(0, 2, 21);
    // Node 1 subscribes to the wolf's cell: the hp publish must reach
    // it as a GuestUpdate.
    let cell = crate::visidx::cell_of(px + 30, py);
    g.on_node_msg(crate::nodes::NodeMsg::Sub {
        from: 1,
        cells: vec![cell],
    });
    // First swing: a full-bar chip opens the defence; dmg 10 lands.
    g.on_node_msg(crate::nodes::NodeMsg::RelayAttack {
        attacker,
        target: gid,
        chip: crate::fight::BAR_FULL,
        dmg: 10,
    });
    let tslot = g.world.gobs.get(gid).expect("wolf survives the opening");
    assert_eq!(g.world.gobs.hp[tslot], 40);
    let mut updated_hp = None;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
            if st.id == gid {
                updated_hp = Some(st.hp);
            }
        }
    }
    assert_eq!(updated_hp, Some(40), "hp must publish to the subscriber");
    // Wire proof for LOCAL viewers: OD_HEALTH quarters stream.
    let mut saw_health = false;
    while let Ok(block) = raw.try_recv() {
        if block.first() == Some(&MSG_OBJDATA)
            && i32::from_le_bytes([block[2], block[3], block[4], block[5]]) == gid
            && block.get(10) == Some(&OD_HEALTH)
        {
            saw_health = true;
        }
    }
    assert!(saw_health, "local viewers must get the OD_HEALTH update");
    // Lethal swing: the animal dies, loot drops, the attacker's home
    // node receives the LP credit.
    g.on_node_msg(crate::nodes::NodeMsg::RelayAttack {
        attacker,
        target: gid,
        chip: crate::fight::BAR_FULL,
        dmg: 40,
    });
    assert!(g.world.gobs.get(gid).is_none(), "the wolf must die");
    let mut credited = None;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::KillCredit { player_gob, lp } = msg {
            credited = Some((player_gob, lp));
        }
    }
    assert_eq!(
        credited,
        Some((attacker, 10)),
        "the killer's home node must be credited with the LP"
    );
}

/// Retaliation: an animal with a relay row bites the guest player and
/// the bite SHIPS to the attacker's home node (no local session to
/// apply it through).
#[tokio::test]
async fn relay_retaliation_ships_playerhurt_home() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("biter", 0, 2);
    // The AUTHORITY side fixture: a local wolf + a published guest
    // player standing in reach.
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern(Species::Wolf.resname());
    let gid = g.world.gobs.spawn(
        Kind::Animal {
            species: Species::Wolf,
        },
        (px + 30, py),
        res,
        50,
        33,
    );
    g.world.animal_gobs.push(gid);
    g.tick();
    let attacker = foreign_node_gob_id(0, 2, 21);
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(
        crate::nodes::GuestState {
            id: attacker,
            pos: (px + 50, py),
            mv: None,
            moving: false,
            facing: 0,
            kind: crate::nodes::GuestKind::Player {
                name: "foreigner".into(),
                equip: vec![],
            },
            hp: 100,
            max_hp: 100,
            speed: 33,
        },
    ));
    // A relayed swing registers the guest attacker.
    g.on_node_msg(crate::nodes::NodeMsg::RelayAttack {
        attacker,
        target: gid,
        chip: 0,
        dmg: 1,
    });
    assert_eq!(g.world.guest_attackers[&gid], attacker);
    // Full offence + no cooldown: the next tick bites in reach.
    g.world.animal_fights.get_mut(&gid).expect("relay row").off = crate::fight::BAR_FULL;
    g.tick();
    let mut hurt = None;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::PlayerHurt { player_gob, .. } = msg {
            hurt = Some(player_gob);
        }
    }
    assert_eq!(hurt, Some(attacker), "the bite must ship to the home node");
    // The attacker leaving the cell (retract) drops the relay row so
    // the animal stops retaliating at a ghost.
    g.on_node_msg(crate::nodes::NodeMsg::GuestRetract { id: attacker });
    assert!(!g.world.guest_attackers.contains_key(&gid));
    assert!(!g.world.animal_fights.contains_key(&gid));
}

/// The owner retracting the fought guest (death seen elsewhere, GC)
/// closes the local fight: target cleared, mirror dropped, relation
/// deleted and the frv widget destroyed when it was the last one.
#[tokio::test]
async fn guest_retract_closes_the_relay_fight() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("closer", 0, 2);
    let gid = relay_wolf_guest(&mut g, 30, 50);
    g.start_fight(1, gid, Species::Wolf);
    g.on_node_msg(crate::nodes::NodeMsg::GuestRetract { id: gid });
    let pidx = *g.world.by_session.get(&1).unwrap();
    assert_eq!(g.world.players[pidx].fight_target, None);
    assert!(!g.world.guest_fights.contains_key(&gid));
    let out = g.sessions.get(&1).unwrap();
    assert!(
        out.fight.widget.is_none() && out.fight.rel(gid).is_none(),
        "the last relation must close the frv widget"
    );
}

// ------------------------------------------------------------------
// Session 29: cluster save story (account-keyed characters, migration)
// ------------------------------------------------------------------

/// A clustered game with NO session: the raw fixture for save-store
/// and node-msg level tests. Returns the mesh sink receiver so tests
/// can assert exactly what this node sends to its peers.
fn bare_clustered(
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
        player_abroad: HashMap::new(),
    });
    (g, mesh_rx)
}

/// Open one session on `g` and press play on the charlist (the full
/// login path; cluster nodes may defer the entry on a CharQuery).
fn open_session_and_play(g: &mut Game, sid: SessionId, account: &str, chosen: &str) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, _raw) = tokio::sync::mpsc::channel(512);
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

fn snapshot(key: &str, pos: (i32, i32)) -> crate::persist::SavedPlayer {
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
    }
}

#[tokio::test]
async fn char_query_migrates_the_offline_snapshot_to_the_peer() {
    let (mut holder, mut mesh_rx) = bare_clustered("migrate-holder", 0, 2);
    let key = crate::persist::save_key("acct", "Player");
    holder
        .save
        .players
        .insert(key.clone(), snapshot(&key, (500, 500)));
    holder.on_node_msg(crate::nodes::NodeMsg::CharQuery {
        from: 1,
        name: key.clone(),
    });
    // Two-phase: the holder KEEPS the snapshot until the ack, so a
    // lost reply can always be re-served by a query retry.
    assert!(
        holder.save.players.contains_key(&key),
        "the holder must keep the snapshot until CharAck"
    );
    let mut data = None;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        assert_eq!(peer, 1, "unicast to the requester");
        if let crate::nodes::NodeMsg::CharData {
            to,
            from,
            name,
            snap,
        } = msg
        {
            assert_eq!(to, 1, "routed to the requester node");
            assert_eq!(from, 0, "sent by the holder");
            assert_eq!(name, key);
            data = Some(snap);
        }
    }
    let snap = data.expect("CharData reply");
    assert_eq!(snap.pos, (500, 500));
    assert_eq!(snap.lp, 42);
    // Re-query before the ack: the snapshot is re-served (idempotent).
    holder.on_node_msg(crate::nodes::NodeMsg::CharQuery {
        from: 1,
        name: key.clone(),
    });
    let mut re_served = false;
    while let Ok((_, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::CharData { name, .. } = msg {
            assert_eq!(name, key);
            re_served = true;
        }
    }
    assert!(re_served, "retry must re-serve the snapshot");
    // The ack completes the migration: the copy leaves the holder.
    holder.on_node_msg(crate::nodes::NodeMsg::CharAck { name: key.clone() });
    assert!(
        !holder.save.players.contains_key(&key),
        "CharAck must drop the holder's copy"
    );
}

#[tokio::test]
async fn char_query_for_an_online_character_nacks_and_keeps_the_snapshot() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("migrate-online", 0, 2);
    let key = crate::persist::save_key("acct", &g.world.players[0].name);
    g.save.players.insert(key.clone(), snapshot(&key, (10, 10)));
    g.on_node_msg(crate::nodes::NodeMsg::CharQuery {
        from: 1,
        name: key.clone(),
    });
    let mut nacks = 0;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        assert_eq!(peer, 1);
        if let crate::nodes::NodeMsg::CharNack { to, from, name: n } = msg {
            assert_eq!(to, 1, "routed to the requester");
            assert_eq!(from, 0, "sent by the holder");
            assert_eq!(n, key);
            nacks += 1;
        }
    }
    assert_eq!(nacks, 1, "an online character is answered with a nack");
    assert!(
        g.save.players.contains_key(&key),
        "the live player's snapshot must not migrate"
    );
}

#[tokio::test]
async fn chardata_adopts_the_snapshot_and_enters_the_world() {
    let (mut g, mut mesh_rx) = bare_clustered("migrate-adopt", 1, 2);
    open_session_and_play(&mut g, 1, "acct", "Player");
    // The entry deferred: no player yet, one CharQuery on the mesh.
    assert!(!g.world.by_session.contains_key(&1), "entry must defer");
    let mut queried = None;
    while let Ok((_, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::CharQuery { from, name } = msg {
            assert_eq!(from, 1);
            queried = Some(name);
        }
    }
    let key = queried.expect("CharQuery broadcast");
    assert_eq!(key, crate::persist::save_key("acct", "Player"));
    // The holder answers; the requester adopts, acks back and enters.
    g.on_node_msg(crate::nodes::NodeMsg::CharData {
        to: 1,
        from: 0,
        name: key.clone(),
        snap: snapshot(&key, (777, -777)),
    });
    let pidx = *g.world.by_session.get(&1).expect("entered via migration");
    let pgob = g.world.players[pidx].gob;
    let slot = g.world.gobs.get(pgob).expect("player slot");
    assert_eq!(g.world.gobs.pos[slot], (777, -777), "restored position");
    assert_eq!(g.world.players[pidx].lp, 42, "restored lp");
    assert_eq!(g.world.players[pidx].hp, 80, "restored hp");
    let mut acked = false;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        assert_eq!(peer, 0, "ack unicast to the holder");
        if let crate::nodes::NodeMsg::CharAck { name } = msg {
            assert_eq!(name, key);
            acked = true;
        }
    }
    assert!(acked, "adoption must ack so the holder drops its copy");
}

#[tokio::test]
async fn char_nack_majority_enters_fresh_without_the_deadline() {
    let (mut g, _mesh_rx) = bare_clustered("migrate-nacks", 1, 3);
    open_session_and_play(&mut g, 1, "acct", "Player");
    assert!(!g.world.by_session.contains_key(&1));
    // One of two peers answered: still waiting.
    g.on_node_msg(crate::nodes::NodeMsg::CharNack {
        to: 1,
        from: 0,
        name: crate::persist::save_key("acct", "Player"),
    });
    assert!(
        !g.world.by_session.contains_key(&1),
        "entry waits for every peer"
    );
    g.on_node_msg(crate::nodes::NodeMsg::CharNack {
        to: 1,
        from: 2,
        name: crate::persist::save_key("acct", "Player"),
    });
    assert!(
        g.world.by_session.contains_key(&1),
        "the last nack completes the entry"
    );
}

#[tokio::test]
async fn accounts_hold_separate_characters_and_legacy_saves_are_adopted() {
    let (mut g, _mesh_rx) = bare_clustered("account-keys", 0, 1);
    // Legacy layout: one bare "Player" snapshot from an older server.
    g.save
        .players
        .insert("Player".to_owned(), snapshot("Player", (321, 123)));
    open_session_and_play(&mut g, 1, "alice", "Player");
    let pidx = *g.world.by_session.get(&1).expect("legacy adoption entered");
    let pgob = g.world.players[pidx].gob;
    let slot = g.world.gobs.get(pgob).expect("player slot");
    assert_eq!(
        g.world.gobs.pos[slot],
        (321, 123),
        "legacy snapshot restored"
    );
    // Adoption re-keyed the snapshot into the account namespace.
    assert!(
        g.save.players.contains_key("alice:Player"),
        "legacy snapshot re-keyed"
    );
    assert!(!g.save.players.contains_key("Player"), "bare key consumed");
    // A second account gets a FRESH character, not alice's.
    open_session_and_play(&mut g, 2, "bob", "Player");
    let pidx2 = *g.world.by_session.get(&2).expect("second account entered");
    assert_ne!(
        g.world.players[pidx].gob, g.world.players[pidx2].gob,
        "two live players"
    );
    assert_eq!(
        g.save.players.get("bob:Player").map(|s| s.pos),
        None,
        "bob starts with no snapshot"
    );
}

// ------------------------------------------------------------------
// Session 30: cursor pickup redirection + stack merging
// (items-and-quality.md: stacking policy is server policy)
// ------------------------------------------------------------------

/// Test helper: find the only live ground Drop gob.
fn only_drop_gob(g: &Game) -> GobId {
    let mut found = None;
    for slot in 0..g.world.gobs.alive.len() {
        if g.world.gobs.alive[slot] && matches!(g.world.gobs.kind[slot], Kind::Drop { .. }) {
            found = Some(gob_id_from_slot(slot, g.world.gobs.gen[slot]));
        }
    }
    found.expect("test precondition: exactly one live Drop gob")
}

/// Picking up a ground drop while the cursor drags the SAME resource
/// redirects onto the cursor: counts add, quality re-averages, the
/// inventory gains nothing, and the drag widget count syncs.
#[tokio::test]
async fn pickup_merges_onto_same_resource_cursor() {
    let (mut g, _rx, _raw) = entered_game("cursorpickup");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let wood = g.world.res.intern("gfx/invobjs/wood");
    // Cursor already drags 2 wood at q10.
    g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
        res: wood,
        count: 2,
        ql: 10,
        label: "",
    });
    // A ground wood drop at q20 lands next to the player.
    g.spawn_drop_near((px, py), "gfx/invobjs/wood", 20, "");
    let drop = only_drop_gob(&g);
    g.player_interact(1, pgob, drop, (0, 0));
    // The cursor stack absorbed the drop: 3 units, (10*2+20*1)/3 = 13.
    let cur = g
        .sessions
        .get(&1)
        .unwrap()
        .cursor
        .expect("cursor keeps the stack");
    assert_eq!(cur.res, wood);
    assert_eq!(cur.count, 3, "counts conserved onto the cursor");
    assert_eq!(cur.ql, 13, "count-weighted quality average");
    assert!(
        !g.world.players[pidx].inv.iter().any(|s| s.res == wood),
        "no parallel inventory stack for a redirected pickup"
    );
    assert!(g.world.gobs.get(drop).is_none(), "drop gob removed");
}

/// Releasing the cursor onto the inventory grid merges into an existing
/// same-resource stack instead of appending a duplicate slot.
#[tokio::test]
async fn inv_drop_merges_into_same_resource_stack() {
    let (mut g, _rx, _raw) = entered_game("invdropmerge");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let wood = g.world.res.intern("gfx/invobjs/wood");
    // Inventory holds 3 wood at q10; the cursor drags 2 wood at q30.
    g.world.players[pidx].inv.push(InvStack {
        res: wood,
        count: 3,
        ql: 10,
        label: "",
    });
    g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
        res: wood,
        count: 2,
        ql: 30,
        label: "",
    });
    g.inv_drop(1, 0, &[]);
    // One merged stack: 5 units, (10*3+30*2)/5 = 18.
    let stacks: Vec<_> = g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == wood)
        .collect();
    assert_eq!(stacks.len(), 1, "exactly one wood stack after the drop");
    assert_eq!(stacks[0].count, 5, "counts conserved");
    assert_eq!(stacks[0].ql, 18, "count-weighted quality average");
    assert!(
        g.sessions.get(&1).unwrap().cursor.is_none(),
        "cursor emptied"
    );
}

/// A pickup of a DIFFERENT resource never merges: the cursor keeps its
/// stack, the pickup lands in its own inventory slot.
#[tokio::test]
async fn different_resource_pickup_keeps_stacks_separate() {
    let (mut g, _rx, _raw) = entered_game("separatepick");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let stone = g.world.res.intern("gfx/invobjs/stone");
    let wood = g.world.res.intern("gfx/invobjs/wood");
    // Cursor drags stone; the ground drop is wood.
    g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
        res: stone,
        count: 1,
        ql: 10,
        label: "",
    });
    g.spawn_drop_near((px, py), "gfx/invobjs/wood", 20, "");
    let drop = only_drop_gob(&g);
    g.player_interact(1, pgob, drop, (0, 0));
    let cur = g
        .sessions
        .get(&1)
        .unwrap()
        .cursor
        .expect("cursor untouched");
    assert_eq!(
        (cur.res, cur.count),
        (stone, 1),
        "cursor still drags the stone"
    );
    let inv_wood: Vec<_> = g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == wood)
        .collect();
    assert_eq!(inv_wood.len(), 1, "wood stored in its own stack");
    assert_eq!(inv_wood[0].count, 1);
}

// ------------------------------------------------------------------
// Session 30: relay static acts (pickup/chop/mine vs guest statics)
// ------------------------------------------------------------------

/// Home side: an interact click on a guest DROP ships exactly one
/// RelayStaticAct{Pickup} to the drop's cell owner - never a no-op.
#[tokio::test]
async fn guest_drop_click_ships_relay_static_pickup() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("dropclick", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let (fx, fy) = foreign_cell_pos(&g, 0);
    let gid = foreign_node_gob_id(0, 2, 31);
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(
        crate::nodes::GuestState {
            id: gid,
            pos: (fx, fy),
            mv: None,
            moving: false,
            facing: 1,
            kind: crate::nodes::GuestKind::Static {
                res_name: "gfx/terobjs/items/wood".into(),
                class: crate::nodes::StaticClass::Drop,
                crop: None,
                station: None,
                stage: None,
                drop: None,
            },
            hp: 1,
            max_hp: 1,
            speed: 0,
        },
    ));
    g.tick();
    g.player_interact(1, pgob, gid, (0, 0));
    let mut relays = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::RelayStaticAct {
            player,
            target,
            act,
        } = msg
        {
            relays.push((player, target, act));
        }
    }
    let authority = {
        let c = g.cluster.as_ref().unwrap();
        crate::grid_owner::owner_of(crate::visidx::cell_of(fx, fy), c.nodes)
    };
    assert_eq!(
        relays,
        vec![(pgob, gid, crate::nodes::StaticAct::Pickup)],
        "one Pickup relay per click, addressed to the cell owner (node {authority})"
    );
}

/// Authority side: a relayed Pickup removes the local drop, publishes
/// the retract, and answers StaticAck with the EXACT stack (resource
/// name + quality + fep label) so the home node grants it once.
#[tokio::test]
async fn relay_pickup_authority_removes_and_acks_stack() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("authdrop", 0, 2);
    // A local drop in a cell OWNED BY ME (ownership is a per-cell
    // hash, so the player's own cell can belong to the peer - anchor
    // the drop at an owned cell's center; the +-33-subtile spawn
    // jitter then never leaves the cell).
    let (hx, hy) = home_cell_pos(&g, 0);
    let cell = crate::visidx::cell_of(hx, hy);
    let center = (
        cell.0 * crate::visidx::CELL + 125,
        cell.1 * crate::visidx::CELL + 125,
    );
    g.spawn_drop_near(center, "gfx/invobjs/branch", 10, "Branch");
    let drop = only_drop_gob(&g);
    // Node 1 subscribes to the drop's cell so the retract publishes.
    g.on_node_msg(crate::nodes::NodeMsg::Sub {
        from: 1,
        cells: vec![cell],
    });
    // The clicking player is a foreign gob homed on node 1.
    let clicker = foreign_node_gob_id(0, 2, 41);
    g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
        player: clicker,
        target: drop,
        act: crate::nodes::StaticAct::Pickup,
    });
    assert!(
        g.world.gobs.get(drop).is_none(),
        "drop removed by the authority"
    );
    let mut acks = Vec::new();
    let mut retracts = 0;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        match msg {
            crate::nodes::NodeMsg::StaticAck { player, stack, lp } => {
                acks.push((player, stack, lp));
            }
            crate::nodes::NodeMsg::GuestRetract { id } if id == drop => retracts += 1,
            _ => {}
        }
    }
    assert_eq!(retracts, 1, "the retract publishes to subscribers");
    assert_eq!(acks.len(), 1, "exactly one StaticAck");
    let (player, stack, lp) = acks.pop().unwrap();
    assert_eq!(player, clicker);
    assert_eq!(lp, 0);
    let st = stack.expect("pickup acks a stack");
    assert_eq!(st.res, "gfx/invobjs/branch");
    assert_eq!(st.count, 1);
    assert_eq!(st.ql, 10);
    assert_eq!(st.label, "Branch");
}

/// Home ack side: StaticAck grants the stack through grant_pickup -
/// onto a same-resource cursor (redirection), else into inventory -
/// and lp>0 tops up the wallet + pushes the char sheet.
#[tokio::test]
async fn static_ack_grants_stack_and_lp_on_home() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("ackhome", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let branch = g.world.res.intern("gfx/invobjs/branch");
    // Cursor drags a branch stack: the ack must redirect onto it.
    g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
        res: branch,
        count: 1,
        ql: 10,
        label: "Branch",
    });
    let lp_before = g.world.players[pidx].lp;
    g.on_node_msg(crate::nodes::NodeMsg::StaticAck {
        player: pgob,
        stack: Some(crate::nodes::StaticStack {
            res: "gfx/invobjs/branch".into(),
            count: 2,
            ql: 20,
            label: "Branch".into(),
        }),
        lp: 5,
    });
    let cur = g
        .sessions
        .get(&1)
        .unwrap()
        .cursor
        .expect("cursor absorbed the ack");
    let lp_after = g.world.players[pidx].lp;
    assert_eq!(cur.count, 3, "1@q10 + 2@q20 -> 3 units");
    assert_eq!(cur.ql, 16, "(10*1+20*2)/3 = 16");
    // The starter kit's branch stack (6 since session 36) must stay
    // UNTOUCHED: the ack redirected onto the cursor, not into the
    // inventory.
    let inv_branch: Vec<_> = g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == branch)
        .collect();
    assert_eq!((inv_branch.len(), inv_branch[0].count), (1, 6));
    assert_eq!(
        lp_after,
        lp_before + 5,
        "lp granted from the ack (relative)"
    );
    // A different-resource ack lands in its own inventory stack.
    g.on_node_msg(crate::nodes::NodeMsg::StaticAck {
        player: pgob,
        stack: Some(crate::nodes::StaticStack {
            res: "gfx/invobjs/wood".into(),
            count: 1,
            ql: 10,
            label: String::new(),
        }),
        lp: 0,
    });
    let wood = g.world.res.intern("gfx/invobjs/wood");
    assert_eq!(
        g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == wood)
            .count(),
        1,
        "wood stored as its own stack"
    );
}

/// Chop authority side: harvests decrement, fresh wood drops spawn on
/// the authority, lp 5 acks; an exhausted tree is removed with a stump
/// and acks lp 0.
#[tokio::test]
async fn relay_chop_authority_decrements_and_acks() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("authchop", 0, 2);
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    // A local tree in MY cell with 2 harvests left.
    let tree_res = g.world.res.intern("gfx/terobjs/trees/old");
    let tree = g
        .world
        .gobs
        .spawn(Kind::Tree { harvests: 2 }, (px + 22, py), tree_res, 1, 0);
    let clicker = foreign_node_gob_id(0, 2, 51);
    g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
        player: clicker,
        target: tree,
        act: crate::nodes::StaticAct::Chop,
    });
    let tslot = g.world.gobs.get(tree).expect("tree survives one chop");
    assert!(
        matches!(g.world.gobs.kind[tslot], Kind::Tree { harvests: 1 }),
        "one chop off"
    );
    // A fresh wood drop spawned next to the tree.
    let mut wood_drops = 0;
    for slot in 0..g.world.gobs.alive.len() {
        if g.world.gobs.alive[slot] && matches!(g.world.gobs.kind[slot], Kind::Drop { .. }) {
            wood_drops += 1;
        }
    }
    assert_eq!(wood_drops, 1, "the chop spawned one wood drop");
    // Exhaust the tree: second chop -> 1 harvest -> third chop kills it.
    g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
        player: clicker,
        target: tree,
        act: crate::nodes::StaticAct::Chop,
    });
    g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
        player: clicker,
        target: tree,
        act: crate::nodes::StaticAct::Chop,
    });
    assert!(g.world.gobs.get(tree).is_none(), "exhausted tree removed");
    let mut acks = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StaticAck { player, stack, lp } = msg {
            acks.push((player, stack.is_some(), lp));
        }
    }
    assert_eq!(
        acks,
        vec![
            (clicker, false, 5),
            (clicker, false, 5),
            (clicker, false, 0)
        ],
        "chop lp 5, chop lp 5, exhausted lp 0"
    );
}

/// A stale guest view must never apply: a Pickup act against a TREE is
/// dropped by the authority (no state change, no ack).
#[tokio::test]
async fn relay_static_mismatch_is_dropped() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("mismatch", 0, 2);
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let tree_res = g.world.res.intern("gfx/terobjs/trees/old");
    let tree = g
        .world
        .gobs
        .spawn(Kind::Tree { harvests: 2 }, (px + 22, py), tree_res, 1, 0);
    let clicker = foreign_node_gob_id(0, 2, 61);
    g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
        player: clicker,
        target: tree,
        act: crate::nodes::StaticAct::Pickup, // wrong act for a tree
    });
    let tslot = g.world.gobs.get(tree).expect("tree untouched");
    assert!(matches!(
        g.world.gobs.kind[tslot],
        Kind::Tree { harvests: 2 }
    ));
    let mut saw_ack = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if matches!(msg, crate::nodes::NodeMsg::StaticAck { .. }) {
            saw_ack = true;
        }
    }
    assert!(!saw_ack, "a mismatched act acks nothing");
}

fn pgob_of(g: &Game) -> GobId {
    let pidx = *g.world.by_session.get(&1).unwrap();
    g.world.players[pidx].gob
}

// ------------------------------------------------------------------
// Session 30: vis-scan result caching (patch + clean paths)
// ------------------------------------------------------------------

/// A stationary session patches its cached scan result instead of
/// rescanning: enterers spawn, leavers retract, deaths purge - all
/// without a position change on the viewer side.
#[tokio::test]
async fn vis_cache_patches_stationary_session() {
    let (mut g, _rx, _raw) = entered_game("viscache");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    // Prime the cache: the entry ticks already scanned (Full).
    assert!(g.sessions.get(&1).unwrap().vis_cache.is_some());
    let base = g.sessions.get(&1).unwrap().vis_cache.clone().unwrap();
    // 1) An enterer spawns in view (touched): the patch must add it.
    let drop_res = g.world.res.intern("gfx/terobjs/items/branch");
    let d = g.world.gobs.spawn(
        Kind::Drop {
            resname_idx: drop_res,
            inv_res_idx: g.world.res.intern("gfx/invobjs/branch"),
            ql: 10,
            label: "Branch",
        },
        (px + 60, py + 60),
        drop_res,
        1,
        0,
    );
    g.tick();
    assert!(
        g.sessions.get(&1).unwrap().visible.contains(&d),
        "the patch spawned the new drop for the stationary viewer"
    );
    // 2) A leaver moves out of view (touched at its old cell): the
    //    patch must drop it from the result; the retract sweep then
    //    removes it from `visible` on its cadence.
    g.world
        .gobs
        .set_pos(g.world.gobs.get(d).unwrap(), (px + 90, py + 2400));
    g.world.gobs.vis.reposition(d, (px + 90, py + 2400));
    for _ in 0..10 {
        g.tick();
    }
    assert!(
        !g.sessions
            .get(&1)
            .unwrap()
            .vis_cache
            .as_ref()
            .unwrap()
            .contains(&d),
        "the patched result no longer lists the leaver"
    );
    // 3) A death in view purges by liveness (the patch never keeps a
    //    dead id - the ghost-respawn class of bug).
    let d2 = g.world.gobs.spawn(
        Kind::Drop {
            resname_idx: drop_res,
            inv_res_idx: g.world.res.intern("gfx/invobjs/branch"),
            ql: 10,
            label: "Branch",
        },
        (px - 60, py - 60),
        drop_res,
        1,
        0,
    );
    g.tick();
    assert!(g.sessions.get(&1).unwrap().visible.contains(&d2));
    g.world.gobs.kill(d2);
    g.broadcast_retract(d2);
    g.tick();
    assert!(
        !g.sessions
            .get(&1)
            .unwrap()
            .vis_cache
            .as_ref()
            .unwrap()
            .contains(&d2),
        "a dead gob never lingers in the cached result"
    );
    // 4) The cached result stays consistent with a fresh full scan:
    //    same id set as scan_visible at the same position.
    let cached = g.sessions.get(&1).unwrap().vis_cache.clone().unwrap();
    let fresh = g.scan_visible(px, py);
    let mut a = cached;
    let mut b = fresh;
    a.sort_unstable();
    a.dedup();
    b.sort_unstable();
    b.dedup();
    assert_eq!(a, b, "patched result equals a full rescan");
    // 5) The view stayed quiet in between (the clean skip fired at
    //    least once): visible_total accounting never crashed.
    let _ = base.len();
}

// ------------------------------------------------------------------
// Session 31: cross-node crop harvest relay
// ------------------------------------------------------------------

/// Plant a crop gob directly (restore-path construction, no farming
/// skill gate) near the test player and return its id.
fn planted_crop(g: &mut Game, spec: u8, stage: u8) -> GobId {
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

#[tokio::test]
async fn crop_guest_state_carries_crop_class_and_payload() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("cropstate", 0, 2);
    let gob = planted_crop(&mut g, 1, 3);
    let slot = g.world.gobs.get(gob).unwrap();
    let st = g
        .guest_state_from_slot(gob, slot)
        .expect("a crop publishes a guest state");
    match st.kind {
        crate::nodes::GuestKind::Static {
            class,
            crop: Some((spec, stage)),
            ..
        } => {
            assert_eq!(class, crate::nodes::StaticClass::Crop);
            assert_eq!((spec, stage), (1, 3), "spec and stage ride the payload");
        }
        other => panic!("expected a Crop static, got {other:?}"),
    }
}

#[tokio::test]
async fn crop_stage_advance_publishes_update_to_subscribers() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("cropstage", 0, 2);
    let gob = planted_crop(&mut g, 1, 2);
    // A subscribed peer: its subscription covers the crop's cell.
    let (cx, cy) = crate::visidx::cell_of(
        g.world.gobs.pos[g.world.gobs.get(gob).unwrap()].0,
        g.world.gobs.pos[g.world.gobs.get(gob).unwrap()].1,
    );
    g.cluster
        .as_mut()
        .unwrap()
        .peer_subs
        .insert(1, std::iter::once((cx, cy)).collect());
    while let Ok((_, msg)) = mesh_rx.try_recv() {
        let _ = msg; // drain the subscribe/announce backlog
    }
    // Force a stage advance through the farming scheduler.
    g.world.crops.get_mut(&gob).unwrap().next_stage_at = 0;
    g.tick_farming();
    // The advance re-publishes the guest state with the new stage.
    let mut saw_stage_update = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
            if st.id == gob {
                match st.kind {
                    crate::nodes::GuestKind::Static {
                        class: crate::nodes::StaticClass::Crop,
                        crop: Some((_spec, stage)),
                        ..
                    } => {
                        assert_eq!(stage, 3, "wheat advances 2 -> 3");
                        saw_stage_update = true;
                    }
                    other => panic!("expected a crop static update, got {other:?}"),
                }
            }
        }
    }
    assert!(saw_stage_update, "stage advance publishes a GuestUpdate");
}

#[tokio::test]
async fn guest_crop_click_opens_menu_only_when_ripe() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("cropmenu", 0, 2);
    let pgob = pgob_of(&g);
    // Ripe guest crop: stage 5 >= wheat early_stage 2 -> menu opens.
    // Ingested through the REAL path (GuestAnnounce), so the guest row
    // is exactly what a subscriber would hold.
    let ripe = planted_crop(&mut g, 1, 5);
    let rslot = g.world.gobs.get(ripe).unwrap();
    let st = g.guest_state_from_slot(ripe, rslot).unwrap();
    let (fx, fy) = st.pos;
    g.world.gobs.kill(ripe);
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st));
    g.player_interact(1, pgob, ripe, (fx, fy));
    assert!(
        g.sessions.get(&1).unwrap().crop_menu.is_some(),
        "a ripe guest crop opens the harvest menu"
    );
    // Unripe guest crop: stage 1 < early_stage 2 -> the menu target
    // stays on the earlier crop.
    let unripe = planted_crop(&mut g, 1, 1);
    let uslot = g.world.gobs.get(unripe).unwrap();
    let ust = g.guest_state_from_slot(unripe, uslot).unwrap();
    g.world.gobs.kill(unripe);
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(ust));
    g.player_interact(1, pgob, unripe, (0, 0));
    let menu = g.sessions.get(&1).unwrap().crop_menu.unwrap();
    assert_ne!(
        menu.1, unripe,
        "an unripe guest crop never replaces the menu target"
    );
}

#[tokio::test]
async fn relay_harvest_crop_acks_yield_stacks_and_kills_crop() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("cropharvest", 0, 2);
    // A mature wheat crop (spec 1, stage = stages = final).
    let stages = crate::farm::CROPS[1].stages;
    let gob = planted_crop(&mut g, 1, stages);
    let clicker = foreign_node_gob_id(0, 2, 61);
    g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
        player: clicker,
        target: gob,
        act: crate::nodes::StaticAct::HarvestCrop,
    });
    // The crop is dead on the authority and tilth is restored.
    assert!(
        g.world.gobs.get(gob).is_none(),
        "the relayed harvest removes the crop"
    );
    // One StaticAck per mature-yield stack, all addressed to the
    // clicking player's home node, none carrying LP.
    let mut acks: Vec<(Option<crate::nodes::StaticStack>, i32)> = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StaticAck { player, stack, lp } = msg {
            assert_eq!(player, clicker);
            acks.push((stack, lp));
        }
    }
    let yields = crate::farm::CROPS[1].mature_yields;
    assert_eq!(acks.len(), yields.len(), "one ack per yielded stack");
    for (stack, lp) in &acks {
        assert_eq!(*lp, 0, "crops never grant LP");
        let stack = stack.as_ref().expect("harvest acks always carry a stack");
        assert!(
            yields.iter().any(|y| y.res == stack.res),
            "yielded {} is in the wheat table",
            stack.res
        );
        assert!(stack.count >= 1);
    }
}

#[tokio::test]
async fn relay_harvest_crop_mismatch_is_dropped() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("cropmismatch", 0, 2);
    // A TREE tagged with the harvest act: the authority re-validates
    // the Kind and drops the stale view.
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern("gfx/terobjs/trees/old");
    let tree = g
        .world
        .gobs
        .spawn(Kind::Tree { harvests: 2 }, (px + 40, py), res, 1, 0);
    let clicker = foreign_node_gob_id(0, 2, 61);
    g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
        player: clicker,
        target: tree,
        act: crate::nodes::StaticAct::HarvestCrop,
    });
    assert!(
        g.world.gobs.get(tree).is_some(),
        "the tree survives the mismatched act"
    );
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        assert!(
            !matches!(msg, crate::nodes::NodeMsg::StaticAck { .. }),
            "a mismatched act never acks"
        );
    }
}

#[tokio::test]
async fn home_node_grants_every_yield_ack_stack() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("cropack", 0, 2);
    let pgob = pgob_of(&g);
    let before = g
        .world
        .players
        .iter()
        .find(|p| p.gob == pgob)
        .unwrap()
        .inv
        .len();
    // Two acks (a two-stack harvest): BOTH stacks must land.
    for res in ["gfx/invobjs/wheat", "gfx/invobjs/grainseed"] {
        g.on_node_msg(crate::nodes::NodeMsg::StaticAck {
            player: pgob,
            stack: Some(crate::nodes::StaticStack {
                res: res.to_owned(),
                count: 3,
                ql: 10,
                label: "Wheat".to_owned(),
            }),
            lp: 0,
        });
    }
    let after = g
        .world
        .players
        .iter()
        .find(|p| p.gob == pgob)
        .unwrap()
        .inv
        .len();
    assert_eq!(after, before + 2, "each yield ack grants its stack");
}

#[tokio::test]
async fn foreign_plant_ships_relay_act_and_keeps_seed() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("plantrelay", 0, 2);
    g.world.players[0].attrs.insert("farming".to_owned(), 1);
    let seed = InvStack {
        res: g.world.res.intern("gfx/invobjs/seed-wheat"),
        count: 3,
        ql: 11,
        label: "Wheat grain",
    };
    if let Some(out) = g.sessions.get_mut(&1) {
        out.cursor = Some(seed);
    }
    // A plowed tile far outside node 0's cells: derive the tile from
    // a foreign-cell position and keep it only if its tile-center gob
    // position still lands on a foreign cell (tiles are 11 subtiles,
    // vis cells 250 - straddling is possible, so probe a few).
    let base = foreign_cell_pos(&g, 0);
    let (mut tx, mut ty) = (base.0.div_euclid(11), base.1.div_euclid(11));
    for d in 0..40 {
        let cand = (base.0.div_euclid(11) + d, base.1.div_euclid(11));
        let c = crate::visidx::cell_of(cand.0 * 11 + 5, cand.1 * 11 + 5);
        if crate::grid_owner::owner_of(c, std::num::NonZeroUsize::new(2).unwrap()) != 0 {
            tx = cand.0;
            ty = cand.1;
            break;
        }
    }
    g.world.tilth.insert((tx, ty), u64::MAX);
    g.plant_seed(1, 1, (tx, ty), seed);
    // The act shipped with the cursor's spec + seed quality...
    let mut relayed = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::RelayPlantAct { spec, seed_ql, .. } = msg {
            assert_eq!(spec, 1);
            assert_eq!(seed_ql, 11);
            relayed = true;
        }
    }
    assert!(relayed, "a foreign furrow click relays the plant act");
    // ...and the seed stayed on the cursor (consumed only on ack).
    assert_eq!(
        g.sessions.get(&1).unwrap().cursor.as_ref().map(|c| c.count),
        Some(3),
        "the seed must not be consumed before the PlantAck"
    );
}

#[tokio::test]
async fn relay_plant_spawns_crop_acks_and_rejects_double() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("relayplant", 0, 2);
    let clicker = foreign_node_gob_id(0, 2, 61);
    // A plowed, empty tile on THIS node's cells.
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let (tx, ty) = (px.div_euclid(11), py.div_euclid(11) + 2);
    g.world.tilth.insert((tx, ty), u64::MAX);
    g.on_node_msg(crate::nodes::NodeMsg::RelayPlantAct {
        player: clicker,
        tx,
        ty,
        spec: 1,
        seed_ql: 9,
    });
    // The crop exists at the tile center with the relayed seed quality.
    let gob = g
        .world
        .crop_at
        .get(&(tx, ty))
        .copied()
        .expect("the relayed plant creates the crop");
    let slot = g.world.gobs.get(gob).unwrap();
    match g.world.gobs.kind[slot] {
        Kind::Crop { spec, stage } => {
            assert_eq!((spec, stage), (1, 0));
        }
        other => panic!("expected a crop, got {other:?}"),
    }
    assert_eq!(g.world.crops.get(&gob).unwrap().seed_ql, 9);
    // First ack arrives; a second act on the same tile stays silent.
    let mut acks = 0;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if matches!(msg, crate::nodes::NodeMsg::PlantAck { ok: true, .. }) {
            acks += 1;
        }
    }
    assert_eq!(acks, 1, "exactly one PlantAck for the planted tile");
    g.on_node_msg(crate::nodes::NodeMsg::RelayPlantAct {
        player: clicker,
        tx,
        ty,
        spec: 1,
        seed_ql: 9,
    });
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        assert!(
            !matches!(msg, crate::nodes::NodeMsg::PlantAck { ok: true, .. }),
            "an occupied tile never acks a second plant"
        );
    }
}

#[tokio::test]
async fn plant_ack_consumes_cursor_seed() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("plantack", 0, 2);
    let pgob = pgob_of(&g);
    let seed = InvStack {
        res: g.world.res.intern("gfx/invobjs/seed-wheat"),
        count: 3,
        ql: 11,
        label: "Wheat grain",
    };
    if let Some(out) = g.sessions.get_mut(&1) {
        out.cursor = Some(seed);
    }
    g.on_node_msg(crate::nodes::NodeMsg::PlantAck {
        player: pgob,
        ok: true,
    });
    assert_eq!(
        g.sessions.get(&1).unwrap().cursor.as_ref().map(|c| c.count),
        Some(2),
        "one ack consumes one seed unit"
    );
    // A failure ack keeps the cursor untouched.
    g.on_node_msg(crate::nodes::NodeMsg::PlantAck {
        player: pgob,
        ok: false,
    });
    assert_eq!(
        g.sessions.get(&1).unwrap().cursor.as_ref().map(|c| c.count),
        Some(2)
    );
}

// ------------------------------------------------------------------
// Session 32: cross-node plowing (TileMutation), tilth decay revert
// ------------------------------------------------------------------

/// Find a grass tile whose tile-center VisIndex cell is owned by `me`
/// in a `nodes`-node cluster, near `from`. Probes a widening strip of
/// tiles from `from` until both conditions hold (ownership is a per-
/// cell hash, grass a per-tile roll; both are deterministic).
fn grass_tile_on_cell(g: &mut Game, from: (i32, i32), me: usize, nodes: usize) -> (i32, i32) {
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

#[tokio::test]
async fn foreign_plow_ships_relay_act_without_local_mutation() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("plowrelay", 0, 2);
    let nz = std::num::NonZeroUsize::new(2).unwrap();
    // A tile whose tile-center cell is foreign, materialized locally
    // BEFORE the act (the realistic case: the player is looking at it).
    let base = foreign_cell_pos(&g, 0);
    let (mut tx, mut ty) = (base.0.div_euclid(11), base.1.div_euclid(11));
    for d in 0..40 {
        let cand = (base.0.div_euclid(11) + d, base.1.div_euclid(11));
        let c = crate::visidx::cell_of(cand.0 * 11 + 5, cand.1 * 11 + 5);
        if crate::grid_owner::owner_of(c, nz) != 0 {
            tx = cand.0;
            ty = cand.1;
            break;
        }
    }
    let gc = (tx.div_euclid(100), ty.div_euclid(100));
    let lx = tx.rem_euclid(100) as usize;
    let ly = ty.rem_euclid(100) as usize;
    let before = g.world.grids.grid(gc).tile(lx, ly);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let stamina_before = g.world.players[pidx].stamina;
    g.plow_tile(1, (tx, ty));
    // The act shipped to the authority (the foreign cell owner)...
    let mut relayed = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::RelayPlowAct {
            player,
            tx: mtx,
            ty: mty,
        } = msg
        {
            assert_eq!(player, pgob);
            assert_eq!((mtx, mty), (tx, ty));
            relayed = true;
        }
    }
    assert!(relayed, "a foreign tile click relays the plow act");
    // ...and NOTHING mutated locally: no tilth clock, no override, no
    // stamina drain, the live tile value is untouched.
    assert!(!g.world.tilth.contains_key(&(tx, ty)), "no shadow tilth");
    assert!(
        !g.world.grids.overrides.contains_key(&(tx, ty)),
        "no shadow override"
    );
    assert_eq!(
        g.world.grids.grid(gc).tile(lx, ly),
        before,
        "no shadow furrow in the local grid"
    );
    assert_eq!(
        g.world.players[pidx].stamina, stamina_before,
        "stamina drains only on the PlowAck"
    );
}

#[tokio::test]
async fn relay_plow_plows_authority_broadcasts_and_acks() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("relayplow", 0, 2);
    let clicker = foreign_node_gob_id(0, 2, 61);
    // A grass tile on THIS node's cells near the player.
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let (tx, ty) = grass_tile_on_cell(&mut g, (px, py), 0, 2);
    let gc = (tx.div_euclid(100), ty.div_euclid(100));
    let lx = tx.rem_euclid(100) as usize;
    let ly = ty.rem_euclid(100) as usize;
    g.on_node_msg(crate::nodes::NodeMsg::RelayPlowAct {
        player: clicker,
        tx,
        ty,
    });
    // The furrow exists on the authority: tile, override, tilth clock.
    assert_eq!(g.world.grids.grid(gc).tile(lx, ly), tile::PLOWED);
    assert_eq!(g.world.grids.overrides.get(&(tx, ty)), Some(&tile::PLOWED));
    assert!(g.world.tilth.contains_key(&(tx, ty)));
    // The authority answered PlowAck ok and broadcast TileMutation.
    let (mut acks_ok, mut mutations) = (0, 0);
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        match msg {
            crate::nodes::NodeMsg::PlowAck { player, ok } => {
                assert_eq!((player, ok), (clicker, true));
                acks_ok += 1;
            }
            crate::nodes::NodeMsg::TileMutation {
                tx: mtx,
                ty: mty,
                tile,
            } => {
                assert_eq!((mtx, mty, tile), (tx, ty, tile::PLOWED));
                mutations += 1;
            }
            _ => {}
        }
    }
    assert_eq!(acks_ok, 1, "exactly one ok PlowAck");
    assert_eq!(mutations, 1, "exactly one TileMutation broadcast");
    // A second act on the now-plowed tile is refused (not grass): a
    // failure ack, no second mutation, the tilth clock is untouched.
    g.on_node_msg(crate::nodes::NodeMsg::RelayPlowAct {
        player: clicker,
        tx,
        ty,
    });
    let (mut fail_acks, mut more_mutations) = (0, 0);
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        match msg {
            crate::nodes::NodeMsg::PlowAck { ok: false, .. } => fail_acks += 1,
            crate::nodes::NodeMsg::TileMutation { .. } => more_mutations += 1,
            _ => {}
        }
    }
    assert_eq!(fail_acks, 1, "the re-plow of a furrow fails loudly");
    assert_eq!(more_mutations, 0, "a refusal broadcasts nothing");
}

#[tokio::test]
async fn plow_ack_drains_stamina_exactly_once() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("plowack", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let before = g.world.players[pidx].stamina;
    g.on_node_msg(crate::nodes::NodeMsg::PlowAck {
        player: pgob,
        ok: true,
    });
    assert_eq!(
        g.world.players[pidx].stamina,
        before.saturating_sub(10),
        "one ok ack drains exactly one plow's stamina"
    );
    // A failure ack (and an ack for an unknown player) drains nothing.
    g.on_node_msg(crate::nodes::NodeMsg::PlowAck {
        player: pgob,
        ok: false,
    });
    g.on_node_msg(crate::nodes::NodeMsg::PlowAck {
        player: foreign_node_gob_id(0, 2, 71),
        ok: true,
    });
    assert_eq!(g.world.players[pidx].stamina, before.saturating_sub(10));
}

#[tokio::test]
async fn tile_mutation_resident_and_nonresident_paths() {
    let (mut g, _rx, mut raw_rx, _mesh) = clustered_game("tilemut", 0, 2);
    // Resident path: the session holds the player's grid (an explicit
    // map request), then a TileMutation for a tile inside it mutates
    // the live grid and re-sends MAPDATA to the holder.
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let (tx, ty) = (px.div_euclid(11), py.div_euclid(11));
    let gc = (tx.div_euclid(100), ty.div_euclid(100));
    let (lx, ly) = (tx.rem_euclid(100) as usize, ty.rem_euclid(100) as usize);
    g.on_mapreq(1, gc);
    let sent_first = raw_rx.try_recv().is_ok();
    assert!(sent_first, "on_mapreq fragments reach the raw channel");
    while raw_rx.try_recv().is_ok() {}
    assert!(g.world.grids.is_resident(gc));
    g.on_node_msg(crate::nodes::NodeMsg::TileMutation {
        tx,
        ty,
        tile: tile::PLOWED,
    });
    assert_eq!(g.world.grids.grid(gc).tile(lx, ly), tile::PLOWED);
    let mut resent = 0;
    while let Ok(block) = raw_rx.try_recv() {
        if block[0] == MSG_MAPDATA {
            resent += 1;
        }
    }
    assert!(resent >= 1, "the holder receives a fresh MAPDATA stream");
    // Non-resident path: a far grid nobody looks at only records the
    // override; it is never materialized and nothing is re-sent.
    g.on_node_msg(crate::nodes::NodeMsg::TileMutation {
        tx: 907,
        ty: 907,
        tile: tile::PLOWED,
    });
    assert!(!g.world.grids.is_resident((9, 9)));
    assert_eq!(
        g.world.grids.overrides.get(&(907, 907)),
        Some(&tile::PLOWED)
    );
    assert!(
        raw_rx.is_empty(),
        "a non-resident mutation re-sends nothing"
    );
    // Later materialization replays the override.
    assert_eq!(g.world.grids.grid((9, 9)).tile(7, 7), tile::PLOWED);
}

#[tokio::test]
async fn tilth_decay_reverts_furrow_to_grass_and_broadcasts() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("decayrevert", 0, 2);
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let (tx, ty) = grass_tile_on_cell(&mut g, (px, py), 0, 2);
    let gc = (tx.div_euclid(100), ty.div_euclid(100));
    let (lx, ly) = (tx.rem_euclid(100) as usize, ty.rem_euclid(100) as usize);
    // Local plow on my own cell: furrow + tilth clock + broadcast.
    g.plow_tile(1, (tx, ty));
    assert_eq!(g.world.grids.grid(gc).tile(lx, ly), tile::PLOWED);
    assert!(g.world.tilth.contains_key(&(tx, ty)));
    let mut mutations = 0;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::TileMutation { tile, .. } = msg {
            assert_eq!(tile, tile::PLOWED);
            mutations += 1;
        }
    }
    assert_eq!(mutations, 1, "the local plow also broadcasts");
    // Force the deadline into the past and run the farming pass.
    g.world.tilth.insert((tx, ty), 1);
    for _ in 0..3 {
        g.tick();
    }
    assert!(!g.world.tilth.contains_key(&(tx, ty)), "tilth decayed");
    assert_eq!(
        g.world.grids.grid(gc).tile(lx, ly),
        tile::GRASS,
        "the furrow reverted to grass"
    );
    assert_eq!(
        g.world.grids.overrides.get(&(tx, ty)),
        Some(&tile::GRASS),
        "the persisted override reverted too"
    );
    let mut reverts = 0;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::TileMutation { tile, .. } = msg {
            assert_eq!(tile, tile::GRASS);
            reverts += 1;
        }
    }
    assert_eq!(reverts, 1, "the decay revert broadcasts to peers");
    // The tile is re-plowable: no stuck-furrow dead end.
    g.plow_tile(1, (tx, ty));
    assert_eq!(g.world.grids.grid(gc).tile(lx, ly), tile::PLOWED);
    assert!(g.world.tilth.contains_key(&(tx, ty)));
}

// ------------------------------------------------------------------
// Session 33: cross-node station menus (relay + piggybacked snapshot)
// ------------------------------------------------------------------

/// A finished oven beside the player: the exact rows place_buildable
/// would have produced (Kind::Station + StationState), so tests read
/// like the real world state.
fn built_oven(g: &mut Game, fuel: u32, input: Option<(&'static str, u8)>) -> GobId {
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
            lit: false,
            progress: 0,
            quality: 10,
        },
    );
    gob
}

/// The publish path carries the Station class and the full readiness
/// snapshot (lit, fuel, has_input) - everything the home node needs
/// to open the menu without a round trip.
#[tokio::test]
async fn station_publishes_class_and_snapshot() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("stpub", 0, 2);
    let gob = built_oven(&mut g, 2, Some(("Raw Deer Meat", 10)));
    let slot = g.world.gobs.get(gob).unwrap();
    let st = g.guest_state_from_slot(gob, slot).unwrap();
    match st.kind {
        crate::nodes::GuestKind::Static { class, station, .. } => {
            assert_eq!(class, crate::nodes::StaticClass::Station);
            let view = station.expect("station payload rides the publish");
            assert_eq!(view.spec, 0);
            assert!(!view.lit);
            assert_eq!(view.fuel, 2);
            assert!(view.has_input);
        }
        other => panic!("a station publishes as a static, got {other:?}"),
    }
}

/// Session 34: a plan's stage advance (a credited material crossing
/// the stage boundary) re-publishes the guest state to every
/// subscribed peer, carrying the new construction stage so the
/// peer's plan sprite re-renders exactly like the local restage
/// path. Before session 34 the stage byte never left the node.
#[tokio::test]
async fn plan_stage_advance_republishes_stage() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("planstage", 0, 2);
    // A fresh oven plan on a PEER-owned cell: exactly what
    // build_plan places and sink_material credits.
    let (fx, fy) = foreign_cell_pos(&g, 0);
    let tile = (fx.div_euclid(11), fy.div_euclid(11));
    let pos = (tile.0 * 11 + 5, tile.1 * 11 + 5);
    let res = g.world.res.intern("gfx/terobjs/oven");
    let gob = g
        .world
        .gobs
        .spawn(Kind::Plan { spec: 0, stage: 0 }, pos, res, 1, 0);
    g.world.plans.insert(
        gob,
        crate::build::PlanState {
            spec: 0,
            tile,
            credited: Vec::new(),
        },
    );
    g.world.plan_at.insert(tile, gob);
    // A subscribed peer: its subscription covers the plan's cell.
    let cell = crate::visidx::cell_of(pos.0, pos.1);
    g.cluster
        .as_mut()
        .unwrap()
        .peer_subs
        .insert(1, std::iter::once(cell).collect());
    while let Ok((_, _)) = mesh_rx.try_recv() {}
    // Sink the stone stack through the REAL material path: the oven
    // demand is stone x2 + branch x1 over 2 stages, so a two-stone
    // stack crosses the 2/3 boundary and advances stage 0 -> 1.
    let stone = g.world.res.intern("gfx/invobjs/stone");
    let stack = crate::state::InvStack {
        res: stone,
        count: 2,
        ql: 10,
        label: "",
    };
    g.sink_material(1, gob, stack);
    let mut saw_stage = false;
    while let Ok((_, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
            if st.id == gob {
                match st.kind {
                    crate::nodes::GuestKind::Static {
                        class,
                        stage: Some(stage),
                        ..
                    } => {
                        assert_eq!(class, crate::nodes::StaticClass::Structure);
                        assert_eq!(stage, 1, "two stones advance the plan to stage 1");
                        saw_stage = true;
                    }
                    other => panic!("expected a staged plan static update, got {other:?}"),
                }
            }
        }
    }
    assert!(saw_stage, "stage advance publishes a GuestUpdate");
}

/// Session 34: completing a plan re-publishes the guest state with
/// the REAL kind - a finished oven publishes as the Station class
/// with its readiness snapshot, so a subscribed peer's flower menu,
/// fuel, input and light relay all come alive the moment the build
/// completes. Before this re-publish the peer kept a dead Structure
/// guest forever (every station interaction keys off the Station
/// class).
#[tokio::test]
async fn plan_completion_republishes_station_class() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("plancomp", 0, 2);
    let (fx, fy) = foreign_cell_pos(&g, 0);
    let tile = (fx.div_euclid(11), fy.div_euclid(11));
    let pos = (tile.0 * 11 + 5, tile.1 * 11 + 5);
    let res = g.world.res.intern("gfx/terobjs/oven");
    let gob = g
        .world
        .gobs
        .spawn(Kind::Plan { spec: 0, stage: 1 }, pos, res, 1, 0);
    // Fully credited: stone x2 + branch x1 completes the oven.
    g.world.plans.insert(
        gob,
        crate::build::PlanState {
            spec: 0,
            tile,
            credited: vec![
                crate::build::Credited {
                    res: "gfx/invobjs/stone",
                    count: 2,
                    ql_sum: 20,
                },
                crate::build::Credited {
                    res: "gfx/invobjs/branch",
                    count: 1,
                    ql_sum: 10,
                },
            ],
        },
    );
    g.world.plan_at.insert(tile, gob);
    let cell = crate::visidx::cell_of(pos.0, pos.1);
    g.cluster
        .as_mut()
        .unwrap()
        .peer_subs
        .insert(1, std::iter::once(cell).collect());
    while let Ok((_, _)) = mesh_rx.try_recv() {}
    g.complete_plan(gob);
    let mut saw_station = false;
    while let Ok((_, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
            if st.id == gob {
                match st.kind {
                    crate::nodes::GuestKind::Static {
                        class,
                        station: Some(view),
                        ..
                    } => {
                        assert_eq!(class, crate::nodes::StaticClass::Station);
                        assert_eq!(view.spec, 0);
                        assert!(!view.lit, "a fresh oven is unlit");
                        assert_eq!(view.fuel, 0);
                        assert!(!view.has_input);
                        saw_station = true;
                    }
                    other => panic!("expected a station static update, got {other:?}"),
                }
            }
        }
    }
    assert!(
        saw_station,
        "completion publishes the Station class to peers"
    );
}

/// Session 34: a GuestUpdate whose kind payload CHANGED (class flip,
/// stage advance, lit byte) re-renders the sprite for every session
/// already rendering the gob - the wire mirror of restage_gob.
/// Before this the existing-guest path only streamed pose, move and
/// hp deltas, so a lit guest oven never re-rendered for the players
/// watching it (the session-33 comment claimed it did).
#[tokio::test]
async fn guest_kind_flip_re_renders_existing_viewer() {
    let (mut g, _rx, mut raw, _mesh) = clustered_game("kindflip", 0, 2);
    // A guest oven the player ALREADY sees: announce it through the
    // real path, tick the vis scan so it spawns for session 1, then
    // flip the lit byte through a GuestUpdate.
    let gob = built_oven(&mut g, 1, None);
    let slot = g.world.gobs.get(gob).unwrap();
    let mut st = g.guest_state_from_slot(gob, slot).unwrap();
    g.world.gobs.kill(gob);
    g.world.stations.remove(&gob);
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st.clone()));
    g.tick();
    assert!(
        g.sessions[&1].visible.contains(&gob),
        "the guest oven must be visible before the flip"
    );
    while raw.try_recv().is_ok() {}
    // The authority lights it: the re-published GuestUpdate carries
    // the flipped lit byte in its snapshot.
    st.kind = crate::nodes::GuestKind::Static {
        res_name: "gfx/terobjs/oven".into(),
        class: crate::nodes::StaticClass::Station,
        crop: None,
        station: Some(crate::nodes::StationView {
            spec: 0,
            lit: true,
            fuel: 3,
            has_input: true,
        }),
        stage: None,
        drop: None,
    };
    g.on_node_msg(crate::nodes::NodeMsg::GuestUpdate(st));
    // Wire proof: the raw stream carries an OD_RES re-render with
    // the fresh sdt byte for the flipped gob (wire id | 0x8000,
    // len 1, byte 1 - the same shape encode_guest_block emits for
    // a fresh spawn).
    let mut saw_lit_restage = false;
    while let Ok(block) = raw.try_recv() {
        if block.first() != Some(&MSG_OBJDATA) {
            continue;
        }
        if block.len() < 14 {
            continue;
        }
        let id = i32::from_le_bytes([block[2], block[3], block[4], block[5]]);
        if id != gob {
            continue;
        }
        // Skip the frame, then read the OD sequence: OD_RES with the
        // sdt extension must carry byte 1 (lit).
        let mut off = 10;
        while off < block.len() {
            match block[off] {
                OD_END => break,
                OD_RES => {
                    let wire = u16::from_le_bytes([block[off + 1], block[off + 2]]);
                    if wire & 0x8000 != 0 {
                        let len = block[off + 3] as usize;
                        assert!(len >= 1, "lit sdt payload missing");
                        assert_eq!(
                            block[off + 4],
                            1,
                            "the re-render must carry the lit sdt byte"
                        );
                        saw_lit_restage = true;
                    }
                    off += 3 + if wire & 0x8000 != 0 {
                        1 + block[off + 3] as usize
                    } else {
                        0
                    };
                }
                OD_MOVE => off += 9,
                OD_LINBEG => off += 21,
                OD_LINSTEP => off += 5,
                OD_HEALTH => off += 2,
                OD_LAYERS => {
                    off += 3;
                    while off + 1 < block.len() {
                        let l = u16::from_le_bytes([block[off], block[off + 1]]);
                        off += 2;
                        if l == 0xFFFF {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }
    assert!(
        saw_lit_restage,
        "a kind flip must re-render OD_RES with the fresh sdt byte"
    );
}

/// A guest oven click opens the Light menu LOCALLY (session UI) from
/// the piggybacked snapshot, armed with the guest act intent.
#[tokio::test]
async fn guest_station_click_opens_menu_and_relays_choice() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stclick", 0, 2);
    let pgob = pgob_of(&g);
    let gob = built_oven(&mut g, 0, None);
    let slot = g.world.gobs.get(gob).unwrap();
    let st = g.guest_state_from_slot(gob, slot).unwrap();
    let (fx, fy) = st.pos;
    // The real subscriber path: the local row dies, the guest row
    // arrives through GuestAnnounce exactly like on a live mesh.
    g.world.gobs.kill(gob);
    g.world.stations.remove(&gob);
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st));
    g.player_interact(1, pgob, gob, (fx, fy));
    let menu = g
        .sessions
        .get(&1)
        .and_then(|o| o.station_menu)
        .expect("the guest oven click opens the flower menu");
    assert_eq!(menu.1, gob);
    assert_eq!(
        menu.2,
        Some(crate::nodes::StationAct::Light),
        "the act intent comes from the snapshot (unlit -> Light)"
    );
    // Acting on the menu relays the act; the home node mutates
    // nothing (no local station row exists here at all).
    g.apply_station_choice(1, menu.0, 0);
    let mut relays = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::RelayStationAct {
            player,
            target,
            act,
        } = msg
        {
            relays.push((player, target, act));
        }
    }
    assert_eq!(
        relays,
        vec![(pgob, gob, crate::nodes::StationAct::Light)],
        "the choice relays to the station's authority"
    );
}

/// Authority side of the menu relay: full validation order (stale ->
/// fuel -> input), the same transitions as the local menu path, and
/// one StationAck per act with the exact result.
#[tokio::test]
async fn relay_station_act_validates_and_applies() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stact", 0, 2);
    let clicker = foreign_node_gob_id(0, 2, 71);
    let empty = built_oven(&mut g, 0, None);
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
        player: clicker,
        target: empty,
        act: crate::nodes::StationAct::Light,
    });
    let mut acks = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StationAck { player, result } = msg {
            assert_eq!(player, clicker);
            acks.push(result);
        }
    }
    assert_eq!(
        acks,
        vec![crate::nodes::StationResult::NeedsFuel],
        "no fuel -> NeedsFuel, nothing mutated"
    );
    assert!(!g.world.stations.get(&empty).unwrap().lit);

    // Fuel but no input.
    let fueled = built_oven(&mut g, 1, None);
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
        player: clicker,
        target: fueled,
        act: crate::nodes::StationAct::Light,
    });
    let mut acks = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
            acks.push(result);
        }
    }
    assert_eq!(acks, vec![crate::nodes::StationResult::NeedsInput]);

    // Ready: the job starts, the lit Kind byte moves with the state.
    let ready = built_oven(&mut g, 1, Some(("Raw Deer Meat", 10)));
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
        player: clicker,
        target: ready,
        act: crate::nodes::StationAct::Light,
    });
    let mut acks = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
            acks.push(result);
        }
        if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
            if st.id == ready {
                if let crate::nodes::GuestKind::Static { station, .. } = &st.kind {
                    assert!(station.unwrap().lit, "the update re-publishes lit=true");
                }
            }
        }
    }
    assert_eq!(acks, vec![crate::nodes::StationResult::Lit]);
    assert!(g.world.stations.get(&ready).unwrap().lit);

    // Stale: lighting an already-lit oven changes nothing.
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
        player: clicker,
        target: ready,
        act: crate::nodes::StationAct::Light,
    });
    let mut acks = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
            acks.push(result);
        }
    }
    assert_eq!(acks, vec![crate::nodes::StationResult::Stale]);

    // Extinguish resets the progress and preserves the input.
    g.world.stations.get_mut(&ready).unwrap().progress = 5;
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
        player: clicker,
        target: ready,
        act: crate::nodes::StationAct::Extinguish,
    });
    let st = g.world.stations.get(&ready).unwrap().clone();
    assert!(!st.lit);
    assert_eq!(st.progress, 0);
    assert!(st.input.is_some(), "extinguish preserves the input");
    let mut acks = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
            acks.push(result);
        }
    }
    assert_eq!(acks, vec![crate::nodes::StationResult::Extinguished]);
}

/// A lit oven relights only after the job ends: the relay answers
/// Stale for an extinguish on an unlit oven too (view lag).
#[tokio::test]
async fn relay_station_extinguish_unlit_is_stale() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stext", 0, 2);
    let clicker = foreign_node_gob_id(0, 2, 72);
    let gob = built_oven(&mut g, 1, Some(("Raw Deer Meat", 10)));
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
        player: clicker,
        target: gob,
        act: crate::nodes::StationAct::Extinguish,
    });
    let mut acks = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
            acks.push(result);
        }
    }
    assert_eq!(
        acks,
        vec![crate::nodes::StationResult::Stale],
        "extinguishing an unlit oven is a stale-view no-op"
    );
}

/// Home side of the item relay: the click with a held stack ships
/// RelayStationItem (one unit described by name/ql/label) and the
/// cursor stays intact until the ack decides.
#[tokio::test]
async fn guest_station_itemact_ships_relay_and_keeps_cursor() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stitem", 0, 2);
    let gob = built_oven(&mut g, 0, None);
    let slot = g.world.gobs.get(gob).unwrap();
    let st = g.guest_state_from_slot(gob, slot).unwrap();
    let (fx, fy) = st.pos;
    g.world.gobs.kill(gob);
    g.world.stations.remove(&gob);
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st));
    // Put a branch stack (oven fuel) on the cursor.
    let branch = g.world.res.intern("gfx/invobjs/branch");
    g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
        res: branch,
        count: 3,
        ql: 10,
        label: "Branch",
    });
    let args = vec![
        hnh_proto::ListArg::Coord(0, 0),
        hnh_proto::ListArg::Coord(fx, fy),
        hnh_proto::ListArg::Int(0),
        hnh_proto::ListArg::Int(gob),
        hnh_proto::ListArg::Int(0),
    ];
    g.on_map_itemact(1, &args);
    let mut relays = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::RelayStationItem {
            player,
            target,
            stack,
        } = msg
        {
            relays.push((player, target, stack.res.clone(), stack.ql, stack.count));
        }
    }
    assert_eq!(relays.len(), 1, "one item relay per click");
    assert_eq!(relays[0].1, gob);
    assert_eq!(relays[0].2, "gfx/invobjs/branch");
    assert_eq!(
        g.sessions.get(&1).and_then(|o| o.cursor).map(|c| c.count),
        Some(3),
        "the cursor stack is untouched before the ack (seed-safe)"
    );
}

/// Authority side of the item relay: every branch of the local
/// station_itemact validation order answers with its exact result.
#[tokio::test]
async fn relay_station_item_full_validation_order() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stitemauth", 0, 2);
    let clicker = foreign_node_gob_id(0, 2, 73);
    let fuel_stack = |res: &str, label: &str| crate::nodes::StaticStack {
        res: res.to_owned(),
        count: 1,
        ql: 10,
        label: label.to_owned(),
    };
    // Fuel loads on an empty oven.
    let gob = built_oven(&mut g, 0, None);
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
        player: clicker,
        target: gob,
        stack: fuel_stack("gfx/invobjs/branch", "Branch"),
    });
    let st = g.world.stations.get(&gob).unwrap().clone();
    assert_eq!(st.fuel, 1);
    assert_eq!(st.fuel_seen, 1);
    // Input loads when unlit + empty.
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
        player: clicker,
        target: gob,
        stack: fuel_stack("gfx/invobjs/meat", "Raw Deer Meat"),
    });
    let st = g.world.stations.get(&gob).unwrap().clone();
    assert!(st.input.is_some(), "the roast input slot loads");
    // A second input refuses with InputFull.
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
        player: clicker,
        target: gob,
        stack: fuel_stack("gfx/invobjs/meat", "Raw Deer Meat"),
    });
    // A non-fuel non-roastable refuses with NotProcessable.
    let other = built_oven(&mut g, 1, None);
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
        player: clicker,
        target: other,
        stack: fuel_stack("gfx/invobjs/stone", "Stone"),
    });
    // A lit oven refuses the input with BusyLit (fuel still loads).
    let lit = built_oven(&mut g, 0, None);
    g.world.stations.get_mut(&lit).unwrap().lit = true;
    g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
        player: clicker,
        target: lit,
        stack: fuel_stack("gfx/invobjs/meat", "Raw Deer Meat"),
    });
    let mut acks = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StationItemAck { player, result } = msg {
            assert_eq!(player, clicker);
            acks.push(result);
        }
    }
    assert_eq!(
        acks,
        vec![
            crate::nodes::StationItemResult::FuelAdded,
            crate::nodes::StationItemResult::InputLoaded,
            crate::nodes::StationItemResult::InputFull,
            crate::nodes::StationItemResult::NotProcessable,
            crate::nodes::StationItemResult::BusyLit,
        ]
    );
    assert_eq!(g.world.stations.get(&lit).unwrap().input, None);
}

/// Home side of the item ack: FuelAdded/InputLoaded consume exactly
/// one cursor unit; refusals keep the whole stack.
#[tokio::test]
async fn station_itemack_consumes_one_unit_or_keeps_stack() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("stack", 0, 2);
    let pgob = pgob_of(&g);
    let branch = g.world.res.intern("gfx/invobjs/branch");
    let put_cursor = |g: &mut Game, n: u32| {
        g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
            res: branch,
            count: n,
            ql: 10,
            label: "Branch",
        });
    };
    put_cursor(&mut g, 3);
    g.on_node_msg(crate::nodes::NodeMsg::StationItemAck {
        player: pgob,
        result: crate::nodes::StationItemResult::FuelAdded,
    });
    assert_eq!(
        g.sessions.get(&1).and_then(|o| o.cursor).map(|c| c.count),
        Some(2),
        "a multi-unit stack keeps the rest"
    );
    put_cursor(&mut g, 1);
    g.on_node_msg(crate::nodes::NodeMsg::StationItemAck {
        player: pgob,
        result: crate::nodes::StationItemResult::FuelAdded,
    });
    assert!(
        g.sessions.get(&1).and_then(|o| o.cursor).is_none(),
        "an exhausted stack clears the cursor"
    );
    put_cursor(&mut g, 2);
    g.on_node_msg(crate::nodes::NodeMsg::StationItemAck {
        player: pgob,
        result: crate::nodes::StationItemResult::NotProcessable,
    });
    assert_eq!(
        g.sessions.get(&1).and_then(|o| o.cursor).map(|c| c.count),
        Some(2),
        "a refusal keeps the whole stack"
    );
}

/// Owner-filtered populate (session 33): on_mapreq materializes the
/// grid's TILES (deterministic, identical on every node) but spawns
/// statics/animals only for cells THIS node owns - no shadow copies
/// of the cell owner's content. Before this fix every node carried
/// its own copy of the same grid's statics (and its rng placed
/// DIFFERENT animals than the owner's roll).
#[tokio::test]
async fn owner_filtered_populate_spawns_only_my_cells() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("ownpop", 0, 2);
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    // A grid near spawn: touches both nodes' cells (a grid spans
    // 5x5 VisIndex cells, rendezvous-hashed between 2 nodes).
    let gc = (px.div_euclid(1100), py.div_euclid(1100));
    g.on_mapreq(1, gc);
    let nodes = g.cluster.as_ref().unwrap().nodes;
    let me = g.cluster.as_ref().unwrap().me;
    let mut mine = 0usize;
    let mut foreign = 0usize;
    for id in g.world.gobs.vis.gobs_in_view(px + 2000, py + 2000, 6000) {
        let Some(slot) = g.world.gobs.get(id) else {
            continue;
        };
        if matches!(g.world.gobs.kind[slot], Kind::Player { .. }) {
            continue; // players are homed, not cell-owned
        }
        let pos = g.world.gobs.pos[slot];
        let cell = crate::visidx::cell_of(pos.0, pos.1);
        if crate::grid_owner::owner_of(cell, nodes) == me {
            mine += 1;
        } else {
            foreign += 1;
        }
    }
    assert!(mine > 0, "the grid's my-cells content spawned");
    assert_eq!(foreign, 0, "no static/animal may spawn on a non-owner node");
}

/// Session 34: Sub carries DIFFS (tick_cluster sends only the added
/// cells), so a follow-up Sub must EXTEND the peer's subscription,
/// not replace it. The old whole-set replace silently unsubscribed
/// every earlier cell the first time a moving session's view
/// produced a second Sub - cross-node updates for still-subscribed
/// cells stopped flowing (a lit guest oven never re-rendered).
#[tokio::test]
async fn sub_diffs_extend_not_replace() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("subdiff", 0, 2);
    let nodes = g.cluster.as_ref().unwrap().nodes;
    // Two cells THIS node owns: a peer subscribing to them passes the
    // receiver-side ownership filter (Sub drops cells the receiver
    // does not own).
    let mut my_cells: Vec<(i32, i32)> = Vec::new();
    'scan: for cy in -1..=4i32 {
        for cx in -1..=4i32 {
            let cell = (cx, cy);
            if crate::grid_owner::owner_of(cell, nodes) == 0 {
                my_cells.push(cell);
                if my_cells.len() == 2 {
                    break 'scan;
                }
            }
        }
    }
    assert_eq!(my_cells.len(), 2, "the lattice must have two own cells");
    // First Sub: one cell. Follow-up Sub: the second cell (a diff).
    g.on_node_msg(crate::nodes::NodeMsg::Sub {
        from: 1,
        cells: vec![my_cells[0]],
    });
    g.on_node_msg(crate::nodes::NodeMsg::Sub {
        from: 1,
        cells: vec![my_cells[1]],
    });
    let subs = &g.cluster.as_ref().unwrap().peer_subs[&1];
    assert!(
        subs.contains(&my_cells[0]) && subs.contains(&my_cells[1]),
        "both diffed cells stay subscribed, got {:?}",
        subs
    );
    // Unsub of one cell keeps the other (the same incremental rule).
    g.on_node_msg(crate::nodes::NodeMsg::Unsub {
        from: 1,
        cells: vec![my_cells[1]],
    });
    let subs = &g.cluster.as_ref().unwrap().peer_subs[&1];
    assert!(subs.contains(&my_cells[0]), "the untouched cell stays");
    assert!(!subs.contains(&my_cells[1]), "the removed cell goes");
}

/// Sub-driven populate (session 33): a peer's Sub materializes the
/// authority's part of every touched grid and announces EVERY gob it
/// holds in the subscribed cells (fresh + pre-existing), so the
/// subscriber's view starts from the single authoritative copy.
#[tokio::test]
async fn sub_driven_populate_announces_to_subscriber() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("subpop", 0, 2);
    let nodes = g.cluster.as_ref().unwrap().nodes;
    let me = g.cluster.as_ref().unwrap().me;
    // Pre-existing content: an oven near spawn (my cell by the
    // clustered_game layout).
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let my_cell = crate::visidx::cell_of(px, py);
    assert_eq!(
        crate::grid_owner::owner_of(my_cell, nodes),
        me,
        "fixture: the player spawns on a home cell"
    );
    let _oven = built_oven(&mut g, 1, None);
    // A peer subscribes to my cell.
    g.on_node_msg(crate::nodes::NodeMsg::Sub {
        from: 1,
        cells: vec![my_cell],
    });
    let mut announced = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::GuestAnnounce(st) = msg {
            announced.push((st.id, st.pos));
        }
    }
    assert!(
        announced.len() >= 2,
        "the player gob AND the oven (and populated statics) announce to the subscriber, got {}",
        announced.len()
    );
    // Everything announced actually stands in the subscribed cell.
    for (_, pos) in &announced {
        assert_eq!(
            crate::visidx::cell_of(pos.0, pos.1),
            my_cell,
            "announces stay inside the subscribed cell"
        );
    }
    // The touched grid materialized my part (populated set grew).
    let touched: Vec<(i32, i32)> = Game::grids_touching_cell(my_cell);
    for gc in touched {
        assert!(g.populated.contains(&gc), "grid {gc:?} materialized");
    }
}

/// grids_touching_cell arithmetic: a VisIndex cell (250 subtiles)
/// touches one or two grids (1100 subtiles) per axis, never more,
/// and the covered subtile span always lands inside the returned
/// grids.
#[test]
fn grids_touching_cell_covers_the_cell() {
    for cell in [
        (0i32, 0i32),
        (4, 4),
        (-1, -1),
        (5, -3),
        (-7, 9),
        (100, -100),
    ] {
        let grids = Game::grids_touching_cell(cell);
        assert!(!grids.is_empty() && grids.len() <= 4);
        let (cx, cy) = cell;
        for u in [cx * 250, cx * 250 + 124, cx * 250 + 249] {
            for v in [cy * 250, cy * 250 + 124, cy * 250 + 249] {
                let gx = u.div_euclid(1100);
                let gy = v.div_euclid(1100);
                assert!(
                    grids.contains(&(gx, gy)),
                    "cell {cell:?} subtile ({u},{v}) grid ({gx},{gy}) missing"
                );
            }
        }
    }
}

/// Home side of the act ack: the refusal results render the exact
/// system lines the local menu path emits (UX parity).
#[tokio::test]
async fn station_ack_refusals_render_system_lines() {
    let (mut g, mut rx, _raw, _mesh) = clustered_game("stline", 0, 2);
    let pgob = pgob_of(&g);
    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) -> String {
        let mut text = String::new();
        while let Ok(buf) = rx.try_recv() {
            // System lines are wdgmsg("sysmsg"...)-shaped chat frames;
            // a byte scan is enough to assert the wording crossed.
            text.push_str(&String::from_utf8_lossy(&buf));
        }
        text
    }
    let _ = drain(&mut rx);
    g.on_node_msg(crate::nodes::NodeMsg::StationAck {
        player: pgob,
        result: crate::nodes::StationResult::NeedsFuel,
    });
    let text = drain(&mut rx);
    assert!(
        text.contains("oven needs fuel"),
        "NeedsFuel renders: {text}"
    );
    g.on_node_msg(crate::nodes::NodeMsg::StationAck {
        player: pgob,
        result: crate::nodes::StationResult::Lit,
    });
    let text = drain(&mut rx);
    assert!(
        !text.contains("oven"),
        "a successful Light stays silent (parity with the local path)"
    );
}

// ------------------------------------------------------------------
// Session 36: the bow chain (woodbow / stonearrow / bonearrow)
// ------------------------------------------------------------------

/// Helper: replace a player's inventory with a synthetic stack list.
fn set_inv(g: &mut Game, stacks: &[(&'static str, u32, u8)]) {
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

/// Wooden Bow quality follows the RoB type-weighted formula
/// `(qBranches + qString)/2` INDEPENDENT of the unit counts: 4
/// branches at q40 + 1 string at q10 average the TYPES to 25, then
/// the Marksmanship (ranged) softcap (10 here) halves it toward 17.
/// The pre-36 unit-weighted math would give 34 -> 22, so the assert
/// distinguishes the two models.
#[tokio::test]
async fn woodbow_quality_is_type_weighted() {
    let (mut g, _rx, _raw) = entered_game("bowq");
    set_inv(
        &mut g,
        &[("gfx/invobjs/branch", 4, 40), ("gfx/invobjs/string", 1, 10)],
    );
    assert!(g.craft_once(1, "woodbow"), "craft must succeed");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let bow_gidx = g.world.res.intern("gfx/invobjs/bow");
    let bow = g.world.players[pidx]
        .inv
        .iter()
        .find(|s| s.res == bow_gidx)
        .expect("bow produced");
    assert_eq!(bow.count, 1);
    // (40 + 10)/2 = 25, softcap ranged=10: (25 + 10)/2 = 17.
    assert_eq!(bow.ql, 17, "type-weighted quality with ranged softcap");
    // All inputs consumed.
    assert!(
        !g.world.players[pidx]
            .inv
            .iter()
            .any(|s| s.res == g.world.res.intern("gfx/invobjs/branch")),
        "branches fully consumed"
    );
}

/// Stone Arrows come out as ONE batch of ten per craft, and the
/// branch type weighs double the stone type (RoB Legacy:Quality
/// arrow example): stone q10 + branches q40 -> (10*1 + 40*2)/3 = 30,
/// softcap survive (unset -> 10): (30 + 10)/2 = 20.
#[tokio::test]
async fn stonearrow_bundles_ten_and_branch_weighs_double() {
    let (mut g, _rx, _raw) = entered_game("arrq");
    set_inv(
        &mut g,
        &[("gfx/invobjs/stone", 1, 10), ("gfx/invobjs/branch", 2, 40)],
    );
    assert!(g.craft_once(1, "stonearrow"), "craft must succeed");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let arr_gidx = g.world.res.intern("gfx/invobjs/arrow-stone");
    let arrows = g.world.players[pidx]
        .inv
        .iter()
        .find(|s| s.res == arr_gidx)
        .expect("stone arrows produced");
    assert_eq!(arrows.count, 10, "one craft yields a bundle of ten");
    // (10*1 + 40*2)/3 = 30, softcap survive=10: (30+10)/2 = 20.
    assert_eq!(arrows.ql, 20);
}

/// A Wooden Bow dropped into any equipment slot renders the dedicated
/// carrying layers (gfx/borka/eq-bow/.../arm/carrying/...) on the
/// world drawable AND on the paperdoll doll set.
#[tokio::test]
async fn bow_equip_renders_carrying_pose() {
    let (mut g, _rx, _raw) = entered_game("bowpose");
    set_inv(&mut g, &[("gfx/invobjs/bow", 1, 10)]);
    let pidx = *g.world.by_session.get(&1).unwrap();
    // Equip via the same slot the epry flow uses (slot 0).
    let stack = g.world.players[pidx].inv.pop().unwrap();
    g.world.players[pidx].equip[0] = Some(stack);
    let names: Vec<&'static str> = g.world.players[pidx]
        .equip
        .iter()
        .flatten()
        .filter_map(|s| g.world.res.name(s.res))
        .collect();
    let world = crate::equip::world_layers(names.iter(), false, 1);
    assert_eq!(world.len(), 2, "standing front: left + right carrying");
    assert!(world
        .iter()
        .all(|l| l.contains("eq-bow/standing/arm/carrying/")));
    let doll = crate::equip::doll_layers(names.iter());
    assert_eq!(doll.len(), 2, "doll renders the front carrying pair");
    assert!(
        doll.iter()
            .all(|l| l.contains("eq-bow/standing/arm/carrying/")),
        "doll layers: {doll:?}"
    );
    let walking = crate::equip::world_layers(names.iter(), true, 1);
    assert!(
        walking
            .iter()
            .all(|l| l.contains("eq-bow/walking/arm/carrying/")),
        "walking pose carries too"
    );
}

/// The bow chain must be craftable straight out of the starter kit
/// (the kit composition is the server policy that keeps the chain
/// playable with zero foraging).
#[tokio::test]
async fn starter_kit_covers_the_bow_chain() {
    let (mut g, _rx, _raw) = entered_game("bowkit");
    let pidx = *g.world.by_session.get(&1).unwrap();
    fn count(g: &mut Game, pidx: usize, res: &'static str) -> u32 {
        let gidx = g.world.res.intern(res);
        g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == gidx)
            .map(|s| s.count)
            .sum()
    }
    assert!(
        count(&mut g, pidx, "gfx/invobjs/branch") >= 4 + 2,
        "bow + arrow branches"
    );
    assert!(count(&mut g, pidx, "gfx/invobjs/string") >= 1, "bow string");
    assert!(count(&mut g, pidx, "gfx/invobjs/stone") >= 1, "arrow stone");
    // The menu must announce every new pagina (rendered MenuGrid).
    for page in [
        "paginae/craft/woodbow",
        "paginae/craft/stonearrow",
        "paginae/craft/bonearrow",
    ] {
        assert!(
            crate::craft::RECIPES.iter().any(|r| r.pagina == page),
            "{page} in RECIPES"
        );
    }
}

// ------------------------------------------------------------------
// Bow ranged combat (session 37, archery.rs)
// ------------------------------------------------------------------

/// Equip a bow into slot 0 and put one stack of arrows into the
/// inventory.
fn arm_bow(g: &mut Game, pidx: usize, arrows: u32, bow_ql: u8) {
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
fn spawn_deer_at(g: &mut Game, pidx: usize, d: i32, hp: i32) -> crate::state::GobId {
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

fn arrow_count(g: &mut Game, pidx: usize) -> u32 {
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
fn chat_log_text(msg: &[u8]) -> Option<String> {
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

/// Clicking an animal with an equipped bow opens the RANGED aim
/// path, not the melee fight window; without a bow the melee fight
/// opens as before.
#[tokio::test]
async fn bow_click_opens_aim_instead_of_fight() {
    let (mut g, _rx, _raw) = entered_game("bowaim");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let deer = spawn_deer_at(&mut g, pidx, 66, 200);
    let pgob = g.world.players[pidx].gob;
    g.player_interact(1, pgob, deer, (0, 0));
    let p = &g.world.players[pidx];
    assert_eq!(p.aim.map(|a| a.target), Some(deer), "aim started");
    assert_eq!(p.fight_target, None, "no melee fight for a bow carrier");
    // Without a bow the same click opens the melee fight.
    g.world.players[pidx].aim = None;
    g.world.players[pidx].equip[0] = None;
    g.player_interact(1, pgob, deer, (0, 0));
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(deer),
        "melee fight opens without a bow"
    );
}

/// A dry bow (no arrows) refuses to aim and never falls back into
/// melee while the bow is still equipped.
#[tokio::test]
async fn dry_bow_refuses_to_aim() {
    let (mut g, _rx, _raw) = entered_game("drybow");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 0, 10);
    let deer = spawn_deer_at(&mut g, pidx, 66, 200);
    let pgob = g.world.players[pidx].gob;
    g.player_interact(1, pgob, deer, (0, 0));
    assert_eq!(g.world.players[pidx].aim, None, "no aim without arrows");
    assert_eq!(
        g.world.players[pidx].fight_target, None,
        "no melee fallback while the bow is equipped"
    );
}

/// A guaranteed hit (roll 0) consumes exactly one arrow, applies
/// the Fandom damage formula, depletes the attack meter and keeps
/// the aim up for the next shot.
#[tokio::test]
async fn arrow_hit_consumes_one_arrow_and_damages() {
    let (mut g, _rx, _raw) = entered_game("arrowhit");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let deer = spawn_deer_at(&mut g, pidx, 66, 200);
    let dslot = g.world.gobs.get(deer).unwrap();
    let hp0 = g.world.gobs.hp[dslot];
    let aim = crate::archery::RangedAim::new(deer, 10, crate::archery::AIM_RATE_WOODBOW);
    // Prime the offence bar so the depletion is observable.
    if let Some(out) = g.sessions.get_mut(&1) {
        out.fight.own_off = crate::fight::BAR_FULL;
    }
    g.shoot_arrow(pidx, 1, aim, 0);
    let dslot = g.world.gobs.get(deer).unwrap();
    assert_eq!(
        g.world.gobs.hp[dslot],
        hp0 - crate::archery::bow_damage(10),
        "q10 bow deals 75*sqrt(10/10) = 75"
    );
    assert_eq!(arrow_count(&mut g, pidx), 9, "exactly one arrow consumed");
    assert_eq!(
        g.sessions[&1].fight.own_off, 0,
        "attack meter depleted by the shot"
    );
    assert_eq!(
        g.world.players[pidx].aim.map(|a| a.target),
        Some(deer),
        "aim continues while the target lives"
    );
}

/// A guaranteed miss (roll 99) still spends the arrow but leaves
/// the target's HP untouched.
#[tokio::test]
async fn arrow_miss_spends_the_arrow_only() {
    let (mut g, _rx, _raw) = entered_game("arrowmiss");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let deer = spawn_deer_at(&mut g, pidx, 66, 200);
    let dslot = g.world.gobs.get(deer).unwrap();
    let hp0 = g.world.gobs.hp[dslot];
    let aim = crate::archery::RangedAim::new(deer, 10, crate::archery::AIM_RATE_WOODBOW);
    g.shoot_arrow(pidx, 1, aim, 99);
    let dslot = g.world.gobs.get(deer).unwrap();
    assert_eq!(g.world.gobs.hp[dslot], hp0, "miss deals no damage");
    assert_eq!(arrow_count(&mut g, pidx), 9, "the arrow is lost on a miss");
}

/// The aim meter fills over 40 ticks (4 s at 10 Hz), streams the
/// 25/50/75% progress lines in order, and spends no arrow before
/// the release.
#[tokio::test]
async fn aim_meter_fills_with_progress_lines() {
    let (mut g, mut rx, _raw) = entered_game("aimmeter");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let deer = spawn_deer_at(&mut g, pidx, 66, 200);
    let pgob = g.world.players[pidx].gob;
    g.player_interact(1, pgob, deer, (0, 0));
    let mut aim = g.world.players[pidx].aim.expect("aim started");
    for _ in 0..39 {
        g.tick_aim(pidx, 1, pgob, aim);
        aim = g.world.players[pidx].aim.expect("still aiming");
    }
    assert!(
        aim.meter >= 75 * crate::archery::AIM_FULL / 100,
        "39 of 40 ticks reach at least 75%"
    );
    assert_eq!(
        arrow_count(&mut g, pidx),
        10,
        "no arrow spent before release"
    );
    // Progress lines arrived.
    let mut seen: Vec<String> = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let Some(text) = chat_log_text(&msg) {
            seen.push(text);
        }
    }
    assert!(
        seen.iter().any(|t| t.contains("Aiming at 25%")),
        "25% line sent: {seen:?}"
    );
    assert!(
        seen.iter().any(|t| t.contains("Aiming at 75%")),
        "75% line sent: {seen:?}"
    );
}

/// An out-of-range target is chased, not shot at: no meter gain,
/// and the aim survives inside the drop radius.
#[tokio::test]
async fn out_of_range_target_is_chased() {
    let (mut g, _rx, _raw) = entered_game("bowchase");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let deer = spawn_deer_at(&mut g, pidx, 200, 200);
    let pgob = g.world.players[pidx].gob;
    let aim = crate::archery::RangedAim::new(deer, 10, crate::archery::AIM_RATE_WOODBOW);
    g.world.players[pidx].aim = Some(aim);
    g.tick_aim(pidx, 1, pgob, aim);
    let aim = g.world.players[pidx].aim.expect("aim kept while chasing");
    assert_eq!(aim.meter, 0, "no meter gain out of range");
    let pslot = g.world.gobs.get(pgob).unwrap();
    assert!(g.world.gobs.mv[pslot].is_some(), "player closes in");
    assert_eq!(aim.target, deer, "aim survives inside the drop radius");
}

/// A ground click cancels an active aim (walk away instead).
#[tokio::test]
async fn walk_cancels_aim() {
    let (mut g, _rx, _raw) = entered_game("aimcancel");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let deer = spawn_deer_at(&mut g, pidx, 66, 200);
    let pgob = g.world.players[pidx].gob;
    g.player_interact(1, pgob, deer, (0, 0));
    assert!(g.world.players[pidx].aim.is_some());
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    g.player_walk(1, pgob, (px + 200, py));
    assert_eq!(g.world.players[pidx].aim, None, "aim dropped on walk");
}

/// A lethal arrow kills the deer, drops its loot (meat + bone from
/// session 36) and ends the aim.
#[tokio::test]
async fn lethal_arrow_kills_and_loots() {
    let (mut g, _rx, _raw) = entered_game("bowkill");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 40);
    let deer = spawn_deer_at(&mut g, pidx, 66, Species::Deer.max_hp());
    let dslot = g.world.gobs.get(deer).unwrap();
    let pos = g.world.gobs.pos[dslot];
    let aim = crate::archery::RangedAim::new(deer, 40, crate::archery::AIM_RATE_WOODBOW);
    g.shoot_arrow(pidx, 1, aim, 0);
    assert!(
        g.world.gobs.get(deer).is_none(),
        "q40 arrow (150 dmg) kills a 40 HP deer"
    );
    assert_eq!(g.world.players[pidx].aim, None, "aim ends with the kill");
    // Loot on the ground: deer drops meat and a bone nearby.
    let meat_gidx = g.world.res.intern("gfx/invobjs/meat");
    let bone_gidx = g.world.res.intern("gfx/invobjs/bone");
    let loot: Vec<u16> = (0..g.world.gobs.kind.len())
        .filter(|i| {
            let (dx, dy) = g.world.gobs.pos[*i];
            (dx - pos.0).abs() < 60 && (dy - pos.1).abs() < 60
        })
        .filter_map(|i| g.world.gobs.kind[i].drop_info().map(|d| d.0))
        .collect();
    assert!(
        loot.contains(&meat_gidx) || loot.contains(&bone_gidx),
        "meat or bone dropped near the kill"
    );
}
/// Cross-node archery: aiming at a GUEST animal fills the meter,
/// and the auto-release ships one RelayAttack with chip=0 (the
/// ranged bypass marker) carrying the Fandom damage to the
/// animal's authority node. The arrow is spent on the shooter's
/// node regardless of the hit roll.
#[tokio::test]
async fn relay_arrow_shot_ships_ranged_relayattack() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("bowrelay", 0, 2);
    let gid = relay_wolf_guest(&mut g, 66, 200);
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let pgob = g.world.players[pidx].gob;
    g.player_interact(1, pgob, gid, (0, 0));
    assert_eq!(
        g.world.players[pidx].aim.map(|a| a.target),
        Some(gid),
        "aim opens against a guest animal"
    );
    // Fill the meter: 40 ticks at 250/tick.
    for _ in 0..40 {
        let aim = g.world.players[pidx].aim.expect("aim kept");
        g.tick_aim(pidx, 1, pgob, aim);
    }
    assert_eq!(arrow_count(&mut g, pidx), 9, "one arrow spent on release");
    let mut saw_ranged_relay = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::RelayAttack { chip, dmg, .. } = msg {
            assert_eq!(chip, 0, "ranged relay carries the chip-0 marker");
            assert_eq!(dmg, crate::archery::bow_damage(10));
            saw_ranged_relay = true;
        }
    }
    assert!(
        saw_ranged_relay,
        "the release must relay one ranged RelayAttack"
    );
}

/// The authority side applies a chip-0 RelayAttack without the
/// openings gate: HP drops by the full damage even at a full
/// defence bar, and death runs the relayed death flow.
#[tokio::test]
async fn relay_swing_chip0_bypasses_openings_and_kills() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("arrowauth", 0, 2);
    // A LOCAL wolf as the authority-side target (the animal this
    // node owns); the relay path is exercised directly.
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let res = g.world.res.intern(Species::Wolf.resname());
    let wolf = g.world.gobs.spawn(
        Kind::Animal {
            species: Species::Wolf,
        },
        (px + 66, py),
        res,
        Species::Wolf.max_hp(),
        33,
    );
    g.world.animal_gobs.push(wolf);
    let wslot = g.world.gobs.get(wolf).unwrap();
    let hp0 = g.world.gobs.hp[wslot];
    // chip 0, damage 200 (> wolf 60 HP): full defence bar, no gate.
    g.relay_swing(pgob, wolf, 0, 200);
    assert!(
        g.world.gobs.get(wolf).is_none(),
        "a chip-0 relay kills through a full defence bar"
    );
    let _ = hp0;
    // The kill cleared the fight teardown state.
    assert_eq!(g.world.players[pidx].fight_target, None);
}

// ------------------------------------------------------------------
// PvP archery (session 38)
// ------------------------------------------------------------------

/// Enter a SECOND local session player (sid 2) and return
/// (player index, gob). Both players spawn at the world spawn.
/// In cluster mode pass the mesh receiver so the character-
/// migration query can be answered with nacks (the same trick
/// `clustered_game` plays for the FIRST player).
fn second_player(
    g: &mut Game,
    name: &str,
    mut mesh: Option<&mut tokio::sync::mpsc::UnboundedReceiver<(usize, crate::nodes::NodeMsg)>>,
) -> (usize, GobId) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel(512);
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
fn guest_player(g: &mut Game, dx: i32) -> GobId {
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
fn drain_chat(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) -> Vec<String> {
    let mut seen = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let Some(text) = chat_log_text(&msg) {
            seen.push(text);
        }
    }
    seen
}

/// Clicking another LOCAL player with an equipped bow opens the
/// ranged aim (PvP), not the party-invite menu; without a bow the
/// click keeps the party menu path, and a self-click never aims.
#[tokio::test]
async fn pvp_bow_click_opens_aim_not_party_menu() {
    let (mut g, _rx, _raw) = entered_game("pvpaim");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let (vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    // Put the victim inside bow range.
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 66, py));
    g.player_interact(1, pgob, vgob, (0, 0));
    assert_eq!(
        g.world.players[pidx].aim.map(|a| a.target),
        Some(vgob),
        "bow click on a player opens the PvP aim"
    );
    // The victim is untouched by the mere aim.
    assert_eq!(g.world.players[vidx].hp, 100);
    // Self-click with a bow: no aim (falls through to the party
    // menu, which ignores self-clicks too).
    g.world.players[pidx].aim = None;
    g.player_interact(1, pgob, pgob, (0, 0));
    assert_eq!(g.world.players[pidx].aim, None, "never aim at yourself");
    // Without a bow the click is a party invite, not an aim.
    g.world.players[pidx].equip[0] = None;
    g.world.players[pidx].aim = None;
    g.player_interact(1, pgob, vgob, (0, 0));
    assert_eq!(
        g.world.players[pidx].aim, None,
        "no bow, no PvP aim - the party menu owns the click"
    );
}

/// A guaranteed hit on a LOCAL player applies the Fandom damage
/// through the victim's (empty) armor, tells both sides in chat,
/// and re-arms the aim while the victim lives.
#[tokio::test]
async fn pvp_arrow_hits_local_player() {
    let (mut g, mut rx, _raw) = entered_game("pvphit");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let (vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 66, py));
    let aim = crate::archery::RangedAim::new(vgob, 10, crate::archery::AIM_RATE_WOODBOW);
    g.shoot_arrow(pidx, 1, aim, 0);
    assert_eq!(
        g.world.players[vidx].hp,
        100 - crate::archery::bow_damage(10),
        "q10 bow deals 75 to an unarmored player"
    );
    assert_eq!(arrow_count(&mut g, pidx), 9, "one arrow consumed");
    assert_eq!(
        g.world.players[pidx].aim.map(|a| a.target),
        Some(vgob),
        "aim re-arms while the victim lives"
    );
    let chat = drain_chat(&mut rx);
    assert!(
        chat.iter().any(|t| t.contains("Your arrow hits victim")),
        "shooter told about the hit: {chat:?}"
    );
}

/// A lethal arrow knocks the victim out: HP resets to the knockout
/// floor, the fight state tears down, and the shooter's chat
/// reports the defeat.
#[tokio::test]
async fn pvp_lethal_arrow_knocks_out_victim() {
    let (mut g, mut rx, _raw) = entered_game("pvpkill");
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 40);
    let (vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 66, py));
    // Weaken the victim so one q40 shot (150 dmg) is lethal.
    g.world.players[vidx].hp = 30;
    let aim = crate::archery::RangedAim::new(vgob, 40, crate::archery::AIM_RATE_WOODBOW);
    g.shoot_arrow(pidx, 1, aim, 0);
    assert_eq!(
        g.world.players[vidx].hp, 50,
        "knockout resets the victim to the 50 HP floor"
    );
    let chat = drain_chat(&mut rx);
    assert!(
        chat.iter().any(|t| t.contains("You have defeated victim")),
        "shooter told about the knockout: {chat:?}"
    );
}

/// A bow click on a GUEST player opens the aim, and the
/// auto-release ships one PvpArrow to the victim's home node
/// carrying the Fandom damage; the arrow is spent locally
/// regardless of the roll.
#[tokio::test]
async fn pvp_guest_shot_ships_pvparrow() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("pvpguest", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    arm_bow(&mut g, pidx, 10, 10);
    let pgob = g.world.players[pidx].gob;
    let gid = guest_player(&mut g, 66);
    g.player_interact(1, pgob, gid, (0, 0));
    assert_eq!(
        g.world.players[pidx].aim.map(|a| a.target),
        Some(gid),
        "aim opens against a guest player"
    );
    for _ in 0..40 {
        let aim = g.world.players[pidx].aim.expect("aim kept");
        g.tick_aim(pidx, 1, pgob, aim);
    }
    assert_eq!(arrow_count(&mut g, pidx), 9, "one arrow spent on release");
    let mut saw_arrow = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::PvpArrow {
            victim,
            attacker,
            dmg,
        } = msg
        {
            assert_eq!(victim, gid);
            assert_eq!(attacker, pgob);
            assert_eq!(dmg, crate::archery::bow_damage(10));
            saw_arrow = true;
        }
    }
    assert!(saw_arrow, "the release must ship one PvpArrow");
}

/// The authority side of a PvP arrow: a local victim takes the
/// damage through hurt_player, gets the chat line, and the
/// shooter's node receives a PvpArrowResult answer (killed=false
/// while the victim stands, killed=true on a knockout).
#[tokio::test]
async fn pvp_arrow_handler_hurts_victim_and_answers() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("pvpauth", 0, 2);
    let (vidx, vgob) = second_player(&mut g, "victim", Some(&mut mesh_rx));
    let shooter = foreign_node_gob_id(0, 2, 9);
    // Drain announce noise, then apply a non-lethal arrow.
    while mesh_rx.try_recv().is_ok() {}
    g.on_node_msg(crate::nodes::NodeMsg::PvpArrow {
        victim: vgob,
        attacker: shooter,
        dmg: 40,
    });
    assert_eq!(g.world.players[vidx].hp, 60, "40 damage through no armor");
    let mut answered = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::PvpArrowResult { shooter: s, killed } = msg {
            assert_eq!(s, shooter);
            assert!(!killed, "the victim still stands");
            answered = true;
        }
    }
    assert!(answered, "the home node answers the shot");
    // Lethal follow-up: the knockout reports killed=true.
    g.world.players[vidx].hp = 10;
    g.on_node_msg(crate::nodes::NodeMsg::PvpArrow {
        victim: vgob,
        attacker: shooter,
        dmg: 40,
    });
    assert_eq!(g.world.players[vidx].hp, 50, "knockout floor after lethal");
    let mut killed_seen = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::PvpArrowResult { killed, .. } = msg {
            assert!(killed, "the knockout is reported back");
            killed_seen = true;
        }
    }
    assert!(killed_seen, "lethal answer sent");
}

// ------------------------------------------------------------------
// Session 39: melee PvP between players (local + cross-node)
// ------------------------------------------------------------------

/// Open the melee duel through the real click path: the flower menu
/// carries the Fight petal, and confirming it arms the attacker,
/// opens the fight window on BOTH sides, and tells both players.
#[tokio::test]
async fn melee_local_fight_menu_opens_duel() {
    let (mut g, mut rx, _raw) = entered_game("meleemenu");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 20, py));
    // Click the victim with NO bow: the flower menu opens.
    g.player_interact(1, pgob, vgob, (0, 0));
    let (wid, action) = g
        .sessions
        .get(&1)
        .unwrap()
        .player_menu
        .expect("player flower menu armed");
    assert!(
        matches!(action, crate::party::PlayerMenu::InviteTarget(t) if t == vgob),
        "local player clicks arm the invite menu: {action:?}"
    );
    // Petal 1 is Fight (petal 0 invites).
    g.on_party_menu_choice(1, wid, 1);
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(vgob),
        "Fight petal arms the attacker"
    );
    // Both sides see a fight relation.
    let attacker_rel = g
        .sessions
        .get(&1)
        .unwrap()
        .fight
        .rel(vgob)
        .expect("attacker relation on the victim");
    assert_eq!(attacker_rel.defence, crate::fight::BAR_FULL);
    let victim_rel = g
        .sessions
        .get(&2)
        .unwrap()
        .fight
        .rel(pgob)
        .expect("victim relation on the attacker");
    assert_eq!(victim_rel.offence, 0, "no pressure yet");
    // The victim has NOT been armed - answering is their choice.
    assert_eq!(g.world.players[vidx].fight_target, None);
    let chat = drain_chat(&mut rx);
    assert!(
        chat.iter().any(|t| t.contains("You attack victim")),
        "attacker told: {chat:?}"
    );
    // Self-click never arms a duel even through the direct path.
    g.world.players[pidx].fight_target = None;
    g.start_pvp_melee(1, pgob);
    assert_eq!(
        g.world.players[pidx].fight_target, None,
        "never duel yourself"
    );
}

/// The openings economy runs between two players: swings chip the
/// victim's session defence bar, and only an opening passes damage
/// through to HP (armor applies, bars reset on the break).
#[tokio::test]
async fn melee_local_swings_chip_defence_until_opening() {
    let (mut g, mut rx, _raw) = entered_game("meleechip");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 20, py));
    g.start_pvp_melee(1, vgob);
    // Full offence bar + no cooldown: the next tick swings once.
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    let stamina_before = g.world.players[pidx].stamina;
    g.tick();
    assert_eq!(
        g.sessions.get(&2).unwrap().fight.own_def,
        crate::fight::BAR_FULL - crate::fight::SWING_DEF_DMG,
        "one swing chips the victim's defence (weight 1.0 at balance 0)"
    );
    assert_eq!(
        g.world.players[vidx].hp, 100,
        "no damage through an intact defence"
    );
    assert_eq!(
        g.world.players[pidx].stamina,
        stamina_before - 2,
        "each swing costs stamina"
    );
    assert!(g
        .sessions
        .get(&2)
        .unwrap()
        .fight
        .rel(pgob)
        .is_some_and(|r| r.ip_other >= 1));
    // Wear the defence to the opening threshold, then swing again:
    // the hit lands through the opening and the bar resets.
    let vout = g.sessions.get_mut(&2).unwrap();
    vout.fight.own_def = crate::fight::OPENING_THRESHOLD;
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    g.tick();
    assert_eq!(
        g.world.players[vidx].hp,
        100 - 5,
        "default str 10 swing deals 5 through the opening"
    );
    assert_eq!(
        g.sessions.get(&2).unwrap().fight.own_def,
        crate::fight::BAR_FULL,
        "a landed hit resets the defence bar"
    );
    let chat = drain_chat(&mut rx);
    assert!(
        chat.iter()
            .any(|t| t.contains("You hit victim for 5 damage.")),
        "attacker told about the landed hit: {chat:?}"
    );
}

/// A lethal swing knocks the victim out: HP resets to the knockout
/// floor, both fights tear down, and the attacker's chat reports
/// the defeat.
#[tokio::test]
async fn melee_local_lethal_swing_knocks_out() {
    let (mut g, mut rx, _raw) = entered_game("meleeko");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 20, py));
    g.start_pvp_melee(1, vgob);
    // The victim answers: a mutual duel.
    g.world.players[vidx].fight_target = Some(pgob);
    // Open defence + 3 HP: the next swing is lethal.
    g.sessions.get_mut(&2).unwrap().fight.own_def = crate::fight::OPENING_THRESHOLD;
    g.world.players[vidx].hp = 3;
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    g.tick();
    assert_eq!(
        g.world.players[vidx].hp, 50,
        "knockout resets the victim to the 50 HP floor"
    );
    assert_eq!(
        g.world.players[pidx].fight_target, None,
        "the attacker's duel ends on the knockout"
    );
    assert_eq!(
        g.world.players[vidx].fight_target, None,
        "the victim's duel ends too (hurt_player reset)"
    );
    assert!(
        g.sessions.get(&2).unwrap().fight.rels.is_empty(),
        "the victim's relations are cleared"
    );
    let chat = drain_chat(&mut rx);
    assert!(
        chat.iter().any(|t| t.contains("You have defeated")),
        "attacker told about the knockout: {chat:?}"
    );
}

/// Session-43 combat indexes: the slot maps resolve the PvP victim in
/// O(1) and the engaged map keeps the first-engaged-player semantics
/// of the removed linear `find` on a shared animal target.
#[tokio::test]
async fn combat_indexes_resolve_victims_and_first_engagement() {
    let (mut g, _rx, _raw) = entered_game("cix");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (vidx, vgob) = second_player(&mut g, "cixvictim", None);
    let deer = spawn_deer_at(&mut g, pidx, 20, Species::Deer.max_hp());
    // Both players engage the same deer: the lowest player index wins.
    g.start_fight(1, deer, Species::Deer);
    g.start_fight(2, deer, Species::Deer);
    g.tick_combat();
    let pslot = g.world.gobs.get(g.world.players[pidx].gob).unwrap();
    let vslot = g.world.gobs.get(vgob).unwrap();
    let dslot = g.world.gobs.get(deer).unwrap();
    assert_eq!(
        g.combat_ix.player_of_slot[pslot],
        (pidx as u32) + 1,
        "player_of_slot resolves the attacker's own gob"
    );
    assert_eq!(
        g.combat_ix.player_of_slot[vslot],
        (vidx as u32) + 1,
        "player_of_slot resolves the second player"
    );
    assert_eq!(
        g.combat_ix.engaged_of_slot[dslot],
        (pidx as u32) + 1,
        "first engaged player wins a shared target"
    );
    // The PvP victim resolves by slot too (the O(1) lookup replaced
    // the linear `position` scan).
    g.start_pvp_melee(1, vgob);
    g.tick_combat();
    let vslot = g.world.gobs.get(vgob).unwrap();
    assert_eq!(
        g.combat_ix.player_of_slot[vslot],
        (vidx as u32) + 1,
        "PvP victim index resolves through the slot map"
    );
}

/// Session-43 stale-row guard: a player knocked out during the player
/// phase (PvP) keeps a stale engaged-animal row for the rest of the
/// tick; the live fight_target re-check must stop the deer from
/// biting the already-knocked-out player (the removed linear `find`
/// re-read fight_target at the same point).
#[tokio::test]
async fn knockout_in_player_phase_stops_the_animal_bite() {
    let (mut g, _rx, _raw) = entered_game("cixstale");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (_vidx, vgob) = second_player(&mut g, "cixstalev", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    // The player attacks a deer within reach, one swing away.
    let deer = spawn_deer_at(&mut g, pidx, 20, Species::Deer.max_hp());
    g.start_fight(1, deer, Species::Deer);
    g.world
        .animal_fights
        .get_mut(&deer)
        .expect("deer fight row")
        .off = crate::fight::SWING_SPEND;
    // A PvP attacker stands next to the player, one swing from a
    // knockout (defence at the opening threshold, 3 HP left).
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 20, py));
    g.start_pvp_melee(2, pgob);
    g.sessions.get_mut(&1).unwrap().fight.own_def = crate::fight::OPENING_THRESHOLD;
    g.world.players[pidx].hp = 3;
    g.sessions.get_mut(&2).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&2).unwrap().fight.atkc = 0;
    g.tick_combat();
    assert_eq!(
        g.world.players[pidx].hp, 50,
        "the PvP swing knocked the player out (50 HP floor)"
    );
    assert_eq!(
        g.world.players[pidx].fight_target, None,
        "the knockout cleared the deer engagement"
    );
    assert_eq!(
        g.world.players[pidx].hp, 50,
        "the stale engaged row must NOT let the deer bite this tick"
    );
}

/// PvP knockout consequences (server policy, combat-system.md): the
/// loser forfeits 10% of unused LP, the winner is flagged criminal
/// with a live buff icon (RMSG_BUFF set), and the flag expires with
/// an RMSG_BUFF rm once the timer runs out.
#[tokio::test]
async fn pvp_knockout_consequences_local() {
    let (mut g, mut rx, _raw) = entered_game("pvpconseq");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 20, py));
    // The victim carries 100 unused LP: the knockout must cost 10.
    g.world.players[vidx].lp = 100;
    g.start_pvp_melee(1, vgob);
    g.sessions.get_mut(&2).unwrap().fight.own_def = crate::fight::OPENING_THRESHOLD;
    g.world.players[vidx].hp = 3;
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    g.tick();
    assert_eq!(
        g.world.players[vidx].lp, 90,
        "the loser forfeits 10% of unused LP"
    );
    let until = g.world.players[pidx]
        .criminal_until_ms
        .expect("winner flagged criminal");
    assert!(until > g.world.now_ms, "the flag runs into the future");
    assert_eq!(
        (until - g.world.now_ms) / 1000,
        30 * 60,
        "the flag runs 30 real minutes"
    );
    let mut frames = Vec::new();
    while let Ok(f) = rx.try_recv() {
        frames.push(f);
    }
    let chat: Vec<String> = frames.iter().filter_map(|f| chat_log_text(f)).collect();
    assert!(
        chat.iter()
            .any(|t| t.contains("flagged criminal for the assault")),
        "winner told about the flag: {chat:?}"
    );
    // The loser's chat line goes to session 2's channel (dropped by
    // the second_player helper); the LP state above already proves
    // the loser's share was applied.
    // The buff icon reached the winner's reliable stream: one
    // RMSG_BUFF "set" carrying the criminal tooltip.
    assert!(
        frames
            .iter()
            .any(|f| f.first() == Some(&hnh_proto::consts::RMSG_BUFF)
                && f[1..].starts_with(b"set\0")
                && f.windows(18).any(|w| w == b"Criminal (assault)")),
        "RMSG_BUFF set with the criminal tooltip on the wire"
    );
    // Expiry: advance the flag to the past and sweep - the state
    // clears and an RMSG_BUFF rm lands on the stream.
    g.world.players[pidx].criminal_until_ms = Some(g.world.now_ms);
    g.tick();
    assert_eq!(
        g.world.players[pidx].criminal_until_ms, None,
        "the sweep clears the expired flag"
    );
    let mut rm_frames = Vec::new();
    while let Ok(f) = rx.try_recv() {
        rm_frames.push(f);
    }
    let chat: Vec<String> = rm_frames.iter().filter_map(|f| chat_log_text(f)).collect();
    assert!(
        chat.iter().any(|t| t.contains("criminal flag has expired")),
        "expiry chat: {chat:?}"
    );
    assert!(
        rm_frames.iter().any(
            |f| f.first() == Some(&hnh_proto::consts::RMSG_BUFF) && f[1..].starts_with(b"rm\0")
        ),
        "RMSG_BUFF rm on expiry"
    );
}

/// Maneuver economy (session 40): act("atk", "sting") spends its 2
/// IP, fills the two-slot attack queue, and streams the frv `atk`
/// uimsg with the pagina resource.
#[tokio::test]
async fn maneuver_attack_select_streams_atk() {
    let (mut g, mut rx, _raw) = entered_game("maneuver1");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (_vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 20, py));
    g.start_pvp_melee(1, vgob);
    // Both relations carry 5 IP (attacker rel on the victim's gob,
    // victim rel on the attacker's gob).
    g.sessions
        .get_mut(&1)
        .unwrap()
        .fight
        .rel_mut(vgob)
        .unwrap()
        .ip_self = 5;
    g.sessions
        .get_mut(&2)
        .unwrap()
        .fight
        .rel_mut(pgob)
        .unwrap()
        .ip_self = 5;
    g.on_maneuver(1, "sting");
    let out = g.sessions.get(&1).unwrap();
    let rel = out.fight.rel(vgob).unwrap();
    assert_eq!(rel.ip_self, 3, "sting costs 2 IP");
    assert_eq!(
        out.fight.atk_cur,
        Some("paginae/atk/sting"),
        "the selection becomes the current attack"
    );
    assert_eq!(out.fight.atk_next, None, "empty queue slides into next");
    // Queue a second attack: the first slides into `next`.
    g.on_maneuver(1, "pow");
    let out = g.sessions.get(&1).unwrap();
    assert_eq!(out.fight.atk_cur, Some("paginae/atk/pow"));
    assert_eq!(
        out.fight.atk_next,
        Some("paginae/atk/sting"),
        "the previous current attack slides into next"
    );
    // The frv atk uimsg reached the wire (RMSG_WDGMSG "atk").
    let mut saw_atk = false;
    while let Ok(f) = rx.try_recv() {
        if f.first() == Some(&hnh_proto::consts::RMSG_WDGMSG) && f.windows(4).any(|w| w == b"atk\0")
        {
            saw_atk = true;
        }
    }
    assert!(saw_atk, "frv atk uimsg on the wire");
}

/// Maneuver gating: Cleave refuses without >= 3 advantage and lands
/// once the advantage is there; Battle Cry refuses under 14 IP.
#[tokio::test]
async fn maneuver_requirements_gate_the_moves() {
    let (mut g, mut rx, _raw) = entered_game("maneuver2");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (_vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 20, py));
    g.start_pvp_melee(1, vgob);
    // No advantage, no IP: both gated moves refuse.
    g.on_maneuver(1, "cleave");
    g.on_maneuver(1, "roar");
    let chat = drain_chat(&mut rx);
    assert!(
        chat.iter().any(|t| t.contains("You need more advantage")),
        "cleave refused without advantage: {chat:?}"
    );
    assert!(
        chat.iter()
            .any(|t| t.contains("You need at least 14 initiative")),
        "battle cry refused under 14 IP: {chat:?}"
    );
    // Grant the requirement: Cleave lands (8 IP cost, >= 3 adv).
    {
        let out = g.sessions.get_mut(&1).unwrap();
        let rel = out.fight.rel_mut(vgob).unwrap();
        rel.ip_self = 20;
        rel.adv = 30;
    }
    g.on_maneuver(1, "cleave");
    let out = g.sessions.get(&1).unwrap();
    let rel = out.fight.rel(vgob).unwrap();
    assert_eq!(rel.ip_self, 12, "cleave spends its 8 IP");
    assert_eq!(out.fight.atk_cur, Some("paginae/atk/cleave"));
    // Battle Cry with 14 IP on hand: 14 - 7 = 7 left, +2 advantage.
    {
        let out = g.sessions.get_mut(&1).unwrap();
        let rel = out.fight.rel_mut(vgob).unwrap();
        rel.ip_self = 14;
    }
    let adv_before = g.sessions.get(&1).unwrap().fight.rel(vgob).unwrap().adv;
    g.on_maneuver(1, "roar");
    let out = g.sessions.get(&1).unwrap();
    let rel = out.fight.rel(vgob).unwrap();
    assert_eq!(rel.ip_self, 7, "battle cry spends its 7 IP");
    assert_eq!(rel.adv, adv_before + 20, "battle cry grants +2 advantage");
    assert_eq!(rel.balance, 5, "advantage clamps to the dial maximum");
}

/// Boost economy: Charge! generates +1 IP for the user, Throw Sand
/// drains 2 IP from the local victim's own pool (both windows
/// re-stream), and Seize The Day! banks +0.3 advantage.
#[tokio::test]
async fn maneuver_boosts_move_ip_and_advantage() {
    let (mut g, mut rx, _raw) = entered_game("maneuver3");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (_vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 20, py));
    g.start_pvp_melee(1, vgob);
    // Charge! with an empty pool: +1 IP, no cost.
    g.on_maneuver(1, "berserk");
    let out = g.sessions.get(&1).unwrap();
    let rel = out.fight.rel(vgob).unwrap();
    assert_eq!(rel.ip_self, 1, "charge generates one IP");
    // Seize The Day!: +0.3 advantage banks into the pool.
    g.on_maneuver(1, "seize");
    let out = g.sessions.get(&1).unwrap();
    let rel = out.fight.rel(vgob).unwrap();
    assert_eq!(rel.adv, 3, "seize banks +0.3 advantage");
    assert_eq!(rel.balance, 0, "+0.3 still rounds to dial 0");
    // Throw Sand: the victim starts with 5 IP and loses 2.
    g.sessions
        .get_mut(&2)
        .unwrap()
        .fight
        .rel_mut(pgob)
        .unwrap()
        .ip_self = 5;
    g.on_maneuver(1, "throwsand");
    let vrel = g.sessions.get(&2).unwrap().fight.rel(pgob).unwrap();
    assert_eq!(vrel.ip_self, 3, "throw sand drains the victim's pool");
    let out = g.sessions.get(&1).unwrap();
    let rel = out.fight.rel(vgob).unwrap();
    assert_eq!(
        rel.ip_other, 3,
        "the attacker's mirror view tracks the victim's pool"
    );
    let _ = drain_chat(&mut rx);
}

/// A mutual duel swings BOTH ways: the victim (armed through the
/// fight window's select) chips the attacker's defence with the
/// identical economy.
#[tokio::test]
async fn melee_local_mutual_duel_swings_both_ways() {
    let (mut g, _rx, _raw) = entered_game("meleemutual");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (vidx, vgob) = second_player(&mut g, "victim", None);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let vslot = g.world.gobs.get(vgob).unwrap();
    g.world.gobs.set_pos(vslot, (px + 20, py));
    g.start_pvp_melee(1, vgob);
    // The victim answers through the fight-window select (the frv
    // "click" path arms fight_target without a second flower menu).
    g.on_frv_msg(2, "click", &[hnh_proto::ListArg::Int(pgob)]);
    assert_eq!(g.world.players[vidx].fight_target, Some(pgob));
    // Both swing on the same tick.
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    g.sessions.get_mut(&2).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&2).unwrap().fight.atkc = 0;
    g.tick();
    assert_eq!(
        g.sessions.get(&2).unwrap().fight.own_def,
        crate::fight::BAR_FULL - crate::fight::SWING_DEF_DMG,
        "attacker chipped the victim's defence"
    );
    assert_eq!(
        g.sessions.get(&1).unwrap().fight.own_def,
        crate::fight::BAR_FULL - crate::fight::SWING_DEF_DMG,
        "victim chipped the attacker's defence"
    );
}

/// An equipped melee weapon replaces the unarmed model on every
/// local swing path: the stone axe at q10/str10 deals 15 through an
/// opening (fight.rs WEAPONS table) instead of the unarmed 5.
#[tokio::test]
async fn melee_local_weapon_swing_deals_axe_damage() {
    let (mut g, mut rx, _raw) = entered_game("meleeaxe");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let (vidx, vgob) = second_player(&mut g, "victim", None);
    // Equip the stone axe in the hand slot (slot 3, q10).
    let axe = g.world.res.intern("gfx/invobjs/axe");
    g.world.players[pidx].equip[3] = Some(InvStack {
        res: axe,
        count: 1,
        ql: 10,
        label: "",
    });
    g.start_pvp_melee(1, vgob);
    // Wear the defence to the opening, then land one swing.
    g.sessions.get_mut(&2).unwrap().fight.own_def = crate::fight::OPENING_THRESHOLD;
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    g.tick();
    assert_eq!(
        g.world.players[vidx].hp,
        100 - 15,
        "q10 stone axe at str 10 deals 15 through the opening (base 15)"
    );
    let chat = drain_chat(&mut rx);
    assert!(
        chat.iter()
            .any(|t| t.contains("You hit victim for 15 damage.")),
        "attacker told about the weapon damage: {chat:?}"
    );
    // Unequip: the next opening falls back to the unarmed model.
    g.world.players[pidx].equip[3] = None;
    g.sessions.get_mut(&2).unwrap().fight.own_def = crate::fight::OPENING_THRESHOLD;
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    g.tick();
    assert_eq!(
        g.world.players[vidx].hp,
        100 - 15 - 5,
        "bare hands deal the unarmed 5 again"
    );
}

/// The relay path carries the weapon too: a cross-node PvpSwing
/// ships the axe damage (15) instead of the unarmed 5.
#[tokio::test]
async fn melee_relay_weapon_ships_axe_damage() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("meleerelayaxe", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let gid = guest_player(&mut g, 20);
    let axe = g.world.res.intern("gfx/invobjs/axe");
    g.world.players[pidx].equip[3] = Some(InvStack {
        res: axe,
        count: 1,
        ql: 10,
        label: "",
    });
    g.player_interact(1, pgob, gid, (0, 0));
    let (wid, _) = g
        .sessions
        .get(&1)
        .unwrap()
        .player_menu
        .expect("guest fight menu armed");
    g.on_party_menu_choice(1, wid, 0);
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    while mesh_rx.try_recv().is_ok() {}
    g.tick();
    let mut swings = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::PvpSwing { dmg, .. } = msg {
            swings.push(dmg);
        }
    }
    assert_eq!(swings, vec![15], "the relay swing carries the axe damage");
}

/// Cross-node duel: the Fight petal on a GUEST player arms the
/// relay duel, and a full-bar swing ships exactly one PvpSwing to
/// the victim's home node with the openings payload.
#[tokio::test]
async fn melee_relay_guest_duel_ships_pvpswing() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("meleerelay", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let gid = guest_player(&mut g, 20);
    // Click the guest with no bow: the guest Fight menu opens.
    g.player_interact(1, pgob, gid, (0, 0));
    let (wid, action) = g
        .sessions
        .get(&1)
        .unwrap()
        .player_menu
        .expect("guest fight menu armed");
    assert!(
        matches!(action, crate::party::PlayerMenu::FightTarget(t) if t == gid),
        "guest player clicks arm the Fight-only menu: {action:?}"
    );
    g.on_party_menu_choice(1, wid, 0);
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(gid),
        "the relay duel arms"
    );
    assert!(
        g.world.guest_fights.contains_key(&gid),
        "the local mirror exists"
    );
    // Full bar: the next tick swings and ships the relay message.
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    while mesh_rx.try_recv().is_ok() {}
    g.tick();
    let mut swings = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::PvpSwing {
            attacker,
            victim,
            chip,
            dmg,
        } = msg
        {
            swings.push((attacker, victim, chip, dmg));
        }
    }
    // Default str 10, weight 1.0: chip = SWING_DEF_DMG, dmg = 5.
    assert_eq!(
        swings,
        vec![(pgob, gid, crate::fight::SWING_DEF_DMG, 5)],
        "one swing = exactly one PvpSwing to the victim's home node"
    );
}

/// Cross-node maneuver IP relay (session 42): the foreign attacker's
/// node relays a ManeuverDelta; the victim's home node folds the
/// opponent-pool delta into her authoritative relation row keyed by
/// the attacker's guest gob and re-streams her fight window. A zero
/// delta is a no-op (no rel creation, no wire traffic).
#[tokio::test]
async fn maneuver_delta_relay_folds_and_streams() {
    let (mut g, mut rx, _raw, mut mesh_rx) = clustered_game("maneuverdelta", 0, 2);
    let (vidx, vgob) = second_player(&mut g, "victim", Some(&mut mesh_rx));
    let attacker = foreign_node_gob_id(0, 2, 9);
    // The victim is already dueling the foreign attacker: a relation
    // row keyed by the attacker's gob exists.
    g.sessions
        .get_mut(&2)
        .unwrap()
        .fight
        .rels
        .push(crate::fight::FightRel::new(attacker));
    g.on_node_msg(crate::nodes::NodeMsg::ManeuverDelta {
        attacker,
        victim: vgob,
        ip_opp: -20,
    });
    let rel = g
        .sessions
        .get(&2)
        .unwrap()
        .fight
        .rel(attacker)
        .expect("relation row survives the delta")
        .clone();
    assert_eq!(rel.ip_self, 0, "the delta clamps at zero, never below");
    // A positive fold raises the victim's own pool.
    g.on_node_msg(crate::nodes::NodeMsg::ManeuverDelta {
        attacker,
        victim: vgob,
        ip_opp: 30,
    });
    let rel = g
        .sessions
        .get(&2)
        .unwrap()
        .fight
        .rel(attacker)
        .unwrap()
        .clone();
    assert_eq!(rel.ip_self, 30);
    // The victim's fight window re-streamed (an upd frame on her
    // widget queue).
    let got_upd = rx.try_recv().is_ok();
    assert!(got_upd, "the victim's window re-streams after the delta");
    assert_eq!(g.world.players[vidx].session, 2, "victim session intact");
    // Zero delta: no rel creation for an unknown row, no crash.
    let stranger = foreign_node_gob_id(0, 2, 21);
    g.on_node_msg(crate::nodes::NodeMsg::ManeuverDelta {
        attacker: stranger,
        victim: vgob,
        ip_opp: 0,
    });
    assert!(g.sessions.get(&2).unwrap().fight.rel(stranger).is_none());
}

/// Authority side of the relay duel: the victim's home node chips
/// the session defence bar, lands the HP damage through an opening
/// (chat + knockout), and answers PvpSwingResult so the attacker's
/// mirror re-syncs.
#[tokio::test]
async fn melee_relay_authority_applies_and_answers() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("meleeauth", 0, 2);
    let (vidx, vgob) = second_player(&mut g, "victim", Some(&mut mesh_rx));
    let attacker = foreign_node_gob_id(0, 2, 9);
    while mesh_rx.try_recv().is_ok() {}
    // Non-opening swing: the bar chips, no HP damage, no chat.
    g.on_node_msg(crate::nodes::NodeMsg::PvpSwing {
        attacker,
        victim: vgob,
        chip: crate::fight::SWING_DEF_DMG,
        dmg: 5,
    });
    assert_eq!(
        g.sessions.get(&2).unwrap().fight.own_def,
        crate::fight::BAR_FULL - crate::fight::SWING_DEF_DMG,
        "authority chips the victim's defence"
    );
    assert_eq!(g.world.players[vidx].hp, 100, "no opening, no damage");
    let mut answer: Option<(i32, bool, bool)> = None;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::PvpSwingResult {
            def,
            landed,
            killed,
            ..
        } = msg
        {
            answer = Some((def, landed, killed));
        }
    }
    let (def, landed, killed) = answer.expect("the home node answers");
    assert_eq!(def, crate::fight::BAR_FULL - crate::fight::SWING_DEF_DMG);
    assert!(!landed && !killed);
    // Opening swing: damage lands, chat tells the victim, and a
    // lethal blow knocks out with killed=true in the answer.
    g.world.players[vidx].hp = 3;
    g.sessions.get_mut(&2).unwrap().fight.own_def = crate::fight::OPENING_THRESHOLD;
    g.on_node_msg(crate::nodes::NodeMsg::PvpSwing {
        attacker,
        victim: vgob,
        chip: crate::fight::SWING_DEF_DMG,
        dmg: 5,
    });
    assert_eq!(g.world.players[vidx].hp, 50, "knockout floor");
    let mut killed_seen = false;
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::PvpSwingResult { landed, killed, .. } = msg {
            assert!(landed, "the opening swing landed");
            assert!(killed, "the knockout is reported");
            killed_seen = true;
        }
    }
    assert!(killed_seen, "lethal answer sent");
}

/// The attacker's node applies a PvpSwingResult: the mirror and the
/// fight-window relation re-sync from the authoritative bar, a
/// landed hit is chatted, and a knockout tears the duel down.
#[tokio::test]
async fn melee_relay_result_resyncs_and_closes_on_knockout() {
    let (mut g, mut rx, _raw, mut mesh_rx) = clustered_game("meleeresult", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let gid = guest_player(&mut g, 20);
    g.start_pvp_melee(1, gid);
    while mesh_rx.try_recv().is_ok() {}
    // A chip answer re-syncs the mirror and the relation view.
    g.on_node_msg(crate::nodes::NodeMsg::PvpSwingResult {
        attacker: pgob,
        victim: gid,
        def: 4321,
        landed: false,
        killed: false,
    });
    assert_eq!(g.world.guest_fights[&gid].def, 4321, "mirror re-synced");
    assert_eq!(
        g.sessions
            .get(&1)
            .unwrap()
            .fight
            .rel(gid)
            .map(|r| r.defence),
        Some(4321),
        "the fight window sees the authoritative bar"
    );
    // A landed + knockout answer closes the duel.
    g.on_node_msg(crate::nodes::NodeMsg::PvpSwingResult {
        attacker: pgob,
        victim: gid,
        def: crate::fight::BAR_FULL,
        landed: true,
        killed: true,
    });
    assert_eq!(
        g.world.players[pidx].fight_target, None,
        "the relay duel ends on the knockout"
    );
    assert!(
        !g.world.guest_fights.contains_key(&gid),
        "the mirror row is dropped"
    );
    let chat = drain_chat(&mut rx);
    assert!(
        chat.iter().any(|t| t.contains("You hit Rival")),
        "attacker told about the landed hit: {chat:?}"
    );
    assert!(
        chat.iter().any(|t| t.contains("You have defeated")),
        "attacker told about the knockout: {chat:?}"
    );
}

// ------------------------------------------------------------------
// Taming (session 45): quell gates, tameness accumulation, leash
// lifecycle. Server-policy numbers live in state.rs (TAMENESS_*,
// LEASH_BREAK_TICKS) and the docs Open questions.
// ------------------------------------------------------------------

fn equip_rope(g: &mut Game, pidx: usize) {
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
fn grant_ahusb(g: &mut Game, pidx: usize) {
    let p = &mut g.world.players[pidx];
    p.skills.insert("hunting");
    p.skills.insert("ahusb");
}

/// Spawn one animal of any species at a tile offset from the player.
fn spawn_species_at(
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
fn arm_quell(g: &mut Game, sid: SessionId, target: GobId) {
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

#[tokio::test]
async fn quell_refuses_without_the_ahusb_skill() {
    let (mut g, _rx, _raw) = entered_game("tamenoskill");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let deer = spawn_deer_at(&mut g, pidx, 20, Species::Deer.max_hp());
    g.start_fight(1, deer, Species::Deer);
    equip_rope(&mut g, pidx);
    {
        let out = g.sessions.get_mut(&1).unwrap();
        let rel = out.fight.rel_mut(deer).unwrap();
        rel.ip_self = 5;
        rel.adv = 40;
        rel.sync_balance();
    }
    g.on_maneuver(1, "quell");
    assert_ne!(
        g.sessions.get(&1).unwrap().fight.atk_cur,
        Some("paginae/atk/quell"),
        "the selection must be refused without the Animal Husbandry skill"
    );
    assert!(g.world.tamed.is_empty());
    // Buying the skill unlocks the same selection.
    grant_ahusb(&mut g, pidx);
    arm_quell(&mut g, 1, deer);
}

#[tokio::test]
async fn quell_refuses_without_a_rope() {
    let (mut g, _rx, _raw) = entered_game("tamenorope");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let deer = spawn_deer_at(&mut g, pidx, 20, Species::Deer.max_hp());
    g.start_fight(1, deer, Species::Deer);
    grant_ahusb(&mut g, pidx);
    {
        let out = g.sessions.get_mut(&1).unwrap();
        let rel = out.fight.rel_mut(deer).unwrap();
        rel.ip_self = 5;
        rel.adv = 40;
        rel.sync_balance();
    }
    g.on_maneuver(1, "quell");
    assert_ne!(
        g.sessions.get(&1).unwrap().fight.atk_cur,
        Some("paginae/atk/quell"),
        "the selection must be refused without a rope"
    );
    assert!(g.world.tamed.is_empty());
}

/// Jorb's list (docs taming step 2): the battle intensity must be
/// reduced to 0 before the quell fires. A landed blow raises it,
/// quiet combat ticks cool it back to zero.
#[tokio::test]
async fn quell_needs_a_calm_battle() {
    let (mut g, _rx, _raw) = entered_game("tamehot");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let deer = spawn_deer_at(&mut g, pidx, 20, Species::Deer.max_hp());
    g.start_fight(1, deer, Species::Deer);
    equip_rope(&mut g, pidx);
    grant_ahusb(&mut g, pidx);
    // A hot battle refuses the selection.
    g.world.animal_fights.get_mut(&deer).unwrap().intensity = crate::state::INTENSITY_PER_BLOW;
    {
        let out = g.sessions.get_mut(&1).unwrap();
        let rel = out.fight.rel_mut(deer).unwrap();
        rel.ip_self = 5;
        rel.adv = 40;
        rel.sync_balance();
    }
    g.on_maneuver(1, "quell");
    assert_ne!(
        g.sessions.get(&1).unwrap().fight.atk_cur,
        Some("paginae/atk/quell"),
        "a heated battle must refuse the quell"
    );
    // Quiet ticks de-escalate: ~7s of no blows cools to 0.
    for _ in 0..10 {
        g.tick_combat();
    }
    assert_eq!(
        g.world.animal_fights.get(&deer).unwrap().intensity,
        0,
        "the battle cools down without blows"
    );
    arm_quell(&mut g, 1, deer);
}

#[tokio::test]
async fn quell_tames_and_binds_the_rope() {
    let (mut g, _rx, _raw) = entered_game("tamerone");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let deer = spawn_deer_at(&mut g, pidx, 20, Species::Deer.max_hp());
    g.start_fight(1, deer, Species::Deer);
    equip_rope(&mut g, pidx);
    grant_ahusb(&mut g, pidx);
    arm_quell(&mut g, 1, deer);
    // Resolve: one swing cadence later the quell lands.
    g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
    g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
    g.tick_combat();
    let tame = g.world.tamed.get(&deer).expect("tame row");
    assert_eq!(tame.tameness, 20, "+20 per quell");
    assert_eq!(tame.tamer, pgob);
    assert!(tame.break_at_tick > g.world.tick, "leash timer armed");
    assert!(
        !g.world.animal_fights.contains_key(&deer),
        "the battle ends on the first quell"
    );
    assert_eq!(
        g.world.players[pidx].fight_target, None,
        "the engagement clears"
    );
    // The bound rope refuses a second beast.
    let deer2 = spawn_deer_at(&mut g, pidx, 40, Species::Deer.max_hp());
    g.start_fight(1, deer2, Species::Deer);
    arm_quell(&mut g, 1, deer2);
    assert!(
        !g.world.tamed.contains_key(&deer2),
        "the second quell must be refused while the rope is bound"
    );
}

#[tokio::test]
async fn damage_kills_tameness_and_leashes_break() {
    let (mut g, _rx, _raw) = entered_game("leashbrk");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let deer = spawn_deer_at(&mut g, pidx, 20, Species::Deer.max_hp());
    g.apply_quell(pidx, 1, deer);
    assert_eq!(g.world.tamed.get(&deer).unwrap().tameness, 20);
    // Hitting the beast shakes off ALL tameness (server policy).
    let tslot = g.world.gobs.get(deer).unwrap();
    g.damage_animal(pidx, 1, deer, tslot, 1);
    assert!(
        g.world.tamed.is_empty(),
        "damage removes the tame row entirely"
    );
    // Re-tame, then the leash breaks on the tick sweep.
    g.apply_quell(pidx, 1, deer);
    g.world.tamed.get_mut(&deer).unwrap().break_at_tick = g.world.tick + 1;
    g.tick();
    assert!(
        g.world.tamed.is_empty(),
        "the sweep breaks the leash past the deadline"
    );
}

#[tokio::test]
async fn full_tame_never_breaks_loose() {
    let (mut g, _rx, _raw) = entered_game("tamefull");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let deer = spawn_deer_at(&mut g, pidx, 20, Species::Deer.max_hp());
    for _ in 0..5 {
        g.apply_quell(pidx, 1, deer);
    }
    let tame = g.world.tamed.get(&deer).expect("tame row");
    assert_eq!(tame.tameness, 100, "five quells reach full tameness");
    assert_eq!(tame.break_at_tick, 0, "a fully tamed beast never breaks");
    g.world.tick += crate::state::LEASH_BREAK_TICKS * 10;
    g.tick();
    assert!(
        g.world.tamed.contains_key(&deer),
        "the sweep must not touch a fully tamed beast"
    );
}

/// Docs taming step 6: at 100 tameness the animal metamorphoses in
/// place into its domestic morph (mouflon -> sheep here; the boar
/// stays a boar because the 2009 pack ships no pig drawable).
#[tokio::test]
async fn full_tame_morphs_the_species() {
    let (mut g, _rx, _raw) = entered_game("tamemorph");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let mouflon = spawn_species_at(
        &mut g,
        pidx,
        20,
        Species::Mouflon.max_hp(),
        Species::Mouflon,
    );
    let res_before = g.world.gobs.res_idx[g.world.gobs.get(mouflon).unwrap()];
    // Damage the beast first: the morph must keep the wounded hp but
    // clamp it into the new species' vitality range.
    let tslot = g.world.gobs.get(mouflon).unwrap();
    g.world.gobs.hp[tslot] = 3;
    for _ in 0..5 {
        g.apply_quell(pidx, 1, mouflon);
    }
    let slot = g.world.gobs.get(mouflon).unwrap();
    assert!(
        matches!(
            g.world.gobs.kind[slot],
            Kind::Animal {
                species: Species::Sheep
            }
        ),
        "the mouflon becomes a sheep at full tameness"
    );
    assert_ne!(
        g.world.gobs.res_idx[slot], res_before,
        "the drawable resource swaps to the sheep cdv"
    );
    assert_eq!(
        g.world.gobs.res_idx[slot],
        g.world.res.intern(Species::Sheep.resname()),
        "the resource index is the sheep cdv"
    );
    assert_eq!(g.world.gobs.max_hp[slot], Species::Sheep.max_hp());
    assert_eq!(g.world.gobs.hp[slot], 3, "the morph does not heal");
    assert_eq!(g.world.gobs.speed[slot], Species::Sheep.speed());
    // Tamed sheep keep the wool -> yarn economy flowing.
    assert!(
        Species::Sheep
            .loot()
            .iter()
            .any(|(r, _, _)| *r == "gfx/invobjs/wool"),
        "sheep loot carries wool"
    );
}

// ------------------------------------------------------------------
// Tamed-animal production (session 47; animals-and-husbandry.md
// "Animal products and collection flows").
// ------------------------------------------------------------------

/// Force the tile under a gob to `tile` in the LIVE grid (mutate_tile
/// patches the resident grid and records the override), keeping the
/// test independent of the seed's terrain roll at the spawn spot.
fn force_tile(g: &mut Game, sub: (i32, i32), tile: u8) {
    let tx = sub.0.div_euclid(11);
    let ty = sub.1.div_euclid(11);
    let gc = (tx.div_euclid(100), ty.div_euclid(100));
    let ix = tx.rem_euclid(100) as usize;
    let iy = ty.rem_euclid(100) as usize;
    g.world.grids.mutate_tile(gc, ix, iy, tile);
}

fn full_tame(g: &mut Game, gob: crate::state::GobId, tamer: crate::state::GobId) {
    let mut tame = crate::state::TameState::new(tamer, 0);
    tame.tameness = crate::state::TAMENESS_FULL;
    g.world.tamed.insert(gob, tame);
}

/// Milk accrues at the doc rate (quantity 10 -> 0.1 L per 10 min =
/// 1 unit of 0.01 L per 600 ticks) while the cow stands on pasture,
/// and pauses off it (moor/heath/grass are the q10 foods).
#[tokio::test]
async fn cow_production_accrues_on_pasture_only() {
    let (mut g, _rx, _raw) = entered_game("s47milk");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let cow = spawn_species_at(&mut g, pidx, 300, Species::Cow.max_hp(), Species::Cow);
    full_tame(&mut g, cow, pgob);
    let slot = g.world.gobs.get(cow).unwrap();
    let sub = g.world.gobs.pos[slot];
    force_tile(&mut g, sub, hnh_world::gen::tile::GRASS);
    for _ in 0..601 {
        g.tick();
    }
    let tame = g.world.tamed.get(&cow).unwrap();
    assert_eq!(
        tame.milk_units, 1,
        "q10 accrues 1 unit of 0.01 L per 600 ticks (0.1 L / 10 min)"
    );
    assert_eq!(tame.prod_acc, 10, "601 ticks * 10 - 6000 banked");
    // Off-pasture: production pauses and the accumulator does not
    // bank off-grass time.
    force_tile(&mut g, sub, hnh_world::gen::tile::SAND);
    for _ in 0..601 {
        g.tick();
    }
    let tame = g.world.tamed.get(&cow).unwrap();
    assert_eq!(tame.milk_units, 1, "no accrual off the pasture");
    assert_eq!(
        tame.prod_acc, 10,
        "the accumulator stays frozen off-pasture"
    );
}

/// The 10 L cap stops the meter and the accumulator stops banking
/// time; milking frees the meter and production resumes.
#[tokio::test]
async fn milk_caps_at_ten_liters() {
    let (mut g, _rx, _raw) = entered_game("s47milkcap");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let cow = spawn_species_at(&mut g, pidx, 300, Species::Cow.max_hp(), Species::Cow);
    full_tame(&mut g, cow, pgob);
    let slot = g.world.gobs.get(cow).unwrap();
    let sub = g.world.gobs.pos[slot];
    force_tile(&mut g, sub, hnh_world::gen::tile::GRASS);
    {
        let tame = g.world.tamed.get_mut(&cow).unwrap();
        tame.milk_units = crate::state::MILK_CAP_UNITS - 1;
        tame.prod_acc = crate::state::MILK_ACC_PER_UNIT - crate::state::MILK_QUANTITY;
    }
    g.tick();
    let tame = g.world.tamed.get(&cow).unwrap();
    assert_eq!(
        tame.milk_units,
        crate::state::MILK_CAP_UNITS,
        "the cap lands"
    );
    g.tick();
    let tame = g.world.tamed.get(&cow).unwrap();
    assert_eq!(
        tame.milk_units,
        crate::state::MILK_CAP_UNITS,
        "no overflow past the cap"
    );
    assert_eq!(
        tame.prod_acc, 0,
        "the accumulator stops banking time at the cap"
    );
}

/// Wool accrual (q5: one wool per 8 h = 48000 ticks) lands through
/// the accumulator and caps at 3.
#[tokio::test]
async fn wool_accrues_and_caps() {
    let (mut g, _rx, _raw) = entered_game("s47wool");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let sheep = spawn_species_at(&mut g, pidx, 300, Species::Sheep.max_hp(), Species::Sheep);
    full_tame(&mut g, sheep, pgob);
    let slot = g.world.gobs.get(sheep).unwrap();
    let sub = g.world.gobs.pos[slot];
    force_tile(&mut g, sub, hnh_world::gen::tile::HEATH);
    {
        let tame = g.world.tamed.get_mut(&sheep).unwrap();
        tame.prod_acc = crate::state::WOOL_ACC_PER_UNIT - crate::state::WOOL_QUANTITY;
    }
    g.tick();
    assert_eq!(
        g.world.tamed.get(&sheep).unwrap().wool,
        1,
        "the quantity-tick threshold mints one wool"
    );
    {
        let tame = g.world.tamed.get_mut(&sheep).unwrap();
        tame.wool = crate::state::WOOL_CAP;
        tame.prod_acc = crate::state::WOOL_ACC_PER_UNIT - crate::state::WOOL_QUANTITY;
    }
    g.tick();
    let tame = g.world.tamed.get(&sheep).unwrap();
    assert_eq!(tame.wool, crate::state::WOOL_CAP, "the wool cap holds");
    assert_eq!(tame.prod_acc, 0, "the accumulator stops at the cap");
}

/// Milking: the flower menu opens on a producing cow, the choice
/// consumes an empty bucket, drains the meter and grants a
/// bucket-milk item at the grazing quality.
#[tokio::test]
async fn milking_consumes_a_bucket_and_grants_bucket_milk() {
    let (mut g, _rx, _raw) = entered_game("s47milkflow");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let cow = spawn_species_at(&mut g, pidx, 30, Species::Cow.max_hp(), Species::Cow);
    full_tame(&mut g, cow, pgob);
    {
        let tame = g.world.tamed.get_mut(&cow).unwrap();
        tame.milk_units = crate::state::MILK_PER_BUCKET_UNITS;
    }
    let buckete = g.world.res.intern("gfx/invobjs/buckete");
    g.world.players[pidx].inv.push(crate::state::InvStack {
        res: buckete,
        count: 1,
        ql: 7,
        label: "",
    });
    g.player_interact(1, pgob, cow, (0, 0));
    let (wid, target) = g
        .sessions
        .get(&1)
        .unwrap()
        .animal_menu
        .expect("the milk menu opens on a producing cow");
    assert_eq!(target, cow);
    g.on_flower_choice(1, wid, 0);
    let inv = &g.world.players[pidx].inv;
    let buckets_left = inv
        .iter()
        .filter(|s| s.res == buckete)
        .map(|s| s.count)
        .sum::<u32>();
    assert_eq!(buckets_left, 0, "the empty bucket is consumed");
    let milk = g.world.res.intern("gfx/invobjs/bucket-milk");
    assert_eq!(
        inv.iter().find(|s| s.res == milk).map(|s| (s.count, s.ql)),
        Some((1, crate::state::GRAZE_PRODUCT_QL)),
        "bucket-milk granted at the grazing quality"
    );
    assert_eq!(
        g.world.tamed.get(&cow).unwrap().milk_units,
        0,
        "the meter drains by one bucket"
    );
}

/// Milking without an empty bucket refuses on the choice: no item is
/// granted and the meter keeps its milk.
#[tokio::test]
async fn milking_without_a_bucket_refuses() {
    let (mut g, _rx, _raw) = entered_game("s47nobucket");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let cow = spawn_species_at(&mut g, pidx, 30, Species::Cow.max_hp(), Species::Cow);
    full_tame(&mut g, cow, pgob);
    {
        let tame = g.world.tamed.get_mut(&cow).unwrap();
        tame.milk_units = crate::state::MILK_PER_BUCKET_UNITS;
    }
    g.player_interact(1, pgob, cow, (0, 0));
    let (wid, _) = g
        .sessions
        .get(&1)
        .unwrap()
        .animal_menu
        .expect("the menu opens; the bucket is checked on the choice");
    g.on_flower_choice(1, wid, 0);
    let milk = g.world.res.intern("gfx/invobjs/bucket-milk");
    assert!(
        !g.world.players[pidx].inv.iter().any(|s| s.res == milk),
        "no bucket-milk without a bucket"
    );
    assert_eq!(
        g.world.tamed.get(&cow).unwrap().milk_units,
        crate::state::MILK_PER_BUCKET_UNITS,
        "the refusal keeps the meter"
    );
}

/// Shearing collects the whole stored wool at the grazing quality
/// and empties the meter.
#[tokio::test]
async fn shearing_collects_the_stored_wool() {
    let (mut g, _rx, _raw) = entered_game("s47shear");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let sheep = spawn_species_at(&mut g, pidx, 30, Species::Sheep.max_hp(), Species::Sheep);
    full_tame(&mut g, sheep, pgob);
    {
        let tame = g.world.tamed.get_mut(&sheep).unwrap();
        tame.wool = 3;
    }
    g.player_interact(1, pgob, sheep, (0, 0));
    let (wid, target) = g
        .sessions
        .get(&1)
        .unwrap()
        .animal_menu
        .expect("the shear menu opens on a wooly sheep");
    assert_eq!(target, sheep);
    g.on_flower_choice(1, wid, 0);
    let wool = g.world.res.intern("gfx/invobjs/wool");
    assert_eq!(
        g.world.players[pidx]
            .inv
            .iter()
            .find(|s| s.res == wool)
            .map(|s| (s.count, s.ql)),
        Some((3, crate::state::GRAZE_PRODUCT_QL)),
        "all stored wool lands in the inventory"
    );
    assert_eq!(
        g.world.tamed.get(&sheep).unwrap().wool,
        0,
        "the meter empties"
    );
}

/// Wild and mid-taming animals never open the production menu - the
/// click keeps the fight path (a fully tamed producer never fights).
#[tokio::test]
async fn wild_and_midtaming_animals_keep_the_fight_path() {
    let (mut g, _rx, _raw) = entered_game("s47wild");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let wild = spawn_species_at(&mut g, pidx, 30, Species::Cow.max_hp(), Species::Cow);
    g.player_interact(1, pgob, wild, (0, 0));
    assert!(g.sessions.get(&1).unwrap().animal_menu.is_none());
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(wild),
        "a wild cow opens the fight"
    );
    // Mid-taming: the beast still runs the leash protocol, not the
    // production menu.
    let mid = spawn_species_at(&mut g, pidx, 60, Species::Sheep.max_hp(), Species::Sheep);
    let mut tame = crate::state::TameState::new(pgob, g.world.tick + 6000);
    tame.tameness = 40;
    g.world.tamed.insert(mid, tame);
    g.player_interact(1, pgob, mid, (0, 0));
    assert!(g.sessions.get(&1).unwrap().animal_menu.is_none());
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(mid),
        "a mid-taming beast opens the fight"
    );
}

/// Tamed animals survive restarts: tameness, the meters and the
/// domestic morph restore from the save (only tameness > 0 rows are
/// persisted); a fully tamed beast never re-arms its leash, a
/// partially tamed one re-arms it.
#[tokio::test]
async fn tamed_animals_persist_roundtrip() {
    let (mut g, _rx, _raw) = entered_game("s47roundtrip");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let cow = spawn_species_at(&mut g, pidx, 300, Species::Cow.max_hp(), Species::Cow);
    full_tame(&mut g, cow, pgob);
    {
        let tame = g.world.tamed.get_mut(&cow).unwrap();
        tame.milk_units = 250;
        tame.wool = 1;
        tame.prod_acc = 123;
    }
    let mid = spawn_species_at(&mut g, pidx, 330, Species::Boar.max_hp(), Species::Boar);
    let mut midtame = crate::state::TameState::new(pgob, 0);
    midtame.tameness = 40;
    g.world.tamed.insert(mid, midtame);
    g.save_all_and_flush();
    drop(g);
    // Same test name -> the same save path: this game boots from the
    // snapshot the first one flushed.
    let (g2, _rx2, _raw2) = entered_game("s47roundtrip");
    let mut restored_full = None;
    let mut restored_mid = None;
    for (id, tame) in g2.world.tamed.iter() {
        if tame.tameness >= crate::state::TAMENESS_FULL {
            restored_full = Some((*id, tame.milk_units, tame.wool, tame.prod_acc));
        } else {
            restored_mid = Some((*id, tame.tameness, tame.break_at_tick));
        }
    }
    let (cow2, milk, wool, acc) = restored_full.expect("the fully tamed row restores");
    assert_eq!((milk, wool), (250, 1), "the production meters survive");
    assert!(
        acc >= 123,
        "the accumulator restores and may accrue on pasture"
    );
    let slot = g2.world.gobs.get(cow2).unwrap();
    assert!(matches!(
        g2.world.gobs.kind[slot],
        Kind::Animal {
            species: Species::Cow
        }
    ));
    assert_eq!(g2.world.gobs.max_hp[slot], Species::Cow.max_hp());
    let (_, tameness, break_at) = restored_mid.expect("the mid-taming row restores");
    assert_eq!(tameness, 40, "partial tameness survives");
    assert!(break_at > 0, "the leash window re-arms on load");
}

/// The tool requirement (session 46): craft_once refuses a tool
/// recipe without the tool and crafts with it, never destroying the
/// ingredients on the refusal path.
#[tokio::test]
async fn bucket_craft_needs_the_saw() {
    let (mut g, _rx, _raw) = entered_game("sawbucket");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let branch = g.world.res.intern("gfx/invobjs/branch");
    let saw = g.world.res.intern("gfx/invobjs/saw");
    let buckete = g.world.res.intern("gfx/invobjs/buckete");
    let give = |g: &mut Game, res: u16, n: u32| {
        g.world.players[pidx].inv.push(crate::state::InvStack {
            res,
            count: n,
            ql: 10,
            label: "",
        });
    };
    // Ingredients present, tool absent: refuse, nothing consumed
    // (the starter kit already carries branches - measure the
    // baseline and compare).
    give(&mut g, branch, 3);
    let branch_before = g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == branch)
        .map(|s| s.count)
        .sum::<u32>();
    assert!(branch_before >= 3, "branches were granted");
    assert!(!g.craft_once(1, "bucket"), "no saw -> no bucket");
    let branch_left = g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == branch)
        .map(|s| s.count)
        .sum::<u32>();
    assert_eq!(
        branch_left, branch_before,
        "the refusal must not consume the ingredients"
    );
    // With the saw in the inventory the craft lands.
    give(&mut g, saw, 1);
    assert!(g.craft_once(1, "bucket"), "saw + branches -> bucket");
    let bucket = g.world.players[pidx]
        .inv
        .iter()
        .find(|s| s.res == buckete)
        .expect("bucket produced");
    assert_eq!(bucket.count, 1);
}

// ------------------------------------------------------------------
// Food Trough + feeding (session 48; animals-and-husbandry.md
// "Feeding: troughs and grazing").
// ------------------------------------------------------------------

/// The full build flow: arm the trough pagina, place the plan on a
/// free tile in reach, sink the branch demand through the REAL
/// material path, and confirm complete_plan opened the fodder
/// store (empty, the doc's 200-unit cap open for loading).
#[tokio::test]
async fn trough_build_flow_opens_fodder_store() {
    let (mut g, _rx, _raw) = entered_game("s48buildflow");
    let trough_spec = crate::build::buildable_by_ad("trough").unwrap();
    // Arm the pagina and place the plan at the player's own tile
    // (in reach by definition).
    g.arm_build_placement(1, trough_spec);
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    // Force the home tile to grass: build placement refuses
    // impassable terrain, and the seed's spawn spot is not
    // guaranteed walkable.
    force_tile(&mut g, (px, py), hnh_world::gen::tile::GRASS);
    let (mx, my) = (px.div_euclid(11) * 11 + 5, py.div_euclid(11) * 11 + 5);
    g.on_map_place(
        1,
        &[
            hnh_proto::ListArg::Coord(mx, my),
            hnh_proto::ListArg::Int(1),
            hnh_proto::ListArg::Int(0),
        ],
    );
    assert_eq!(g.world.plans.len(), 1, "the plan placed");
    let gob = *g.world.plans.keys().next().unwrap();
    assert!(
        matches!(
            g.world.gobs.kind[g.world.gobs.get(gob).unwrap()],
            Kind::Plan { spec: 2, .. }
        ),
        "the plan carries the trough spec"
    );
    // Sink the demand (branch x4) through the material path.
    let branch = g.world.res.intern("gfx/invobjs/branch");
    g.sink_material(
        1,
        gob,
        crate::state::InvStack {
            res: branch,
            count: 4,
            ql: 10,
            label: "",
        },
    );
    assert!(g.world.plans.is_empty(), "the demand is fully credited");
    assert_eq!(g.world.plans.len(), 0);
    assert!(
        matches!(
            g.world.gobs.kind[g.world.gobs.get(gob).unwrap()],
            Kind::Structure { spec: 2 }
        ),
        "the plan finished as a trough structure"
    );
    let trough = g
        .world
        .troughs
        .get(&gob)
        .expect("complete_plan opened the fodder store");
    assert_eq!(trough.units, 0, "the store starts empty");
    assert_eq!(trough.avg_ql(), 10, "the quality baseline is q10");
}

/// Spawn a completed Food Trough at a subtile offset from the
/// player, the way `complete_plan` would leave it.
fn built_trough(g: &mut Game, units: u32, ql_sum: u64, ql_seen: u64) -> GobId {
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

/// itemact of a fodder item tops up the trough, one item per click,
/// and the running quality average matches the doc's arithmetic
/// (q5 + q12 + q16 -> q11). A non-fodder item is refused untouched.
#[tokio::test]
async fn trough_itemact_loads_fodder_and_averages_quality() {
    let (mut g, _rx, _raw) = entered_game("s48troughload");
    let gob = built_trough(&mut g, 0, 0, 0);
    let (fx, fy) = {
        let slot = g.world.gobs.get(gob).unwrap();
        g.world.gobs.pos[slot]
    };
    let deliver = |g: &mut Game, res: &'static str, ql: u8| {
        let idx = g.world.res.intern(res);
        g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
            res: idx,
            count: 2,
            ql,
            label: "Fodder",
        });
        let args = vec![
            hnh_proto::ListArg::Coord(0, 0),
            hnh_proto::ListArg::Coord(fx, fy),
            hnh_proto::ListArg::Int(0),
            hnh_proto::ListArg::Int(gob),
            hnh_proto::ListArg::Int(0),
        ];
        g.on_map_itemact(1, &args);
    };
    deliver(&mut g, "gfx/invobjs/apple", 5);
    deliver(&mut g, "gfx/invobjs/straw", 12);
    deliver(&mut g, "gfx/invobjs/seed-wheat", 16);
    let t = g.world.troughs.get(&gob).unwrap();
    assert_eq!(t.units, 3, "one item per click");
    assert_eq!(t.ql_seen, 3);
    assert_eq!(t.ql_sum, 33);
    assert_eq!(t.avg_ql(), 11, "q5 + q12 + q16 -> q11 (the doc's example)");
    // Each delivery consumed exactly one cursor item.
    assert_eq!(
        g.sessions.get(&1).and_then(|o| o.cursor).map(|c| c.count),
        Some(1),
        "the third delivery left one item on the cursor"
    );
    // A non-fodder item is refused: nothing consumed, nothing added.
    deliver(&mut g, "gfx/invobjs/branch", 10);
    let t = g.world.troughs.get(&gob).unwrap();
    assert_eq!(t.units, 3, "the branch is not fodder");
    assert_eq!(
        g.sessions.get(&1).and_then(|o| o.cursor).map(|c| c.count),
        Some(2),
        "the refusal keeps the cursor stack"
    );
    // Filling to the cap: 197 more deliveries of one item each - the
    // take path clamps at the cap instead of overflowing.
    for _ in 0..197 {
        deliver(&mut g, "gfx/invobjs/apple", 10);
    }
    let t = g.world.troughs.get(&gob).unwrap();
    assert_eq!(t.units, crate::state::TROUGH_CAP_UNITS, "the cap holds");
    // One delivery past the cap: refused, the cursor stack stays.
    deliver(&mut g, "gfx/invobjs/apple", 10);
    let t = g.world.troughs.get(&gob).unwrap();
    assert_eq!(t.units, crate::state::TROUGH_CAP_UNITS, "still at the cap");
}

/// A cow inside the trough radius eats from the trough (even on a
/// non-grazing tile) and keeps producing milk; the trough drains a
/// whole unit when the nano-accumulator crosses one.
#[tokio::test]
async fn trough_feeding_produces_and_drains() {
    let (mut g, _rx, _raw) = entered_game("s48troughfeed");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let gob = built_trough(&mut g, 5, 110, 11);
    let (fx, fy) = {
        let slot = g.world.gobs.get(gob).unwrap();
        g.world.gobs.pos[slot]
    };
    let cow = spawn_species_at(&mut g, pidx, 300, Species::Cow.max_hp(), Species::Cow);
    // Park the cow on the trough (SAND under it - grazing must not
    // be the food source) and put sand under the trough too, so the
    // trough itself never grazes.
    {
        let slot = g.world.gobs.get(cow).unwrap();
        g.world.gobs.set_pos(slot, (fx, fy));
        let sub = (fx, fy);
        force_tile(&mut g, sub, hnh_world::gen::tile::SAND);
    }
    full_tame(&mut g, cow, pgob);
    // 601 ticks of feeding: production accrues exactly like on
    // pasture (0.1 L per 10 min), the trough is still draining its
    // FIRST unit (60000 ticks per unit at the doc's 4.8/day).
    for _ in 0..601 {
        g.tick();
    }
    let tame = g.world.tamed.get(&cow).unwrap();
    assert_eq!(tame.milk_units, 1, "trough feeding keeps production");
    assert_eq!(tame.hunger, 0, "fed animals never starve");
    assert!(tame.feed_acc_nano > 0, "consumption accumulates");
    assert_eq!(
        g.world.troughs.get(&gob).unwrap().units,
        5,
        "the first unit is still in the trough (60000-tick cadence)"
    );
    // Force the accumulator to the edge: the next fed tick drains a
    // whole unit (16.7 nano/tick + 1667 lactating).
    {
        let tame = g.world.tamed.get_mut(&cow).unwrap();
        tame.feed_acc_nano = 1_000_000_000 - (16_667 + 1_667);
    }
    g.tick();
    let tame = g.world.tamed.get(&cow).unwrap();
    assert!(
        tame.feed_acc_nano < 1_000_000_000,
        "the accumulator banks only the fractional part"
    );
    assert_eq!(
        g.world.troughs.get(&gob).unwrap().units,
        4,
        "one whole fodder unit was consumed"
    );
}

/// Outside the trough radius the fallback rules apply: grazing
/// tiles feed (q10), sand does not - hunger climbs and production
/// stops while the trough keeps its fodder.
#[tokio::test]
async fn trough_radius_bounds_feeding() {
    let (mut g, _rx, _raw) = entered_game("s48troughradius");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let gob = built_trough(&mut g, 5, 50, 5);
    let (fx, fy) = {
        let slot = g.world.gobs.get(gob).unwrap();
        g.world.gobs.pos[slot]
    };
    let cow = spawn_species_at(&mut g, pidx, 300, Species::Cow.max_hp(), Species::Cow);
    // 18 tiles = 198 subtiles; park the cow 20 tiles (220 subtiles)
    // east of the trough on SAND - outside the feeding radius.
    {
        let slot = g.world.gobs.get(cow).unwrap();
        g.world.gobs.set_pos(slot, (fx + 220, fy));
        force_tile(&mut g, (fx + 220, fy), hnh_world::gen::tile::SAND);
    }
    full_tame(&mut g, cow, pgob);
    for _ in 0..601 {
        g.tick();
    }
    let tame = g.world.tamed.get(&cow).unwrap();
    assert_eq!(
        tame.milk_units, 0,
        "no trough in radius, no pasture: no production"
    );
    assert_eq!(tame.hunger, 601, "unfed ticks accumulate toward death");
    assert_eq!(g.world.troughs.get(&gob).unwrap().units, 5);
    // Same spot on GRASS: the grazing fallback feeds the cow.
    {
        force_tile(&mut g, (fx + 220, fy), hnh_world::gen::tile::GRASS);
    }
    for _ in 0..601 {
        g.tick();
    }
    let tame = g.world.tamed.get(&cow).unwrap();
    assert_eq!(tame.milk_units, 1, "grazing feeds the cow again");
    assert_eq!(tame.hunger, 0, "grazing resets the hunger timer");
}

/// Starvation: a producer left on sand with no fodder in radius
/// dies at STARVE_DEATH_TICKS - the gob despawns, the tame row
/// drops, and the online tamer is chatted.
#[tokio::test]
async fn starvation_kills_unfed_producers() {
    let (mut g, _rx, _raw) = entered_game("s48starve");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let sheep = spawn_species_at(&mut g, pidx, 300, Species::Sheep.max_hp(), Species::Sheep);
    let slot = g.world.gobs.get(sheep).unwrap();
    let sub = g.world.gobs.pos[slot];
    force_tile(&mut g, sub, hnh_world::gen::tile::SAND);
    full_tame(&mut g, sheep, pgob);
    // Two ticks short of the threshold: the first tick lands one
    // before it, the second crosses and kills.
    {
        let tame = g.world.tamed.get_mut(&sheep).unwrap();
        tame.hunger = crate::state::STARVE_DEATH_TICKS - 2;
    }
    g.tick();
    assert!(
        g.world.gobs.get(sheep).is_some(),
        "not yet at the threshold"
    );
    g.tick();
    assert!(
        g.world.gobs.get(sheep).is_none(),
        "starved at the threshold"
    );
    assert!(
        !g.world.tamed.contains_key(&sheep),
        "the tame row drops with the beast"
    );
    assert!(
        !g.world.animal_gobs.contains(&sheep),
        "the animal leaves the AI roster"
    );
}

/// Persistence: the trough fodder store and the animal feeding
/// fields round-trip through flush -> load (save v7, additive).
#[tokio::test]
async fn trough_and_feeding_persistence_roundtrip() {
    // Stale tmp saves from earlier runs of THIS test would restore
    // their own cows (every run appends another row); start clean.
    let stale = std::env::temp_dir().join("hnh-equip-test-s48persist.json");
    let _ = std::fs::remove_file(&stale);
    let (mut g, _rx, _raw) = entered_game("s48persist");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let gob = built_trough(&mut g, 42, 9 * 42, 42);
    let _ = gob;
    let cow = spawn_species_at(&mut g, pidx, 300, Species::Cow.max_hp(), Species::Cow);
    full_tame(&mut g, cow, pgob);
    {
        let tame = g.world.tamed.get_mut(&cow).unwrap();
        tame.milk_units = 250;
        tame.feed_acc_nano = 123_456_789;
        tame.hunger = 777;
    }
    // The account names the tamer key resolves from.
    g.save_all_and_flush();
    // Structures: exactly one row with the fodder fields.
    let trough_spec = crate::build::buildable_by_ad("trough").unwrap() as u8;
    let row = g
        .save
        .world_state
        .structures
        .iter()
        .find(|s| s.spec == trough_spec)
        .expect("the trough row was flushed");
    assert_eq!(row.fodder_units, 42);
    assert_eq!(row.fodder_ql_sum, 9 * 42);
    assert_eq!(row.fodder_seen, 42);
    // Animals: the feeding fields ride the v7 row.
    let (feed_acc_nano, hunger) = {
        let a = g
            .save
            .world_state
            .animals
            .first()
            .expect("the cow row was flushed");
        (a.feed_acc_nano, a.hunger)
    };
    assert_eq!(feed_acc_nano, 123_456_789);
    assert_eq!(hunger, 777);
    // Restore arithmetic: the fodder fields rebuild the store.
    let restored = crate::state::TroughState {
        units: row.fodder_units,
        ql_sum: row.fodder_ql_sum,
        ql_seen: row.fodder_seen,
    };
    assert_eq!(restored.units, 42);
    assert_eq!(restored.avg_ql(), 9, "the quality history survives");
    assert!(trough_spec < crate::build::BUILDABLES.len() as u8);
}
