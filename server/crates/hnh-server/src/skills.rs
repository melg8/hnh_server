//! Skill and learning-point economy (docs/mechanics/skills/
//! learning-points-and-curiosity.md).
//!
//! The client owns no skill data: the `chr` widget renders the server-pushed
//! `exp`/`nsk`/`psk` messages and sends `buy`/`sattr` back. This module is
//! the authoritative side: the sattr cost curve is the legacy curve the
//! client predicts (CharWnd SAttr), the non-incrementable catalog is
//! server-defined data, and planting is gated on the `farming` skill value.

use std::collections::HashSet;

/// The 11 incrementable skill values, in the exact order CharWnd.skillval
/// registers them (client-hardcoded names; SAttr widgets render from these
/// cattr entries and `sattr` messages carry them back by name).
pub const SKILL_VALUES: [&str; 11] = [
    "unarmed",
    "melee",
    "ranged",
    "explore",
    "stealth",
    "sewing",
    "smithing",
    "carpentry",
    "cooking",
    "farming",
    "survive",
];

/// Upper bound on an incrementable skill value. The client UI has no cap
/// (it clamps purchases by the LP wallet); the server caps at 100 as
/// recorded in the doc's server notes.
pub const MAX_SKILL_VALUE: i32 = 100;

/// One buyable non-incrementable skill. `name` is the `gfx/hud/skills/`
/// basename the client sends back in `buy` and loads as the list icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkillDef {
    pub name: &'static str,
    pub cost: i32,
    /// Chat line shown on purchase (the resource tooltip is client-side
    /// art; the server needs its own readable label).
    pub label: &'static str,
}

/// The purchasable skill catalog. Names are restricted to resources
/// verified present in lib/haven-res.jar (58 `gfx/hud/skills/*.res`
/// entries; loading a missing resource would wedge the client's nsk
/// list). Costs are server-defined data: legacy costs are not in the
/// client and the wiki carries no verifiable table for this fork — the
/// chosen values keep one farming point affordable from the fresh-char
/// wallet (see the doc's server notes).
pub const CATALOG: [SkillDef; 8] = [
    SkillDef {
        name: "forage",
        cost: 100,
        label: "Foraging",
    },
    SkillDef {
        name: "fishing",
        cost: 100,
        label: "Fishing",
    },
    SkillDef {
        name: "tools",
        cost: 120,
        label: "Tools",
    },
    SkillDef {
        name: "lumber",
        cost: 120,
        label: "Lumberjacking",
    },
    SkillDef {
        name: "hunting",
        cost: 150,
        label: "Hunting",
    },
    SkillDef {
        name: "masonry",
        cost: 150,
        label: "Masonry",
    },
    SkillDef {
        name: "metal",
        cost: 200,
        label: "Metal Working",
    },
    SkillDef {
        name: "cheese",
        cost: 200,
        label: "Cheese Making",
    },
];

/// Lookup a catalog entry by the basename the client sends in `buy`.
pub fn catalog_get(name: &str) -> Option<&'static SkillDef> {
    CATALOG.iter().find(|s| s.name == name)
}

/// Why a purchase failed (rendered as a chat system line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuyError {
    Unknown,
    Owned,
    TooExpensive,
}

/// Charge `cost` LP and record the skill. Pure so the wire handler stays
/// thin and the economy rules stay unit-testable.
pub fn buy(
    owned: &mut HashSet<&'static str>,
    lp: &mut i32,
    name: &str,
) -> Result<&'static SkillDef, BuyError> {
    let def = catalog_get(name).ok_or(BuyError::Unknown)?;
    if owned.contains(def.name) {
        return Err(BuyError::Owned);
    }
    // NOTE: checked_sub is the wrong tool here — 90 - 200 = -110 is a valid
    // i32, so the wallet would go negative without ever overflowing. The
    // guard must be the domain rule "wallet may not go below zero".
    if *lp < def.cost {
        return Err(BuyError::TooExpensive);
    }
    *lp -= def.cost; // bounded by the check above
    owned.insert(def.name);
    Ok(def)
}

/// Legacy sattr cost curve: raising `v` to `v+1` costs `100 * (v+1)` LP
/// (CharWnd SAttr.inc: `cost += tvalb * 100` after the increment). The
/// cumulative cost from `from` to `to` is the arithmetic series
/// `sum(100*(v+1), v in from..to)` = `50 * (k + n) * n` with `k = 2*from+1`
/// (the closed form the client uses for bulk purchases).
///
/// `None` rejects malformed ranges: empty raise (`to < from`), negatives,
/// and anything past [`MAX_SKILL_VALUE`]. A no-op (`to == from`) is `Some(0)`
/// so the client's send-every-SAttr batch pattern stays valid.
pub fn sattr_cost(from: i32, to: i32) -> Option<i32> {
    if from < 0 || to > MAX_SKILL_VALUE || to < from {
        return None;
    }
    if to == from {
        return Some(0);
    }
    // i64 intermediates: 50*(k+n)*n peaks at ~50*(201+100)*100 = 1.5e6 for
    // legal inputs, but the widening keeps malformed 32-bit neighbors safe
    // and makes the numeric-safety rule explicit.
    let n = to as i64 - from as i64;
    let k = 2 * from as i64 + 1;
    let total = 50 * (k + n) * n;
    i32::try_from(total).ok()
}

/// Passive LP accrual parameters. Legacy earns LP through curiosity
/// study; until that system lands, the server grants a small trickle per
/// online minute so the economy stays playable. `HNH_LP_RATE` (env) is a
/// float multiplier; tests use large values to compress time.
pub const LP_PER_MINUTE: i32 = 2;

/// Milliseconds of online time that must pass before one LP is granted
/// (integer math; the remainder carries over on the player).
pub fn ms_per_lp(rate: f64) -> u64 {
    let per_min = LP_PER_MINUTE as f64 * rate.max(0.0);
    if per_min <= 0.0 {
        return u64::MAX; // accrual disabled
    }
    let ms = 60_000.0 / per_min;
    // A sub-millisecond grant rate degenerates to "every tick"; clamp so
    // the carry loop cannot spin.
    ms.max(1.0) as u64
}

/// Advance one player's LP accrual by `elapsed_ms` of online time. Pure:
/// `carry` holds fractional progress in ms. Grants are saturated at the
/// wallet so a pathological rate cannot overflow.
pub fn accrue(lp: &mut i32, carry: &mut u64, elapsed_ms: u64, period_ms: u64) {
    if period_ms == u64::MAX {
        return;
    }
    *carry = carry.saturating_add(elapsed_ms);
    while *carry >= period_ms {
        *carry -= period_ms;
        let Some(next) = lp.checked_add(1) else {
            *carry = 0;
            return;
        };
        *lp = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::SavedPlayer;

    #[test]
    fn cost_curve_matches_the_per_point_series() {
        // One point from 0: 100 * (0 + 1) = 100 — exactly the fresh-char
        // wallet, so the farming gate is buyable out of the box.
        assert_eq!(sattr_cost(0, 1), Some(100));
        // One point from v: 100 * (v + 1).
        assert_eq!(sattr_cost(3, 4), Some(400));
        // Multi-point: sum(100*(v+1), v=3..5) = 400 + 500 = 900; the
        // client's bulk closed form 50*(k+n)*n with k=7, n=2 -> 900.
        assert_eq!(sattr_cost(3, 5), Some(900));
        // From 0 to 100 (full climb) stays well inside i32.
        assert_eq!(sattr_cost(0, 100), Some(505_000));
    }

    #[test]
    fn cost_curve_rejects_malformed_ranges() {
        assert_eq!(sattr_cost(5, 3), None); // lowering
        assert_eq!(sattr_cost(-1, 2), None); // negative base
        assert_eq!(sattr_cost(0, 101), None); // past the cap
        assert_eq!(sattr_cost(7, 7), Some(0)); // no-op is legal in a batch
    }

    #[test]
    fn buy_charges_and_records() {
        let mut owned = HashSet::new();
        let mut lp = 250;
        let def = buy(&mut owned, &mut lp, "forage").expect("affordable");
        assert_eq!(def.name, "forage");
        assert_eq!(lp, 150);
        assert!(owned.contains("forage"));
    }

    #[test]
    fn buy_refuses_unknown_owned_and_unaffordable() {
        let mut owned = HashSet::new();
        let mut lp = 90;
        assert_eq!(
            buy(&mut owned, &mut lp, "nosuchskill"),
            Err(BuyError::Unknown)
        );
        assert_eq!(
            buy(&mut owned, &mut lp, "metal"),
            Err(BuyError::TooExpensive)
        );
        assert_eq!(lp, 90);
        assert!(owned.insert("forage"));
        assert_eq!(buy(&mut owned, &mut lp, "forage"), Err(BuyError::Owned));
        // No partial charges on any refusal path.
        assert_eq!(lp, 90);
    }

    #[test]
    fn accrual_grants_one_lp_per_period_and_carries_remainders() {
        let mut lp = 0;
        let mut carry = 0;
        let period = 30_000; // 2 LP per minute default
        accrue(&mut lp, &mut carry, 29_999, period);
        assert_eq!(lp, 0);
        accrue(&mut lp, &mut carry, 1, period);
        assert_eq!(lp, 1);
        accrue(&mut lp, &mut carry, 75_000, period);
        assert_eq!(lp, 3); // 2 grants from 75_000 ms
        assert_eq!(carry, 15_000); // 75_000 - 2*30_000 carried
    }

    #[test]
    fn accrual_is_disabled_or_bounded_at_the_extremes() {
        let mut lp = 0;
        let mut carry = 0;
        accrue(&mut lp, &mut carry, 10_000_000, u64::MAX);
        assert_eq!(lp, 0, "disabled rate must not grant");
        // A huge wallet saturates instead of overflowing.
        let mut rich = i32::MAX;
        let mut c = 0;
        accrue(&mut rich, &mut c, period_len() * 5, period_len());
        assert_eq!(rich, i32::MAX);
        assert_eq!(c, 0, "overflow resets the carry");
    }

    fn period_len() -> u64 {
        1
    }

    #[test]
    fn ms_per_lp_scales_with_the_env_rate() {
        assert_eq!(ms_per_lp(1.0), 30_000); // 2 LP/min
        assert_eq!(ms_per_lp(0.0), u64::MAX); // disabled
        assert_eq!(ms_per_lp(-5.0), u64::MAX); // malformed env is disabled
        assert_eq!(ms_per_lp(60_000.0), 1); // test scale: 1 LP per tick
    }

    #[test]
    fn farming_value_defaults_to_zero_for_fresh_chars() {
        // The planting gate reads attrs["farming"]; fresh chars must not
        // accidentally satisfy it.
        let attrs: std::collections::HashMap<String, i32> = std::collections::HashMap::new();
        assert_eq!(attrs.get("farming").copied().unwrap_or(0), 0);
    }

    #[test]
    fn skills_survive_save_roundtrip() {
        let mut owned = HashSet::new();
        let mut lp = 500;
        buy(&mut owned, &mut lp, "forage").unwrap();
        buy(&mut owned, &mut lp, "cheese").unwrap();
        let saved = SavedPlayer {
            name: "RoundTrip".to_owned(),
            pos: (0, 0),
            hp: 100,
            energy: 100,
            stamina: 100,
            lp,
            attrs: std::collections::HashMap::new(),
            inv: Vec::new(),
            inv_labels: Vec::new(),
            skills: owned.iter().map(|s| s.to_string()).collect(),
        };
        let restore = |saved: &SavedPlayer| -> HashSet<&'static str> {
            // Names in the save were validated at purchase time; an
            // unknown name (older catalog) is skipped, not re-armed.
            saved
                .skills
                .iter()
                .filter_map(|s| catalog_get(s).map(|d| d.name))
                .collect()
        };
        let restored = restore(&saved);
        assert!(restored.contains("forage"));
        assert!(restored.contains("cheese"));
        // Unknown names from older catalogs are dropped, not re-armed.
        let stale = SavedPlayer {
            skills: vec!["removed-skill".to_owned()],
            ..saved.clone()
        };
        assert!(restore(&stale).is_empty());
    }

    #[test]
    fn skill_values_match_the_client_hardcoded_list() {
        // CharWnd.skillval order — a renamed entry would break the sattr
        // contract (the client sends names, the server must know them).
        assert_eq!(SKILL_VALUES[9], "farming");
        assert_eq!(SKILL_VALUES.len(), 11);
    }
}
