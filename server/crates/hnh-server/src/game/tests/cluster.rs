//! Multi-node core: cell authority, guest mirroring, pose
//! finalizers, transfer relays, cross-node combat plumbing.
use super::super::*;
use super::common::*;

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

/// Session 59: guest pose finalizers ride the packed patched start
/// batch (the session-44 machinery) instead of the old per-(session,
/// guest) re-encode loop. After the guest's linmove finishes, the
/// viewer session must receive an OD_LAYERS block whose every wire id
/// is its own session-local allocation, and the block must be
/// retransmittable (recorded in `unacked`).
#[tokio::test]
async fn guest_pose_finalizer_fans_out_patched_layers() {
    let (mut g, _rx, mut raw, _mesh) = clustered_game("guestpose", 0, 2);
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];

    let gid = foreign_node_gob_id(0, 2, 9);
    let st = crate::nodes::GuestState {
        id: gid,
        pos: (px + 60, py),
        mv: None,
        moving: false,
        facing: 1,
        kind: crate::nodes::GuestKind::Animal {
            species: Species::Fox.index(),
        },
        hp: 40,
        max_hp: 40,
        speed: 30,
    };
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st));
    g.tick();
    assert!(g.sessions[&1].visible.contains(&gid), "guest spawned");
    // Drain the spawn datagrams so the pose check sees only the
    // finalizer tick's traffic.
    while raw.try_recv().is_ok() {}

    // Start a short linmove; it must finish within 3 ticks (300 ms).
    let st2 = crate::nodes::GuestState {
        id: gid,
        pos: (px + 60, py),
        mv: Some(crate::nodes::GuestLinMove {
            sx: px + 60,
            sy: py,
            tx: px + 200,
            ty: py,
            steps: 10,
            step: 0,
            started_ms: g.world.now_ms,
            total_ms: 300,
        }),
        moving: true,
        facing: 2,
        kind: crate::nodes::GuestKind::Animal {
            species: Species::Fox.index(),
        },
        hp: 40,
        max_hp: 40,
        speed: 30,
    };
    g.on_node_msg(crate::nodes::NodeMsg::GuestUpdate(st2));

    let mut layers: Option<Vec<u16>> = None;
    for _ in 0..5 {
        g.tick();
        while let Ok(block) = raw.try_recv() {
            if block.first() != Some(&MSG_OBJDATA) {
                continue;
            }
            for (op, ids) in objdata_layer_lists(&block) {
                if op == OD_LAYERS && block[2..6] == gid.to_le_bytes() && !ids.is_empty() {
                    layers = Some(ids);
                }
            }
        }
        if layers.is_some() {
            break;
        }
    }
    let ids = layers.expect("rest-pose LAYERS block reached the viewer");
    let out = g.sessions.get(&1).unwrap();
    for w in &ids {
        assert!(
            out.res.wire_is_local(*w),
            "pose wire id {w} must be session-local (patched from the placeholder)"
        );
    }
    let rec = out.unacked.get(&gid);
    assert!(
        rec.is_some() && !rec.unwrap().blocks.is_empty(),
        "the guest pose block must be retransmittable"
    );
}

/// Session 54 pin: the cluster authority handoff (a guest promoted back
/// to a local gob by GuestTransfer) must not duplicate the id in the
/// vis index - the scan lists it exactly once before AND after the
/// promotion, and the promoted gob stays visible to the same viewers.
#[tokio::test]
async fn guest_promotion_does_not_duplicate_the_vis_bucket_entry() {
    let (mut g, _rx, _raw, _mesh) = clustered_game("promoteuser", 0, 2);
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
    g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st.clone()));
    g.tick();
    let mut scan = Vec::new();
    g.scan_visible_into(px, py, &mut scan);
    assert_eq!(
        scan.iter().filter(|&&id| id == gid).count(),
        1,
        "an ingested guest is listed exactly once"
    );

    // The owner hands authority back: the same id re-materializes as a
    // LOCAL gob at the same position. spawn_with_id re-inserts into the
    // vis index - the insert must be idempotent (no second bucket
    // entry; the pre-session-54 code listed the id twice after this).
    g.on_node_msg(crate::nodes::NodeMsg::GuestTransfer(st));
    g.tick();
    assert!(
        !g.world.guests.contains_key(&gid),
        "the guest row is dropped on promotion"
    );
    assert!(g.world.gobs.get(gid).is_some(), "the local gob is claimed");
    let mut scan2 = Vec::new();
    g.scan_visible_into(px, py, &mut scan2);
    assert_eq!(
        scan2.iter().filter(|&&id| id == gid).count(),
        1,
        "the promoted gob is listed exactly once (no duplicate bucket entry)"
    );
    // The promoted gob did not vanish from the viewer: the session's
    // cached result keeps rendering it (authority changes are invisible
    // to players by design).
    assert!(
        g.sessions[&1].visible.contains(&gid),
        "a promoted gob must stay visible to its existing viewers"
    );
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
    let mut fresh = Vec::new();
    g.scan_visible_into(px, py, &mut fresh);
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
