//! Crop farming simulation (docs/mechanics/livestock/farming-and-plants.md).
//!
//! A crop is a server-owned gob whose resource carries a one-byte growth
//! stage inside the sprite dynamic data (sdt). The client's
//! `GrowingPlant.Factory.create` (`src/haven/resutil/GrowingPlant.java`)
//! reads `sdt.uint8()` as the stage index and rebuilds the sprite whenever
//! a new `OD_RES` delta arrives with a non-empty sdt. The server owns all
//! growth: stage advance on wall-clock timers, harvest outcomes per stage,
//! and the quality roll.
//!
//! Timing: legacy durations are real hours (wiki table, beehive-assisted).
//! `HNH_CROP_TIME_SCALE` divides those hours for developer playability
//! (default 60 -> 1 legacy hour becomes 1 real minute); 1.0 is legacy
//! real-time; tests push it very high so stages advance in milliseconds.
//! The scale is applied once at startup and baked into stage deadlines.

use std::time::Duration;

/// One harvest outcome: an item stack pushed to the harvester.
#[derive(Debug, Clone, Copy)]
pub struct Yield {
    pub label: &'static str,
    /// Inventory resource for the dropped/pushed item.
    pub res: &'static str,
    pub count: (u32, u32),
}

/// Static per-crop data (docs farming-and-plants.md table). Item resources
/// verified present in the shipped pack (`gameres/gfx/invobjs`); gob
/// resources verified in `gameres/gfx/terobjs/plants`.
pub struct CropSpec {
    /// Inventory seed stack label (also matched on plant itemact).
    pub seed_label: &'static str,
    /// Planted gob resource (`gfx/terobjs/plants/...`).
    pub gob_res: &'static str,
    /// Number of stage advances from planted (0) to fully mature.
    /// The wire stage byte is the zero-based index; the crop is
    /// harvestable once `stage >= early_stage` (byproduct) and at its
    /// best at `stages` (main product + seed return).
    pub stages: u8,
    /// Legacy real hours per stage advance (wiki; see module docs).
    pub stage_hours: f32,
    /// First stage that can be harvested at all (byproduct outcome).
    pub early_stage: u8,
    pub early_yield: Yield,
    pub mature_yields: &'static [Yield],
}

impl CropSpec {
    /// Real-time duration of one stage advance at the current scale.
    pub fn stage_duration(&self, scale: f32) -> Duration {
        // Floor at 250 ms so test-scale (10^7) stages still fire quickly.
        let mins = (self.stage_hours * 60.0 / scale.max(1.0)).max(0.25 / 60.0);
        Duration::from_millis((mins * 60_000.0) as u64)
    }
}

const CARROT: CropSpec = CropSpec {
    seed_label: "Carrot Seeds",
    gob_res: "gfx/terobjs/plants/carrot",
    // Wiki: 5 stages (wire 0..4); harvest at 4h/8h/12h.
    stages: 4,
    stage_hours: 4.0,
    early_stage: 2,
    early_yield: Yield {
        label: "Carrot",
        res: "gfx/invobjs/carrot",
        count: (1, 1),
    },
    mature_yields: &[
        Yield {
            label: "Carrot",
            res: "gfx/invobjs/carrot",
            count: (1, 3),
        },
        Yield {
            label: "Carrot Seeds",
            res: "gfx/invobjs/seed-carrot",
            count: (1, 3),
        },
    ],
};

const WHEAT_EARLY: Yield = Yield {
    label: "Straw",
    res: "gfx/invobjs/straw",
    count: (1, 2),
};
// The 2009 pack carries no grain item for wheat (sprout/grist/malt only);
// the mature harvest therefore returns seeds to keep the planting loop
// playable. Recorded in farming-and-plants.md Open questions.
const WHEAT_MATURE: [Yield; 1] = [Yield {
    label: "Wheat Seeds",
    res: "gfx/invobjs/seed-wheat",
    count: (2, 4),
}];

const FLAX_EARLY: Yield = Yield {
    label: "Flax Fibres",
    res: "gfx/invobjs/flaxfibre",
    count: (1, 2),
};
const FLAX_MATURE: [Yield; 1] = [Yield {
    label: "Flax Seeds",
    res: "gfx/invobjs/flaxseed",
    count: (2, 3),
}];

const HEMP_EARLY: Yield = Yield {
    label: "Plant Fibres",
    res: "gfx/invobjs/flaxfibre",
    count: (1, 2),
};
const HEMP_MATURE: [Yield; 2] = [
    Yield {
        label: "Hemp Seeds",
        res: "gfx/invobjs/seed-hemp",
        count: (2, 3),
    },
    HEMP_EARLY,
];

const PUMPKIN_MATURE: [Yield; 2] = [
    Yield {
        // fep.conf: Pumpkin Flesh = STR:1 CON:1.
        label: "Pumpkin Flesh",
        res: "gfx/invobjs/pumpkinflesh",
        count: (1, 2),
    },
    Yield {
        label: "Pumpkin Seeds",
        res: "gfx/invobjs/seed-pumpkin",
        count: (1, 3),
    },
];

const TEA_MATURE: [Yield; 1] = [Yield {
    label: "Fresh Tea Leaf",
    res: "gfx/invobjs/tea-fresh",
    count: (2, 3),
}];

const POPPY_MATURE: [Yield; 2] = [
    Yield {
        label: "Poppy Flower",
        res: "gfx/invobjs/flower-poppy",
        count: (1, 2),
    },
    Yield {
        label: "Poppy Seeds",
        res: "gfx/invobjs/seed-poppy",
        count: (1, 2),
    },
];

const ONION_MATURE: [Yield; 1] = [Yield {
    // fep.conf: Yellow Onion = HHP:1; the onion is its own seed.
    label: "Yellow Onion",
    res: "gfx/invobjs/onion",
    count: (1, 3),
}];
const WHEAT: CropSpec = CropSpec {
    seed_label: "Wheat Seeds",
    gob_res: "gfx/terobjs/plants/wheat",
    // Wiki: 1d / 1.5d / 2d harvest points -> 3 stages, 12h per stage.
    stages: 3,
    stage_hours: 12.0,
    early_stage: 2,
    early_yield: WHEAT_EARLY,
    mature_yields: &WHEAT_MATURE,
};

const FLAX: CropSpec = CropSpec {
    seed_label: "Flax Seeds",
    gob_res: "gfx/terobjs/plants/flax",
    stages: 3,
    stage_hours: 12.0,
    early_stage: 2,
    early_yield: FLAX_EARLY,
    mature_yields: &FLAX_MATURE,
};

const HEMP: CropSpec = CropSpec {
    seed_label: "Hemp Seeds",
    gob_res: "gfx/terobjs/plants/hemp",
    // Wiki: 33h / 34h -> 2 stages, 16h per stage.
    stages: 2,
    stage_hours: 16.0,
    early_stage: 1,
    early_yield: HEMP_EARLY,
    mature_yields: &HEMP_MATURE,
};

const PUMPKIN: CropSpec = CropSpec {
    seed_label: "Pumpkin Seeds",
    gob_res: "gfx/terobjs/plants/pumpkin",
    // Wiki: 5-7 days, coarse 2-stage model, 60h per stage.
    stages: 2,
    stage_hours: 60.0,
    early_stage: 1,
    // Unripe pumpkin still gives fibres? No: reuse flesh, count 1.
    early_yield: Yield {
        label: "Pumpkin Flesh",
        res: "gfx/invobjs/pumpkinflesh",
        count: (1, 1),
    },
    mature_yields: &PUMPKIN_MATURE,
};

const TEA: CropSpec = CropSpec {
    seed_label: "Tea Seeds",
    gob_res: "gfx/terobjs/plants/tea",
    stages: 1,
    stage_hours: 12.0,
    early_stage: 1,
    early_yield: TEA_MATURE[0],
    mature_yields: &TEA_MATURE,
};

const POPPY: CropSpec = CropSpec {
    seed_label: "Poppy Seeds",
    gob_res: "gfx/terobjs/plants/poppy",
    stages: 1,
    stage_hours: 12.0,
    early_stage: 1,
    early_yield: POPPY_MATURE[0],
    mature_yields: &POPPY_MATURE,
};

const ONION: CropSpec = CropSpec {
    seed_label: "Yellow Onion",
    gob_res: "gfx/terobjs/plants/onion",
    stages: 1,
    stage_hours: 12.0,
    early_stage: 1,
    early_yield: ONION_MATURE[0],
    mature_yields: &ONION_MATURE,
};

/// Static crop registry. Index doubles as the wire `spec` id and must stay
/// stable across saves (persisted by index).
pub static CROPS: &[CropSpec] = &[CARROT, WHEAT, FLAX, HEMP, PUMPKIN, TEA, POPPY, ONION];

/// Find a crop spec by the seed stack's display label.
pub fn spec_by_seed_label(label: &str) -> Option<usize> {
    CROPS.iter().position(|c| c.seed_label == label)
}

/// Live per-gob crop state (persisted).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CropState {
    /// Index into `CROPS`.
    pub spec: u8,
    /// Current wire stage byte (0 = just planted).
    pub stage: u8,
    /// Seed quality at planting time.
    pub seed_ql: u8,
    /// Soil quality of the tile at planting time.
    pub soil_ql: u8,
    /// Absolute unix-ms deadline of the next stage advance.
    pub next_stage_at: u64,
}

use serde::{Deserialize, Serialize};

/// Wall-clock scale divisor read once from the environment.
/// `HNH_CROP_TIME_SCALE` divides every legacy stage duration: 60 (default)
/// makes one legacy hour tick by in one real minute; 1 is legacy real-time.
fn time_scale() -> f32 {
    static SCALE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SCALE.get_or_init(|| {
        std::env::var("HNH_CROP_TIME_SCALE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v >= 1.0)
            .unwrap_or(60.0)
    })
}

/// Duration of one stage advance for `spec` at the configured scale.
pub fn stage_duration_ms(spec: &CropSpec) -> Duration {
    spec.stage_duration(time_scale())
}

/// Unplanted furrow reversion delay (legacy exact timer unsourced; 8
/// legacy hours, scaled like crops; 0 = planted, no decay).
pub fn tilth_decay_ms() -> u64 {
    let ms = 8.0 * 3600.0 * 1000.0 / time_scale();
    // 10 s floor keeps extreme test scales sane without racing the plant
    // step (plow -> take -> itemact happens within a second).
    (ms as u64).max(10_000)
}

/// Harvest quality roll (docs "Quality model"):
/// product q = seed q + roll in [-5, +5]; if soil q < seed q the roll is
/// capped to [-5, +2]; soil never helps beyond the seed's own value.
pub fn quality_roll(seed_ql: u8, soil_ql: u8, roll: i32) -> u8 {
    let roll = if soil_ql < seed_ql {
        roll.clamp(-5, 2)
    } else {
        roll.clamp(-5, 5)
    };
    let base = seed_ql as i32 + roll;
    base.clamp(1, 255) as u8
}

/// Draw a random roll in [-5, +5] from a uniform 0..=u32::MAX draw.
pub fn roll_from_uniform(u: u32) -> i32 {
    (u % 11) as i32 - 5
}

/// Deterministic tile soil quality in 30..=60 (world-region gradient is
/// not sourced in the docs; this keeps per-tile variety fixed by seed).
pub fn soil_quality(tx: i32, ty: i32) -> u8 {
    let mut h = (tx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= (ty as u64).rotate_left(21);
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    30 + (h >> 56) as u8 % 31
}

/// Choose a count in an inclusive range from a uniform draw.
pub fn count_from_uniform(range: (u32, u32), u: u32) -> u32 {
    let (lo, hi) = range;
    if hi <= lo {
        return lo;
    }
    lo + u % (hi - lo + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_integrity() {
        // Every spec must resolve its resources through pack names that the
        // resource auditor knows about (names, not files, are the contract).
        assert_eq!(CROPS.len(), 8);
        assert_eq!(spec_by_seed_label("Wheat Seeds"), Some(1));
        assert_eq!(spec_by_seed_label("Carrot Seeds"), Some(0));
        assert_eq!(spec_by_seed_label("nonexistent"), None);
        for c in CROPS {
            assert!(c.stages >= 1);
            assert!(c.early_stage >= 1 && c.early_stage <= c.stages);
            assert!(c.stage_hours > 0.0);
            assert!(c.gob_res.starts_with("gfx/terobjs/plants/"));
        }
    }

    #[test]
    fn stage_duration_scales() {
        // Default dev scale 60: carrot 4h -> 4 real minutes.
        let carrot = &CROPS[0];
        assert_eq!(carrot.stage_duration(60.0), Duration::from_secs(240));
        // Legacy scale 1: exact wiki hours.
        assert_eq!(carrot.stage_duration(1.0), Duration::from_secs(4 * 3600));
        // Aggressive test scale clamps to 250ms so e2e finishes fast.
        assert_eq!(
            carrot.stage_duration(10_000_000.0),
            Duration::from_millis(250)
        );
    }

    #[test]
    fn quality_roll_caps() {
        // Soil >= seed: full [-5,+5] band.
        assert_eq!(quality_roll(50, 50, -9), 45);
        assert_eq!(quality_roll(50, 50, 9), 55);
        // Soil < seed: capped at +2.
        assert_eq!(quality_roll(50, 40, 9), 52);
        assert_eq!(quality_roll(50, 40, -9), 45);
        // Clamp to 1 floor.
        assert_eq!(quality_roll(3, 60, -9), 1);
    }

    #[test]
    fn uniform_draws() {
        assert_eq!(roll_from_uniform(0), -5);
        assert_eq!(roll_from_uniform(11), -5);
        assert_eq!(roll_from_uniform(16), 0);
        for u in [0u32, 7, 123_456, u32::MAX] {
            assert!((-5..=5).contains(&roll_from_uniform(u)));
        }
        assert_eq!(count_from_uniform((2, 4), 0), 2);
        assert_eq!(count_from_uniform((2, 4), 2), 4);
        assert_eq!(count_from_uniform((3, 3), 99), 3);
    }

    #[test]
    fn soil_quality_is_deterministic_and_bounded() {
        let a = soil_quality(10, 20);
        assert_eq!(a, soil_quality(10, 20));
        assert_ne!(soil_quality(10, 21), a);
        for t in [(0, 0), (-5, 9), (100_000, -100_000)] {
            let q = soil_quality(t.0, t.1);
            assert!((30..=60).contains(&q));
        }
    }
}
