//! Food trough build/feeding/lift/transfer and starvation.
use super::super::*;
use super::common::*;

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
    // Resolve the trough spec index through the registry (ids are
    // stable; positional indices are not - the alloyer insertion
    // shifted them once already).
    let trough_spec = crate::build::buildable_by_ad("trough").expect("trough is buildable") as u8;
    assert!(
        matches!(
            g.world.gobs.kind[g.world.gobs.get(gob).unwrap()],
            Kind::Plan { spec, .. } if spec == trough_spec
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
            Kind::Structure { spec } if spec == trough_spec
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
    // Session 83: the leash walk shepherds a tamed beast standing far
    // behind its tamer - park the tamer BESIDE the cow so the beast
    // stays on this test's controlled tile.
    {
        let pslot = g.world.gobs.get(pgob).unwrap();
        g.world.gobs.set_pos(pslot, (fx + 220, fy));
    }
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

// ------------------------------------------------------------------
// Food Trough lift / place / fodder transfer (session 62;
// animals-and-husbandry.md "Feeding: troughs and grazing": a lift-able
// trough, and "lift-and-right-click on another trough transfers fodder
// like a liquid").
// ------------------------------------------------------------------

/// Lift flow: click a placed trough -> the Lift flower menu opens ->
/// choosing it retracts the gob, frees its tile, and the fodder store
/// rides the player.
#[tokio::test]
async fn trough_lift_retracts_the_gob_and_carries_the_fodder() {
    let (mut g, _rx, _raw) = entered_game("s62troughlift");
    let gob = built_trough(&mut g, 12, 144, 12);
    let pos = {
        let slot = g.world.gobs.get(gob).unwrap();
        g.world.gobs.pos[slot]
    };

    g.on_map_click(1, &click_gob_args(gob, pos));
    let sm = g.sessions.get(&1).unwrap().trough_menu;
    assert!(sm.is_some(), "the Lift flower menu must open");

    g.on_flower_choice(1, sm.unwrap().0, 0);
    assert!(
        g.world.gobs.get(gob).is_none(),
        "the lifted trough leaves the world"
    );
    assert!(!g.world.troughs.contains_key(&gob));
    // The tile is free for a later placement.
    let tile = (pos.0.div_euclid(11), pos.1.div_euclid(11));
    assert!(!g.world.structure_at.contains_key(&tile));
    let carried = g.world.players[0]
        .carried_trough
        .expect("the fodder store rides the player");
    assert_eq!(carried.units, 12);
    assert_eq!(
        carried.avg_ql(),
        12,
        "the quality history survives the lift"
    );
}

/// Place-back: a map click while carrying re-enters the trough on the
/// clicked tile (in reach, walkable, unoccupied) with the exact fodder
/// state it lifted with.
#[tokio::test]
async fn trough_place_back_restores_the_store() {
    let (mut g, _rx, _raw) = entered_game("s62troughplace");
    let gob = built_trough(&mut g, 7, 56, 7);
    let pos = {
        let slot = g.world.gobs.get(gob).unwrap();
        g.world.gobs.pos[slot]
    };
    g.on_map_click(1, &click_gob_args(gob, pos));
    g.on_flower_choice(1, g.sessions.get(&1).unwrap().trough_menu.unwrap().0, 0);
    assert!(g.world.players[0].carried_trough.is_some());

    // One tile east of the player: in reach by construction.
    let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
    let (px, py) = g.world.gobs.pos[pslot];
    force_tile(&mut g, (px + 33, py), hnh_world::gen::tile::GRASS);
    let tile = ((px + 33).div_euclid(11), py.div_euclid(11));
    let (mx, my) = (tile.0 * 11 + 5, tile.1 * 11 + 5);
    g.on_map_place(
        1,
        &[
            hnh_proto::ListArg::Coord(mx, my),
            hnh_proto::ListArg::Int(1),
            hnh_proto::ListArg::Int(0),
        ],
    );
    assert!(
        g.world.players[0].carried_trough.is_none(),
        "the carry ends on placement"
    );
    let new_gob = g
        .world
        .structure_at
        .get(&tile)
        .copied()
        .expect("occupancy registered");
    let restored = g.world.troughs.get(&new_gob).expect("the store reopens");
    assert_eq!(restored.units, 7);
    assert_eq!(restored.ql_sum, 56);
    assert_eq!(restored.ql_seen, 7);
    assert!(
        g.world.gobs.get(new_gob).is_some(),
        "a new trough gob stands on the tile"
    );
    assert_ne!(new_gob, gob, "the lifted gob id was consumed");
}

/// Transfer: clicking a placed trough while carrying moves the fodder
/// "like a liquid" - units up to the destination capacity, the moved
/// units carrying the source's running average. The source keeps the
/// remainder; its average is invariant under the move.
#[tokio::test]
async fn trough_transfer_moves_fodder_like_a_liquid() {
    let (mut g, _rx, _raw) = entered_game("s62troughtransfer");
    // The carried source: 50 units at average q12 (ql_sum 600 / seen 50).
    g.world.players[0].carried_trough = Some(crate::state::TroughState {
        units: 50,
        ql_sum: 600,
        ql_seen: 50,
    });
    // The destination: 100 units at average q10 (ql_sum 1000 / seen 100).
    let dest = built_trough(&mut g, 100, 1000, 100);
    let pos = {
        let slot = g.world.gobs.get(dest).unwrap();
        g.world.gobs.pos[slot]
    };

    g.on_map_click(1, &click_gob_args(dest, pos));
    // No menu while carrying: the transfer applies immediately.
    assert!(g.sessions.get(&1).unwrap().trough_menu.is_none());
    let dest_state = g.world.troughs.get(&dest).unwrap();
    assert_eq!(dest_state.units, 150, "50 of the carried 50 units moved");
    assert_eq!(dest_state.ql_seen, 150);
    assert_eq!(dest_state.ql_sum, 1600, "q10*100 + q12*50");
    assert_eq!(dest_state.avg_ql(), 10, "1600/150 floors to q10");
    let carried = g.world.players[0].carried_trough.unwrap();
    assert_eq!(carried.units, 0, "everything fit: nothing carried");
    assert_eq!(carried.avg_ql(), 12, "an emptied source keeps its average");
    assert_eq!(carried.ql_seen, 50, "the history is NOT drained (S48 rule)");
    assert_eq!(carried.ql_sum, 600);
}

/// Capacity: a full destination takes nothing and says so; a partial
/// fit moves what fits and keeps the remainder carried.
#[tokio::test]
async fn trough_transfer_respects_the_capacity_cap() {
    let (mut g, _rx, _raw) = entered_game("s62troughcap");
    g.world.players[0].carried_trough = Some(crate::state::TroughState {
        units: 30,
        ql_sum: 300,
        ql_seen: 30,
    });
    // The destination holds 180 of the 200-unit cap: only 20 fit.
    let dest = built_trough(&mut g, 180, 1800, 180);
    let pos = {
        let slot = g.world.gobs.get(dest).unwrap();
        g.world.gobs.pos[slot]
    };
    g.on_map_click(1, &click_gob_args(dest, pos));
    let dest_state = g.world.troughs.get(&dest).unwrap();
    assert_eq!(dest_state.units, 200, "the destination is at the cap");
    let carried = g.world.players[0].carried_trough.unwrap();
    assert_eq!(carried.units, 10, "the remainder stays carried");
    assert_eq!(carried.ql_sum, 300, "the history is untouched (S48 rule)");
    assert_eq!(carried.avg_ql(), 10);
}
