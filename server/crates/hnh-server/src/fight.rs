//! Fightview (frv) combat window: server-authoritative openings combat.
//!
//! Wire protocol per docs/mechanics/combat/combat-system.md:
//! - One `frv` widget per session, created on first engagement, destroyed
//!   when the last relation ends.
//! - Per-opponent relations carried in uimsg: `new`/`del`/`upd` (soft state)
//!   and `updod` (fast offence/defence bars), `cur` (selected target),
//!   `atkc` (attack cooldown in 1/60 s ticks), `offdef` (own bars).
//! - Client->server: `click` (select focus -> answer `cur`) and `give`
//!   (toggle one bit of the two-bit disengagement handshake).
//!
//! Simulation model (openings economy, Legacy:Combat_Actions):
//! - Offence bars fill while engaged; a swing spends half its bar and chips
//!   the defender's defence proportionally to advantage-scaled attack
//!   weight. Damage reaches HP only when the defender's defence is broken
//!   (an "opening"); otherwise the hit only consumes defence.
//! - Defence regenerates toward full every tick.
//! - IP accrues per swing for both sides.

use crate::resources::wdg;

/// Offence/defence bars are percentages scaled by 100 (10000 = 100%).
pub const BAR_FULL: i32 = 10000;
/// Offence regenerated per tick while engaged (10 Hz ticks). Half a bar
/// per second keeps swings landing between client movement bursts.
pub const OFF_REGEN: i32 = 625;
/// Defence regenerated per tick toward full.
pub const DEF_REGEN: i32 = 200;
/// Offence spent per swing (half the bar).
pub const SWING_SPEND: i32 = 5000;
/// Defence consumed by one standard swing at neutral advantage.
pub const SWING_DEF_DMG: i32 = 3000;
/// Defence below this counts as an opening: damage reaches HP.
pub const OPENING_THRESHOLD: i32 = 2000;
/// Attack cooldown ticks sent in `atkc` (legacy reads them as 1/60 s).
pub const ATKC_TICKS: i32 = 8;

/// Melee weapon base damage per equipped resource (server policy).
///
/// Legacy only documents the Soldier's Sword base damage (400, RoB
/// worked example - an item this resource pack does not ship) and its
/// linear `basedamage * ql * str / 10` formula does not reproduce its
/// own example arithmetically (items-and-quality.md Open questions).
/// This server therefore reuses the scaling every other quality system
/// in the pack already applies (armor QM, Fandom bow damage):
/// `dmg = base * sqrt(q/10) * (str/10)`.
///
/// The stone axe (the craftable melee weapon of this pack) sits at 15:
/// three unarmed blows at q10/str10, still far under the bow's 75.
pub const WEAPONS: &[(&str, i32)] = &[("gfx/invobjs/axe", 15)];

/// Unarmed melee damage: the strength-only legacy variant
/// `(5 * str / 10).max(1)` (Punch family, Combat_Actions).
pub fn unarmed_dmg(str_: i32) -> i32 {
    (5 * str_ / 10).max(1)
}

/// Equipped melee weapon damage, or None when the resource is not a
/// weapon (the unarmed model applies). Quality and strength scale as
/// documented on [`WEAPONS`].
pub fn weapon_dmg(resname: &str, ql: i32, str_: i32) -> Option<i32> {
    let &(_, base) = WEAPONS.iter().find(|(r, _)| *r == resname)?;
    let qm = ((ql.max(1) as f64) / 10.0).sqrt();
    let sm = (str_.max(1) as f64) / 10.0;
    Some(((base as f64) * qm * sm).max(1.0) as i32)
}

/// Build one uimsg payload for the frv widget.
///
/// Arguments are encoded as typed list ints, matching Fightview.uimsg.
pub fn uimsg(widget: u16, name: &str, args: &[i32]) -> Vec<u8> {
    let vals: Vec<crate::resources::wdg::ListVal> = args
        .iter()
        .map(|v| crate::resources::wdg::ListVal::I(*v))
        .collect();
    wdg::wdgmsg(widget, name, &vals)
}

/// Per-opponent relation record (wire: Fightview.Relation).
#[derive(Debug, Clone)]
pub struct FightRel {
    pub gob: i32,
    pub balance: i32,
    pub intensity: i32,
    pub give: i32,
    pub ip_self: i32,
    pub ip_other: i32,
    /// Opponent's offence toward you (scaled percentage).
    pub offence: i32,
    /// Opponent's defence against your attacks (scaled percentage).
    pub defence: i32,
    /// Advantage accumulator in TENTHS (+3 = +0.3), -50..+50. The
    /// integer `balance` streamed on the wire is the rounded, clamped
    /// view of this pool (fractional accumulations like Seize The Day!
    /// +0.3 need the sub-integer source; Legacy:Combat_Actions).
    pub adv: i32,
}

impl FightRel {
    pub fn new(gob: i32) -> Self {
        FightRel {
            gob,
            balance: 0,
            intensity: 0,
            give: 0,
            ip_self: 0,
            ip_other: 0,
            offence: 0,
            defence: BAR_FULL,
            adv: 0,
        }
    }

    /// Re-derive the wire balance (-5..+5) from the advantage pool.
    pub fn sync_balance(&mut self) {
        self.balance = (self.adv as f32 / 10.0).round().clamp(-5.0, 5.0) as i32;
    }
}

/// What the fight window does with a maneuver selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManeuverKind {
    /// An attack choice: frv `atk [cur, next]` (the two-slot attack
    /// queue the client renders).
    Attack,
    /// A defensive stance: frv `blk [res]`.
    Block,
    /// A pure IP/advantage play: relation `upd` only.
    Boost,
}

/// One fight-window maneuver (a `paginae/atk/*` action button, selected
/// through MenuGrid `act("atk", id)`).
///
/// Numbers: every IP cost/gain and advantage value documented on the
/// legacy wiki (RoB Legacy:Combat_Actions - Sting 2, Chop 4, Sidestep 4,
/// Opportunity Knocks 5, Knock His Teeth Out! 6, Valorous Strike 6,
/// Battle Cry 7 [needs 14 IP], Cleave 8 [needs >= 3 advantage],
/// Invocation of Skuld 3 [needs 10 IP], Charge! +1 IP, Feign Flight +2,
/// Throw Sand -2 opponent IP, Float Like A Butterfly +1 opponent IP,
/// Seize The Day! +0.3 advantage, Sidestep/Skuld +1, Battle Cry +2) is
/// marked DOC in the comments below; the rest are this server's policy
/// (marked POLICY) and live in combat-system.md Open questions.
pub struct Maneuver {
    pub id: &'static str,
    /// The pagina resource the client renders for the `atk`/`blk` slot.
    pub res: &'static str,
    pub kind: ManeuverKind,
    /// IP spent from the user's pool.
    pub ip_cost: i32,
    /// IP generated for the user.
    pub ip_gain: i32,
    /// IP delta applied to the OPPONENT's pool (may be negative).
    pub ip_opp: i32,
    /// Advantage delta in tenths.
    pub adv: i32,
    /// Minimum user IP required (Battle Cry 14, Skuld 10 - DOC).
    pub req_ip: i32,
    /// Minimum advantage in tenths required (Cleave >= 3 advantage - DOC).
    pub req_adv: i32,
}

pub const MANEUVERS: &[Maneuver] = &[
    // ---- attacks (DOC numbers where the wiki lists them) ----
    Maneuver {
        id: "pow",
        res: "paginae/atk/pow",
        kind: ManeuverKind::Attack,
        ip_cost: 0,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // POLICY: Punch is free
    Maneuver {
        id: "sting",
        res: "paginae/atk/sting",
        kind: ManeuverKind::Attack,
        ip_cost: 2,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // DOC
    Maneuver {
        id: "baseaxe",
        res: "paginae/atk/axe",
        kind: ManeuverKind::Attack,
        ip_cost: 4,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // DOC (Chop)
    Maneuver {
        id: "sidestep",
        res: "paginae/atk/sidestep",
        kind: ManeuverKind::Attack,
        ip_cost: 4,
        ip_gain: 0,
        ip_opp: 0,
        adv: 10,
        req_ip: 0,
        req_adv: 0,
    }, // DOC
    Maneuver {
        id: "oppknock",
        res: "paginae/atk/oppknock",
        kind: ManeuverKind::Attack,
        ip_cost: 5,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // DOC
    Maneuver {
        id: "knockteeth",
        res: "paginae/atk/knockteeth",
        kind: ManeuverKind::Attack,
        ip_cost: 6,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // DOC
    Maneuver {
        id: "valstr",
        res: "paginae/atk/valstr",
        kind: ManeuverKind::Attack,
        ip_cost: 6,
        ip_gain: 0,
        ip_opp: 2,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // DOC (+2 opp IP)
    Maneuver {
        id: "roar",
        res: "paginae/atk/roar",
        kind: ManeuverKind::Attack,
        ip_cost: 7,
        ip_gain: 0,
        ip_opp: 0,
        adv: 20,
        req_ip: 14,
        req_adv: 0,
    }, // DOC (Battle Cry)
    Maneuver {
        id: "cleave",
        res: "paginae/atk/cleave",
        kind: ManeuverKind::Attack,
        ip_cost: 8,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 30,
    }, // DOC (>= 3 advantage)
    Maneuver {
        id: "skuld",
        res: "paginae/atk/skuld",
        kind: ManeuverKind::Attack,
        ip_cost: 3,
        ip_gain: 0,
        ip_opp: 0,
        adv: 10,
        req_ip: 10,
        req_adv: 0,
    }, // DOC (needs 10 IP)
    // ---- attacks without documented numbers (POLICY) ----
    Maneuver {
        id: "strangle",
        res: "paginae/atk/strangle",
        kind: ManeuverKind::Attack,
        ip_cost: 2,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "bee",
        res: "paginae/atk/bee",
        kind: ManeuverKind::Attack,
        ip_cost: 2,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "ashoot",
        res: "paginae/atk/ashoot",
        kind: ManeuverKind::Attack,
        ip_cost: 0,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "sos",
        res: "paginae/atk/sos",
        kind: ManeuverKind::Attack,
        ip_cost: 8,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    // Quell the Beast (session 45 taming): the docs quote Jorb's
    // prerequisite list - two initiative points available for the
    // attack's IP cost, advantage fully in the tamer's favor (>= 3),
    // battle intensity 0, a Rope equipped. The rope check is
    // target-specific, so it lives in the game.rs on_maneuver gate,
    // not in the static table.
    Maneuver {
        id: "quell",
        res: "paginae/atk/quell",
        kind: ManeuverKind::Attack,
        ip_cost: 2,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 2,
        req_adv: 30,
    },
    // ---- block (POLICY) ----
    Maneuver {
        id: "dodge",
        res: "paginae/atk/dodge",
        kind: ManeuverKind::Block,
        ip_cost: 0,
        ip_gain: 0,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    // ---- boosts: DOC numbers where listed ----
    Maneuver {
        id: "berserk",
        res: "paginae/atk/berserk",
        kind: ManeuverKind::Boost,
        ip_cost: 0,
        ip_gain: 1,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // DOC (Charge! +1)
    Maneuver {
        id: "feignflight",
        res: "paginae/atk/feignflight",
        kind: ManeuverKind::Boost,
        ip_cost: 0,
        ip_gain: 2,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // DOC
    Maneuver {
        id: "throwsand",
        res: "paginae/atk/throwsand",
        kind: ManeuverKind::Boost,
        ip_cost: 0,
        ip_gain: 0,
        ip_opp: -2,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // DOC
    Maneuver {
        id: "butterfly",
        res: "paginae/atk/butterfly",
        kind: ManeuverKind::Boost,
        ip_cost: 0,
        ip_gain: 0,
        ip_opp: 1,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    }, // DOC
    Maneuver {
        id: "seize",
        res: "paginae/atk/seize",
        kind: ManeuverKind::Boost,
        ip_cost: 0,
        ip_gain: 0,
        ip_opp: 0,
        adv: 3,
        req_ip: 0,
        req_adv: 0,
    }, // DOC (+0.3)
    // ---- boosts without documented numbers (POLICY) ----
    Maneuver {
        id: "jump",
        res: "paginae/atk/jump",
        kind: ManeuverKind::Boost,
        ip_cost: 0,
        ip_gain: 1,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "slide",
        res: "paginae/atk/slide",
        kind: ManeuverKind::Boost,
        ip_cost: 0,
        ip_gain: 1,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "flex",
        res: "paginae/atk/flex",
        kind: ManeuverKind::Boost,
        ip_cost: 2,
        ip_gain: 0,
        ip_opp: 0,
        adv: 1,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "advpush",
        res: "paginae/atk/padv",
        kind: ManeuverKind::Boost,
        ip_cost: 4,
        ip_gain: 0,
        ip_opp: 0,
        adv: 10,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "paingain",
        res: "paginae/atk/paingain",
        kind: ManeuverKind::Boost,
        ip_cost: 4,
        ip_gain: 1,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "bloodshot",
        res: "paginae/atk/bloodshot",
        kind: ManeuverKind::Boost,
        ip_cost: 3,
        ip_gain: 0,
        ip_opp: -1,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "fcons",
        res: "paginae/atk/cflame",
        kind: ManeuverKind::Boost,
        ip_cost: 4,
        ip_gain: 2,
        ip_opp: 0,
        adv: 0,
        req_ip: 0,
        req_adv: 0,
    },
    Maneuver {
        id: "fflame",
        res: "paginae/atk/fflame",
        kind: ManeuverKind::Boost,
        ip_cost: 4,
        ip_gain: 0,
        ip_opp: 0,
        adv: 5,
        req_ip: 0,
        req_adv: 0,
    },
];

/// Look up a maneuver by its `ad[1]` id (act("atk", id)).
pub fn maneuver(id: &str) -> Option<&'static Maneuver> {
    MANEUVERS.iter().find(|m| m.id == id)
}

/// Per-session fight window state. `widget` is a session-local widget id.
#[derive(Default)]
pub struct FightState {
    pub widget: Option<u16>,
    pub rels: Vec<FightRel>,
    /// Own offence/defence bars against the current relation.
    pub own_off: i32,
    pub own_def: i32,
    /// Swing cooldown in ticks (also reported via `atkc`).
    pub atkc: i32,
    /// Selected attack queue (frv `atk [cur, next]`), as pagina
    /// resource names; None renders the client's empty slot (-1).
    pub atk_cur: Option<&'static str>,
    pub atk_next: Option<&'static str>,
    /// Selected defensive stance (frv `blk`), as a pagina resource.
    pub blk: Option<&'static str>,
}

impl FightState {
    /// A fresh fight state: the defence bar starts FULL (an unengaged
    /// player presents a whole defence against the first swing) while
    /// offence starts empty. `Default` leaves `own_def` at 0, which
    /// would open every fresh player to instant damage.
    pub fn new() -> Self {
        FightState {
            widget: None,
            rels: Vec::new(),
            own_off: 0,
            own_def: BAR_FULL,
            atkc: 0,
            atk_cur: None,
            atk_next: None,
            blk: None,
        }
    }

    pub fn rel(&self, gob: i32) -> Option<&FightRel> {
        self.rels.iter().find(|r| r.gob == gob)
    }

    pub fn rel_mut(&mut self, gob: i32) -> Option<&mut FightRel> {
        self.rels.iter_mut().find(|r| r.gob == gob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uimsg_encodes_new_relation() {
        let m = uimsg(7, "new", &[42, 0, 1, 0, 3, 0, 1000, 9000]);
        // RMSG_WDGMSG(1) + wid(2) + name + list payload.
        assert_eq!(m[0], hnh_proto::consts::RMSG_WDGMSG);
        let wid = u16::from_le_bytes([m[1], m[2]]);
        assert_eq!(wid, 7);
        let name_end = m[3..].iter().position(|&b| b == 0).expect("NUL") + 3;
        assert_eq!(&m[3..name_end], b"new");
    }

    #[test]
    fn relation_defaults_to_full_defence() {
        let r = FightRel::new(9);
        assert_eq!(r.defence, BAR_FULL);
        assert_eq!(r.balance, 0);
    }

    #[test]
    fn unarmed_damage_is_strength_only() {
        // Punch family: (5 * str / 10).max(1).
        assert_eq!(unarmed_dmg(10), 5);
        assert_eq!(unarmed_dmg(30), 15);
        assert_eq!(unarmed_dmg(1), 1, "minimum one point");
    }

    #[test]
    fn weapon_damage_scales_with_quality_and_strength() {
        // Stone axe base 15: q10/str10 = 15 (three unarmed blows).
        assert_eq!(weapon_dmg("gfx/invobjs/axe", 10, 10), Some(15));
        // Quality doubles from 10 to 40 (sqrt scaling, the same QM the
        // armor and bow systems apply).
        assert_eq!(weapon_dmg("gfx/invobjs/axe", 40, 10), Some(30));
        // Strength enters linearly: str 20 doubles the q10 blow.
        assert_eq!(weapon_dmg("gfx/invobjs/axe", 10, 20), Some(30));
        // Floor: a q1 axe still swings for at least the unarmed minimum.
        assert!(weapon_dmg("gfx/invobjs/axe", 1, 1).is_some_and(|d| d >= 1));
        // Non-weapons fall through to the unarmed model (None).
        assert_eq!(weapon_dmg("gfx/invobjs/woodbow", 10, 10), None);
        assert_eq!(weapon_dmg("gfx/invobjs/stonearrow", 10, 10), None);
        assert_eq!(weapon_dmg("gfx/invobjs/branch", 10, 10), None);
    }

    #[test]
    fn maneuver_table_carries_documented_values() {
        // RoB Legacy:Combat_Actions numbers (see MANEUVERS comments).
        let sting = maneuver("sting").unwrap();
        assert_eq!(sting.ip_cost, 2);
        assert_eq!(sting.kind, ManeuverKind::Attack);
        let chop = maneuver("baseaxe").unwrap();
        assert_eq!(chop.ip_cost, 4);
        let sidestep = maneuver("sidestep").unwrap();
        assert_eq!(sidestep.ip_cost, 4);
        assert_eq!(sidestep.adv, 10, "Sidestep grants +1 advantage");
        let cleave = maneuver("cleave").unwrap();
        assert_eq!(cleave.ip_cost, 8);
        assert_eq!(cleave.req_adv, 30, "Cleave needs >= 3 advantage");
        let roar = maneuver("roar").unwrap();
        assert_eq!(roar.ip_cost, 7);
        assert_eq!(roar.req_ip, 14, "Battle Cry needs at least 14 IP");
        assert_eq!(roar.adv, 20, "Battle Cry grants +2 advantage");
        let skuld = maneuver("skuld").unwrap();
        assert_eq!(skuld.ip_cost, 3);
        assert_eq!(skuld.req_ip, 10);
        let charge = maneuver("berserk").unwrap();
        assert_eq!(charge.ip_gain, 1, "Charge! generates +1 IP");
        assert_eq!(charge.ip_cost, 0);
        let sand = maneuver("throwsand").unwrap();
        assert_eq!(sand.ip_opp, -2, "Throw Sand costs the opponent 2 IP");
        let butterfly = maneuver("butterfly").unwrap();
        assert_eq!(butterfly.ip_opp, 1);
        let seize = maneuver("seize").unwrap();
        assert_eq!(seize.adv, 3, "Seize The Day! grants +0.3 advantage");
        // Unknown ids do not resolve.
        assert!(maneuver("nope").is_none());
        // Every entry maps ad ids the client actually sends (each res
        // ships an action layer with ad ["atk", id] - verified against
        // the served pack in this session's extraction).
        assert!(MANEUVERS.len() >= 25);
        for m in MANEUVERS {
            assert!(m.res.starts_with("paginae/atk/"), "{}", m.res);
        }
    }

    #[test]
    fn advantage_pool_syncs_the_wire_balance() {
        let mut rel = FightRel::new(1);
        rel.adv = 3; // +0.3
        rel.sync_balance();
        assert_eq!(rel.balance, 0, "+0.3 rounds to 0");
        rel.adv = 5; // +0.5 rounds to 1 (legacy dial granularity)
        rel.sync_balance();
        assert_eq!(rel.balance, 1);
        rel.adv = 47;
        rel.sync_balance();
        assert_eq!(rel.balance, 5, "clamped to the dial maximum");
        rel.adv = -50;
        rel.sync_balance();
        assert_eq!(rel.balance, -5);
    }
}
