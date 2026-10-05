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
}
