//! Taming, breeding and dairy production: the per-animal state
//! rows, inheritance math and the dev-rate knobs.

use super::GobId;

/// Taming progress for one animal (session 45; animals-and-husbandry.md
/// taming service). Each successful Quell adds TAMENESS_PER_QUELL; at
/// TAMENESS_FULL the animal is permanently tame (no more leash breaks).
/// The leash-break deadline is a game-tick stamp: surviving logout means
/// game-time based, matching the wiki's lagg-relative wording.
#[derive(Debug, Clone, Copy)]
pub struct TameState {
    /// The tamer's player gob id (the follow target + rope binding).
    pub tamer: GobId,
    /// Accumulated tameness (0..=100). 100 = fully tamed.
    pub tameness: i32,
    /// World tick when the leash breaks and the beast re-aggros.
    /// Ignored once tameness reaches TAMENESS_FULL.
    pub break_at_tick: u64,
    /// Stored milk in 0.01 L units (session 47; cows only). Persisted.
    pub milk_units: u32,
    /// Stored wool count (session 47; sheep only). Persisted.
    pub wool: u8,
    /// Production accumulator: species quantity ticks toward the next
    /// unit (MILK_ACC_PER_UNIT / WOOL_ACC_PER_UNIT thresholds).
    pub prod_acc: u32,
    /// Consumption accumulator in nano-units of fodder (session 48):
    /// the fractional part of trough feeding between whole units.
    /// Persisted so restarts do not give a free meal.
    pub feed_acc_nano: u64,
    /// Consecutive ticks without a bite (no trough fodder in radius AND
    /// no grazing tile). Reset on any feeding; at STARVE_DEATH_TICKS the
    /// animal dies (session 48 starvation policy). Persisted.
    pub hunger: u64,
    /// Sex (session 90; animals-and-husbandry.md "Domestic lifecycle
    /// and breeding"). Assigned at the first quell by the WILD kind:
    /// wild Cows are cows, wild Aurochs are bulls (the domestic bull
    /// enters the herd through the aurochs morph). Bred calves roll
    /// 50/50. Only females lactate; males sire calves. Persisted.
    pub sex: Sex,
    /// Per-animal breed stat row (session 90): Milk Quantity for cows,
    /// Wool Quantity for sheep - the per-tick production accumulator
    /// weight. Wild stock draws a small spread around the legacy
    /// constants; calves INHERIT the parents' average with a spread
    /// (see breed_stat_child). Persisted.
    pub prod_quantity: u8,
    /// Per-animal breeding quality (session 90): the softcap the
    /// calf's inherited stats are clamped through (the doc's
    /// "softcapped by the bull's breeding quality" - server policy:
    /// the sire's row). Persisted.
    pub breed_ql: u8,
    /// Pregnancy accumulator (session 90; females only): quantity-
    /// ticks from conception to birth. 0 = not pregnant. Grows only
    /// while a fed sire stands within BREED_SEEK_RADIUS; births at
    /// GESTATION_TICKS. Persisted.
    pub pregnant_acc: u64,
    /// Juvenile accumulator (session 90): bred calves mature at
    /// MATURATION_TICKS. Adults (wild tames) start at the ceiling;
    /// newborns start at 0 and grow only while fed. Persisted.
    pub juvenile_acc: u64,
    /// True after a TIME-based leash break (session 83): the beast runs
    /// wild AI again, the follow render is off and the rope is free, but
    /// the accumulated tameness stays BANKED - the docs model the break
    /// as "a tameness-decay timer that re-enables the animal's hostile
    /// state unless quelled again" and "Tameness is per-animal
    /// persistent server state". The next quell re-leashes the beast
    /// (loose=false, break timer re-armed) and adds on top of the banked
    /// value. A DAMAGE-based break still wipes the whole row. Ignored at
    /// full tameness (never loose).
    pub loose: bool,
}

impl TameState {
    pub fn new(tamer: GobId, break_at_tick: u64) -> Self {
        TameState {
            tamer,
            tameness: 0,
            break_at_tick,
            milk_units: 0,
            wool: 0,
            prod_acc: 0,
            feed_acc_nano: 0,
            hunger: 0,
            sex: Sex::Female,
            prod_quantity: MILK_QUANTITY.min(255) as u8,
            breed_ql: GRAZE_PRODUCT_QL,
            pregnant_acc: 0,
            // Wild tames are adult stock (session 90): only bred
            // calves start at 0 and grow toward the ceiling.
            juvenile_acc: MATURATION_TICKS,
            loose: false,
        }
    }

    /// Is this animal an adult (session 90)? Bred calves mature at
    /// MATURATION_TICKS of fed growth; everything wild-tamed starts
    /// grown.
    pub fn is_adult(&self) -> bool {
        self.juvenile_acc >= MATURATION_TICKS
    }
}

/// Sex of a domestic animal (session 90; the wire never carries it -
/// the client renders species only, so this is pure server state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sex {
    #[default]
    Female,
    Male,
}

impl Sex {
    /// Wire/persist index. Stable append-only order like Species.
    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(v: u8) -> Option<Sex> {
        match v {
            0 => Some(Sex::Female),
            1 => Some(Sex::Male),
            _ => None,
        }
    }
}

/// Tameness added per successful Quell (docs step 3: five cycles to 100).
pub const TAMENESS_PER_QUELL: i32 = 20;
/// Full tameness: the animal is permanently tame.
pub const TAMENESS_FULL: i32 = 100;
/// Leash-break deadline: ~10 real minutes of game ticks (docs step 5
/// "about ten minutes (5-15, variable)" - the floor as server policy).
/// 600 s / TICK_MS(100) = 6000 ticks.
pub const LEASH_BREAK_TICKS: u64 = 6000;

/// Leash follow distance (session 83): a tamed beast standing farther
/// than this behind its tamer walks toward them (docs step 4/6: "the
/// animal follows the tamer (leashed)"). 30 subtiles keeps the beast
/// within the combat swing reach (33), so a quelled herd stays at the
/// tamer's heel instead of freezing wherever the quell landed.
pub const LEASH_FOLLOW_DIST: i32 = 30;

/// Session 83: `HNH_LEASH_TICKS` overrides the leash-break deadline in
/// game ticks for live verification - the legacy deadline spaces the
/// five quell cycles of one taming out over ~50 real minutes, which no
/// e2e probe can sit out. The value floors at 1 tick (a 0 would break
/// the leash on the arming tick itself); unset or malformed falls back
/// to the legacy constant. The tamed table is empty in the steady
/// state, so this stays a dev-only knob like HNH_MILK_RATE.
pub fn leash_break_ticks() -> u64 {
    static TICKS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *TICKS.get_or_init(|| {
        std::env::var("HNH_LEASH_TICKS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(LEASH_BREAK_TICKS)
    })
}

/// Session 90: `HNH_NO_AGGRO=1` disables the aggressive animals'
/// auto-aggro (the forced-duel chase) for live probes. The breeding
/// e2e walks a freshly leashed herd across half a map of wolves and
/// boars; each one that reaches swing reach locks the player's single
/// Fightview for the whole sprint-away window, and the quell protocol
/// for the sire then starves ("the fight never stayed open"). The
/// tamed-table/production state stays untouched, so this is a dev-only
/// knob in the HNH_LEASH_TICKS family; unset or malformed is `false`.
pub fn no_aggro() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        std::env::var("HNH_NO_AGGRO")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Session 90: `HNH_FAST_TAME=1` banks FULL tameness on every quell (one
/// landed quell tames). The live breeding e2e spends 3+ minutes per beast
/// on the five quell rounds (each round is a chase + a leash window), and
/// a probe timeout mid-protocol left the bull half-tamed and the herd
/// unfed. The five-round stack itself stays covered by the unit tests and
/// the dairy live probe; this knob only shortens the live breeding
/// probe's approach so it reaches the actual breeding legs (gestation,
/// birth, inheritance, persistence) within its watchdog. Dev-only, same
/// family as HNH_NO_AGGRO; unset or malformed is `false`.
pub fn fast_tame() -> bool {
    static FAST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FAST.get_or_init(|| {
        std::env::var("HNH_FAST_TAME")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

// --- Production meters (session 47; animals-and-husbandry.md "Animal
// products and collection flows"). ---

/// Milk cap: 10 L stored in 0.01 L units (doc "cows store up to 10 L").
pub const MILK_CAP_UNITS: u32 = 1000;
/// Milk rate denominator: the doc's rate is `Milk Quantity * 0.01` L per
/// 10 minutes, i.e. quantity q accrues q units per 6000 ticks
/// (10 min = 600 s / TICK_MS 100). Quantity 10 -> 10 units/10 min =
/// 0.1 L/10 min, the doc's quoted example.
pub const MILK_ACC_PER_UNIT: u32 = 6000;
/// Wool cap: 3 (doc "sheep store up to 3 wool").
pub const WOOL_CAP: u8 = 3;
/// Wool rate: 1 wool per 8 real hours at Wool Quantity 5 (doc), scaled
/// linearly with quantity: acc += quantity per tick, 1 unit per
/// 48000 ticks * 5 = 240000 accumulated quantity-ticks.
pub const WOOL_ACC_PER_UNIT: u32 = 240_000;
/// Server-policy breed stats pending legacy verification (the doc
/// carries no verified per-animal quantity numbers; both default to
/// the wiki's example quantities).
pub const MILK_QUANTITY: u32 = 10;
pub const WOOL_QUANTITY: u32 = 5;
/// Milk drawn per bucket: 1 L = 100 units of 0.01 L. The doc names the
/// bucket as the milking interaction but carries no bucket volume;
/// 1 L is the server policy (documented in the livestock doc).
pub const MILK_PER_BUCKET_UNITS: u32 = 100;
/// Milk/wool quality while grazing: the doc's grazing rule counts moor,
/// heath and grassland as food of quality level 10, so the product
/// quality follows at 10 (server policy pending bred-stat systems).
pub const GRAZE_PRODUCT_QL: u8 = 10;

// --- Domestic breeding (session 90; animals-and-husbandry.md
// "Domestic lifecycle and breeding"). ---

/// Cow gestation: 4.5 real days of game ticks (doc "a cow's gestation
/// is 4.5 real days"); 4.5 * 86400 s / TICK_MS(100).
pub const GESTATION_TICKS: u64 = 3_888_000;
/// Calf maturation: 10 real days of game ticks (doc "a calf matures in
/// 10 real days"); 10 * 86400 s / TICK_MS(100).
pub const MATURATION_TICKS: u64 = 8_640_000;
/// Sire seek radius: a fed bull inside this many subtiles of a fed cow
/// sires her calf (server policy - the doc carries no range; 5 tiles
/// matches the trough-feeding and hive-gate interaction ranges).
pub const BREED_SEEK_RADIUS: i32 = 55;
/// Twin chance at birth (doc "rare twins"; 1 in 20 - server policy).
pub const CALF_TWINS_PCT: u32 = 5;
/// Runaway-breeding cap (session 90): a cluster with this many fed
/// adults of one species inside the seek radius stops breeding. The
/// doc quotes only the real-time rates (gestation 4.5 d, maturation
/// 10 d) and carries no density rule; at the HNH_BREED_SCALE test
/// pace an unguarded fed cluster doubles every ~30 s (live trace: a
/// solo probe left on the server melted it into a 160 MB log with
/// 80k+ gobs), so the server enforces a carrying capacity per
/// cluster - the nursing block (a maturing calf in the radius) is
/// the primary brake, this cap is the hard ceiling.
pub const BREED_CLUSTER_CAP: usize = 8;
/// Breed-stat spread: a calf's row averages the parents and rolls
/// +0..=2 / -0..=1 around it (the doc's "+20 to -5" is the wiki's
/// 0..20-gain / 0..5-loss percent shape scaled to a stat row - server
/// policy, tunable).
pub const BREED_STAT_SPREAD_UP: u8 = 2;
pub const BREED_STAT_SPREAD_DOWN: u8 = 1;
/// Wild-stock breed-row spread: a wild tame draws its Milk/Wool
/// Quantity within +/-1 of the legacy constant (server policy: wild
/// variance exists but stays near the doc's quoted example).
pub const WILD_BREED_ROW_MIN: u8 = 9;
pub const WILD_BREED_ROW_MAX: u8 = 11;

/// A calf's inherited stat row (session 90): the parents' average with
/// the doc's random spread, softcapped by the SIRE's breeding quality
/// ("each stat of the calf averages the parents' stats with a +20 to
/// -5 random spread, softcapped by the bull's breeding quality").
/// Both inputs are already 1..=255 rows.
pub fn breed_stat_child(dam: u8, sire: u8, sire_breed_ql: u8, roll: u8) -> u8 {
    let avg = (dam as u16 + sire as u16).div_ceil(2); // round-half-up
    let spread = (roll % (BREED_STAT_SPREAD_UP + BREED_STAT_SPREAD_DOWN + 1)) as i16
        - BREED_STAT_SPREAD_DOWN as i16;
    let v = (avg as i16 + spread).max(1) as u16;
    v.min(sire_breed_ql.max(1) as u16) as u8
}

/// A calf's breeding quality (session 90): the same average+spread
/// inheritance, softcapped by the higher parent row (breeding quality
/// compounds slower than production rows - server policy).
pub fn breed_ql_child(dam_ql: u8, sire_ql: u8, roll: u8) -> u8 {
    let avg = (dam_ql as u16 + sire_ql as u16).div_ceil(2);
    let spread = (roll % (BREED_STAT_SPREAD_UP + BREED_STAT_SPREAD_DOWN + 1)) as i16
        - BREED_STAT_SPREAD_DOWN as i16;
    let cap = dam_ql.max(sire_ql).max(1) as u16;
    ((avg as i16 + spread).max(1) as u16).min(cap) as u8
}

/// Session 90: `HNH_BREED_SCALE` multiplies the per-tick pregnancy and
/// maturation accumulators for developer playability - the legacy
/// gestation (4.5 real days) and maturation (10 real days) cannot be
/// verified live. The scale multiplies the accumulators only; the
/// thresholds stay legacy-true. 1 is legacy real-time; malformed
/// values fall back to 1 (the milk_rate pattern).
pub fn breed_scale() -> u64 {
    static SCALE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SCALE.get_or_init(|| {
        std::env::var("HNH_BREED_SCALE")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(1)
    })
}

/// Session 83: `HNH_MILK_RATE` multiplies tame-cow milk production for
/// developer playability - the legacy rate needs ~100 real minutes for
/// one bucket (MILK_ACC_PER_UNIT / MILK_QUANTITY ticks per unit x
/// MILK_PER_BUCKET_UNITS), which no live verification can sit out.
/// The scale multiplies the per-tick accumulator only (the stored
/// meters and the drawn-per-bucket volume stay legacy-true); 1 is
/// legacy real-time. Malformed values fall back to 1 rather than wedge
/// boot.
pub fn milk_rate() -> u32 {
    static RATE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *RATE.get_or_init(|| {
        std::env::var("HNH_MILK_RATE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(1)
    })
}

/// Session 93: `HNH_WOOL_RATE` multiplies sheep wool production for
/// developer playability - the legacy rate needs one wool per 8 real
/// hours at quantity 5 (WOOL_ACC_PER_UNIT / prod_quantity sweep ticks),
/// which no live verification can sit out. The milk_rate pattern: the
/// scale multiplies the per-tick accumulator only (the 3-wool cap and
/// the stored meters stay legacy-true); 1 is legacy real-time; a
/// malformed value falls back to 1 rather than wedge boot. Both rams
/// and ewes grow wool (the doc rate), so the knob scales the whole
/// herd's accrual.
pub fn wool_rate() -> u32 {
    static RATE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *RATE.get_or_init(|| {
        std::env::var("HNH_WOOL_RATE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(1)
    })
}

#[cfg(test)]
mod milk_rate_tests {
    use super::*;

    /// The scale contract: whatever the environment holds (tests run
    /// without HNH_MILK_RATE set, and a stray value would still be
    /// pinned at >= 1), the gain never drops below the legacy rate and
    /// the saturating product cannot panic on absurd scales.
    #[test]
    fn milk_rate_never_scales_below_legacy() {
        assert!(milk_rate() >= 1);
        let gain = MILK_QUANTITY.saturating_mul(milk_rate());
        assert!(gain >= MILK_QUANTITY);
        assert_eq!(u32::MAX.saturating_mul(milk_rate()), u32::MAX);
    }

    /// The legacy bucket math the live dairy probe leans on: at rate 1
    /// one full bucket needs the documented accumulator walk (100
    /// units x MILK_ACC_PER_UNIT / MILK_QUANTITY per tick), and scaling
    /// divides the wait by exactly the rate factor (ceil, so a partial
    /// rate still fills the bucket within one extra tick).
    #[test]
    fn milk_bucket_walk_is_rate_scaled() {
        let rate = milk_rate() as u64;
        let legacy = (MILK_PER_BUCKET_UNITS as u64 * MILK_ACC_PER_UNIT as u64)
            .div_ceil(MILK_QUANTITY as u64);
        let per_tick = MILK_QUANTITY as u64 * rate;
        let ticks = legacy.div_ceil(per_tick);
        // The walk covers the bucket (>=) without overshooting it by a
        // full rate window (< legacy + rate): the ceil guarantee.
        assert!(ticks * per_tick >= legacy);
        assert!((ticks - 1) * per_tick < legacy);
        // The documented legacy number: 6000 ticks per unit x 100 units
        // at quantity 10 per tick = 60000 ticks = 100 real minutes.
        assert_eq!(legacy, 60_000);
    }
}

#[cfg(test)]
mod wool_rate_tests {
    use super::*;

    /// The scale contract (the milk_rate pattern): whatever the
    /// environment holds, the gain never drops below the legacy rate
    /// and the saturating product cannot panic on absurd scales.
    #[test]
    fn wool_rate_never_scales_below_legacy() {
        assert!(wool_rate() >= 1);
        let gain = WOOL_QUANTITY.saturating_mul(wool_rate());
        assert!(gain >= WOOL_QUANTITY);
        assert_eq!(u32::MAX.saturating_mul(wool_rate()), u32::MAX);
    }

    /// The legacy wool math the live sheep probe leans on: at rate 1
    /// one wool unit needs the documented accumulator walk
    /// (WOOL_ACC_PER_UNIT / quantity per tick; 240000 / 5 = 48000
    /// sweeps = 8 real hours at the sweep cadence), and the rate
    /// divides the wait by exactly the factor (the ceil guarantee).
    #[test]
    fn wool_unit_walk_is_rate_scaled() {
        let rate = wool_rate() as u64;
        let legacy = WOOL_ACC_PER_UNIT as u64;
        let per_tick = WOOL_QUANTITY as u64 * rate;
        let ticks = legacy.div_ceil(per_tick);
        assert!(ticks * per_tick >= legacy);
        assert!((ticks - 1) * per_tick < legacy);
        // The documented legacy number: one wool per 8 real hours at
        // quantity 5 (session-90 notes).
        assert_eq!(legacy, 240_000);
    }
}

#[cfg(test)]
mod breed_tests {
    use super::*;

    /// Inheritance contract (session 90): the child row averages the
    /// parents (round-half-up), rolls the +0..=2/-0..=1 spread, floors
    /// at 1, and softcaps through the SIRE's breeding quality.
    #[test]
    fn breed_stat_child_bounds() {
        // 10 + 10 -> 10; every roll lands inside 9..=12.
        for roll in 0..=6u8 {
            let v = breed_stat_child(10, 10, 255, roll);
            assert!((9..=12).contains(&v), "roll {roll} gave {v}");
        }
        // The exact spread ladder (roll%4 - 1).
        assert_eq!(breed_stat_child(10, 10, 255, 0), 9);
        assert_eq!(breed_stat_child(10, 10, 255, 1), 10);
        assert_eq!(breed_stat_child(10, 10, 255, 2), 11);
        assert_eq!(breed_stat_child(10, 10, 255, 3), 12);
        // Averaging rounds half-up: (9+10+1)/2 = 10.
        assert_eq!(breed_stat_child(9, 10, 255, 1), 10);
        // The sire softcap clamps an otherwise-high roll.
        assert_eq!(breed_stat_child(12, 12, 11, 3), 11);
        // ...but never pulls the row under 1.
        assert_eq!(breed_stat_child(1, 1, 1, 0), 1);
    }

    /// Breeding quality compounds slower: the child row softcaps at
    /// the HIGHER parent, never above it.
    #[test]
    fn breed_ql_child_bounds() {
        for roll in 0..=6u8 {
            let v = breed_ql_child(10, 12, roll);
            assert!((9..=12).contains(&v), "roll {roll} gave {v}");
            assert!(v <= 12, "never above the higher parent");
        }
        assert_eq!(breed_ql_child(12, 12, 3), 12);
        assert_eq!(breed_ql_child(1, 1, 0), 1);
    }

    /// The dev-knob contract (the milk_rate pattern): unset falls back
    /// to 1 and never below it.
    #[test]
    fn breed_scale_never_below_legacy() {
        assert!(breed_scale() >= 1);
    }
}

#[cfg(test)]
mod leash_ticks_tests {
    use super::*;

    /// The knob contract: tests run without HNH_LEASH_TICKS, so the
    /// deadline must fall back to the documented legacy floor (6000
    /// ticks = 10 real minutes); a floor-1 value can never arm a
    /// leash that breaks on the arming tick itself (>= 1), and the
    /// save/apply paths add it to `world.tick` unchanged.
    #[test]
    fn leash_break_ticks_falls_back_to_legacy() {
        let t = leash_break_ticks();
        assert!(t >= 1, "the deadline must be at least one tick");
        if std::env::var("HNH_LEASH_TICKS").is_err() {
            assert_eq!(t, LEASH_BREAK_TICKS);
        }
    }

    #[test]
    fn no_aggro_defaults_off() {
        // The knob contract: tests run without HNH_NO_AGGRO, so the
        // chase intent stays live; only an explicit "1"/"true" (the
        // breeding probe's server) disables it.
        let off = no_aggro();
        if std::env::var("HNH_NO_AGGRO").is_err() {
            assert!(!off);
        }
    }

    #[test]
    fn fast_tame_defaults_off() {
        // The knob contract: tests run without HNH_FAST_TAME, so the
        // quell stack stays the five-round legacy path; only an
        // explicit "1"/"true" (the breeding probe's server) banks full
        // tameness on the first landed quell.
        let fast = fast_tame();
        if std::env::var("HNH_FAST_TAME").is_err() {
            assert!(!fast);
        }
    }
}
