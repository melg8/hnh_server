//! Food Trough feeding: fodder table, consumption rates and the
//! per-trough store.

// --- Food Trough + feeding (session 48; animals-and-husbandry.md
// "Feeding: troughs and grazing"). ---

/// Fodder capacity of one Food Trough (doc "Capacity 200 fodder units").
pub const TROUGH_CAP_UNITS: u32 = 200;
/// Feeding radius of a Food Trough in tiles (doc "radius 18 tiles").
/// One tile is 11x11 subtiles (plans/structures snap per tile).
pub const TROUGH_RADIUS_TILES: i32 = 18;
/// Squared subtile radius: (18 * 11)^2. Euclidean over subtile coords.
pub const TROUGH_RADIUS_SQ: i32 = (TROUGH_RADIUS_TILES * 11) * (TROUGH_RADIUS_TILES * 11);
/// In-game day: 8 real hours (farming-and-plants.md "one in-game day =
/// 8 real hours"). At TICK_MS 100 that is 288000 ticks.
pub const DAY_TICKS: u64 = 8 * 60 * 60 * 10;
/// Consumption rate (docs Legacy:Cattle): a non-pregnant cow eats 4.8
/// fodder units per in-game day. Stored as nano-units per tick for
/// loss-free integer accumulation: 4.8 units/day / 288000 ticks =
/// 16.667 nano-units/tick (0.002% rounding, documented).
pub const COW_EAT_NANO_PER_TICK: u64 = 16_667;
/// Sheep consumption rate: the doc quotes no sheep number; server
/// policy is half the cow rate (2.4 units/day = 8333 nano/tick),
/// documented in the livestock doc.
pub const SHEEP_EAT_NANO_PER_TICK: u64 = 8_333;
/// Lactating surcharge (docs Legacy:Cattle): "a lactating cow eats 4.8
/// plus 0.1 unit per liter of milk produced". This server's cow
/// produces MILK_QUANTITY(10) * 0.01 L per 6000 ticks = 1/60000 L per
/// tick, so the surcharge is 0.1/60000 units/tick = 1667 nano-units.
/// Bound to the production rate (what is produced, not what is stored),
/// exactly like the doc's wording.
pub const LACTATE_NANO_PER_TICK: u64 = 1_667;
/// Starvation (docs "Starvation should kill or stop production"): a
/// fully tamed producer that finds no food - no trough in radius with
/// fodder, no grazing tile - dies after 3 in-game days without a bite
/// (server policy; the wiki only says tamed animals "require food or
/// grassland to survive and breed"). Production is already gated on
/// feeding, so the stop-half is implicit.
pub const STARVE_DEATH_TICKS: u64 = 3 * DAY_TICKS;

/// One fodder unit per item for every listed fodder item; the doc's
/// "Giant Pumpkin (worth 16 seeds)" has no resource in the 2009 pack.
/// Matches by inventory resource name ("any seeds" is the seed- prefix
/// plus flaxseed; the rest are the doc's list intersected with the
/// resources the 2009 jar actually ships - Blueberries, Chantrelles,
/// Bloated Bolete, Peapod and Beetroot/Leaves have NO invobj resources
/// in this pack and therefore cannot be matched; recorded in the doc).
pub fn fodder_units(resname: &str) -> Option<u32> {
    const DIRECT: &[&str] = &[
        "gfx/invobjs/apple",
        "gfx/invobjs/applecore",
        "gfx/invobjs/mulberry",
        "gfx/invobjs/straw",
        "gfx/invobjs/pumpkinflesh",
        "gfx/invobjs/carrot",
        "gfx/invobjs/flower-poppy",
    ];
    if DIRECT.contains(&resname) {
        return Some(1);
    }
    if resname == "gfx/invobjs/flaxseed" || resname.starts_with("gfx/invobjs/seed-") {
        return Some(1);
    }
    None
}

/// Fodder store of one placed Food Trough. Quality is the average of
/// what was placed (doc: "q5 + q12 + q16 -> q11"): running sum/count,
/// consumed fodder carries the running average.
#[derive(Debug, Clone, Copy, Default)]
pub struct TroughState {
    /// Stored fodder units (0..=TROUGH_CAP_UNITS).
    pub units: u32,
    /// Sum of qualities over every unit ever placed (running average
    /// denominator in `ql_seen`).
    pub ql_sum: u64,
    /// Number of units whose quality is summed (== units placed total;
    /// consumption drains units but NOT the quality history, matching
    /// the doc's "average quality of what was placed").
    pub ql_seen: u64,
}

impl TroughState {
    /// Running average quality of the fodder placed so far (Q10 before
    /// anything is placed - the grazing baseline).
    pub fn avg_ql(&self) -> u8 {
        if self.ql_seen == 0 {
            return 10;
        }
        (self.ql_sum / self.ql_seen).min(255) as u8
    }

    /// Drain up to `want` units; returns the amount actually taken.
    pub fn take(&mut self, want: u32) -> u32 {
        let got = want.min(self.units);
        self.units -= got;
        got
    }
}

#[cfg(test)]
mod cluster_tests {
    use super::*;
    // --- Food Trough + feeding (session 48). ---

    /// The fodder table matches the doc's list intersected with the
    /// resources the 2009 pack actually ships: any seed-* resource plus
    /// flaxseed, the direct list (apple, apple core, mulberry, straw,
    /// pumpkin flesh, carrot, poppy flower). Branch and stone are NOT
    /// fodder.
    #[test]
    fn fodder_table_matches_the_doc() {
        assert_eq!(fodder_units("gfx/invobjs/seed-wheat"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/seed-carrot"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/seed-pumpkin"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/flaxseed"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/apple"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/applecore"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/mulberry"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/straw"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/pumpkinflesh"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/carrot"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/flower-poppy"), Some(1));
        // Non-fodder: the crafting-chain materials and foods.
        assert_eq!(fodder_units("gfx/invobjs/branch"), None);
        assert_eq!(fodder_units("gfx/invobjs/stone"), None);
        assert_eq!(fodder_units("gfx/invobjs/meat"), None);
        assert_eq!(fodder_units("gfx/invobjs/bucket-milk"), None);
    }

    /// The trough quality average follows the doc's example: q5 + q12 +
    /// q16 -> q11 (33 / 3). Consumption drains units, not the history;
    /// an untouched trough averages Q10 (the grazing baseline).
    #[test]
    fn trough_quality_averaging_and_take() {
        let mut t = TroughState::default();
        assert_eq!(t.avg_ql(), 10, "empty trough sits at the q10 baseline");
        t.units += 1;
        t.ql_sum += 5;
        t.ql_seen += 1;
        t.units += 1;
        t.ql_sum += 12;
        t.ql_seen += 1;
        t.units += 1;
        t.ql_sum += 16;
        t.ql_seen += 1;
        assert_eq!(t.avg_ql(), 11, "q5 + q12 + q16 -> q11 (the doc's example)");
        assert_eq!(t.take(2), 2, "take drains stored units");
        assert_eq!(t.units, 1);
        assert_eq!(t.avg_ql(), 11, "consumption does not rewrite the history");
        assert_eq!(t.take(9), 1, "take clamps at the stored amount");
        assert_eq!(t.units, 0);
    }
}
