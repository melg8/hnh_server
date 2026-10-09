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
    // Session 66 metal-chain refinement tier, on top of the ore ->
    // bar smelting leg. The SHIPPED pagina bloom2wrought (ad
    // "wroughtiron") refines cast iron into wrought iron - legacy ran
    // this step on a finery forge; a hand-craft entry is the
    // tanhide-pattern stand-in until that station exists. Unit count
    // 1:1 is server policy (Open questions).
    Recipe {
        id: "wroughtiron",
        name: "Wrought Iron",
        inputs: &[("gfx/invobjs/bar-castiron", 1)],
        outputs: &[("gfx/invobjs/bar-wroughtiron", 1)],
        pagina: "paginae/craft/bloom2wrought",
        softcap_attr: "str",
        tool: None,
        q_weights: &[],
    },
    // Smithy's Hammer: the first metal tool (paginae/craft/shammer,
    // ad "shammer"; gfx/invobjs/hammer-smithys ships). A wrought-iron
    // bar + a branch handle; the anvil-era recipes will tool-gate on
    // it through the session-46 plumbing later.
    Recipe {
        id: "shammer",
        name: "Smithy's Hammer",
        inputs: &[
            ("gfx/invobjs/bar-wroughtiron", 1),
            ("gfx/invobjs/branch", 1),
        ],
        outputs: &[("gfx/invobjs/hammer-smithys", 1)],
        pagina: "paginae/craft/shammer",
        softcap_attr: "str",
        tool: None,
        q_weights: &[1, 1],
    },
    // Session 71: the baking chain's hand craft - flour + water kneads
    // into dough (paginae/craft/dough ships in the pack, ad
    // ["craft", "dough"]; the legacy button name "Bread" covers the
    // whole dough hand shape). The dough bakes in the oven
    // (craft::BAKE_MAP). Water enters the economy through the
    // bucket-fill mechanic (a Bucket itemact on a water tile,
    // game/items.rs). The empty bucket returns alongside the dough -
    // the recipe consumes one Bucket of Water and kneads it all in.
    // Unit counts are server policy: 2 flour -> 2 dough keeps a flat
    // 1:1 flour-to-dough mass balance. Softcap: Cooking caps
    // Perception (RoB Legacy:Quality pairing).
    Recipe {
        id: "dough",
        name: "Bread Dough",
        inputs: &[("gfx/invobjs/flour", 2), ("gfx/invobjs/bucket-water", 1)],
        outputs: &[("gfx/invobjs/dough", 2), ("gfx/invobjs/buckete", 1)],
        pagina: "paginae/craft/dough",
        softcap_attr: "per",
        tool: None,
        q_weights: &[3, 1],
    },
    // Session 77: the sausage branch (paginae/craft/sausages -> 13
    // wurst-*.res pages, ad "craft|wurst_<id>"). Twelve of the thirteen
    // legacy wurst pages become recipes; Piglet Wursts stays out - its
    // Raw Pork input has no source until the pig morph ships (the pack
    // ships no pig kritter; recorded in Open questions). Inputs follow
    // the doc's meat-per-wurst pairing (Fox Wurst <- Fox Meat, Cow
    // Chorizo <- Beef, ...); the counts and the equal per-type quality
    // weights are server policy (no verified legacy numbers - Open
    // questions), Intestines x1-2 is the universal casing. Softcap:
    // Cooking caps Perception, matching the dough entry. fep.conf
    // carries a FEP row for every implemented label - all twelve eat
    // (the Chicken Chorizo and Bierwurst keys have no item resource in
    // the pack and stay unimplemented; food-and-fep.md provenance).
    Recipe {
        id: "wurst_fox",
        name: "Fox Wurst",
        inputs: &[("gfx/invobjs/meat", 2), ("gfx/invobjs/intestines", 1)],
        outputs: &[("gfx/invobjs/wurst-fox", 1)],
        pagina: "paginae/craft/wurst-fox",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_foxfuet",
        name: "Fox Fuet",
        inputs: &[("gfx/invobjs/meat", 3), ("gfx/invobjs/intestines", 1)],
        outputs: &[("gfx/invobjs/wurst-foxfuet", 1)],
        pagina: "paginae/craft/wurst-foxfuet",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_boarbaloney",
        name: "Boar Baloney",
        inputs: &[("gfx/invobjs/meat", 2), ("gfx/invobjs/intestines", 1)],
        outputs: &[("gfx/invobjs/wurst-boarbaloney", 1)],
        pagina: "paginae/craft/wurst-boarbaloney",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_boarboudin",
        name: "Boar Boudin",
        inputs: &[("gfx/invobjs/meat", 3), ("gfx/invobjs/intestines", 2)],
        outputs: &[("gfx/invobjs/wurst-boarboudin", 1)],
        pagina: "paginae/craft/wurst-boarboudin",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_cowchorizo",
        name: "Cow Chorizo",
        inputs: &[("gfx/invobjs/meat", 3), ("gfx/invobjs/intestines", 1)],
        outputs: &[("gfx/invobjs/wurst-cowchorizo", 1)],
        pagina: "paginae/craft/wurst-cowchorizo",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_ddd",
        name: "Delicious Deer Dog",
        inputs: &[("gfx/invobjs/meat", 2), ("gfx/invobjs/intestines", 1)],
        outputs: &[("gfx/invobjs/wurst-ddd", 1)],
        pagina: "paginae/craft/wurst-ddd",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_bearsalami",
        name: "Bear Salami",
        inputs: &[("gfx/invobjs/meat", 3), ("gfx/invobjs/intestines", 1)],
        outputs: &[("gfx/invobjs/wurst-bearsalami", 1)],
        pagina: "paginae/craft/wurst-bearsalami",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_bigbearbanger",
        name: "Big Bear Banger",
        inputs: &[("gfx/invobjs/meat", 5), ("gfx/invobjs/intestines", 2)],
        outputs: &[("gfx/invobjs/wurst-bigbearbanger", 1)],
        pagina: "paginae/craft/wurst-bigbearbanger",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_lambsausages",
        name: "Lamb Sausages",
        inputs: &[("gfx/invobjs/meat", 2), ("gfx/invobjs/intestines", 1)],
        outputs: &[("gfx/invobjs/wurst-lambsausages", 1)],
        pagina: "paginae/craft/wurst-lambsausages",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_runrabbit",
        name: "Running Rabbit Sausage",
        inputs: &[("gfx/invobjs/meat", 2), ("gfx/invobjs/intestines", 1)],
        outputs: &[("gfx/invobjs/wurst-runningrabbit", 1)],
        pagina: "paginae/craft/wurst-runningrabbit",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    // The mixed-meat pair: the liverwurst is the domestic blend (Beef +
    // Raw Mutton), the WWW the wild blend (Bear + Deer + Fox) - the
    // doc's "tame game" vs "wonderful wilderness" wording.
    Recipe {
        id: "wurst_tamegame",
        name: "Tame Game Liverwurst",
        inputs: &[("gfx/invobjs/meat", 4), ("gfx/invobjs/intestines", 2)],
        outputs: &[("gfx/invobjs/wurst-tamegame", 1)],
        pagina: "paginae/craft/wurst-tamegame",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    Recipe {
        id: "wurst_www",
        name: "Wonderful Wilderness Wurst",
        inputs: &[("gfx/invobjs/meat", 3), ("gfx/invobjs/intestines", 2)],
        outputs: &[("gfx/invobjs/wurst-www", 1)],
        pagina: "paginae/craft/wurst-www",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1, 1],
    },
    // Session 79: the pottery branch (paginae/craft/ceramics parents the
    // four molding pages; ad args extracted from the action layers:
    // mug->mugdough, jar->jardough, teapot->teapotdough, treepot->
    // treepotdough). Clay is obtainable (shore clay deposits, session 69);
    // RoB Legacy pins Jar = Clay x3 and Treeplanter's Pot = Clay x10, the
    // Mug x2 / Teapot x5 counts are server policy (the legacy pages record
    // no ratios - Open questions). The outputs are the UNBURNT wares
    // (tooltips read from the dough-*.res resources); they fire in the
    // kiln through craft::KILN_MAP - the same station path the brick leg
    // drives since session 70. Softcap: molding is a Dexterity job
    // (ceramics-adjacent crafting; RoB pairs Pottery with DEX/PSY - the
    // doc records the policy).
    Recipe {
        id: "mugdough",
        name: "Unburnt Clay Mug",
        inputs: &[("gfx/invobjs/clay", 2)],
        outputs: &[("gfx/invobjs/dough-mug", 1)],
        pagina: "paginae/craft/mug",
        softcap_attr: "dex",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "jardough",
        name: "Unburnt Jar",
        inputs: &[("gfx/invobjs/clay", 3)],
        outputs: &[("gfx/invobjs/dough-jar", 1)],
        pagina: "paginae/craft/jar",
        softcap_attr: "dex",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "teapotdough",
        name: "Unburnt Teapot",
        inputs: &[("gfx/invobjs/clay", 5)],
        outputs: &[("gfx/invobjs/dough-pot-tea", 1)],
        pagina: "paginae/craft/teapot",
        softcap_attr: "dex",
        tool: None,
        q_weights: &[1],
    },
    Recipe {
        id: "treepotdough",
        name: "Unburnt Treeplanter's Pot",
        inputs: &[("gfx/invobjs/clay", 10)],
        outputs: &[("gfx/invobjs/dough-treeplanterspot", 1)],
        pagina: "paginae/craft/treepot",
        softcap_attr: "dex",
        tool: None,
        q_weights: &[1],
    },
    // Session 79: the butter leg (paginae/craft/butter, ad
    // ["craft", "butter"], parent paginae/craft/cooking). Milk rides the
    // bucket-grant flow (game/animals.rs milking, session 47); one filled
    // bucket churns into one Butter and returns the empty bucket - the
    // same bucket-return shape the dough recipe runs under. The legacy
    // Churn is a buildable terobj (paginae/build/churn ships in the pack)
    // but the hand shape keeps the chain playable without a new station
    // kind; the deviation is recorded in mechanics/crafting-and-building.md.
    // Softcap: Cooking caps Perception (the dough recipe's pairing).
    Recipe {
        id: "butter",
        name: "Butter",
        inputs: &[("gfx/invobjs/bucket-milk", 1)],
        outputs: &[("gfx/invobjs/butter", 1), ("gfx/invobjs/buckete", 1)],
        pagina: "paginae/craft/butter",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1],
    },
    // Session 79: the baking breadth lands through Carrot Cake - the ONE
    // pie whose every ingredient the economy reaches today (Carrot from
    // the carrot crop, Butter from the butter leg above, Flour/Water from
    // the session-71 chain). The remaining five dough pages ship their
    // oven mappings (craft::BAKE_MAP) but stay recipe-less: Apple /
    // Blueberries / Honey / Raisins / Chanterelles have no production
    // source yet (the Piglet Wursts policy - no dead recipes). Unit
    // counts are server policy: one dough per cake (the legacy 0.5 L
    // water dough batch), Carrot x2 per the RoB Legacy page. The page's
    // ad name is "Carrot Cake" (paginae/craft/ccdough); the crafted
    // stack label carries the dough name the oven dispatch keys on.
    Recipe {
        id: "ccdough",
        name: "Carrot Cake Dough",
        inputs: &[
            ("gfx/invobjs/flour", 2),
            ("gfx/invobjs/bucket-water", 1),
            ("gfx/invobjs/carrot", 2),
            ("gfx/invobjs/butter", 1),
        ],
        outputs: &[
            ("gfx/invobjs/dough-cake-carrot", 2),
            ("gfx/invobjs/buckete", 1),
        ],
        pagina: "paginae/craft/ccdough",
        softcap_attr: "per",
        tool: None,
        q_weights: &[3, 1, 2, 2],
    },
    // Session 81: the five dough ingredient chains close the baking
    // breadth (the S79 dead ends). Apple pies and the raisin
    // butter-cake mirror the carrot-cake shape (fruit + butter over
    // the flour-and-water base); the counts are server policy where
    // the legacy pages record no ratios, matched to the ccdough
    // precedent. Apples come off wild apple trees (state::Kind::
    // FruitTree), raisins from the sun-dried hand recipe below.
    Recipe {
        id: "apdough",
        name: "Apple Pie Dough",
        inputs: &[
            ("gfx/invobjs/flour", 2),
            ("gfx/invobjs/bucket-water", 1),
            ("gfx/invobjs/apple", 2),
            ("gfx/invobjs/butter", 1),
        ],
        outputs: &[
            ("gfx/invobjs/dough-pie-apple", 2),
            ("gfx/invobjs/buckete", 1),
        ],
        pagina: "paginae/craft/apdough",
        softcap_attr: "per",
        tool: None,
        q_weights: &[3, 1, 2, 2],
    },
    // Blueberry pies are the butter-less pie shape (RoB Legacy:
    // blueberries over dough); berries grow on forest/heath bushes.
    Recipe {
        id: "dough_blueberrypie",
        name: "Blueberry Pie Dough",
        inputs: &[
            ("gfx/invobjs/flour", 2),
            ("gfx/invobjs/bucket-water", 1),
            ("gfx/invobjs/bluberry", 3),
        ],
        outputs: &[
            ("gfx/invobjs/dough-pie-blueberry", 2),
            ("gfx/invobjs/buckete", 1),
        ],
        pagina: "paginae/craft/dough_blueberrypie",
        softcap_attr: "per",
        tool: None,
        q_weights: &[3, 1, 2],
    },
    // Honeybuns take TWO filled buckets (water + honey); both empty
    // buckets come back - the multi-output grant merges them into one
    // buckete stack.
    Recipe {
        id: "hbdough",
        name: "Honeybun Dough",
        inputs: &[
            ("gfx/invobjs/flour", 2),
            ("gfx/invobjs/bucket-water", 1),
            ("gfx/invobjs/bucket-honey", 1),
        ],
        outputs: &[
            ("gfx/invobjs/dough-bun-honey", 2),
            ("gfx/invobjs/buckete", 2),
        ],
        pagina: "paginae/craft/hbdough",
        softcap_attr: "per",
        tool: None,
        q_weights: &[3, 1, 2],
    },
    Recipe {
        id: "rbcdough",
        name: "Raisin Butter-cake Dough",
        inputs: &[
            ("gfx/invobjs/flour", 2),
            ("gfx/invobjs/bucket-water", 1),
            ("gfx/invobjs/raisins", 2),
            ("gfx/invobjs/butter", 1),
        ],
        outputs: &[
            ("gfx/invobjs/dough-cake-raisinbutter", 2),
            ("gfx/invobjs/buckete", 1),
        ],
        pagina: "paginae/craft/rbcdough",
        softcap_attr: "per",
        tool: None,
        q_weights: &[3, 1, 2, 2],
    },
    // Pirozhki (Chantrelle & Onion): the mushroom-and-onion savory
    // pie. Chanterelles come off forest patches, onions off wild
    // grass patches (the pirozhki chain's own seed source).
    Recipe {
        id: "dough_pirozhki",
        name: "Pirozhki Dough",
        inputs: &[
            ("gfx/invobjs/flour", 2),
            ("gfx/invobjs/bucket-water", 1),
            ("gfx/invobjs/shrooms-picked", 2),
            ("gfx/invobjs/onion", 2),
        ],
        outputs: &[
            ("gfx/invobjs/dough-pirozhki", 2),
            ("gfx/invobjs/buckete", 1),
        ],
        pagina: "paginae/craft/dough_pirozhki",
        softcap_attr: "per",
        tool: None,
        q_weights: &[3, 1, 2, 2],
    },
    // Session 81: the raisin leg. Legacy dries grapes on a drying
    // frame over two in-game days; this server keeps the chain
    // playable with the hand shape (the S79 butter precedent - the
    // deviation is recorded in crafting-and-building.md). Two grape
    // bunches sun-dry into one raisin pack.
    Recipe {
        id: "raisins",
        name: "Raisins",
        inputs: &[("gfx/invobjs/grapes", 2)],
        outputs: &[("gfx/invobjs/raisins", 1)],
        pagina: "paginae/craft/raisins",
        softcap_attr: "per",
        tool: None,
        q_weights: &[1],
    },
];

/// The one inventory resource every raw meat rides on; the species is
/// told apart by the stack's display label (state::Species::meat_label).
pub const MEAT_RES: &str = "gfx/invobjs/meat";

/// Session 77: the wurst recipes' meat-slot labels. The wurst inputs
/// must key on the DISPLAY LABEL, not just the resource - without the
/// label gate a Fox Wurst would happily grind Beef. Keys are recipe
/// ids; values are (label, count) slots that REPLACE the recipe's
/// generic meat input: the slot counts must sum exactly to that
/// recipe's (MEAT_RES, N) line (pinned by a unit test below). The
/// mixed blends carry two slots (tame game = Beef + Raw Mutton,
/// wilderness = Bear + Deer); the labels are fep.conf-verified keys.
pub const WURST_MEAT_SLOTS: &[(&str, &[(&str, u32)])] = &[
    ("wurst_fox", &[("Fox Meat", 2)]),
    ("wurst_foxfuet", &[("Fox Meat", 3)]),
    ("wurst_boarbaloney", &[("Boar Meat", 2)]),
    ("wurst_boarboudin", &[("Boar Meat", 3)]),
    ("wurst_cowchorizo", &[("Beef", 3)]),
    ("wurst_ddd", &[("Raw Deer Meat", 2)]),
    ("wurst_bearsalami", &[("Bear Meat", 3)]),
    ("wurst_bigbearbanger", &[("Bear Meat", 5)]),
    ("wurst_lambsausages", &[("Raw Mutton", 2)]),
    ("wurst_runrabbit", &[("Rabbit Meat", 2)]),
    ("wurst_tamegame", &[("Beef", 2), ("Raw Mutton", 2)]),
    ("wurst_www", &[("Bear Meat", 1), ("Raw Deer Meat", 2)]),
];

/// The meat-slot list of a wurst recipe, `None` for every other recipe
/// (game/craft.rs keys the per-label validation + consumption on it).
pub fn meat_slots(recipe_id: &str) -> Option<&'static [(&'static str, u32)]> {
    WURST_MEAT_SLOTS
        .iter()
        .find(|(id, _)| *id == recipe_id)
        .map(|(_, slots)| *slots)
}

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
/// is not built; see the doc's Open questions).
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

/// Clay -> fired wares mapping for the kiln station (build.rs
/// StationKind::Kiln). Session 69 fired the brick leg; session 79 adds
/// the pottery legs - the unburnt wares molded by the hand recipes
/// (mugdough/jardough/teapotdough/treepotdough) fire into their finished
/// forms. Keys are the unburnt display labels (the dough-*.res tooltips;
/// the treeplanterspot dough ships no tooltip layer, so its label is the
/// recipe's server policy name), values are (inventory resource, display
/// label). One ware per firing, the brick unit policy. Fired wares ride
/// the station quality formula (2*q_item + q_kiln + q_fuel)/4.
pub const KILN_MAP: &[(&str, (&str, &str))] = &[
    ("Clay", ("gfx/invobjs/brick", "Brick")),
    ("Unburnt Clay Mug", ("gfx/invobjs/mug", "Clay Mug")),
    ("Unburnt Jar", ("gfx/invobjs/jar", "Clay Jar")),
    ("Unburnt Teapot", ("gfx/invobjs/pot-tea", "Teapot")),
    (
        "Unburnt Treeplanter's Pot",
        ("gfx/invobjs/treeplanterspot", "Treeplanter's Pot"),
    ),
];

pub fn kiln_result(raw_label: &str) -> Option<(&'static str, &'static str)> {
    KILN_MAP
        .iter()
        .find(|(raw, _)| raw.eq_ignore_ascii_case(raw_label))
        .map(|(_, out)| *out)
}

/// Grist -> flour mapping for the quern station (build.rs
/// StationKind::Quern, session 71 baking chain). Legacy Quern: "grinds
/// grain into flour"; the pack ships no grain item, so the quern takes
/// Grist of Wheat (the farm's mature wheat product, farm.rs) and grinds
/// it into Flour. One grist per job (the kiln's one-clay-per-brick
/// policy carried over; the legacy page records no ratio). Output
/// quality follows the station formula with no fuel term (a quern is
/// hand-cranked, fuel_quality() stays 0).
pub const GRIND_MAP: &[(&str, (&str, &str))] =
    &[("Grist of Wheat", ("gfx/invobjs/flour", "Flour"))];

pub fn grind_result(raw_label: &str) -> Option<(&'static str, &'static str)> {
    GRIND_MAP
        .iter()
        .find(|(raw, _)| raw.eq_ignore_ascii_case(raw_label))
        .map(|(_, out)| *out)
}

/// Dough -> baked goods mapping for the oven (build.rs
/// StationKind::Oven). Session 71 shipped the Bread leg; session 79 adds
/// the full dough set the pack ships - apple/blueberry pies, carrot cake,
/// raisin butter-cake, honeybun and the pirozhki. The output LABELS are
/// the fep.conf keys (eating resolves through FepTable::get), not
/// necessarily the resource tooltips: pie-blueberry.res carries the
/// legacy "Bluberry Pie" typo and honeybun.res says "Honeybun" while
/// fep.conf keys "Blueberry Pie" and "Honey Bun" - the server label
/// wins, so both bake AND eat. Five of the six doughs still have no
/// production source (see the RECIPES session-79 note) - the mappings
/// make the oven ready the moment their gathering chains land.
///
/// (StationKind::Oven; the session-71 chain: the oven roasts raw meat
/// through craft::ROAST_MAP AND bakes dough - a dough label in BAKE_MAP
/// bakes into the mapped item resource instead. Legacy Bread:
/// flour-and-water dough baked in an oven; one dough per loaf, unit
/// count is server policy. The recipe hand shape (Flour + Water ->
/// Dough) is the craft.rs "dough" recipe.)
pub const BAKE_MAP: &[(&str, (&str, &str))] = &[
    ("Bread Dough", ("gfx/invobjs/bread", "Bread")),
    ("Apple Pie Dough", ("gfx/invobjs/pie-apple", "Apple Pie")),
    (
        "Blueberry Pie Dough",
        ("gfx/invobjs/pie-blueberry", "Blueberry Pie"),
    ),
    (
        "Carrot Cake Dough",
        ("gfx/invobjs/cake-carrot", "Carrot Cake"),
    ),
    (
        "Raisin Butter-cake Dough",
        ("gfx/invobjs/cake-raisinbutter", "Raisin Butter-Cake"),
    ),
    ("Honeybun Dough", ("gfx/invobjs/honeybun", "Honey Bun")),
    (
        "Pirozhki Dough",
        ("gfx/invobjs/feast-pirozhki", "Chantrelle & Onion Pirozhki"),
    ),
];

pub fn bake_result(raw_label: &str) -> Option<(&'static str, &'static str)> {
    BAKE_MAP
        .iter()
        .find(|(raw, _)| raw.eq_ignore_ascii_case(raw_label))
        .map(|(_, out)| *out)
}

/// Alloying Crucible charge (build.rs StationKind::Alloyer), the bronze
/// leg of the metal chain. Legacy Ring of Brodgar: 2 Bars of Copper + 1
/// Bar of Tin smelt into 3 Bars of Bronze - a 1:1 metal-to-bronze mass
/// balance. This server's station holds one input slot plus one aux
/// slot, so the charge is split across both slots as 1 copper + 1 tin
/// and yields ALLOY_OUT_COUNT bars - the same 1:1 balance, rounded to
/// whole bars (recorded in the mechanics doc, "Production stations").
pub const ALLOY_INPUT_COPPER: &str = "Bar of Copper";
pub const ALLOY_INPUT_TIN: &str = "Bar of Tin";
pub const ALLOY_OUTPUT: (&str, &str) = ("gfx/invobjs/bar-bronze", "Bar of Bronze");
pub const ALLOY_OUT_COUNT: u32 = 2;

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

    /// Session 71 (baking chain): the quern takes the farm's Grist of
    /// Wheat into Flour and the oven bakes Bread Dough into Bread. The
    /// pack-aware pins (optional like every resources-dependent test)
    /// enforce the render path: the flour and bread invobjs must ship
    /// (they do - no alias needed) and the dough recipe's water input
    /// (bucket-water) plus the returned empty bucket (buckete) must
    /// exist too. The dough recipe shape is pinned flat: 2 flour + 1
    /// water bucket in, 2 dough + the empty bucket out (the 1:1
    /// flour-to-dough mass balance).
    #[test]
    fn bake_chain_maps_and_dough_recipe() {
        assert_eq!(
            grind_result("Grist of Wheat"),
            Some(("gfx/invobjs/flour", "Flour"))
        );
        assert_eq!(grind_result("Flour"), None);
        assert_eq!(
            bake_result("Bread Dough"),
            Some(("gfx/invobjs/bread", "Bread"))
        );
        assert_eq!(bake_result("Beef"), None);
        let dough = RECIPES
            .iter()
            .find(|r| r.id == "dough")
            .expect("dough recipe");
        assert_eq!(
            dough.inputs,
            &[("gfx/invobjs/flour", 2), ("gfx/invobjs/bucket-water", 1)]
        );
        assert_eq!(
            dough.outputs,
            &[("gfx/invobjs/dough", 2), ("gfx/invobjs/buckete", 1)]
        );
        // The bucket hand craft feeds the water leg (branch x3 -> empty
        // bucket, RoB Legacy:Bucket).
        let bucket = RECIPES
            .iter()
            .find(|r| r.id == "bucket")
            .expect("bucket recipe");
        assert_eq!(bucket.inputs, &[("gfx/invobjs/branch", 3)]);
        assert_eq!(bucket.outputs, &[("gfx/invobjs/buckete", 1)]);
        if crate::resources::RES_DIR.get().is_none() {
            let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gameres");
            if pack.is_dir() {
                crate::resources::init_res_dir(pack);
            }
        }
        if crate::resources::RES_DIR.get().is_some() {
            for res in [
                "gfx/invobjs/grist-wheat",
                "gfx/invobjs/flour",
                "gfx/invobjs/dough",
                "gfx/invobjs/bread",
                "gfx/invobjs/bucket-water",
                "gfx/invobjs/buckete",
            ] {
                assert!(crate::resources::served(res), "{res} must ship in the pack");
            }
        }
    }

    /// Session 66 (metal chain): every smelter input the world spawns
    /// (state::OreKind labels) resolves to a bar, and non-ore items
    /// refuse. When the gameres pack is locatable (generated next to the
    /// repo root; a fresh clone may not have it - the wire gate must run
    /// without it), the bar resource AND its world-shape render path
    /// (own terobjs shape or the game.rs alias table) are pinned too.
    /// The crucible charge pins: the output bar is the bronze resource,
    /// and the 1+1 -> ALLOY_OUT_COUNT shape keeps the legacy 1:1
    /// metal-to-bronze mass balance (2 copper + 1 tin -> 3 bronze).
    #[test]
    fn alloy_charge_pins() {
        assert_eq!(ALLOY_OUTPUT.0, "gfx/invobjs/bar-bronze");
        assert_eq!(ALLOY_OUTPUT.1, "Bar of Bronze");
        assert_eq!(ALLOY_OUT_COUNT, 2);
        // 2 input bars in -> 2 output bars out: the same 1:1 balance
        // the legacy 2+1 -> 3 charge realizes (mass, not bar count).
        // Pack-aware pin (optional like every resources-dependent test):
        // the bronze bar must exist in the served pack when present.
        if crate::resources::RES_DIR.get().is_none() {
            let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gameres");
            if pack.is_dir() {
                crate::resources::init_res_dir(pack);
            }
        }
        if crate::resources::RES_DIR.get().is_some() {
            assert!(
                crate::resources::served(ALLOY_OUTPUT.0),
                "{} must exist in the served pack",
                ALLOY_OUTPUT.0
            );
        }
        // Session 67: the world-shape render path is part of the charge
        // contract too. The 2009 pack ships no bronze bar sprite; the
        // output drop MUST resolve through the alias table instead of
        // falling back to the branch shape (the smelt_map pins enforce
        // the same rule for smelter outputs; the crucible was exempt -
        // a gap the bronze probe walk caught).
        let base = ALLOY_OUTPUT.0.rsplit('/').next().unwrap_or(ALLOY_OUTPUT.0);
        let own = format!("gfx/terobjs/items/{base}");
        assert!(
            !crate::resources::RES_DIR.get().is_some()
                || crate::resources::served(&own)
                || crate::game::drop_world_alias(base).is_some(),
            "{}: no world shape and no alias",
            ALLOY_OUTPUT.0
        );
    }

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

    /// Session 69 (kiln chain): the kiln input the world spawns (the
    /// Clay label from the shore deposits) resolves to the brick item,
    /// and non-clay items refuse. The pack ships both the brick invobj
    /// AND its world shape, so the output drop renders without an
    /// alias - pinned here when the pack is locatable (the
    /// smelt-map-test pattern).
    #[test]
    fn kiln_map_fires_clay_into_bricks() {
        if crate::resources::RES_DIR.get().is_none() {
            let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gameres");
            if pack.is_dir() {
                crate::resources::init_res_dir(pack);
            }
        }
        let (res, label) = kiln_result("Clay").expect("Clay must fire");
        assert_eq!(res, "gfx/invobjs/brick");
        assert_eq!(label, "Brick");
        assert_eq!(kiln_result("clay"), Some((res, label)), "case-insensitive");
        assert_eq!(kiln_result("Bar of Copper"), None);
        assert_eq!(kiln_result("Stone"), None);
        if crate::resources::RES_DIR.get().is_some() {
            assert!(
                crate::resources::served(res),
                "{res} must exist in the served pack"
            );
            assert!(
                crate::resources::served("gfx/terobjs/items/brick"),
                "the brick world shape must exist in the served pack"
            );
        }
    }

    /// Session 79: the pottery legs fire the molded unburnt wares into
    /// their finished forms. Every output resource AND its world shape
    /// must ship in the pack (the drop-render rule the alloy/smelt pins
    /// run under), and every unburnt key must match a registered molding
    /// recipe's output label - a kiln key no recipe produces would be a
    /// dead station leg.
    #[test]
    fn kiln_map_pottery_legs_are_wired() {
        if crate::resources::RES_DIR.get().is_none() {
            let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gameres");
            if pack.is_dir() {
                crate::resources::init_res_dir(pack);
            }
        }
        let molding_labels: Vec<&str> = RECIPES
            .iter()
            .filter(|r| {
                matches!(
                    r.id,
                    "mugdough" | "jardough" | "teapotdough" | "treepotdough"
                )
            })
            .map(|r| r.name)
            .collect();
        for (key, (res, label)) in KILN_MAP {
            if *key == "Clay" {
                continue; // the session-69 brick leg, pinned separately
            }
            assert!(
                molding_labels.contains(key),
                "{key}: the kiln key must be a molding recipe's output label"
            );
            if crate::resources::RES_DIR.get().is_some() {
                assert!(
                    crate::resources::served(res),
                    "{res} must exist in the served pack"
                );
                let base = res.rsplit('/').next().unwrap_or(res);
                let own = format!("gfx/terobjs/items/{base}");
                assert!(
                    crate::resources::served(&own),
                    "{own}: the fired ware's world shape must exist in the pack"
                );
            }
            let _ = label; // the fired label rides the stack to the eat/look flow
        }
    }

    /// Session 79: every BAKE_MAP output label must resolve its
    /// fep.conf row - the label is the eat key (FepTable::get through
    /// the Item.name() precedence), so a mapped label without a row
    /// would bake into an uneatable item. The fep.conf ships with the
    /// repo (etc/needed/fep.conf); the test reads the same file the
    /// live server boots with (game.rs's candidate list, repo-root
    /// depth first).
    #[test]
    fn bake_map_outputs_carry_fep_rows() {
        let conf =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../etc/needed/fep.conf");
        let text = std::fs::read_to_string(&conf)
            .unwrap_or_else(|e| panic!("fep.conf must ship with the repo ({conf:?}): {e}"));
        let fep = FepTable::parse(&text).expect("the shipped fep.conf must parse");
        for (dough, (_, label)) in BAKE_MAP {
            assert!(
                fep.get(label).is_some(),
                "{dough}: baked label {label:?} must resolve its fep.conf row"
            );
        }
    }

    /// Session 79: the six new recipes are wired end to end - the
    /// pottery moldings (clay -> unburnt ware), the butter churn leg
    /// (milk bucket -> butter + empty bucket back) and the carrot-cake
    /// dough (the one pie the economy reaches). Pins: pagina and output
    /// resources ship in the pack, the bucket-return legs match the
    /// dough recipe's shape, and the cake dough's output label keys the
    /// BAKE_MAP oven dispatch.
    #[test]
    fn pottery_butter_and_cake_recipes_are_wired() {
        if crate::resources::RES_DIR.get().is_none() {
            let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gameres");
            if pack.is_dir() {
                crate::resources::init_res_dir(pack);
            }
        }
        let clay_ladder: &[(&str, u32, &str)] = &[
            ("mugdough", 2, "Unburnt Clay Mug"),
            ("jardough", 3, "Unburnt Jar"),
            ("teapotdough", 5, "Unburnt Teapot"),
            ("treepotdough", 10, "Unburnt Treeplanter's Pot"),
        ];
        for (id, clay, label) in clay_ladder {
            let r = RECIPES
                .iter()
                .find(|r| r.id == *id)
                .unwrap_or_else(|| panic!("{id}: the molding recipe must be registered"));
            assert_eq!(
                r.inputs,
                &[("gfx/invobjs/clay", *clay)],
                "{id}: clay ladder"
            );
            assert_eq!(r.outputs.len(), 1, "{id}: one ware per molding");
            assert_eq!(r.name, *label, "{id}: the label keys the kiln dispatch");
            assert_eq!(r.softcap_attr, "dex", "{id}: molding is a dexterity job");
        }
        let butter = RECIPES
            .iter()
            .find(|r| r.id == "butter")
            .expect("the butter recipe must be registered");
        assert_eq!(butter.inputs, &[("gfx/invobjs/bucket-milk", 1)]);
        assert_eq!(
            butter.outputs,
            &[("gfx/invobjs/butter", 1), ("gfx/invobjs/buckete", 1)],
            "the filled bucket must return its empty bucket"
        );
        let cake = RECIPES
            .iter()
            .find(|r| r.id == "ccdough")
            .expect("the carrot cake dough recipe must be registered");
        assert_eq!(
            cake.inputs,
            &[
                ("gfx/invobjs/flour", 2),
                ("gfx/invobjs/bucket-water", 1),
                ("gfx/invobjs/carrot", 2),
                ("gfx/invobjs/butter", 1),
            ]
        );
        assert_eq!(
            cake.outputs,
            &[
                ("gfx/invobjs/dough-cake-carrot", 2),
                ("gfx/invobjs/buckete", 1),
            ],
            "the water bucket returns alongside the dough"
        );
        // The baked dough's label must key the oven dispatch (the
        // primary output carries recipe.name as its stack label).
        let baked = bake_result(cake.name).expect("the cake dough must bake");
        assert_eq!(baked.1, "Carrot Cake");
        if crate::resources::RES_DIR.get().is_some() {
            for r in RECIPES.iter().filter(|r| {
                matches!(
                    r.id,
                    "mugdough" | "jardough" | "teapotdough" | "treepotdough" | "butter" | "ccdough"
                )
            }) {
                assert!(
                    crate::resources::served(r.pagina),
                    "{}: the craft pagina must exist in the served pack",
                    r.pagina
                );
                for (res, _) in r.outputs {
                    assert!(
                        crate::resources::served(res),
                        "{res}: the output resource must exist in the served pack"
                    );
                }
            }
        }
    }

    /// Session 81: the five dough ingredient chains close the baking
    /// breadth. Pins: every dough recipe's primary output label keys
    /// the BAKE_MAP oven dispatch, every pagina ships in the pack (the
    /// raisins page is a fork page under res/compiled), the forage
    /// item inputs resolve their fep.conf eat rows (raw eating stays
    /// alive), and the forage registry's world sprites ship.
    #[test]
    fn dough_chain_recipes_are_wired() {
        if crate::resources::RES_DIR.get().is_none() {
            let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gameres");
            if pack.is_dir() {
                crate::resources::init_res_dir(pack);
            }
        }
        let dough_ladder: &[(&str, &str)] = &[
            ("apdough", "Apple Pie Dough"),
            ("dough_blueberrypie", "Blueberry Pie Dough"),
            ("hbdough", "Honeybun Dough"),
            ("rbcdough", "Raisin Butter-cake Dough"),
            ("dough_pirozhki", "Pirozhki Dough"),
        ];
        for (id, label) in dough_ladder {
            let r = RECIPES
                .iter()
                .find(|r| r.id == *id)
                .unwrap_or_else(|| panic!("{id}: the dough recipe must be registered"));
            assert_eq!(r.name, *label, "{id}: the label keys the oven dispatch");
            // The dough base is invariant: flour x2 + one water bucket,
            // and the water bucket comes back.
            assert_eq!(r.inputs[0], ("gfx/invobjs/flour", 2), "{id}: flour base");
            assert_eq!(
                r.inputs[1],
                ("gfx/invobjs/bucket-water", 1),
                "{id}: water base"
            );
            assert!(
                r.outputs
                    .iter()
                    .any(|(res, _)| *res == "gfx/invobjs/buckete"),
                "{id}: the water bucket must return"
            );
            let baked = bake_result(r.name)
                .unwrap_or_else(|| panic!("{id}: the dough must bake (BAKE_MAP key)"));
            let _ = baked;
        }
        // The honeybun dough returns TWO empty buckets (water + honey).
        let hb = RECIPES
            .iter()
            .find(|r| r.id == "hbdough")
            .expect("the honeybun dough recipe must be registered");
        assert_eq!(
            hb.outputs,
            &[
                ("gfx/invobjs/dough-bun-honey", 2),
                ("gfx/invobjs/buckete", 2),
            ],
            "both filled buckets return empty"
        );
        // The raisin leg: two grapes sun-dry into one raisin pack.
        let raisins = RECIPES
            .iter()
            .find(|r| r.id == "raisins")
            .expect("the raisins recipe must be registered");
        assert_eq!(raisins.inputs, &[("gfx/invobjs/grapes", 2)]);
        assert_eq!(raisins.outputs, &[("gfx/invobjs/raisins", 1)]);
        // The forage registry: every item label resolves its fep.conf
        // eat row, every world sprite ships in the pack.
        let conf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../etc/needed/fep.conf")
            .display()
            .to_string();
        let text = std::fs::read_to_string(&conf)
            .unwrap_or_else(|e| panic!("fep.conf must ship with the repo ({conf}): {e}"));
        let fep = FepTable::parse(&text).expect("the shipped fep.conf must parse");
        for f in [
            crate::state::ForageKind::Blueberry,
            crate::state::ForageKind::Chantrelle,
            crate::state::ForageKind::Grapevine,
            crate::state::ForageKind::WildOnion,
        ] {
            assert!(
                fep.get(f.label()).is_some(),
                "{:?}: the forage label {:?} must resolve its fep.conf row",
                f,
                f.label()
            );
        }
        if crate::resources::RES_DIR.get().is_some() {
            for r in RECIPES.iter().filter(|r| {
                matches!(
                    r.id,
                    "apdough"
                        | "dough_blueberrypie"
                        | "hbdough"
                        | "rbcdough"
                        | "dough_pirozhki"
                        | "raisins"
                )
            }) {
                assert!(
                    crate::resources::served(r.pagina),
                    "{}: the craft pagina must exist in the served pack",
                    r.pagina
                );
                for (res, _) in r.inputs {
                    assert!(
                        crate::resources::served(res),
                        "{res}: the input resource must exist in the served pack"
                    );
                }
                for (res, _) in r.outputs {
                    assert!(
                        crate::resources::served(res),
                        "{res}: the output resource must exist in the served pack"
                    );
                }
            }
            for f in [
                crate::state::ForageKind::Blueberry,
                crate::state::ForageKind::Chantrelle,
                crate::state::ForageKind::Grapevine,
                crate::state::ForageKind::WildOnion,
            ] {
                assert!(
                    crate::resources::served(f.world_res()),
                    "{}: the forage world sprite must ship in the pack",
                    f.world_res()
                );
                assert!(
                    crate::resources::served(f.item_res()),
                    "{}: the forage item must ship in the pack",
                    f.item_res()
                );
            }
            assert!(
                crate::resources::served("gfx/terobjs/bhive"),
                "the beehive world sprite must ship in the pack"
            );
            assert!(
                crate::resources::served("gfx/terobjs/trees/appletree"),
                "the apple tree sprite must ship in the pack"
            );
        }
    }

    /// Session 66 refinement tier (on top of the ore -> bar smelting
    /// leg): the shipped bloom2wrought pagina refines a cast-iron bar
    /// into wrought iron, and the shammer pagina turns a wrought-iron
    /// bar + branch into the smithy's hammer. Both paginae and both
    /// output items ship with the pack (skip the filesystem pins when
    /// the pack is absent - the quiver-test pattern).
    #[test]
    fn wrought_iron_and_hammer_recipes_are_wired() {
        let refine = RECIPES
            .iter()
            .find(|r| r.id == "wroughtiron")
            .expect("the wroughtiron recipe must be registered");
        assert_eq!(
            refine.inputs,
            &[("gfx/invobjs/bar-castiron", 1)],
            "cast iron is the refinement input"
        );
        assert_eq!(refine.outputs, &[("gfx/invobjs/bar-wroughtiron", 1)]);
        assert_eq!(refine.pagina, "paginae/craft/bloom2wrought");
        assert_eq!(refine.softcap_attr, "str");

        let hammer = RECIPES
            .iter()
            .find(|r| r.id == "shammer")
            .expect("the shammer recipe must be registered");
        assert_eq!(
            hammer.inputs,
            &[
                ("gfx/invobjs/bar-wroughtiron", 1),
                ("gfx/invobjs/branch", 1)
            ]
        );
        assert_eq!(hammer.outputs, &[("gfx/invobjs/hammer-smithys", 1)]);
        assert_eq!(hammer.pagina, "paginae/craft/shammer");

        let pack = std::path::Path::new("../../gameres");
        if pack.is_dir() {
            assert!(pack.join("paginae/craft/bloom2wrought.res").exists());
            assert!(pack.join("paginae/craft/shammer.res").exists());
            assert!(pack.join("gfx/invobjs/bar-wroughtiron.res").exists());
            assert!(pack.join("gfx/invobjs/hammer-smithys.res").exists());
        }
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
