//! Bow ranged combat (the Shoot action, docs/mechanics/combat/
//! combat-system.md "Party and ranged combat actions").
//!
//! Documented model (Legacy:Combat_Actions + Fandom Bow, both cited in
//! the combat doc):
//! - Firing depletes the attack meter (the frv offence bar), but the
//!   chance of success depends ONLY on the accuracy meter. Our server
//!   fills an accuracy meter while a bow-equipped player keeps a target
//!   engaged; the shot auto-releases at a full meter.
//! - Bow damage `75 * sqrt(q / 10)` (Fandom Bow page; the same shape as
//!   the documented unarmed formulas `k * sqrt(Attr / 10)`).
//! - Aim speed is bow-dependent (RoB: "a Ranger's Bow aims at half the
//!   speed of a Wooden Bow") - the rate lives in `BOWS` so a second bow
//!   can be tuned without touching the tick loop.
//! - Arrows bypass the openings economy (no defence-bar chip): the
//!   hit/miss roll IS the whole resolution, per the accuracy-meter rule.
//!
//! Hit chance is a documented server policy (the legacy formula is not
//! recoverable client-side - combat-system.md Open questions):
//! `95 - 55 * (dist / BOW_RANGE) + min(20, Marksmanship / 5)`, clamped
//! to 15..99 percent.

use crate::state::GobId;

/// Aim meter full scale (percent * 100).
pub const AIM_FULL: i32 = 10000;
/// Aim meter gained per 100 ms combat tick for a Wooden Bow: full aim
/// in 4 s (40 ticks).
pub const AIM_RATE_WOODBOW: i32 = 250;
/// Maximum engagement distance for ranged fire (~12 tiles at 11 units
/// per tile; melee REACH is 33 = 3 tiles).
pub const BOW_RANGE: i32 = 132;
/// Chat progress thresholds (percent). Each fires one chat line so the
/// player sees the meter without a client widget.
pub const AIM_REPORTS: [i32; 3] = [25, 50, 75];

/// Known bows: resource name -> aim rate per tick. The Wooden Bow is
/// the session-36 craftable; the table keeps the RoB aim-speed note a
/// data-driven property.
pub const BOWS: &[(&str, i32)] = &[("gfx/invobjs/bow", AIM_RATE_WOODBOW)];

/// Arrow resources consumed by shots (first found stack wins).
pub const ARROWS: &[&str] = &["gfx/invobjs/arrow-stone", "gfx/invobjs/arrow-bone"];

/// Shot damage for a bow of quality `ql` (Fandom Bow: 75*sqrt(q/10)).
pub fn bow_damage(ql: u8) -> i32 {
    let q = ql.max(1) as f32;
    (75.0 * (q / 10.0).sqrt()) as i32
}

/// Hit chance in percent for a shot at Chebyshev distance `dist` with
/// `marks` Marksmanship. Linear falloff from point-blank 95% to 40% at
/// max range, plus up to +20% for Marksmanship, clamped 15..99.
pub fn hit_chance(dist: i32, marks: i32) -> i32 {
    let frac = (dist.max(0) as f32 / BOW_RANGE as f32).min(1.0);
    let bonus = (marks / 5).min(20);
    ((95.0 - 55.0 * frac) as i32 + bonus.clamp(0, 20)).clamp(15, 99)
}

/// Per-player aiming state (transient; never persisted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangedAim {
    pub target: GobId,
    /// Accuracy meter 0..AIM_FULL.
    pub meter: i32,
    /// Bow quality snapshot from the equipped stack at aim start.
    pub bow_ql: u8,
    /// Aim-rate snapshot (per-tick meter gain; bow-dependent).
    pub rate: i32,
    /// Highest AIM_REPORTS threshold already sent to chat.
    pub reported: i32,
}

impl RangedAim {
    pub fn new(target: GobId, bow_ql: u8, rate: i32) -> Self {
        RangedAim {
            target,
            meter: 0,
            bow_ql,
            rate,
            reported: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_follows_the_fandom_curve() {
        // 75*sqrt(q/10): q10 -> 75, q40 -> 150, q90 -> 225.
        assert_eq!(bow_damage(10), 75);
        assert_eq!(bow_damage(40), 150);
        assert_eq!(bow_damage(90), 225);
        // Degenerate quality never zeroes the shot.
        assert!(bow_damage(0) > 0);
    }

    #[test]
    fn chance_falls_with_distance_and_rises_with_skill() {
        let near = hit_chance(0, 0);
        let far = hit_chance(BOW_RANGE, 0);
        assert_eq!(near, 95, "point blank, unskilled");
        assert_eq!(far, 40, "max range, unskilled");
        assert!(hit_chance(0, 50) > near, "marksmanship bonus");
        assert!(hit_chance(0, 500) <= 99, "capped at 99");
        assert!(hit_chance(BOW_RANGE + 100, 0) >= 15, "floor 15");
    }

    #[test]
    fn woodbow_fills_the_meter_in_four_seconds() {
        // 40 ticks at AIM_RATE_WOODBOW == AIM_FULL (10 Hz combat tick).
        let total = AIM_FULL / AIM_RATE_WOODBOW;
        assert_eq!(total, 40);
        // The RoB note (Ranger's Bow at half the Wooden Bow's speed) is
        // expressible as a table entry without code changes.
        assert!(BOWS.contains(&("gfx/invobjs/bow", AIM_RATE_WOODBOW)));
    }

    #[test]
    fn aim_reports_are_ordered_and_below_full() {
        let mut last = 0;
        for r in AIM_REPORTS {
            assert!(r > last && r < 100);
            last = r;
        }
    }
}
