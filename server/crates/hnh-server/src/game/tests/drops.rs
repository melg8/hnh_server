//! Ground drops, pickup merging and the static-act relay chain
//! (chop/mine/stump).
use super::super::*;
use super::common::*;

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
    // The starter kit's branch stack (10 since session 58) must stay
    // UNTOUCHED: the ack redirected onto the cursor, not into the
    // inventory.
    let inv_branch: Vec<_> = g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == branch)
        .collect();
    assert_eq!((inv_branch.len(), inv_branch[0].count), (1, 10));
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
    // A fresh branch drop spawned next to the tree (session 60: picking
    // yields BRANCH - the crafting chain's material - not dead wood).
    let mut branch_drops = 0;
    for slot in 0..g.world.gobs.alive.len() {
        if g.world.gobs.alive[slot] {
            if let Kind::Drop { inv_res_idx, .. } = g.world.gobs.kind[slot] {
                if g.world.res.name(inv_res_idx) == Some("gfx/invobjs/branch") {
                    branch_drops += 1;
                }
            }
        }
    }
    assert_eq!(branch_drops, 1, "the pick spawned one branch drop");
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

/// World gathering (session 60): each boulder pick takes ONE stone off
/// the BOULDER_STONES supply; the depleted boulder disappears. Every
/// pick (including the last) acks the stone LP.
#[tokio::test]
async fn relay_mine_boulder_yields_one_stone_per_pick() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("authmine", 0, 2);
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let stone_res = g.world.res.intern("gfx/terobjs/bumlings/01");
    let boulder = g.world.gobs.spawn(
        Kind::Boulder {
            left: crate::state::BOULDER_STONES,
        },
        (px + 22, py),
        stone_res,
        1,
        0,
    );
    let clicker = foreign_node_gob_id(0, 2, 71);
    for _ in 0..crate::state::BOULDER_STONES {
        g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
            player: clicker,
            target: boulder,
            act: crate::nodes::StaticAct::Mine,
        });
    }
    assert!(
        g.world.gobs.get(boulder).is_none(),
        "depleted boulder removed"
    );
    // One stone drop per pick, all carrying the stone inventory resource.
    let mut stone_drops = 0;
    for slot in 0..g.world.gobs.alive.len() {
        if g.world.gobs.alive[slot] {
            if let Kind::Drop { inv_res_idx, .. } = g.world.gobs.kind[slot] {
                if g.world.res.name(inv_res_idx) == Some("gfx/invobjs/stone") {
                    stone_drops += 1;
                }
            }
        }
    }
    assert_eq!(stone_drops, 5, "one stone drop per pick at BOULDER_STONES");
    let mut acks_lp = Vec::new();
    while let Ok((_peer, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::StaticAck { lp, .. } = msg {
            acks_lp.push(lp);
        }
    }
    assert_eq!(acks_lp, vec![3; 5], "stone LP acks every pick");
}

/// Picking a stump yields nothing: the stump survives, no drop spawns,
/// no LP is granted (local click path; docs "World gathering").
#[tokio::test]
async fn stump_pick_yields_nothing() {
    let (mut g, _rx, _raw, mesh_rx) = clustered_game("stump", 0, 2);
    let pgob = pgob_of(&g);
    let pslot = g.world.gobs.get(pgob).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let pidx = *g.world.by_session.get(&1).unwrap();
    let lp_before = g.world.players[pidx].lp;
    let log_res = g.world.res.intern("gfx/terobjs/trees/log");
    let stump = g
        .world
        .gobs
        .spawn(Kind::Stump, (px + 11, py), log_res, 1, 0);
    g.player_interact(1, pgob, stump, (px + 11, py));
    assert!(
        g.world.gobs.get(stump).is_some(),
        "the stump survives the pick"
    );
    let mut drops = 0;
    for slot in 0..g.world.gobs.alive.len() {
        if g.world.gobs.alive[slot] && matches!(g.world.gobs.kind[slot], Kind::Drop { .. }) {
            drops += 1;
        }
    }
    assert_eq!(drops, 0, "a stump yields no drops");
    assert_eq!(g.world.players[pidx].lp, lp_before, "no LP for a stump");
    let _ = mesh_rx;
}

/// The exhausted tree's stump renders for cross-node guests too: the
/// relay chop that exhausts a tree spawns a Stump kind whose guest view
/// classifies under Structure (no relay act), unlike the old
/// Stone-mineable classification (session 60).
#[tokio::test]
async fn relay_chop_exhaustion_leaves_a_structure_class_stump() {
    let (mut g, _rx, _raw, _mesh_rx) = clustered_game("stumpview", 0, 2);
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    let tree_res = g.world.res.intern("gfx/terobjs/trees/old");
    let tree = g
        .world
        .gobs
        .spawn(Kind::Tree { harvests: 1 }, (px + 22, py), tree_res, 1, 0);
    let clicker = foreign_node_gob_id(0, 2, 81);
    // Two picks: the first takes the last harvest, the second finds the
    // tree exhausted and leaves the stump.
    g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
        player: clicker,
        target: tree,
        act: crate::nodes::StaticAct::Chop,
    });
    assert!(
        g.world.gobs.get(tree).is_some(),
        "the last-harvest tree still stands"
    );
    g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
        player: clicker,
        target: tree,
        act: crate::nodes::StaticAct::Chop,
    });
    assert!(g.world.gobs.get(tree).is_none(), "tree removed");
    // The stump gob exists at the tree's spot and classifies as
    // Structure in the guest view mapping.
    let stump = (0..g.world.gobs.alive.len())
        .find(|&slot| {
            g.world.gobs.alive[slot]
                && matches!(g.world.gobs.kind[slot], Kind::Stump)
                && g.world.gobs.pos[slot] == (px + 22, py)
        })
        .map(|slot| crate::state::gob_id_from_slot(slot, g.world.gobs.gen[slot]));
    let stump = stump.expect("stump spawned at the tree spot");
    let slot = g.world.gobs.get(stump).unwrap();
    let state = g
        .guest_state_from_slot(stump, slot)
        .expect("stump guest state");
    assert_eq!(
        state.kind,
        crate::nodes::GuestKind::Static {
            res_name: "gfx/terobjs/trees/log".to_string(),
            class: crate::nodes::StaticClass::Structure,
            crop: None,
            station: None,
            stage: None,
            drop: None,
        },
        "stump guests carry no relay act"
    );
}

// ------------------------------------------------------------------
// Session 30: vis-scan result caching (patch + clean paths)
// ------------------------------------------------------------------
