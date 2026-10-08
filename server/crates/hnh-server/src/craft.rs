//! Crafting, food and FEP mechanics.
//!
//! Implements the generic making protocol (crafting-and-building.md): the
//! server owns recipe data, ingredient validation and quality math; the
//! client only renders the `make` widget and relays `make 0/1` presses.
//! Also implements the food/FEP loop (food-and-fep.md): `etc/needed/fep.conf`
//! parsing, per-attribute accumulators, weighted attribute gain, HHP healing
//! and the `food` character-sheet widget message.

use std::collections::HashMap;

/// Base attribute touched by a FEP entry. `Hhp` is the special hard-HP
/// healing component documented in food-and-fep.md (not a real attribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FepAttr {
    Str,
    Agi,
    Int,
    Con,
    Per,
    Cha,
    Dex,
    Psy,
    Hhp,
}

impl FepAttr {
    /// Parse a fep.conf key (STR, AGI, INT, CON, PER, CHA, DEX, PSY, HHP).
    pub fn from_key(key: &str) -> Option<FepAttr> {
        Some(match key {
            "STR" => FepAttr::Str,
            "AGI" => FepAttr::Agi,
            "INT" => FepAttr::Int,
            "CON" => FepAttr::Con,
            "PER" => FepAttr::Per,
            "CHA" => FepAttr::Cha,
            "DEX" => FepAttr::Dex,
            "PSY" => FepAttr::Psy,
            "HHP" => FepAttr::Hhp,
            _ => return None,
        })
    }

    /// Attribute id in the CATTR attribute table (attributes-and-vitals.md).
    pub fn cattr_id(self) -> Option<&'static str> {
        Some(match self {
            FepAttr::Str => "str",
            FepAttr::Agi => "agi",
            FepAttr::Int => "int",
            FepAttr::Con => "con",
            FepAttr::Per => "per",
            FepAttr::Cha => "cha",
            FepAttr::Dex => "dex",
            FepAttr::Psy => "psy",
            FepAttr::Hhp => return None,
        })
    }

    /// Server-chosen segment color for the food bar (client default map is
    /// gray placeholders, Config.FEPColorMap; server picks per attribute).
    pub fn color(self) -> (u8, u8, u8, u8) {
        match self {
            FepAttr::Str => (220, 64, 64, 255),
            FepAttr::Agi => (96, 200, 96, 255),
            FepAttr::Int => (96, 128, 255, 255),
            FepAttr::Con => (220, 160, 64, 255),
            FepAttr::Per => (160, 96, 220, 255),
            FepAttr::Cha => (255, 200, 96, 255),
            FepAttr::Dex => (96, 220, 220, 255),
            FepAttr::Psy => (220, 96, 200, 255),
            FepAttr::Hhp => (255, 255, 255, 255),
        }
    }
}

/// Parsed `etc/needed/fep.conf` (Config.loadFEP format). Keys are lowercased
/// display names; values are (attribute, delta) pairs with fractional FEPs.
#[derive(Default)]
pub struct FepTable {
    by_name: HashMap<String, Vec<(FepAttr, f32)>>,
}

impl FepTable {
    /// Parse the config text. Lines are `DisplayName=ATTR:float ...`;
    /// comment/blank lines tolerated. Mirrors Config.loadFEP leniency.
    pub fn parse(text: &str) -> Result<FepTable, String> {
        let mut by_name = HashMap::new();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            let Some((name, rest)) = line.split_once('=') else {
                return Err(format!(
                    "fep.conf line {}: missing '=' separator",
                    lineno + 1
                ));
            };
            let key = name.trim().to_lowercase();
            if key.is_empty() {
                return Err(format!("fep.conf line {}: empty display name", lineno + 1));
            }
            let mut entries = Vec::new();
            for tok in rest.split_whitespace() {
                let Some((attr, val)) = tok.split_once(':') else {
                    return Err(format!(
                        "fep.conf line {}: token {:?} is not ATTR:float",
                        lineno + 1,
                        tok
                    ));
                };
                let attr = FepAttr::from_key(attr).ok_or_else(|| {
                    format!("fep.conf line {}: unknown attribute {attr:?}", lineno + 1)
                })?;
                let val: f32 = val
                    .parse()
                    .map_err(|_| format!("fep.conf line {}: bad float {val:?}", lineno + 1))?;
                entries.push((attr, val));
            }
            if entries.is_empty() {
                return Err(format!("fep.conf line {}: no FEP entries", lineno + 1));
            }
            by_name.insert(key, entries);
        }
        if by_name.is_empty() {
            return Err("fep.conf contains no food entries".to_owned());
        }
        Ok(FepTable { by_name })
    }

    /// Look up a food by display name (case-insensitive, per Config.loadFEP
    /// lowercasing; Item.name() precedence documented in food-and-fep.md).
    pub fn get(&self, display_name: &str) -> Option<&[(FepAttr, f32)]> {
        self.by_name
            .get(&display_name.to_lowercase())
            .map(|v| v.as_slice())
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }
}

/// A hand-crafting recipe. Static server data; ids match the `ad` argument
/// of the corresponding `paginae/craft/*` resource so the client's
/// `act("craft", <id>)` resolves here.
#[derive(Debug, Clone, Copy)]
pub struct Recipe {
    /// Menu action id (`act("craft", id)`), e.g. "axe".
    pub id: &'static str,
    /// Display name pushed as the makewindow factory argument.
    pub name: &'static str,
    /// (resource name, count) inputs, consumed lowest-quality-first.
    pub inputs: &'static [(&'static str, u32)],
    /// (resource name, count) outputs.
    pub outputs: &'static [(&'static str, u32)],
    /// Pagina resource to push at login so the entry renders in MenuGrid.
    pub pagina: &'static str,
    /// Attribute used as the quality softcap skill (loftar rule: if the
    /// "skill" quality is below the ingredient average, the two are averaged).
    pub softcap_attr: &'static str,
    /// Tool resource that must be held in the inventory or equipment for
    /// the craft to fire (session 46 tool plumbing; the craft pagina
    /// "prereq" code is the advisory hint - e.g. the bucket pagina's
    /// "crp" carpentry prereq pairs with the saw here). `None` = hands.
    /// Server policy: the legacy per-recipe tool lists are not
    /// recoverable from this pack, so tool fields are chosen per recipe
    /// and recorded in crafting-and-building.md Open questions.
    pub tool: Option<&'static str>,
    /// Per-INPUT-TYPE quality weights (RoB Legacy:Quality: `q = sum(q_i * w_i)
    /// / sum(w_i)` over ingredient TYPES). Empty = weight by consumed UNIT
    /// count (the pre-session-36 behavior). Per-type weights decouple the
    /// quality math from the ingredient counts: RoB's Wooden Bow formula
    /// `(qBranches + qString) / 2` averages the two types equally even
    /// though the recipe takes several branches and one string.
    pub q_weights: &'static [u32],
}

impl Recipe {
    /// One-line provenance note for docs and tests: where the quality
    /// model comes from.
    #[cfg(test)]
    pub fn q_note(&self) -> &'static str {
        if self.q_weights.is_empty() {
            "unit-weighted average (pre-36 behavior)"
        } else {
            "type-weighted average (RoB Legacy:Quality)"
        }
    }
}

/// Recipes implemented this session. Ingredient/output resources must exist
/// in the served resource pack (verified against lib/haven-res.jar).
pub const RECIPES: &[Recipe] = &[
    Recipe {
        id: "axe",
        name: "Stone axe",
        inputs: &[("gfx/invobjs/branch", 1), ("gfx/invobjs/stone", 1)],
        outputs: &[("gfx/invobjs/axe", 1)],
        pagina: "paginae/craft/axe",
        softcap_attr: "str",
        tool: None,
        q_weights: &[],
    },
    // First armor entry into the economy: two cow hides sew into a hide
    // cloak (gfx/invobjs/cloak-hide, armor::PIECES). Gives the armor
    // class pipeline a craftable source end to end.
    Recipe {
        id: "hcloak",
        name: "Hide cloak",
        inputs: &[("gfx/invobjs/hide-raw-cow", 2)],
        outputs: &[("gfx/invobjs/cloak-hide", 1)],
        pagina: "paginae/craft/hcloak",
        softcap_attr: "dex",
        tool: None,
        q_weights: &[],
    },
    // Session 36: the bow chain. RoB Legacy:Bow verifies the ingredient
    // TYPES (branches + string) and the quality formula
    // `(qBranches + qString) / 2, softcapped by Marksmanship` (the
    // Fandom Marksmanship page carries the same worked example,
    // (50 + 40) / 2). The per-type weights [1, 1] implement that average
    // independent of the consumed unit counts. The unit counts
    // (4 branches, 1 string) are a chosen server policy pending legacy
    // verification - recorded in crafting-and-building.md Open questions.
    Recipe {
        id: "woodbow",
        name: "Wooden Bow",
        inputs: &[("gfx/invobjs/branch", 4), ("gfx/invobjs/string", 1)],
        outputs: &[("gfx/invobjs/bow", 1)],
        pagina: "paginae/craft/woodbow",
        softcap_attr: "ranged",
        tool: None,
        q_weights: &[1, 1],
    },
    // Stone arrows: RoB Legacy:Quality documents the arrow example as a
    // weighted average with a HEAVIER WEIGHT ON BRANCH than the tip
    // material; Survival softcaps arrows. Batch of 10 per craft (chosen
    // server policy, documented as an open question). Weights [1, 2]
    // give the branch the heavier share.
    Recipe {
        id: "stonearrow",
        name: "Stone Arrow",
        inputs: &[("gfx/invobjs/stone", 1), ("gfx/invobjs/branch", 2)],
        outputs: &[("gfx/invobjs/arrow-stone", 10)],
        pagina: "paginae/craft/stonearrow",
        softcap_attr: "survive",
        tool: None,
        q_weights: &[1, 2],
    },
    // Bone arrows: same weighted model as stone arrows (RoB Legacy:Quality
    // cites the bone-arrow example for the branch-heavier rule). Bones
    // enter the economy through animal loot (state.rs Species::loot).
    Recipe {
        id: "bonearrow",
        name: "Bone Arrow",
        inputs: &[("gfx/invobjs/bone", 1), ("gfx/invobjs/branch", 2)],
        outputs: &[("gfx/invobjs/arrow-bone", 10)],
        pagina: "paginae/craft/bonearrow",
        softcap_attr: "survive",
        tool: None,
        q_weights: &[1, 2],
    },
    // Session 37: the quiver. The craft pagina (paginae/craft/quiver,
    // tooltip "Quiver With Arrows", ad ["craft","quiver"]) links the
    // Leather Working skill page, and namu.wiki's H&H tech page places
    // the quiver in the BACK slot carrying arrows. The exact legacy
    // unit recipe is not recoverable from the blocked wikis - the
    // 2 hides + 1 string counts and the [1,1] type weights are a
    // documented server policy (crafting-and-building.md Open
    // questions), mirroring the hide-cloak economy (2 cow hides).
    Recipe {
        id: "quiver",
        name: "Quiver",
        inputs: &[("gfx/invobjs/hide-raw-cow", 2), ("gfx/invobjs/string", 1)],
        outputs: &[("gfx/invobjs/quiver", 1)],
        pagina: "paginae/craft/quiver",
        softcap_attr: "ranged",
        tool: None,
        q_weights: &[1, 1],
    },
    // Session 45: the gear-chain batch - four previously unreachable pack
    // pieces made craftable. The pagina resources ship with the legacy pack
    // and their action layers pin the ids (ad strings) and the display
    // parents: rope (paginae/craft/cloth, prereq "ahusb"), waterskin
    // (paginae/craft/tools, prereq "hunting"), backpack and poorbelt
    // (paginae/craft/leather, prereq "leather"). Prerequisite strings stay
    // advisory like every other recipe (the softcap attribute stands in);
    // ingredient counts are a chosen server policy (crafting-and-building.md
    // Open questions) - the legacy wikis stay unreachable for verification.
    // Rope matters beyond cosmetics: animals-and-husbandry.md names a Rope
    // equipped as the weapon as a taming precondition.
    Recipe {
        id: "rope",
        name: "Rope",
        inputs: &[("gfx/invobjs/string", 3)],
        outputs: &[("gfx/invobjs/rope", 1)],
        pagina: "paginae/craft/rope",
        softcap_attr: "survive",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "waterskin",
        name: "Waterskin",
        inputs: &[("gfx/invobjs/hide-raw-cow", 2), ("gfx/invobjs/string", 1)],
        outputs: &[("gfx/invobjs/waterskin", 1)],
        pagina: "paginae/craft/waterskin",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[2, 1],
    },
    Recipe {
        id: "backpack",
        name: "Backpack",
        inputs: &[("gfx/invobjs/hide-raw-cow", 3), ("gfx/invobjs/string", 2)],
        outputs: &[("gfx/invobjs/backpack", 1)],
        pagina: "paginae/craft/backpack",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[2, 1],
    },
    Recipe {
        id: "poorbelt",
        name: "Poor Man's Belt",
        inputs: &[("gfx/invobjs/hide-raw-cow", 1), ("gfx/invobjs/string", 1)],
        outputs: &[("gfx/invobjs/belt-poor", 1)],
        pagina: "paginae/craft/poorbelt",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[2, 1],
    },
    // Session 46: the cloth chain (wool -> yarn -> linen cloth -> wearable
    // shirt/pants) plus the bucket. Every pagina id comes from the shipped
    // action layers (ad = ["craft", id], verified by scanning the res
    // bytes: yarn/linencloth/linenpants/linenshirt/bucket), and every
    // input/output invobj resource ships in the pack. Wool enters the
    // economy through the session-46 sheep/mouflon loot rows (state.rs).
    // Unit counts are chosen server policy pending legacy verification
    // (crafting-and-building.md Open questions).
    Recipe {
        id: "yarn",
        name: "Yarn",
        inputs: &[("gfx/invobjs/wool", 1)],
        outputs: &[("gfx/invobjs/yarn", 1)],
        pagina: "paginae/craft/yarn",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "cloth",
        name: "Linen Cloth",
        inputs: &[("gfx/invobjs/yarn", 2)],
        outputs: &[("gfx/invobjs/linencloth", 1)],
        pagina: "paginae/craft/linencloth",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "shirt",
        name: "Linen shirt",
        inputs: &[("gfx/invobjs/linencloth", 3)],
        outputs: &[("gfx/invobjs/linenshirt", 1)],
        pagina: "paginae/craft/linenshirt",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "pants",
        name: "Linen pants",
        inputs: &[("gfx/invobjs/linencloth", 3)],
        outputs: &[("gfx/invobjs/linenpants", 1)],
        pagina: "paginae/craft/linenpants",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    // The bucket pagina pins ad ["craft", "bucket"] with the carpentry
    // ("crp") prereq code; the saw is the matching tool (server policy).
    // The empty bucket resource is gfx/invobjs/buckete.
    Recipe {
        id: "bucket",
        name: "Bucket",
        inputs: &[("gfx/invobjs/branch", 3)],
        outputs: &[("gfx/invobjs/buckete", 1)],
        pagina: "paginae/craft/bucket",
        softcap_attr: "carpentry",
        tool: Some("gfx/invobjs/saw"),
        q_weights: &[1],
    },
    //
    // Session 58: the breadth batch. Every pagina below ships in the
    // legacy pack except string/tanhide (see the fork-page note on
    // those). Ingredient resources were verified present in
    // lib/haven-res.jar (gfx/invobjs); unit counts stay a chosen server
    // policy recorded in crafting-and-building.md Open questions.
    //
    // Stone/bone tools. The saw closes the session-46 bucket loop: the
    // bucket demanded a saw no recipe produced. Tool texts follow the
    // RoB tool pages' material lists (branch + stone family).
    Recipe {
        id: "saw",
        name: "Saw",
        inputs: &[("gfx/invobjs/branch", 2), ("gfx/invobjs/stone", 1)],
        outputs: &[("gfx/invobjs/saw", 1)],
        pagina: "paginae/craft/saw",
        softcap_attr: "carpentry",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "bonesaw",
        name: "Bone Saw",
        inputs: &[
            ("gfx/invobjs/bone", 1),
            ("gfx/invobjs/branch", 1),
            ("gfx/invobjs/string", 1),
        ],
        outputs: &[("gfx/invobjs/saw-bone", 1)],
        pagina: "paginae/craft/bonesaw",
        softcap_attr: "survive",
        tool: None,
        q_weights: &[1, 1, 1],
    },
    // Pickaxe: pagina ad ["craft", "pickaxe"] (page file paxe.res).
    Recipe {
        id: "pickaxe",
        name: "Pickaxe",
        inputs: &[("gfx/invobjs/branch", 1), ("gfx/invobjs/stone", 2)],
        outputs: &[("gfx/invobjs/paxe", 1)],
        pagina: "paginae/craft/paxe",
        softcap_attr: "explore",
        tool: None,
        q_weights: &[1, 2],
    },
    Recipe {
        id: "scythe",
        name: "Scythe",
        inputs: &[("gfx/invobjs/branch", 2), ("gfx/invobjs/stone", 2)],
        outputs: &[("gfx/invobjs/scythe", 1)],
        pagina: "paginae/craft/scythe",
        softcap_attr: "farming",
        tool: None,
        q_weights: &[1, 1],
    },
    // Farm-tier headwear: straw comes from the wheat early harvest,
    // pumpkins grow as the pumpkin crop, sprucecap is branch-woven.
    Recipe {
        id: "shat",
        name: "Straw Hat",
        inputs: &[("gfx/invobjs/straw", 3)],
        outputs: &[("gfx/invobjs/shat", 1)],
        pagina: "paginae/craft/shat",
        softcap_attr: "farming",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "pumpkinhat",
        name: "Pumpkin Hat",
        inputs: &[("gfx/invobjs/pumpkin", 1)],
        outputs: &[("gfx/invobjs/pumpkinhat", 1)],
        pagina: "paginae/craft/pumpkinhat",
        softcap_attr: "farming",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "sprucecap",
        name: "Sprucecap",
        inputs: &[("gfx/invobjs/branch", 2)],
        outputs: &[("gfx/invobjs/hat-sprucecap", 1)],
        pagina: "paginae/craft/sprucecap",
        softcap_attr: "survive",
        tool: None,
        q_weights: &[1],
    },
    // Woodworking: kuksa is the carved cup; it is the first recipe that
    // CONSUMES the saw (tool field) rather than merely demanding it.
    Recipe {
        id: "kuksa",
        name: "Kuksa",
        inputs: &[("gfx/invobjs/branch", 1)],
        outputs: &[("gfx/invobjs/kuksa", 1)],
        pagina: "paginae/craft/kuksa",
        softcap_attr: "carpentry",
        tool: Some("gfx/invobjs/saw"),
        q_weights: &[1],
    },
    // Fishing-gear items (fishing itself remains future work; the pack
    // pages and icons exist, so the gear is craftable).
    Recipe {
        id: "fpole",
        name: "Fishing Pole",
        inputs: &[("gfx/invobjs/branch", 1), ("gfx/invobjs/string", 1)],
        outputs: &[("gfx/invobjs/fpole", 1)],
        pagina: "paginae/craft/fpole",
        softcap_attr: "survive",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "bonehook",
        name: "Bone Hook",
        inputs: &[("gfx/invobjs/bone", 1), ("gfx/invobjs/branch", 1)],
        outputs: &[("gfx/invobjs/hook-bone", 1)],
        pagina: "paginae/craft/bonehook",
        softcap_attr: "survive",
        tool: None,
        q_weights: &[1, 1],
    },
    // Linen tier: cloth conversions on top of the session-46 chain.
    Recipe {
        id: "toga",
        name: "Toga",
        inputs: &[("gfx/invobjs/linencloth", 4)],
        outputs: &[("gfx/invobjs/toga", 1)],
        pagina: "paginae/craft/toga",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "tophat",
        name: "Cylinder Hat",
        inputs: &[("gfx/invobjs/linencloth", 3)],
        outputs: &[("gfx/invobjs/hat-top", 1)],
        pagina: "paginae/craft/tophat",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "gauze",
        name: "Gauze",
        inputs: &[("gfx/invobjs/linencloth", 1)],
        outputs: &[("gfx/invobjs/gauze", 1)],
        pagina: "paginae/craft/gauze",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    // String: the pack economy consumes string (bow, arrows, rope,
    // waterskin, backpack, quiver) but ships no producing page - the
    // fork paginae/craft/string (res/compiled, built by
    // server/scripts/make_fork_paginae.py) carries the invobj icon and
    // ad ["craft", "string"]. Flax fibres come from the flax/hemp
    // early harvest (farm.rs).
    Recipe {
        id: "string",
        name: "String",
        inputs: &[("gfx/invobjs/flaxfibre", 2)],
        outputs: &[("gfx/invobjs/string", 1)],
        pagina: "paginae/craft/string",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    // Leather: the 2009 pack's tanning-tub station is not implemented,
    // so the tanhide fork page (icon from gfx/invobjs/leather, ad
    // ["craft", "tanhide"]) gives the hide->leather conversion a home
    // page in the leather category. The legacy tub formula
    // (3*hide + bark + water + tub)/6 stays the reference in
    // crafting-and-building.md; this recipe is the hand-tier stand-in.
    Recipe {
        id: "tanhide",
        name: "Leather",
        inputs: &[("gfx/invobjs/hide-raw-cow", 2)],
        outputs: &[("gfx/invobjs/leather", 1)],
        pagina: "paginae/craft/tanhide",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    // Leather tier: boots, pants, cloak, waterskin - the pages ship
    // with the pack (paginae/craft/{lboots,lpants,lcloak,waterflask}).
    Recipe {
        id: "lboots",
        name: "Leather boots",
        inputs: &[("gfx/invobjs/leather", 2), ("gfx/invobjs/string", 1)],
        outputs: &[("gfx/invobjs/lboots", 1)],
        pagina: "paginae/craft/lboots",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[2, 1],
    },
    Recipe {
        id: "lpants",
        name: "Leather pants",
        inputs: &[("gfx/invobjs/leather", 3)],
        outputs: &[("gfx/invobjs/lpants", 1)],
        pagina: "paginae/craft/lpants",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "lcloak",
        name: "Leather Cloak",
        inputs: &[("gfx/invobjs/leather", 3), ("gfx/invobjs/string", 1)],
        outputs: &[("gfx/invobjs/cloak-leather", 1)],
        pagina: "paginae/craft/lcloak",
        softcap_attr: "sewing",
        tool: None,
        q_weights: &[2, 1],
    },
    Recipe {
        id: "waterflask",
        name: "Waterflask",
        inputs: &[("gfx/invobjs/leather", 1), ("gfx/invobjs/string", 1)],
        outputs: &[("gfx/invobjs/waterflask", 1)],
        pagina: "paginae/craft/waterflask",
        softcap_attr: "survive",
        tool: None,
        q_weights: &[1, 1],
    },
];

/// Raw -> roasted meat mapping for the `roast` recipe (paginae/craft/roastmeat,
/// ad = ["craft", "roast"]). Keys are the raw item display labels; values the
/// roasted labels; both must exist in fep.conf (food-and-fep.md provenance).
pub const ROAST_MAP: &[(&str, &str)] = &[
    ("Beef", "Roasted Beef"),
    ("Raw Deer Meat", "Roasted Deer Meat"),
    ("Raw Chicken Meat", "Roasted Chicken Meat"),
    ("Raw Mutton", "Roasted Mutton"),
    ("Raw Pork", "Roast Pork"),
    ("Boar Meat", "Roasted Boar Meat"),
    ("Bear Meat", "Roasted Bear Meat"),
    ("Fox Meat", "Roasted Fox Meat"),
    ("Rabbit Meat", "Roasted Rabbit Meat"),
];

pub fn roast_result(raw_label: &str) -> Option<&'static str> {
    ROAST_MAP
        .iter()
        .find(|(raw, _)| raw.eq_ignore_ascii_case(raw_label))
        .map(|(_, roasted)| *roasted)
}

/// Ore -> metal bar mapping for the smelter station (build.rs
/// StationKind::Smelter). Keys are the ore display labels the server
/// assigns to world-gathered ore (state::OreKind::label); values are
/// (inventory resource, display label) of the smelted output.
///
/// Server policy (mechanics doc, "Production stations"): the legacy
/// pack's smelted-metal set is covered by the bars the economy reaches -
/// copper and tin smelt to their bars, iron ore smelts to cast iron (the
/// legacy finery-forge leg that would refine cast iron into wrought iron
/// is not built; see the doc's Open questions). Bronze alloying is an
/// open question, not invented data.
pub const SMELT_MAP: &[(&str, (&str, &str))] = &[
    ("Copper Nugget", ("gfx/invobjs/bar-copper", "Bar of Copper")),
    ("Tin Nugget", ("gfx/invobjs/bar-tin", "Bar of Tin")),
    ("Iron Ore", ("gfx/invobjs/bar-castiron", "Bar of Cast Iron")),
];

pub fn smelt_result(raw_label: &str) -> Option<(&'static str, &'static str)> {
    SMELT_MAP
        .iter()
        .find(|(raw, _)| raw.eq_ignore_ascii_case(raw_label))
        .map(|(_, out)| *out)
}

/// FEP accumulator state per player (integer tenths per attribute; the wire
/// `food` message carries tenths, CharWnd divides by 10 for display).
#[derive(Debug, Default, Clone)]
pub struct FepState {
    /// Accumulated tenths per attribute wire id.
    pub acc: HashMap<&'static str, i32>,
}

impl FepState {
    /// Apply one food's FEP vector scaled by quality (qmult = sqrt(q/10),
    /// Item.calcFEP / Item.java:412). Returns the tenths added per attribute
    /// (excluding Hhp which is handled as direct healing by the caller).
    pub fn grant(&mut self, feps: &[(FepAttr, f32)], quality: u8) -> Vec<(&'static str, i32)> {
        // qmult matches the client's Math.sqrt(q / 10) so tooltips cannot lie.
        let qmult = ((quality as f32) / 10.0).sqrt().max(0.1);
        let mut granted = Vec::new();
        for (attr, delta) in feps {
            let Some(attr) = FepAttr::cattr_id(*attr) else {
                continue; // Hhp: caller heals the hard pool directly
            };
            let tenths = (*delta * qmult * 10.0).round() as i32;
            if tenths > 0 {
                *self.acc.entry(attr).or_insert(0) += tenths;
                granted.push((attr, tenths));
            }
        }
        granted
    }

    /// Total accumulated tenths.
    pub fn total(&self) -> i32 {
        self.acc.values().sum()
    }

    /// Weighted draw of which attribute rises when the requirement is met:
    /// each attribute's share of the accumulated FEPs (fandom FEP page).
    pub fn pick_gain(&self, rng: &mut impl FnMut() -> u32) -> Option<&'static str> {
        let total = self.total();
        if total <= 0 {
            return None;
        }
        let mut roll = (rng() % (total as u32 + 1)) as i32;
        let ids: Vec<&'static str> = self.acc.keys().copied().collect();
        let mut sorted = ids;
        sorted.sort_unstable(); // deterministic iteration order
        let picked = sorted.iter().find_map(|id| {
            let v = self.acc[id];
            if roll < v {
                Some(*id)
            } else {
                roll -= v;
                None
            }
        });
        picked.or_else(|| sorted.last().copied())
    }

    /// Reset after a successful attribute gain (overflow lost, fandom FEP).
    pub fn reset(&mut self) {
        self.acc.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# comment line
Bark Bread=CON:5 HHP:0.2
Bear Salami=STR:4 CHA:3
Peapod=STR:0.1 PER:0.9
";

    #[test]
    fn fep_table_parses_and_matches_case_insensitively() {
        let t = FepTable::parse(SAMPLE).expect("parse");
        assert_eq!(t.len(), 3);
        let got = t.get("bear salami").expect("bear salami");
        assert_eq!(got, &[(FepAttr::Str, 4.0), (FepAttr::Cha, 3.0)]);
        assert!(t.get("bark bread").is_some());
        assert!(t.get("Peapod").is_some());
        assert!(t.get("missing food").is_none());
    }

    #[test]
    fn fep_table_rejects_malformed_lines() {
        assert!(FepTable::parse("no separator here").is_err());
        assert!(FepTable::parse("Food=XYZ:1").is_err());
        assert!(FepTable::parse("Food=STR:abc").is_err());
        assert!(FepTable::parse("Food=").is_err());
        assert!(FepTable::parse("").is_err());
    }

    #[test]
    fn grant_scales_by_quality_sqrt_law() {
        let mut st = FepState::default();
        let granted = st.grant(&[(FepAttr::Str, 4.0)], 10);
        // Quality 10 => qmult 1.0 => 40 tenths.
        assert_eq!(granted, vec![("str", 40)]);
        let mut st2 = FepState::default();
        let granted2 = st2.grant(&[(FepAttr::Str, 4.0)], 40);
        // Quality 40 => qmult 2.0 => 80 tenths (Item.calcFEP parity).
        assert_eq!(granted2, vec![("str", 80)]);
        let mut st3 = FepState::default();
        let granted3 = st3.grant(&[(FepAttr::Str, 0.1)], 10);
        assert_eq!(granted3, vec![("str", 1)]);
    }

    #[test]
    fn grant_skips_hhp_and_honors_variety_accumulation() {
        let mut st = FepState::default();
        let feps = [(FepAttr::Con, 5.0), (FepAttr::Hhp, 0.2)];
        st.grant(&feps, 10);
        assert_eq!(st.total(), 50);
        assert!(st.acc.contains_key("con"));
        assert!(!st.acc.contains_key("hhp"));
    }

    #[test]
    fn pick_gain_is_weighted_and_resets() {
        let mut st = FepState::default();
        st.grant(&[(FepAttr::Str, 4.0)], 10); // 40 tenths
        st.grant(&[(FepAttr::Cha, 3.0)], 10); // 30 tenths
        let mut seq = || 0u32; // deterministic rng stand-in: always 0
        let first = st.pick_gain(&mut seq).expect("pick");
        assert_eq!(first, "cha"); // roll 0 < 30 => first sorted id wins
        st.reset();
        assert_eq!(st.total(), 0);
        assert!(st.pick_gain(&mut seq).is_none());
    }

    #[test]
    fn roast_map_resolves_display_names() {
        assert_eq!(roast_result("beef"), Some("Roasted Beef"));
        assert_eq!(roast_result("Raw Deer Meat"), Some("Roasted Deer Meat"));
        assert_eq!(roast_result("Stone"), None);
    }

    /// Session 66 (metal chain): every smelter input the world spawns
    /// (state::OreKind labels) resolves to a bar, and non-ore items
    /// refuse. When the gameres pack is locatable (generated next to the
    /// repo root; a fresh clone may not have it - the wire gate must run
    /// without it), the bar resource AND its world-shape render path
    /// (own terobjs shape or the game.rs alias table) are pinned too.
    #[test]
    fn smelt_map_covers_the_world_ore_mix() {
        // Locate the pack once; tests never require it, so a missing
        // pack only skips the filesystem-dependent pins.
        if crate::resources::RES_DIR.get().is_none() {
            let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gameres");
            if pack.is_dir() {
                crate::resources::init_res_dir(pack);
            }
        }
        let pack_available = crate::resources::RES_DIR.get().is_some();
        for ore in [
            crate::state::OreKind::Copper,
            crate::state::OreKind::Tin,
            crate::state::OreKind::Iron,
        ] {
            let (res, label) =
                smelt_result(ore.label()).unwrap_or_else(|| panic!("{} must smelt", ore.label()));
            assert!(res.starts_with("gfx/invobjs/bar-"), "{label}: bar res");
            if !pack_available {
                continue;
            }
            assert!(
                crate::resources::served(res),
                "{res} must exist in the served pack"
            );
            // The pack ships world shapes for only some bars; every
            // smelted output must resolve through its own shape or the
            // alias table (never the branch fallback, which would look
            // wrong on the ground).
            let base = res.rsplit('/').next().unwrap_or(res);
            let own = format!("gfx/terobjs/items/{base}");
            assert!(
                crate::resources::served(&own) || crate::game::drop_world_alias(base).is_some(),
                "{res}: no world shape and no alias"
            );
        }
        assert_eq!(smelt_result("Stone"), None);
        assert_eq!(smelt_result("Beef"), None);
    }

    /// Session 36: the three new recipes resolve, carry RoB-verified
    /// per-type quality weights, and the bone-arrow ingredient is fed by
    /// the animal loot table (state.rs Species::loot drops bones for
    /// every species).
    #[test]
    fn bow_chain_recipes_are_consistent() {
        for (id, weights, softcap) in [
            ("woodbow", &[1, 1][..], "ranged"),
            ("stonearrow", &[1, 2][..], "survive"),
            ("bonearrow", &[1, 2][..], "survive"),
        ] {
            let r = RECIPES.iter().find(|r| r.id == id).unwrap_or_else(|| {
                panic!("{id} must be registered");
            });
            assert_eq!(r.q_weights, weights, "{id} per-type weights");
            assert_eq!(r.softcap_attr, softcap, "{id} softcap attribute");
            // Inputs and outputs must be distinct resources with at
            // least one input (a no-input recipe would be a free item
            // fountain).
            assert!(!r.inputs.is_empty(), "{id} has inputs");
            assert!(!r.outputs.is_empty(), "{id} has outputs");
        }
        // Every animal species drops bones so the bone-arrow recipe has
        // an in-world source (state.rs Session 36 loot extension).
        for sp in crate::state::Species::ALL {
            let loot = sp.loot();
            assert!(
                loot.iter().any(|(res, _, _)| *res == "gfx/invobjs/bone"),
                "{sp:?} must drop bones for the bone-arrow economy"
            );
        }
    }

    /// The provenance note distinguishes the two quality models so docs
    /// and future sessions can tell them apart at a glance.
    #[test]
    fn q_note_marks_type_weighted_recipes() {
        let bow = RECIPES.iter().find(|r| r.id == "woodbow").unwrap();
        assert!(bow.q_note().starts_with("type-weighted"));
        let axe = RECIPES.iter().find(|r| r.id == "axe").unwrap();
        assert!(axe.q_note().starts_with("unit-weighted"));
    }

    /// Session 37: the quiver recipe is type-weighted like the bow
    /// ((qHide + qString)/2, ranged softcap), outputs the equippable
    /// gfx/invobjs/quiver (the PIECES table renders gfx/borka/quiver
    /// back layers), and its craft pagina exists in the resource pack.
    #[test]
    fn quiver_recipe_and_back_layers_are_wired() {
        let q = RECIPES.iter().find(|r| r.id == "quiver").unwrap();
        assert_eq!(q.inputs.len(), 2);
        assert_eq!(q.outputs, &[("gfx/invobjs/quiver", 1)]);
        assert_eq!(q.pagina, "paginae/craft/quiver");
        assert_eq!(q.softcap_attr, "ranged");
        assert_eq!(q.q_weights, &[1, 1]);
        assert!(q.q_note().starts_with("type-weighted"));
        // The resource pack ships the craft pagina and the avatar
        // back-layer directory the PIECES entry maps onto (skip when
        // the pack is absent from the test sandbox - same pattern as
        // the equip.rs pose test).
        let pack = std::path::Path::new("../../gameres");
        if pack.is_dir() {
            assert!(pack.join("paginae/craft/quiver.res").exists());
            assert!(pack.join("gfx/borka/quiver/standing").exists());
        }
        // equip.rs PIECES maps the invobj onto gfx/borka/quiver layers
        // for standing AND walking poses (verified through the same
        // world_layers path the server streams to the client).
        let layers = crate::equip::world_layers(["gfx/invobjs/quiver"].iter(), false, 1);
        assert!(
            layers
                .iter()
                .all(|l| l.starts_with("gfx/borka/quiver/standing/")),
            "standing back layers: {layers:?}"
        );
        let walking = crate::equip::world_layers(["gfx/invobjs/quiver"].iter(), true, 1);
        assert!(
            walking
                .iter()
                .all(|l| l.starts_with("gfx/borka/quiver/walking/")),
            "walking back layers: {walking:?}"
        );
    }

    /// Session 45: the gear-chain batch (rope, waterskin, backpack,
    /// poorbelt). Ids match the `ad` strings parsed out of the shipped
    /// pagina action layers; every output resource exists in the pack;
    /// the two avatar pieces map onto real borka layer directories.
    #[test]
    fn gear_chain_recipes_are_wired() {
        for (id, pagina, softcap, weights) in [
            ("rope", "paginae/craft/rope", "survive", &[1][..]),
            (
                "waterskin",
                "paginae/craft/waterskin",
                "sewing",
                &[2, 1][..],
            ),
            ("backpack", "paginae/craft/backpack", "sewing", &[2, 1][..]),
            ("poorbelt", "paginae/craft/poorbelt", "sewing", &[2, 1][..]),
        ] {
            let r = RECIPES
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id} must be registered"));
            assert_eq!(r.pagina, pagina, "{id} pagina id matches the ad string");
            assert_eq!(r.softcap_attr, softcap, "{id} softcap attribute");
            assert_eq!(r.q_weights, weights, "{id} per-type weights");
            assert!(!r.inputs.is_empty() && !r.outputs.is_empty(), "{id} shaped");
        }
        // Every referenced resource must exist in the pack so the client
        // renders inputs, outputs and the menu pagina (skip when absent).
        let pack = std::path::Path::new("../../gameres");
        if pack.is_dir() {
            for id in ["rope", "waterskin", "backpack", "poorbelt"] {
                let rec = RECIPES.iter().find(|x| x.id == id).unwrap();
                assert!(
                    pack.join(format!("{}.res", rec.pagina)).exists(),
                    "{} res shipped",
                    rec.pagina
                );
                for (res, _) in rec.inputs.iter().chain(rec.outputs.iter()) {
                    assert!(pack.join(format!("{res}.res")).exists(), "{res} shipped");
                }
            }
        }
        // The two avatar pieces render through the PIECES table (the
        // same world_layers path the server streams for equipment).
        let belt = crate::equip::world_layers(["gfx/invobjs/belt-poor"].iter(), false, 1);
        assert!(
            belt.iter().any(|l| l.contains("belt-poor")),
            "belt layers: {belt:?}"
        );
        let bpk = crate::equip::world_layers(["gfx/invobjs/backpack"].iter(), false, 1);
        assert!(
            bpk.iter().any(|l| l.contains("backpack")),
            "backpack layers: {bpk:?}"
        );
    }

    /// Session 46: the cloth chain + bucket recipes are wired (pagina
    /// ids verified against the shipped action layers), the sheep loot
    /// feeds the wool input, the two wearables render through the
    /// PIECES table, and the bucket pins the saw tool requirement.
    #[test]
    fn cloth_chain_and_bucket_are_wired() {
        for (id, pagina, softcap, tool) in [
            ("yarn", "paginae/craft/yarn", "sewing", None),
            ("cloth", "paginae/craft/linencloth", "sewing", None),
            ("shirt", "paginae/craft/linenshirt", "sewing", None),
            ("pants", "paginae/craft/linenpants", "sewing", None),
            (
                "bucket",
                "paginae/craft/bucket",
                "carpentry",
                Some("gfx/invobjs/saw"),
            ),
        ] {
            let r = RECIPES
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id} must be registered"));
            assert_eq!(r.pagina, pagina, "{id} pagina id");
            assert_eq!(r.softcap_attr, softcap, "{id} softcap");
            assert_eq!(r.tool, tool, "{id} tool requirement");
            assert!(!r.inputs.is_empty() && !r.outputs.is_empty(), "{id} shaped");
        }
        // The wool -> yarn -> cloth -> wearable chain has no dead ends:
        // every non-terminal output is the next stage's input resource.
        for (made, consumed_by) in [
            ("gfx/invobjs/yarn", "cloth"),
            ("gfx/invobjs/linencloth", "shirt"),
        ] {
            assert!(
                RECIPES
                    .iter()
                    .any(|r| r.id == consumed_by && r.inputs.iter().any(|(res, _)| *res == made)),
                "{made} must feed {consumed_by}"
            );
        }
        // Sheep/mouflon loot carries the wool that starts the chain.
        for sp in [crate::state::Species::Mouflon, crate::state::Species::Sheep] {
            let loot = sp.loot();
            assert!(
                loot.iter().any(|(res, _, _)| *res == "gfx/invobjs/wool"),
                "{sp:?} must drop wool"
            );
        }
        // The two wearables render on the avatar through PIECES.
        let shirt = crate::equip::world_layers(["gfx/invobjs/linenshirt"].iter(), false, 1);
        assert!(
            shirt.iter().any(|l| l.contains("shirt-linen")),
            "linen shirt layers: {shirt:?}"
        );
        let pants = crate::equip::world_layers(["gfx/invobjs/linenpants"].iter(), false, 1);
        assert!(
            pants.iter().any(|l| l.contains("pants-linen")),
            "linen pants layers: {pants:?}"
        );
        // Every new input/output/pagina resource ships in the pack
        // (skip when the pack is absent from the test sandbox).
        let pack = std::path::Path::new("../../gameres");
        if pack.is_dir() {
            for id in ["yarn", "cloth", "shirt", "pants", "bucket"] {
                let rec = RECIPES.iter().find(|x| x.id == id).unwrap();
                assert!(
                    pack.join(format!("{}.res", rec.pagina)).exists(),
                    "{} res shipped",
                    rec.pagina
                );
                for (res, _) in rec.inputs.iter().chain(rec.outputs.iter()) {
                    assert!(pack.join(format!("{res}.res")).exists(), "{res} shipped");
                }
            }
            assert!(
                pack.join("gfx/invobjs/saw.res").exists(),
                "bucket tool resource shipped"
            );
        }
    }

    /// Species morphology table (session 46): the doc's morph pairs that
    /// the 2009 pack can render; the boar stays a boar (no pig kritter).
    #[test]
    fn species_morphs_follow_the_doc() {
        use crate::state::Species;
        assert_eq!(Species::Mouflon.morph(), Some(Species::Sheep));
        assert_eq!(Species::Aurochs.morph(), Some(Species::Cow));
        assert_eq!(Species::Boar.morph(), None, "no pig drawable in the pack");
        assert_eq!(Species::Sheep.morph(), None, "domestic forms do not morph");
        assert_eq!(Species::Cow.morph(), None);
        // Wire indices stay append-only for the node link.
        assert_eq!(Species::Mouflon.index(), 7);
        assert_eq!(Species::Sheep.index(), 8);
        assert_eq!(Species::from_index(7), Some(Species::Mouflon));
        assert_eq!(Species::from_index(8), Some(Species::Sheep));
        // The sheep family drops Raw Mutton (fep.conf verified).
        assert_eq!(Species::Sheep.meat_label(), "Raw Mutton");
    }
}
