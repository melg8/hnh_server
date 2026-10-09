//! Crops, harvest relays, planting, plowing, tilth decay.
use super::super::*;
use super::common::*;

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
