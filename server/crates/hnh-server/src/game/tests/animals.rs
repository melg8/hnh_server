//! Predator engagement, taming (quell), production (milk/wool)
//! and the tameness lifecycle.
use super::super::*;
use super::common::*;

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
        // Session 83: a swing at an ANIMAL needs a selected attack (the
        // Fightview queue rule). The wolf opens the duel itself (Chase
        // from reach); arm the free Punch once the duel lives, and
        // re-arm after any teardown the pack swap causes.
        if g.world.players[0].fight_target.is_some()
            && g.sessions.get(&1).unwrap().fight.atk_cur.is_none()
        {
            g.on_maneuver(1, "pow");
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
    // The bound rope refuses a second beast. NOTE (session 83): the
    // refusal must NOT leave a selection behind - arm_quell's helper
    // asserts acceptance, so drive this one by hand and require BOTH
    // the empty tame row AND the empty attack slot (the stale-atk_cur
    // leak used to satisfy the old assert vacuously after the first
    // fight's window closed).
    let deer2 = spawn_deer_at(&mut g, pidx, 40, Species::Deer.max_hp());
    g.start_fight(1, deer2, Species::Deer);
    {
        let out = g.sessions.get_mut(&1).unwrap();
        let rel = out.fight.rel_mut(deer2).unwrap();
        rel.ip_self = 5;
        rel.adv = 40;
        rel.sync_balance();
    }
    g.on_maneuver(1, "quell");
    assert_eq!(
        g.sessions.get(&1).unwrap().fight.atk_cur,
        None,
        "a refused quell never selects"
    );
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
    // Re-tame, then the leash breaks on the tick sweep: the ROW survives
    // with the tameness BANKED (session 83 - docs: "a tameness-decay
    // timer that re-enables the animal's hostile state unless quelled
    // again"), the beast goes wild-again (loose) instead of forgetting
    // its progress.
    g.apply_quell(pidx, 1, deer);
    assert_eq!(g.world.tamed.get(&deer).unwrap().tameness, 20);
    g.world.tamed.get_mut(&deer).unwrap().break_at_tick = g.world.tick + 1;
    g.tick();
    let row = g
        .world
        .tamed
        .get(&deer)
        .expect("the row survives the break");
    assert!(row.loose, "the sweep flips the row to loose");
    assert_eq!(row.tameness, 20, "the tameness stays banked");
    // The banked progress stacks: the NEXT quell re-leashes the row and
    // climbs toward the full 100 (the five-cycle protocol).
    g.apply_quell(pidx, 1, deer);
    let row = g.world.tamed.get(&deer).expect("the row re-leashes");
    assert_eq!(
        row.tameness, 40,
        "the second quell stacks on the banked row"
    );
    assert!(!row.loose, "apply_quell re-leashes the beast");
    assert!(row.break_at_tick > g.world.tick, "the break timer re-arms");
}

#[tokio::test]
async fn loose_beast_runs_wild_ai_and_a_fresh_fight_needs_a_fresh_quell() {
    let (mut g, _rx, _raw) = entered_game("loosewild");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).expect("player gob");
    let (px, _py) = g.world.gobs.pos[pslot];
    let deer = spawn_deer_at(&mut g, pidx, 20, Species::Deer.max_hp());
    g.apply_quell(pidx, 1, deer);
    g.world.tamed.get_mut(&deer).unwrap().loose = true;
    // A loose beast is wild again: the AI filter must include it (only
    // on-leash rows skip AI) - it panics away from the nearby player.
    let dslot = g.world.gobs.get(deer).expect("deer gob");
    g.world.gobs.mv[dslot] = None;
    g.tick_animals();
    let hop = g.world.gobs.mv[dslot].expect("the loose beast runs the wild AI");
    assert!(
        (hop.tx - (px + 20)) > 0,
        "the loose beast panics AWAY from the player (tx={})",
        hop.tx - (px + 20)
    );
    // The quell gate passes on a loose beast (banked progress, wild
    // again): the docs' "quell again" step.
    equip_rope(&mut g, pidx);
    grant_ahusb(&mut g, pidx);
    let why = g.quell_gate(pidx, deer);
    assert!(why.is_none(), "a loose beast is quellable: {:?}", why);
    // The rope is NOT bound by a loose row: a second beast is quellable
    // in parallel (docs step 4: "until it is tamed or breaks loose").
    let deer2 = spawn_deer_at(&mut g, pidx, 40, Species::Deer.max_hp());
    let why2 = g.quell_gate(pidx, deer2);
    assert!(why2.is_none(), "a loose row frees the rope: {:?}", why2);
    // The stale-selection leak (session 83 live trace): atk_cur must NOT
    // survive the fight window closing. A quell selected in fight one
    // resolved 0.9 s into fight two with advantage 0 - the swing tick
    // trusted a selection whose gates ran in the PREVIOUS fight.
    g.start_fight(1, deer2, Species::Deer);
    arm_quell(&mut g, 1, deer2);
    g.fight_del(1, deer2);
    assert_eq!(
        g.sessions.get(&1).unwrap().fight.atk_cur,
        None,
        "the selected attack dies with the fight window"
    );
    // And the loose row keeps pointing at the tamer for the next quell.
    assert_eq!(g.world.tamed.get(&deer).unwrap().tamer, pgob);
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
    // Session 83: the leash walk shepherds a tamed beast standing far
    // behind its tamer - park the tamer BESIDE the cow so the beast
    // stays on this test's controlled tile.
    {
        let pslot = g.world.gobs.get(pgob).unwrap();
        g.world.gobs.set_pos(pslot, sub);
    }
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
    // production menu. Session 83: melee engagement opens only from
    // swing reach (33), so the click lands from dist 30. One Fightview
    // per player: the click is refused while the wild duel lives, so
    // the test first walks the DISENGAGE path (fight row gone, target
    // reset) before re-clicking.
    let mid = spawn_species_at(&mut g, pidx, 30, Species::Sheep.max_hp(), Species::Sheep);
    let mut tame = crate::state::TameState::new(pgob, g.world.tick + 6000);
    tame.tameness = 40;
    g.world.tamed.insert(mid, tame);
    g.player_interact(1, pgob, mid, (0, 0));
    assert!(g.sessions.get(&1).unwrap().animal_menu.is_none());
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(wild),
        "one Fightview: the click cannot steal the live wild duel"
    );
    // DISENGAGE-equivalent teardown, then the mid-taming beast DOES
    // engage (still the fight path, never the production menu).
    g.world.players[pidx].fight_target = None;
    g.world.animal_fights.remove(&wild);
    g.player_interact(1, pgob, mid, (0, 0));
    assert!(g.sessions.get(&1).unwrap().animal_menu.is_none());
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(mid),
        "a mid-taming beast opens the fight"
    );
    // Session 83: a click from BEYOND swing reach is refused instead of
    // opening a fight the next DISENGAGE tick would tear down (the
    // client streams the beast's older position while it bolts). The
    // live engagement stays untouched.
    let far = spawn_species_at(&mut g, pidx, 120, Species::Cow.max_hp(), Species::Cow);
    g.player_interact(1, pgob, far, (0, 0));
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(mid),
        "an out-of-reach beast never engages (the live fight stays)"
    );
    assert!(
        !g.world.animal_fights.contains_key(&far),
        "no fight row for the out-of-reach beast"
    );
}

/// Session 83: the panic hop must run straight AWAY from the player.
/// The old random ±550 scatter regularly bounded THROUGH the player
/// and starved the quell swing cadence of its 33-subtile reach - the
/// live dairy probe never landed a quell. A beast already IN a fight
/// additionally keeps its hops small enough that the swing reach
/// re-closes within a tick or two of the tamer's chase.
#[tokio::test]
async fn panic_hops_away_and_fighting_beasts_stay_reachable() {
    let (mut g, _rx, _raw) = entered_game("s83panic");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).expect("player gob");
    let (px, py) = g.world.gobs.pos[pslot];

    // A grazing cow 100 units east of the player bounds further away.
    let cow = spawn_species_at(&mut g, pidx, 100, Species::Cow.max_hp(), Species::Cow);
    g.tick_animals();
    let cslot = g.world.gobs.get(cow).expect("cow gob");
    let hop = g.world.gobs.mv[cslot].expect("the grazing cow hopped");
    assert!(
        (hop.tx - px).abs() > 150,
        "the panic hop must out-distance the old position (tx={})",
        hop.tx - px
    );
    assert!(
        (hop.tx - (px + 100)) > 0,
        "the hop must move away from the player, not toward it (tx={})",
        hop.tx - px
    );
    assert!(
        (hop.ty - py).abs() < 80,
        "the cross-axis scatter stays bounded (ty={})",
        hop.ty - py
    );

    // A fighting beast hops small: the whole step stays inside a
    // disengage-scale neighborhood so the 33-subtile swing reach
    // re-closes on the next chase tick.
    let aurochs = spawn_species_at(&mut g, pidx, 40, Species::Cow.max_hp(), Species::Aurochs);
    g.start_fight(1, aurochs, Species::Aurochs);
    g.tick_animals();
    let aslot = g.world.gobs.get(aurochs).expect("aurochs gob");
    let hop = g.world.gobs.mv[aslot].expect("the fighting beast hopped");
    let step = ((hop.tx - (px + 40)).abs(), (hop.ty - py).abs());
    assert!(
        step.0.max(step.1) < 40,
        "a fighting beast's hop stays small (step=({},{}))",
        step.0,
        step.1
    );
    assert!(
        (hop.tx - px).abs() > 40,
        "the small hop still moves away from the player (tx={})",
        hop.tx - px
    );
}

/// Session 83: the tamed beast follows its tamer SERVER-side (docs
/// step 4/6: "the animal follows the tamer (leashed)"), not just in
/// the client's OD_FOLLOW render - grazing and production read the
/// server tile under the beast. A beast stranded far behind walks
/// toward the tamer; a beast already at the heel stays put.
#[tokio::test]
async fn tamed_beast_follows_the_tamer() {
    let (mut g, _rx, _raw) = entered_game("s83follow");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).expect("player gob");
    let (px, _py) = g.world.gobs.pos[pslot];

    // Stranded 100 east: walks back toward the tamer's position.
    let far = spawn_species_at(&mut g, pidx, 100, Species::Cow.max_hp(), Species::Cow);
    let mut tame = crate::state::TameState::new(pgob, 0);
    tame.tameness = 40;
    g.world.tamed.insert(far, tame);
    // Already at the heel (20 < LEASH_FOLLOW_DIST): stays put.
    let near = spawn_species_at(&mut g, pidx, 20, Species::Cow.max_hp(), Species::Cow);
    g.world
        .tamed
        .insert(near, crate::state::TameState::new(pgob, 0));

    g.tick();
    let fslot = g.world.gobs.get(far).expect("far cow gob");
    let walk = g.world.gobs.mv[fslot].expect("the stranded cow walks");
    assert!(
        (walk.tx - px).abs() < 100,
        "the follow walk heads toward the tamer (tx-px={})",
        walk.tx - px
    );
    let nslot = g.world.gobs.get(near).expect("near cow gob");
    assert!(
        g.world.gobs.mv[nslot].is_none(),
        "a cow at the heel does not thrash start_move"
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

/// Session 83: the aggro leash. A boar that forces a duel on a player
/// used to shadow them FOREVER - its 55 subt/s beats the 50 run gait,
/// so the DISENGAGE axis never grows past 300 while the beast itself
/// holds the gap, and the one-Fightview rule kept the forced duel (and
/// the stolen fight window) alive for whole minutes of the live dairy
/// probe. The leash: an aggressor past AGGRO_GIVEUP from its home
/// anchor surrenders server-side - the duel is torn down (the player is
/// RELEASED), and the beast walks home without re-aggroing (the
/// surrender hysteresis), until it arrives back in its home circle.
#[tokio::test]
async fn aggro_leash_surrenders_the_chase_and_releases_the_duel() {
    let (mut g, _rx, _raw) = entered_game("s83leash");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let boar = spawn_species_at(&mut g, pidx, 25, Species::Boar.max_hp(), Species::Boar);
    let bslot = g.world.gobs.get(boar).unwrap();
    let home = g.world.gobs.pos[bslot];
    // The spawn anchor: the chase budget is measured from where the
    // beast spawned (populate_animals fills this; the helper bypasses
    // it, so the test plants it by hand).
    g.world.animal_home.insert(boar, home);
    // A live forced duel: the boar caught the player.
    g.start_fight(1, boar, Species::Boar);
    assert!(g.world.animal_fights.contains_key(&boar));
    assert_eq!(g.world.players[pidx].fight_target, Some(boar));
    // The player sprints the boar well past the give-up radius (the
    // beast follows, so the PLAYER-beast gap stays small: only the
    // HOME distance crosses the line).
    let far = (home.0 + crate::state::AGGRO_GIVEUP + 100, home.1 + 20);
    g.world.gobs.set_pos(bslot, far);
    let pgob = g.world.players[pidx].gob;
    let pslot = g.world.gobs.get(pgob).unwrap();
    g.world.gobs.set_pos(pslot, (far.0 + 40, far.1)); // still in duel range
    g.tick_animals();
    assert_eq!(
        g.world.players[pidx].fight_target, None,
        "the surrender tears the forced duel down server-side"
    );
    assert!(
        !g.world.animal_fights.contains_key(&boar),
        "the fight row goes with it"
    );
    assert!(
        g.world.animal_surrender.contains(&boar),
        "the beast is in the surrender march (no fresh aggro)"
    );
    let mv = g.world.gobs.mv[bslot].expect("the boar walks home");
    assert!(
        (mv.tx - home.0).abs() + (mv.ty - home.1).abs() < 200,
        "the march target is the home anchor, not the player"
    );
    // The hysteresis: a player standing RIGHT NEXT to the returning
    // boar (well inside the 300 aggro circle) does not restart the
    // chase - the walk home wins until the home circle is reached.
    g.world.gobs.mv[bslot] = None; // let the intent pass run again
    g.world.gobs.set_pos(pslot, (far.0 + 10, far.1)); // ~30 off the boar
    g.tick_animals();
    assert!(g.world.animal_surrender.contains(&boar));
    assert!(
        g.world.players[pidx].fight_target.is_none(),
        "no re-aggro mid-march even at melee range"
    );
    // Arrived home: the set releases and normal aggro re-arms (the
    // player is still inside the aggro circle - the boar may chase
    // again; 200 off keeps it a CHASE, not an instant re-engage).
    g.world.gobs.mv[bslot] = None;
    g.world.gobs.set_pos(bslot, (home.0 + 30, home.1));
    g.world.gobs.set_pos(pslot, (home.0 + 230, home.1));
    g.tick_animals();
    assert!(
        !g.world.animal_surrender.contains(&boar),
        "arrival releases the hysteresis"
    );
    // The release lands in the ARRIVAL tick's apply phase, so the fresh
    // chase is the NEXT intent pass.
    g.world.gobs.mv[bslot] = None;
    g.tick_animals();
    let mv = g.world.gobs.mv[bslot].expect("fresh aggro: the chase restarts");
    assert!(
        (mv.tx - (home.0 + 230)).abs() + (mv.ty - home.1).abs() < 100,
        "the march target is the player again"
    );
}
