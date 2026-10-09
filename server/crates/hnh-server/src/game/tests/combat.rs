//! Melee duels, PvP consequences, maneuvers, relay authority.
use super::super::*;
use super::common::*;

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
