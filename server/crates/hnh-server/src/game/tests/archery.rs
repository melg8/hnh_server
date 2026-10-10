//! Bow aiming, arrow economy, ranged relays.
use super::super::*;
use super::common::*;

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
    // Without a bow the same click opens the melee fight. Session 83:
    // melee engagement opens only from swing reach (33 subtiles), so
    // the bow-less leg clicks a deer standing close by - the 66-unit
    // deer is out of reach for a melee click.
    g.world.players[pidx].aim = None;
    g.world.players[pidx].equip[0] = None;
    let close = spawn_deer_at(&mut g, pidx, 20, 200);
    g.player_interact(1, pgob, close, (0, 0));
    assert_eq!(
        g.world.players[pidx].fight_target,
        Some(close),
        "melee fight opens without a bow from swing reach"
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
