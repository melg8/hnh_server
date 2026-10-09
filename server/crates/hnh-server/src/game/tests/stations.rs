//! Building plans, stations (oven/kiln/smelter/quern),
//! subscriptions and the bake contracts.
use super::super::*;
use super::common::*;

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
        text.contains("station needs fuel"),
        "NeedsFuel renders: {text}"
    );
    g.on_node_msg(crate::nodes::NodeMsg::StationAck {
        player: pgob,
        result: crate::nodes::StationResult::Lit,
    });
    let text = drain(&mut rx);
    assert!(
        !text.contains("station"),
        "a successful Light stays silent (parity with the local path)"
    );
}

// ------------------------------------------------------------------
// Session 36: the bow chain (woodbow / stonearrow / bonearrow)
// ------------------------------------------------------------------

/// The full oven bake contract on the REAL itemact -> menu -> tick
/// paths:
/// 1. the input gate matches the DISPLAY LABEL, not the resource - a
///    label-less dough stack (the S71 live finding: crafted stacks
///    shipped empty labels) is refused and never consumed;
/// 2. the labeled hand craft ("Bread Dough" = recipe.name) is
///    accepted and exactly ONE unit leaves the cursor;
/// 3. Light refuses while the fuel store is empty;
/// 4. one branch fills the fuel store (FUEL_PER_JOB accounting);
/// 5. the lit job consumes fuel + input after job_ticks and drops
///    the BAKE_MAP output ("Bread") beside the station.
#[tokio::test]
async fn oven_bake_contract_label_gate_fuel_and_output() {
    let (mut g, mut rx, _raw) = entered_game("bakecontract");
    let oven_idx = station_spec_idx("oven");
    let oven = built_oven(&mut g, 0, None);
    let job_ticks = crate::build::BUILDABLES[oven_idx]
        .station
        .as_ref()
        .unwrap()
        .job_ticks;
    let dough_res = g.world.res.intern("gfx/invobjs/dough");

    // (1) The label gate: the resource alone is not enough.
    click_station_with_cursor(
        &mut g,
        oven,
        InvStack {
            res: dough_res,
            count: 2,
            ql: 10,
            label: "",
        },
    );
    assert!(
        g.world.stations[&oven].input.is_none(),
        "a label-less dough stack must be refused (BAKE_MAP keys are display names)"
    );
    assert_eq!(
        g.sessions.get(&1).unwrap().cursor.as_ref().map(|c| c.count),
        Some(2),
        "the refused stack stays on the cursor untouched"
    );
    let lines = drain_chat(&mut rx);
    assert!(
        lines.iter().any(|l| l.contains("cannot process that.")),
        "the refusal must be announced in chat, got {lines:?}"
    );

    // (2) The labeled hand craft is accepted; one unit moves.
    click_station_with_cursor(
        &mut g,
        oven,
        InvStack {
            res: dough_res,
            count: 2,
            ql: 10,
            label: "Bread Dough",
        },
    );
    let st = &g.world.stations[&oven];
    assert_eq!(
        st.input.as_ref().map(|(_, _, l)| *l),
        Some("Bread Dough"),
        "the labeled dough enters the input slot"
    );
    assert_eq!(
        g.sessions.get(&1).unwrap().cursor.as_ref().map(|c| c.count),
        Some(1),
        "the station takes exactly one unit per itemact"
    );

    // (3) Light refuses without fuel.
    g.open_station_menu(1, oven);
    let (menu_wid, _, _) = g
        .sessions
        .get(&1)
        .unwrap()
        .station_menu
        .expect("the station click opens the flower menu");
    g.apply_station_choice(1, menu_wid, 0);
    assert!(
        !g.world.stations[&oven].lit,
        "an unfueled station must refuse to light"
    );
    let lines = drain_chat(&mut rx);
    assert!(
        lines.iter().any(|l| l.contains("needs fuel")),
        "the fuel refusal must be announced, got {lines:?}"
    );

    // (4) One branch fills the fuel store (FUEL_PER_JOB = 1).
    let branch = g.world.res.intern("gfx/invobjs/branch");
    click_station_with_cursor(
        &mut g,
        oven,
        InvStack {
            res: branch,
            count: 1,
            ql: 10,
            label: "",
        },
    );
    assert_eq!(g.world.stations[&oven].fuel, 1);
    assert!(
        g.sessions.get(&1).unwrap().cursor.is_none(),
        "the single-unit fuel delivery empties the cursor"
    );

    // (5) Light starts the job; job_ticks later the mapped output
    // drops beside the station with the BAKE_MAP label.
    g.open_station_menu(1, oven);
    let (menu_wid, _, _) = g
        .sessions
        .get(&1)
        .unwrap()
        .station_menu
        .expect("the second station click opens the flower menu");
    g.apply_station_choice(1, menu_wid, 0);
    assert!(
        g.world.stations[&oven].lit,
        "fuel + input + Light must start the job"
    );
    for _ in 0..job_ticks {
        g.tick();
    }
    let st = &g.world.stations[&oven];
    assert!(!st.lit, "the job auto-extinguishes on completion");
    assert!(st.input.is_none(), "the job consumes the input");
    assert_eq!(st.fuel, 0, "the job burns FUEL_PER_JOB fuel");
    let bread = g.world.res.intern("gfx/invobjs/bread");
    let mut found_bread = false;
    for kind in g
        .world
        .gobs
        .kind
        .iter()
        .zip(g.world.gobs.alive.iter())
        .filter(|(_, a)| **a)
        .map(|(k, _)| k)
    {
        if let Kind::Drop {
            inv_res_idx, label, ..
        } = kind
        {
            if *inv_res_idx == bread && *label == "Bread" {
                found_bread = true;
            }
        }
    }
    assert!(
        found_bread,
        "the finished job must drop Bread beside the station"
    );
}

/// The quern skips the fuel gate (session 71: it turns by hand) - a
/// zero-fuel station still lights and grinds the GRIND_MAP input into
/// flour after job_ticks, and its menu verb is "Grind", not "Light".
#[tokio::test]
async fn quern_grinds_without_the_fuel_gate() {
    let (mut g, mut rx, _raw) = entered_game("quernnofuel");
    let quern_idx = station_spec_idx("quern");
    let quern = built_station(&mut g, quern_idx);
    let job_ticks = crate::build::BUILDABLES[quern_idx]
        .station
        .as_ref()
        .unwrap()
        .job_ticks;

    // The menu verb for an unlit hand-cranked station is "Grind".
    g.open_station_menu(1, quern);
    let (menu_wid, _, _) = g
        .sessions
        .get(&1)
        .unwrap()
        .station_menu
        .expect("the quern click opens the flower menu");
    // (The sm widget's verb text is asserted on the wire tier; the
    // state contract here is the refusal order.)
    g.apply_station_choice(1, menu_wid, 0);
    assert!(
        !g.world.stations[&quern].lit,
        "no input yet - the grind must refuse"
    );
    let lines = drain_chat(&mut rx);
    assert!(
        lines.iter().any(|l| l.contains("needs an input")),
        "the input refusal must come BEFORE the fuel gate, got {lines:?}"
    );

    // Load the grist; the zero-fuel station lights anyway.
    let grist_res = g.world.res.intern("gfx/invobjs/grist-wheat");
    click_station_with_cursor(
        &mut g,
        quern,
        InvStack {
            res: grist_res,
            count: 1,
            ql: 10,
            label: "Grist of Wheat",
        },
    );
    assert_eq!(
        g.world.stations[&quern].input.as_ref().map(|(_, _, l)| *l),
        Some("Grist of Wheat")
    );
    g.open_station_menu(1, quern);
    let (menu_wid, _, _) = g
        .sessions
        .get(&1)
        .unwrap()
        .station_menu
        .expect("the loaded quern opens the flower menu");
    g.apply_station_choice(1, menu_wid, 0);
    let st = &g.world.stations[&quern];
    assert!(st.lit, "the hand-cranked quern must light with zero fuel");
    assert_eq!(st.fuel, 0, "the quern never burns fuel");
    for _ in 0..job_ticks {
        g.tick();
    }
    let flour = g.world.res.intern("gfx/invobjs/flour");
    let mut found_flour = false;
    for kind in g
        .world
        .gobs
        .kind
        .iter()
        .zip(g.world.gobs.alive.iter())
        .filter(|(_, a)| **a)
        .map(|(k, _)| k)
    {
        if let Kind::Drop {
            inv_res_idx, label, ..
        } = kind
        {
            if *inv_res_idx == flour && *label == "Flour" {
                found_flour = true;
            }
        }
    }
    assert!(found_flour, "the grind must drop Flour beside the quern");
}
