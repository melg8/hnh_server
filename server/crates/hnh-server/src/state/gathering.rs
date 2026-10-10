//! World-gathering statics: pick yields, forageable plants and
//! ore kinds (the gathering legs of the crafting chains).

/// Server policy (docs/mechanics/crafting-and-building.md, "World
/// gathering"): a pickable tree yields this many branch picks before it
/// becomes a stump.
pub const TREE_HARVESTS: u8 = 5;
/// Server policy: a boulder yields this many stones before it
/// disappears.
pub const BOULDER_STONES: u8 = 5;
/// Server policy: an ore deposit yields this many ore picks before it
/// disappears (the metal chain's mining leg; see the "World gathering"
/// section of crafting-and-building.md).
pub const ORE_PICKS: u8 = 4;
/// Server policy: a clay deposit yields this many clay picks before it
/// disappears (the kiln chain's gathering leg; the kiln's legacy build
/// demand is Clay x45, so a deposit must outlast a few trips).
pub const CLAY_PICKS: u8 = 8;
/// Server policy (session 81, apple pie chain): an apple tree yields
/// this many apple picks before it degrades to a plain tree.
pub const APPLE_PICKS: u8 = 5;
/// Server policy (session 81, honeybun chain): a wild beehive holds
/// this many harvestable honey units (the legacy hive "holds at most
/// 1.0 L honey"; unit policy recorded in crafting-and-building.md).
pub const HIVE_HONEY_UNITS: u8 = 3;
/// Server policy (session 81): one forage pick yields this many item
/// units (a bush carries a handful, a mushroom patch a couple - one
/// uniform count keeps the dough recipes one-pick-per-ingredient).
pub const FORAGE_YIELD: u32 = 3;
/// Flat gathering quality, matched to the starter kit (branch/stone at
/// ql 10) so world-gathered materials craft identically.
pub const GATHER_QL: u8 = 10;
/// LP granted per branch pick (existing server policy, carried).
pub const TREE_PICK_LP: i32 = 5;
/// LP granted per stone pick (existing server policy, carried).
pub const STONE_PICK_LP: i32 = 3;
/// LP granted per ore pick (server policy: ore is rarer terrain than
/// stone, the pick grants proportionally more learning).
pub const ORE_PICK_LP: i32 = 8;
/// LP granted per clay pick (server policy: the shore trip is longer
/// than a stone walk, the pick grants a mid-tier amount of learning).
pub const CLAY_PICK_LP: i32 = 5;
/// LP granted per apple pick (server policy: fruit off a tree matches
/// the branch-pick learning of the same gob).
pub const APPLE_PICK_LP: i32 = 5;
/// LP granted per forage pick (server policy: between a stone pick and
/// a branch pick - the herb walk is short but the plants are sparser).
pub const FORAGE_PICK_LP: i32 = 4;
/// LP granted per honey harvest (server policy: foraging a hive pays
/// like an herb pick).
pub const HONEY_PICK_LP: i32 = 4;

/// Wild forageable plant registry (session 81, the dough ingredient
/// chains). Fixed data per kind: the world sprite, the item the pick
/// restores, the stack label (the fep.conf key keeps raw eating
/// alive), and the sdt byte for the world sprite (code-carrying plant
/// resources decode the sdt as a growth frame; 0 is the universally
/// valid first frame, 1 is the onion crop's own proven harvest stage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForageKind {
    Blueberry,
    Chantrelle,
    Grapevine,
    WildOnion,
}

impl ForageKind {
    /// World gob resource (gfx/terobjs/...).
    pub fn world_res(self) -> &'static str {
        match self {
            ForageKind::Blueberry => "gfx/terobjs/herbs/blueberry",
            ForageKind::Chantrelle => "gfx/terobjs/herbs/chantrelle",
            ForageKind::Grapevine => "gfx/terobjs/plants/wine",
            ForageKind::WildOnion => "gfx/terobjs/plants/onion",
        }
    }

    /// Inventory item resource the pick restores.
    pub fn item_res(self) -> &'static str {
        match self {
            ForageKind::Blueberry => "gfx/invobjs/bluberry",
            ForageKind::Chantrelle => "gfx/invobjs/shrooms-picked",
            ForageKind::Grapevine => "gfx/invobjs/grapes",
            ForageKind::WildOnion => "gfx/invobjs/onion",
        }
    }

    /// Stack display label; every one is a fep.conf key so the raw
    /// items are eatable (Blueberries / Chantrelles / Grapes /
    /// Yellow Onion).
    pub fn label(self) -> &'static str {
        match self {
            ForageKind::Blueberry => "Blueberries",
            ForageKind::Chantrelle => "Chantrelles",
            ForageKind::Grapevine => "Grapes",
            ForageKind::WildOnion => "Yellow Onion",
        }
    }

    /// sdt byte for the world sprite (growth-frame index the pack's
    /// plant code decodes; see the type docs for the per-kind choice).
    pub fn sdt(self) -> u8 {
        match self {
            ForageKind::Blueberry => 0,
            ForageKind::Chantrelle => 0,
            ForageKind::Grapevine => 0,
            // The onion crop proves stage 1 renders (its own harvest
            // stage); the wild patch shows the mature bulb.
            ForageKind::WildOnion => 1,
        }
    }
}

/// Metal-bearing ore a deposit yields (the metal chain's mining leg).
/// The item mapping is fixed registry data: `item_res` is the inventory
/// icon resource the pickup restores, `label` the display label the
/// smelter input dispatch keys on (craft::SMELT_MAP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OreKind {
    Copper,
    Tin,
    Iron,
}

impl OreKind {
    /// Deterministic worldgen pick from a per-tile roll (copper the
    /// common metal, tin the alloying partner, iron the deep-chain ore).
    pub fn from_roll(roll: u32) -> OreKind {
        match roll {
            0..=4 => OreKind::Copper,
            5..=7 => OreKind::Tin,
            _ => OreKind::Iron,
        }
    }

    /// Inventory icon resource restored on pickup.
    pub fn item_res(self) -> &'static str {
        match self {
            OreKind::Copper => "gfx/invobjs/nugget-copper",
            OreKind::Tin => "gfx/invobjs/nugget-tin",
            OreKind::Iron => "gfx/invobjs/ore-iron",
        }
    }

    /// Display label; the smelter (craft::SMELT_MAP) and the wire
    /// probes key on these exact strings.
    pub fn label(self) -> &'static str {
        match self {
            OreKind::Copper => "Copper Nugget",
            OreKind::Tin => "Tin Nugget",
            OreKind::Iron => "Iron Ore",
        }
    }
}

#[cfg(test)]
mod cluster_tests {
    use crate::state::*;
    /// Session 66 (metal chain): the ore roll is a pure distribution -
    /// copper the common metal (5/10), tin the partner (3/10), iron the
    /// deep-chain ore (2/10).
    #[test]
    fn ore_roll_distribution() {
        let mut copper = 0;
        let mut tin = 0;
        let mut iron = 0;
        for roll in 0..10 {
            match OreKind::from_roll(roll) {
                OreKind::Copper => copper += 1,
                OreKind::Tin => tin += 1,
                OreKind::Iron => iron += 1,
            }
        }
        assert_eq!((copper, tin, iron), (5, 3, 2), "per-tile ore mix");
        // Every ore's label keys the smelter map and every item res is a
        // known invobj (the full smelt side is pinned in craft.rs).
        for (ore, res) in [
            (OreKind::Copper, "gfx/invobjs/nugget-copper"),
            (OreKind::Tin, "gfx/invobjs/nugget-tin"),
            (OreKind::Iron, "gfx/invobjs/ore-iron"),
        ] {
            assert_eq!(ore.item_res(), res);
            assert!(!ore.label().is_empty());
        }
    }

    /// Session 66 (metal chain): the dev seed spawns reachable ore
    /// deposits near the spawn area - the mining leg must be playable,
    /// not theoretical. Populates the 3x3 grid block around the spawn
    /// grid and counts the deposit statics.
    #[test]
    fn dev_seed_spawns_ore_deposits_near_spawn() {
        let mut w = World::new(42);
        let mut spawned = Vec::new();
        for gy in -1..=1 {
            for gx in -1..=1 {
                w.populate_grid((gx, gy), None, &mut spawned);
            }
        }
        let deposits: Vec<usize> = spawned
            .iter()
            .filter_map(|id| w.gobs.get(*id))
            .filter(|s| matches!(w.gobs.kind[*s], Kind::OreDeposit { .. }))
            .collect();
        assert!(
            !deposits.is_empty(),
            "no ore deposits in the 3x3 spawn grid block on seed 42"
        );
        for s in deposits.iter().copied() {
            let Kind::OreDeposit { ore, left } = w.gobs.kind[s] else {
                unreachable!("filtered above");
            };
            assert!(!ore.label().is_empty(), "deposit carries its ore identity");
            assert_eq!(left, ORE_PICKS, "deposits spawn with the full pick supply");
        }
    }
}
