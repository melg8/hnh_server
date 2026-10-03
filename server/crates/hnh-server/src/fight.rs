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
        }
    }
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
}

impl FightState {
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
}
