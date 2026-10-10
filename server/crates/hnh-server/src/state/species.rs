//! The animal species registry: drawables, vitals, loot and the
//! wire-stable discriminants.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Species {
    Deer,
    Fox,
    Wolf,
    Boar,
    Cow,
    Hare,
    Aurochs,
    /// Session 46: the third tameable wild beast (animals-and-husbandry
    /// .md taming list). The pack spells the directory "mufflon".
    Mouflon,
    /// Domestic morph of the mouflon; never spawns wild.
    Sheep,
    /// Session 77 (the sausage chain): the apex predator of the forest
    /// belt. Its meat feeds the Bear Salami / Big Bear Banger recipes and
    /// its carcass carries the doc's Intestines x4 butcher yield.
    Bear,
    /// Session 77: the only source of Raw Chicken Meat (fep.conf
    /// HHP:5) for the wurst-less end of the cooking economy. Spawns
    /// wild at the same weight policy as the rest of the roster until
    /// the Chicken Coop building exists (doc Open questions).
    Hen,
}

impl Species {
    /// Every species variant, for tests that need to sweep the whole
    /// roster (e.g. the session-36 bone-loot consistency check in
    /// craft.rs).
    #[cfg(test)]
    pub const ALL: [Species; 11] = [
        Species::Deer,
        Species::Fox,
        Species::Wolf,
        Species::Boar,
        Species::Cow,
        Species::Hare,
        Species::Aurochs,
        Species::Mouflon,
        Species::Sheep,
        Species::Bear,
        Species::Hen,
    ];

    /// Stable discriminant carried on the node link (GuestKind::Animal);
    /// the subscriber resolves it back with `from_index`. Appends only:
    /// values 0-6 are already live on the node link, renumbering would
    /// corrupt cross-node guests until every node is upgraded.
    pub fn index(self) -> u8 {
        match self {
            Species::Deer => 0,
            Species::Fox => 1,
            Species::Wolf => 2,
            Species::Boar => 3,
            Species::Cow => 4,
            Species::Hare => 5,
            Species::Aurochs => 6,
            Species::Mouflon => 7,
            Species::Sheep => 8,
            Species::Bear => 9,
            Species::Hen => 10,
        }
    }

    /// Inverse of `index` (unknown indices reject at the ingest boundary).
    pub fn from_index(v: u8) -> Option<Species> {
        Some(match v {
            0 => Species::Deer,
            1 => Species::Fox,
            2 => Species::Wolf,
            3 => Species::Boar,
            4 => Species::Cow,
            5 => Species::Hare,
            6 => Species::Aurochs,
            7 => Species::Mouflon,
            8 => Species::Sheep,
            9 => Species::Bear,
            10 => Species::Hen,
            _ => return None,
        })
    }

    pub fn resname(self) -> &'static str {
        // The concrete drawable resource per species. Kritter directories
        // carry plalay pose routers ("gfx/korka"-style) the fork client
        // cannot resolve - the <dir>/cdv resource is the image+neg sprite
        // the legacy server actually layered, so spawn that instead.
        match self {
            Species::Deer => "gfx/kritter/deer/cdv",
            Species::Fox => "gfx/kritter/fox/cdv",
            Species::Wolf => "gfx/kritter/wolf/cdv",
            Species::Boar => "gfx/kritter/boar/cdv",
            Species::Cow => "gfx/kritter/cow/cdv",
            Species::Hare => "gfx/kritter/hare/cdv",
            Species::Aurochs => "gfx/kritter/aurochs/cdv",
            Species::Mouflon => "gfx/kritter/mufflon/cdv",
            Species::Sheep => "gfx/kritter/sheep/cdv",
            Species::Bear => "gfx/kritter/bear/cdv",
            Species::Hen => "gfx/kritter/hen/cdv",
        }
    }

    pub fn max_hp(self) -> i32 {
        match self {
            Species::Hare => 20,
            Species::Deer | Species::Fox => 40,
            Species::Boar | Species::Cow => 70,
            Species::Wolf => 60,
            Species::Aurochs => 90,
            // Tameable wild beasts sit between the deer and the boar;
            // the doc carries no verified numbers (server policy).
            Species::Mouflon => 45,
            Species::Sheep => 40,
            // Apex predator: the doc's butcher list treats the bear as
            // the heaviest common carcass (Meat x8); hp follows (policy).
            Species::Bear => 120,
            Species::Hen => 10,
        }
    }

    pub fn aggressive(self) -> bool {
        // The bear joins the wolves and boars: the wiki's creature table
        // lists it as an attacker-on-sight (server policy until a
        // verified aggro table exists).
        matches!(self, Species::Wolf | Species::Boar | Species::Bear)
    }

    pub fn speed(self) -> i32 {
        match self {
            Species::Hare => 60,
            Species::Deer => 55,
            Species::Fox => 50,
            Species::Wolf => 48,
            Species::Boar => 40,
            Species::Cow | Species::Aurochs => 30,
            Species::Mouflon => 50,
            Species::Sheep => 30,
            Species::Bear => 40,
            Species::Hen => 20,
        }
    }

    /// Loot dropped on death: (resource name, count, display label).
    /// Session 36: every species also drops bones - the Bone Arrow recipe
    /// (craft.rs) needs gfx/invobjs/bone, and in legacy every butchered
    /// carcass yielded bones; counts are a chosen server policy recorded
    /// in animals-and-husbandry.md.
    /// Session 77: Intestines follow the doc's butcher table verbatim
    /// (Aurochs/Cattle x4, Bear x4, Deer x3, Boar x2, Sheep x2, Fox x1;
    /// the mouflon row is undocumented - policy 1) - the casing item of
    /// the sausage chain (craft.rs wurst recipes). Species whose doc row
    /// lists no intestines (Wolf, Hare, Hen) drop none.
    pub fn loot(self) -> Vec<(&'static str, u32, &'static str)> {
        match self {
            Species::Deer => vec![
                ("gfx/invobjs/meat", 3, self.meat_label()),
                ("gfx/invobjs/hide-raw-fox", 2, ""),
                ("gfx/invobjs/intestines", 3, ""),
                ("gfx/invobjs/bone", 2, ""),
            ],
            Species::Aurochs => vec![
                ("gfx/invobjs/meat", 3, self.meat_label()),
                ("gfx/invobjs/hide-raw-fox", 2, ""),
                ("gfx/invobjs/intestines", 4, ""),
                ("gfx/invobjs/bone", 2, ""),
            ],
            Species::Cow => vec![
                ("gfx/invobjs/meat", 4, self.meat_label()),
                ("gfx/invobjs/hide-raw-cow", 3, ""),
                ("gfx/invobjs/intestines", 4, ""),
                ("gfx/invobjs/bone", 2, ""),
            ],
            Species::Boar => vec![
                ("gfx/invobjs/meat", 3, self.meat_label()),
                ("gfx/invobjs/intestines", 2, ""),
                ("gfx/invobjs/bone", 2, ""),
            ],
            Species::Fox => vec![
                ("gfx/invobjs/meat", 1, self.meat_label()),
                ("gfx/invobjs/hide-raw-fox", 1, ""),
                ("gfx/invobjs/intestines", 1, ""),
                ("gfx/invobjs/bone", 1, ""),
            ],
            Species::Wolf => vec![
                ("gfx/invobjs/meat", 2, self.meat_label()),
                ("gfx/invobjs/bone", 2, ""),
            ],
            Species::Hare => vec![
                ("gfx/invobjs/meat", 1, self.meat_label()),
                ("gfx/invobjs/bone", 1, ""),
            ],
            // The mouflon rows mirror the doc's butcher list (Raw Sheep
            // Skin, Raw Mutton); bones follow the all-species policy.
            Species::Mouflon | Species::Sheep => vec![
                ("gfx/invobjs/meat", 2, self.meat_label()),
                ("gfx/invobjs/hide-raw-sheep", 1, ""),
                ("gfx/invobjs/wool", 1, ""),
                (
                    "gfx/invobjs/intestines",
                    if self == Species::Sheep { 2 } else { 1 },
                    "",
                ),
                ("gfx/invobjs/bone", 1, ""),
            ],
            // Session 77: the bear mirrors its doc butcher row (Meat x8,
            // Intestines x4, Raw Bear Hide, bones at the heavy-carcass
            // policy 4; the doc's Bear Tooth has no item resource in the
            // pack - recorded in Open questions).
            Species::Bear => vec![
                ("gfx/invobjs/meat", 8, self.meat_label()),
                ("gfx/invobjs/hide-raw-bear", 1, ""),
                ("gfx/invobjs/intestines", 4, ""),
                ("gfx/invobjs/bone", 4, ""),
            ],
            // The hen row: Raw Chicken Meat + Chicken Feather x3 + bones;
            // the doc's coop-grid egg pipeline is not implemented yet.
            Species::Hen => vec![
                ("gfx/invobjs/meat", 1, self.meat_label()),
                ("gfx/invobjs/feather-chicken", 3, ""),
                ("gfx/invobjs/bone", 1, ""),
            ],
        }
    }

    /// Short species name for chat lines (ranged combat feedback).
    pub fn name(self) -> &'static str {
        match self {
            Species::Deer => "Deer",
            Species::Fox => "Fox",
            Species::Wolf => "Wolf",
            Species::Boar => "Boar",
            Species::Cow => "Cow",
            Species::Hare => "Hare",
            Species::Aurochs => "Aurochs",
            Species::Mouflon => "Mouflon",
            Species::Sheep => "Sheep",
            Species::Bear => "Bear",
            Species::Hen => "Hen",
        }
    }

    /// Server-sent display name for this species' raw meat. Values match
    /// fep.conf keys so eating resolves FEPs (food-and-fep.md: the server's
    /// item tooltip names must match the table keys). Wolf meat has no
    /// legacy fep.conf entry; the empty label falls back to the resource
    /// tooltip and grants no FEPs rather than inventing numbers.
    pub fn meat_label(self) -> &'static str {
        match self {
            Species::Deer => "Raw Deer Meat",
            Species::Cow | Species::Aurochs => "Beef",
            Species::Boar => "Boar Meat",
            Species::Fox => "Fox Meat",
            Species::Hare => "Rabbit Meat",
            Species::Wolf => "",
            // fep.conf verifies "Raw Mutton" (HHP:1) for the sheep family.
            Species::Mouflon | Species::Sheep => "Raw Mutton",
            // Session 77: both fep.conf keys - the bear meat rows (Bear
            // Salami's source) and the only Raw Chicken Meat source.
            Species::Bear => "Bear Meat",
            Species::Hen => "Raw Chicken Meat",
        }
    }

    /// The domestic morph at full tameness (animals-and-husbandry.md:
    /// boar->pig, mouflon->sheep, aurochs->cow/bull). The 2009 pack
    /// ships NO pig kritter and no standalone bull drawable that the
    /// cdv pipeline can layer, so the boar maps to None (it stays a
    /// boar at full tameness - recorded in the doc's Open questions)
    /// and the aurochs morphs to the cow cdv.
    pub fn morph(self) -> Option<Species> {
        match self {
            Species::Mouflon => Some(Species::Sheep),
            Species::Aurochs => Some(Species::Cow),
            _ => None,
        }
    }

    // Name-driven lookup kept for spawn-table and tooling use; gameplay code
    // addresses species by enum value directly.
    #[allow(dead_code)]
    pub fn from_name(name: &str) -> Option<Species> {
        Some(match name {
            "deer" => Species::Deer,
            "fox" => Species::Fox,
            "wolf" => Species::Wolf,
            "boar" => Species::Boar,
            "cow" => Species::Cow,
            "hare" => Species::Hare,
            "aurochs" => Species::Aurochs,
            // The pack directory spelling, plus the wiki spelling.
            "mufflon" | "mouflon" => Species::Mouflon,
            "sheep" => Species::Sheep,
            "bear" => Species::Bear,
            "hen" => Species::Hen,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod cluster_tests {
    use super::*;
    /// Species index round-trip (the node-link discriminant contract).
    /// Values 0-6 predate session 46 and are frozen on the wire; 7-8
    /// (mouflon, sheep) appended; 9-10 (bear, hen) append in session 77.
    #[test]
    fn species_index_roundtrips() {
        for i in 0..11u8 {
            let sp = Species::from_index(i).expect("valid index");
            assert_eq!(sp.index(), i);
        }
        assert!(Species::from_index(11).is_none());
        assert!(Species::from_index(255).is_none());
    }
}
