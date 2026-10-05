//! Armor class (the Equipment window "Armor class: def/abs" line).
//!
//! The client owns only the SUM: Equipory.calcAC parses each equipped
//! item's tooltip against `Armor class: (\d+)/(\d+)` and totals both
//! columns (docs/mechanics/combat/combat-system.md: "the server owns the
//! numbers, the client merely sums the tooltip strings"). This module is
//! the server side: per-piece base values, the quality scaling, the
//! tooltip line composition, and the two combat applications.
//!
//! Quality scaling: defense and absorption scale with QM = sqrt(q/10).
//! Documented anchors (items-and-quality.md): Q10 Boar Tusk Helmet is
//! 1/7 and Q160 is 4/28 - exactly base * 1 and base * 4. Untested pieces
//! reuse the same rule with community-plausible bases (the legacy
//! per-piece numbers are an open question there).
//!
//! Combat model (server-defined, documented here because legacy keeps it
//! unspecified): absorption reduces damage that reaches HP,
//! `dmg_eff = dmg * 50 / (50 + abs_total)`; defense slows the attacker's
//! breakthrough of the defender's defence bar,
//! `chip_eff = chip * 50 / (50 + def_total)`. Both saturate: a bare body
//! is unchanged, and armor never turns a hit into healing.

/// (resource name, base defense, base absorption) for every armor piece
/// the served resource pack ships. Only entries listed here are armor;
/// everything else in the equipment slots contributes nothing.
pub const PIECES: &[(&str, i32, i32)] = &[
    ("gfx/invobjs/cloak-hide", 1, 2),
    ("gfx/invobjs/cloak-leather", 1, 3),
    ("gfx/invobjs/larmor", 2, 5),
    ("gfx/invobjs/lboots", 1, 2),
    ("gfx/invobjs/barmor", 3, 7),
    ("gfx/invobjs/parmor", 5, 10),
    ("gfx/invobjs/dhelm", 2, 6),
    ("gfx/invobjs/helm-tusk", 1, 7),
    ("gfx/invobjs/helm-soldiers", 2, 8),
    ("gfx/invobjs/helm-hird", 2, 8),
    ("gfx/invobjs/linenshirt", 0, 1),
    ("gfx/invobjs/shirt-nettle", 0, 1),
    ("gfx/invobjs/shirt-ranger", 1, 3),
    ("gfx/invobjs/shirt-chainmail", 3, 6),
];

/// Saturation constant of the combat applications above.
const K: i32 = 50;

/// Piece base values, or None for non-armor resources.
pub fn piece(resname: &str) -> Option<(i32, i32)> {
    PIECES
        .iter()
        .find(|(r, _, _)| *r == resname)
        .map(|&(_, d, a)| (d, a))
}

/// Quality-scaled per-piece armor class: base * sqrt(q/10), rounded
/// toward zero (a piece never exceeds its base's decade multiple). Q10
/// yields the base itself; Q160 yields 4x (the documented anchors).
pub fn ac_of(resname: &str, ql: i32) -> Option<(i32, i32)> {
    let (d, a) = piece(resname)?;
    let qm = (ql.max(1) as f64 / 10.0).sqrt() as i32;
    Some((d * qm, a * qm))
}

/// The tooltip line the client's calcAC parses:
/// `Armor class: <def>/<abs>`.
pub fn ac_line(resname: &str, ql: i32) -> Option<String> {
    ac_of(resname, ql).map(|(d, a)| format!("Armor class: {d}/{a}"))
}

/// Damage that reaches HP after absorption: `dmg * K / (K + abs)`.
pub fn reduce_damage(dmg: i32, abs_total: i32) -> i32 {
    if dmg <= 0 {
        return 0;
    }
    let abs_total = abs_total.clamp(0, i32::MAX - K);
    dmg * K / (K + abs_total)
}

/// Breakthrough consumed from the defender's defence bar after armor
/// defense: `chip * K / (K + def)`.
pub fn defense_chip(chip: i32, def_total: i32) -> i32 {
    if chip <= 0 {
        return 0;
    }
    let def_total = def_total.clamp(0, i32::MAX - K);
    chip * K / (K + def_total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_anchors_match_the_documented_pieces() {
        // items-and-quality.md: Q10 Boar Tusk Helmet 1/7, Q160 4/28.
        assert_eq!(ac_of("gfx/invobjs/helm-tusk", 10), Some((1, 7)));
        assert_eq!(ac_of("gfx/invobjs/helm-tusk", 160), Some((4, 28)));
        // Q1 floors the quality multiplier to 0: an honest 0/0 under the
        // documented rule (armor never exceeds the base decade multiple).
        assert_eq!(ac_of("gfx/invobjs/helm-tusk", 1), Some((0, 0)));
    }

    #[test]
    fn non_armor_is_none() {
        assert_eq!(piece("gfx/invobjs/meat"), None);
        assert_eq!(piece("gfx/invobjs/axe"), None);
        assert_eq!(ac_of("gfx/invobjs/axe", 10), None);
        assert_eq!(ac_line("gfx/invobjs/axe", 10), None);
    }

    #[test]
    fn tooltip_line_matches_the_client_pattern() {
        // Equipory.calcAC parses exactly "Armor class: (\d+)/(\d+)".
        let line = ac_line("gfx/invobjs/helm-tusk", 160).unwrap();
        assert_eq!(line, "Armor class: 4/28");
    }

    #[test]
    fn damage_saturates_and_never_amplifies() {
        // Bare body: unchanged.
        assert_eq!(reduce_damage(8, 0), 8);
        // Plate at Q10 (abs 10): 8 -> 8*50/60 = 6.
        assert_eq!(reduce_damage(8, 10), 6);
        // Heavy armor stacks absorb a lot but never all of it.
        assert_eq!(reduce_damage(8, 400), 0);
        // Zero/negative damage stays zero.
        assert_eq!(reduce_damage(0, 0), 0);
        assert_eq!(reduce_damage(-3, 10), 0);
    }

    #[test]
    fn defense_chip_saturates_and_never_amplifies() {
        assert_eq!(defense_chip(3000, 0), 3000);
        // Plate def 5: 3000 -> 3000*50/55 = 2727.
        assert_eq!(defense_chip(3000, 5), 2727);
        // Stacked defense slows the breakthrough to a trickle.
        assert_eq!(defense_chip(3000, 450), 300);
        assert_eq!(defense_chip(0, 0), 0);
        assert_eq!(defense_chip(-1, 5), 0);
    }
}
