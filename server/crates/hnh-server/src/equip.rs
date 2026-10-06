//! Equipment visual layers (the avatar's composited clothing).
//!
//! The original client composites a player from a flat LIST of concrete
//! image resources: the world drawable reads OD_LAYERS (Layered), the
//! Equipment-window doll reads OD_AVATAR (AvaRender). Both sort the
//! final parts by the z attribute baked into every resource, so the
//! server only needs to APPEND the equipped pieces' layers to the body
//! parts - no client-side piece resolution exists, and session 20's
//! "no doll" fix established the pipeline.
//!
//! This module is the server-side invobj -> borka-layer table. Every
//! entry was inventoried from the served resource pack by
//! scripts/inventory_clothes.py + scripts/diff_clothes_poses.py (the
//! per-piece file prefixes are NOT derivable from the piece name and
//! CHANGE between the standing and walking poses: hat-chief ships
//! standing-N/walking-N, hat-sprucecap standing-N/sprucecap-N, the
//! leather boots swap bootz-/boots-). Layer names materialize ONCE as
//! leaked 'static strings keyed by (piece, pose, octant) so every call
//! site composes with zero allocations (data-oriented: flat table
//! lookup by movement octant, the art index applied here exactly like
//! PoseTable).
//!
//! Hand layers exist in two arm poses: `idle` (the world pose) and
//! `banzai` (the doll's spread arms). Only pieces that ship a banzai
//! variant render on the doll; `eq-*` hand items (idle only) render on
//! the world avatar only. Pieces whose pack only has a `carrying` arm
//! pose (bows, carried tools) are not listed yet - rendering them in an
//! idle pose would draw misaligned arms; a carrying-pose state can add
//! them later.

use std::collections::HashMap;
use std::sync::OnceLock;

/// One wearable: inventory resource name + per-pose layer templates.
/// Placeholders inside the tails: {d} = art octant digit, {hand} = idle
/// (world) | banzai (doll). The doll set derives from the standing
/// templates ({hand} -> banzai); templates carrying {hand} are dropped
/// from the doll set when `doll_arms` is false (the pack has no banzai
/// variant for that piece).
struct PieceDef {
    inv: &'static str,
    st: &'static [&'static str],
    wk: &'static [&'static str],
    doll_arms: bool,
}

/// Compose one table entry: the standing tails join under
/// `gfx/borka/<folder>/standing/`, the walking tails under
/// `gfx/borka/<folder>/walking/`.
macro_rules! piece {
    ($inv:expr, $folder:expr, [$($st:expr),+ $(,)?], [$($wk:expr),+ $(,)?], $doll_arms:expr) => {
        PieceDef {
            inv: $inv,
            st: &[$(concat!("gfx/borka/", $folder, "/standing/", $st)),+],
            wk: &[$(concat!("gfx/borka/", $folder, "/walking/", $wk)),+],
            doll_arms: $doll_arms,
        }
    };
}

/// Inventory resource -> borka layer templates, inventoried from
/// server/gameres (scripts/inventory_clothes.py). Names must match the
/// served invobj resources exactly (world.res.name() output).
static PIECES: &[PieceDef] = &[
    // ---- shirts: torso + both idle arms (linen/nettle/ranger/chainmail)
    // ---- or torso only (larmor/parmor); barmor ships standing-N/walking-N.
    piece!(
        "gfx/invobjs/linenshirt",
        "shirt-linen",
        [
            "torso/male-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}"
        ],
        [
            "torso/male-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}"
        ],
        true
    ),
    piece!(
        "gfx/invobjs/shirt-nettle",
        "shirt-nettle",
        [
            "torso/male-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}"
        ],
        [
            "torso/male-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}"
        ],
        true
    ),
    piece!(
        "gfx/invobjs/shirt-ranger",
        "shirt-ranger",
        [
            "torso/male-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}"
        ],
        [
            "torso/male-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}"
        ],
        true
    ),
    piece!(
        "gfx/invobjs/shirt-chainmail",
        "shirt-chainmail",
        [
            "torso/male-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}"
        ],
        [
            "torso/male-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}"
        ],
        true
    ),
    piece!(
        "gfx/invobjs/larmor",
        "shirt-larmor",
        ["torso/male-{d}"],
        ["torso/male-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/parmor",
        "shirt-parmor",
        ["torso/male-{d}"],
        ["torso/male-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/barmor",
        "shirt-barmor",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    // ---- pants (the file prefix varies per piece and per pose)
    piece!(
        "gfx/invobjs/linenpants",
        "pants-linen",
        ["pants-{d}"],
        ["pants-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/pants-nettle",
        "pants-nettle",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/pants-ranger",
        "pants-ranger",
        ["pants-{d}"],
        ["pants-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/lpants",
        "pants-larmor",
        ["lpants-{d}"],
        ["lpants-{d}"],
        false
    ),
    // ---- shoes (the leather boots swap bootz-/boots- between poses)
    piece!(
        "gfx/invobjs/lboots",
        "shoes-lboots",
        ["bootz-{d}"],
        ["boots-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/shoes-toffels",
        "shoes-toffels",
        ["bootz-{d}"],
        ["boots-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/shoes-ranger",
        "shoes-ranger",
        ["boots-{d}"],
        ["bootz-{d}"],
        false
    ),
    // ---- hats (prefix varies; most ship standing-N/walking-N)
    piece!(
        "gfx/invobjs/shat",
        "hat-straw",
        ["shat-{d}"],
        ["shat-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/foxhat",
        "hat-fox",
        ["foxhat-{d}"],
        ["foxhat-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/what",
        "hat-working",
        ["what-{d}"],
        ["what-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/hat-chief",
        "hat-chief",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/hat-gandalf",
        "hat-gandalf",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/hat-high",
        "hat-high",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/hat-sprucecap",
        "hat-sprucecap",
        ["standing-{d}"],
        ["sprucecap-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/hat-top",
        "hat-top",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/pumpkinhat",
        "hat-pumpkin",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/bandits-mask",
        "hat-bandit",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    // ---- helms (druid ships dhelm-N; the rest standing-N/walking-N)
    piece!(
        "gfx/invobjs/dhelm",
        "helm-druid",
        ["dhelm-{d}"],
        ["dhelm-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/helm-tusk",
        "helm-tusk",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/helm-soldiers",
        "helm-soldiers",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/helm-hird",
        "helm-hird",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/helm-miners-plain",
        "helm-miners-plain",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/helm-miners-lit",
        "helm-miners-lit",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/helm-miners-candle",
        "helm-miners-candle",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    // ---- capes (bear/ranger add a hood head layer; prefixes vary)
    piece!(
        "gfx/invobjs/cape",
        "cape",
        ["cape-{d}"],
        ["cape-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/cape-black",
        "cape-black",
        ["cape-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/cape-gandalf",
        "cape-gandalf",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/cape-bear",
        "cape-bear",
        ["cape-{d}", "head-{d}"],
        ["cape-{d}", "head-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/cape-ranger",
        "cape-ranger",
        ["cape-{d}", "head-{d}"],
        ["cape-{d}", "head-{d}"],
        false
    ),
    // ---- cloaks: torso layer (+ arms where shipped; druid adds a hood)
    piece!(
        "gfx/invobjs/cloak-hide",
        "cloak-hide",
        ["torso-{d}"],
        ["torso-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/cloak-gandalf",
        "cloak-gandalf",
        ["torso-{d}"],
        ["torso-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/cloak-leather",
        "cloak-leather",
        ["torso-{d}", "arm/{hand}/left-{d}", "arm/{hand}/right-{d}"],
        ["torso-{d}", "arm/{hand}/left-{d}", "arm/{hand}/right-{d}"],
        true
    ),
    piece!(
        "gfx/invobjs/cloak-necro",
        "cloak-necro",
        ["torso-{d}", "arm/{hand}/left-{d}", "arm/{hand}/right-{d}"],
        ["torso-{d}", "arm/{hand}/left-{d}", "arm/{hand}/right-{d}"],
        true
    ),
    piece!(
        "gfx/invobjs/cloak-druid",
        "cloak-druid",
        [
            "torso-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}",
            "head-{d}"
        ],
        [
            "torso-{d}",
            "arm/{hand}/left-{d}",
            "arm/{hand}/right-{d}",
            "head-{d}"
        ],
        true
    ),
    piece!(
        "gfx/invobjs/cloak-merchant",
        "cloak-merchant",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    // ---- misc
    piece!(
        "gfx/invobjs/belt-poor",
        "belt-poor",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/backpack",
        "backpack",
        ["backpack-{d}"],
        ["backpack-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/quiver",
        "quiver",
        ["standing-{d}"],
        ["walking-{d}"],
        false
    ),
    // ---- hands: gloves (idle + banzai), one-handed gear (idle only)
    piece!(
        "gfx/invobjs/glove-poor",
        "glove-poor",
        ["arm/{hand}/left-{d}", "arm/{hand}/right-{d}"],
        ["arm/{hand}/left-{d}", "arm/{hand}/right-{d}"],
        true
    ),
    piece!(
        "gfx/invobjs/sword",
        "eq-sword",
        ["arm/{hand}/right-{d}"],
        ["arm/{hand}/right-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/wsword",
        "eq-wsword",
        ["arm/{hand}/right-{d}"],
        ["arm/{hand}/right-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/bronzesword",
        "eq-bronzesword",
        ["arm/{hand}/right-{d}"],
        ["arm/{hand}/right-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/knife-flint",
        "eq-knife",
        ["arm/{hand}/right-{d}"],
        ["arm/{hand}/right-{d}"],
        false
    ),
    piece!(
        "gfx/invobjs/shield-wood",
        "eq-shield",
        ["arm/{hand}/left-{d}"],
        ["arm/{hand}/left-{d}"],
        false
    ),
    // Session 36: the Wooden Bow is TWO-HANDED and rides on the back -
    // the pack ships dedicated carrying layers (gfx/borka/eq-bow/
    // {standing,walking,dead}/arm/carrying/{left,right}-{d}), one per
    // side per octant, with NO {hand} (idle/banzai) variant. Carrying
    // templates carry no {hand} placeholder so they survive on the doll
    // too (the doll renders the front carrying pair).
    piece!(
        "gfx/invobjs/bow",
        "eq-bow",
        ["arm/carrying/left-{d}", "arm/carrying/right-{d}"],
        ["arm/carrying/left-{d}", "arm/carrying/right-{d}"],
        false
    ),
];

/// Materialized layer lists for one piece. Indexing mirrors PoseTable:
/// `world[pose][octant]` with pose 0 = standing, 1 = walking; the
/// emitted resource names carry the art octant. The doll set is one
/// fixed list (standing pose, art octant 0 = the camera-facing front).
struct PieceLayers {
    world: [[&'static [&'static str]; 8]; 2],
    doll: &'static [&'static str],
}

struct Table {
    /// Inventory resource name -> piece index (exact-match lookup).
    by_inv: HashMap<&'static str, usize>,
    pieces: Vec<PieceLayers>,
}

static TABLE: OnceLock<Table> = OnceLock::new();

/// Leak a string once per unique value (the materialization cache keeps
/// the leaked set bounded to the actual distinct layer names).
fn leak_unique(cache: &mut HashMap<String, &'static str>, s: String) -> &'static str {
    if let Some(&v) = cache.get(&s) {
        return v;
    }
    let leaked: &'static str = Box::leak(s.clone().into_boxed_str());
    cache.insert(s, leaked);
    leaked
}

fn materialize(
    cache: &mut HashMap<String, &'static str>,
    tpls: &[&'static str],
    hand: &str,
    art: u8,
) -> Vec<&'static str> {
    tpls.iter()
        .map(|t| {
            leak_unique(
                cache,
                t.replace("{hand}", hand)
                    .replace("{d}", &(art as char).to_string()),
            )
        })
        .collect()
}

fn build() -> Table {
    let mut cache: HashMap<String, &'static str> = HashMap::new();
    let mut by_inv = HashMap::with_capacity(PIECES.len());
    let mut pieces = Vec::with_capacity(PIECES.len());
    for (pi, def) in PIECES.iter().enumerate() {
        by_inv.insert(def.inv, pi);
        let mut world: [[&'static [&'static str]; 8]; 2] = Default::default();
        for octant in 0u8..8 {
            // Table stays indexed by the MOVEMENT octant; the emitted
            // name carries the art sprite index (session 22 convention).
            let art = b'0' + crate::game::art_dir(octant);
            let standing = materialize(&mut cache, def.st, "idle", art);
            let walking = materialize(&mut cache, def.wk, "idle", art);
            world[0][octant as usize] = Box::leak(standing.into_boxed_slice());
            world[1][octant as usize] = Box::leak(walking.into_boxed_slice());
        }
        // The doll set: standing pose, art octant 0 (the full front
        // view). Art octant 0 = movement octant 1 (art_dir(1) = 0), the
        // same convention the body doll layers follow.
        let art_front = b'0' + crate::game::art_dir(1);
        let hand = if def.doll_arms { "banzai" } else { "idle" };
        let doll: Vec<&'static str> = if def.doll_arms {
            materialize(&mut cache, def.st, hand, art_front)
        } else {
            // No banzai variant in the pack: {hand} templates drop out
            // of the doll set entirely.
            def.st
                .iter()
                .filter(|t| !t.contains("{hand}"))
                .map(|t| {
                    leak_unique(
                        &mut cache,
                        t.replace("{hand}", hand)
                            .replace("{d}", &(art_front as char).to_string()),
                    )
                })
                .collect()
        };
        pieces.push(PieceLayers {
            world,
            doll: Box::leak(doll.into_boxed_slice()),
        });
    }
    Table { by_inv, pieces }
}

fn table() -> &'static Table {
    TABLE.get_or_init(build)
}

/// World layers for the equipped pieces (in table order - the client
/// z-sorts the final parts by the resources' own z attributes, so the
/// list order only affects determinism, not correctness).
/// `dir` is the MOVEMENT octant (converted to the art octant here).
pub fn world_layers<'a>(
    equip: impl IntoIterator<Item = &'a &'static str>,
    moving: bool,
    dir: u8,
) -> Vec<&'static str> {
    let t = table();
    let pose = usize::from(moving);
    let mut out = Vec::new();
    for inv in equip {
        if let Some(&pi) = t.by_inv.get(*inv) {
            out.extend_from_slice(t.pieces[pi].world[pose][(dir & 7) as usize]);
        }
    }
    out
}

/// Doll layers (Equipment window, banzai arms, camera-facing front) for
/// the equipped pieces.
pub fn doll_layers<'a>(equip: impl IntoIterator<Item = &'a &'static str>) -> Vec<&'static str> {
    let t = table();
    let mut out = Vec::new();
    for inv in equip {
        if let Some(&pi) = t.by_inv.get(*inv) {
            out.extend_from_slice(t.pieces[pi].doll);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_octant_lands_in_the_emitted_names() {
        // Movement octant 0 renders art octant art_dir(0) = 7. Pin the
        // conversion the same way PoseTable emits it.
        let got = world_layers([&"gfx/invobjs/linenshirt"], false, 0);
        assert_eq!(got.len(), 3);
        assert!(got[0].starts_with("gfx/borka/shirt-linen/standing/torso/male-"));
        assert!(got[0].ends_with(format!("-{}", crate::game::art_dir(0)).as_str()));
        // Walking pose swaps the pose segment, keeps the art digit.
        let w = world_layers([&"gfx/invobjs/linenshirt"], true, 2);
        assert!(w[0].starts_with("gfx/borka/shirt-linen/walking/torso/male-"));
        assert!(w[0].ends_with(format!("-{}", crate::game::art_dir(2)).as_str()));
    }

    #[test]
    fn pose_specific_prefixes_match_the_pack() {
        // barmor: standing-N vs walking-N.
        let b = world_layers([&"gfx/invobjs/barmor"], false, 0);
        assert_eq!(
            b,
            vec![format!(
                "gfx/borka/shirt-barmor/standing/standing-{}",
                crate::game::art_dir(0)
            )]
        );
        let bw = world_layers([&"gfx/invobjs/barmor"], true, 0);
        assert_eq!(
            bw,
            vec![format!(
                "gfx/borka/shirt-barmor/walking/walking-{}",
                crate::game::art_dir(0)
            )]
        );
        // Leather boots: bootz-N standing, boots-N walking.
        let l = world_layers([&"gfx/invobjs/lboots"], false, 0);
        assert!(l[0].contains("/standing/bootz-"));
        let lw = world_layers([&"gfx/invobjs/lboots"], true, 0);
        assert!(lw[0].contains("/walking/boots-"));
        // Ranger boots flip the pair.
        let r = world_layers([&"gfx/invobjs/shoes-ranger"], false, 0);
        assert!(r[0].contains("/standing/boots-"));
        let rw = world_layers([&"gfx/invobjs/shoes-ranger"], true, 0);
        assert!(rw[0].contains("/walking/bootz-"));
        // Spruce cap: standing-N vs sprucecap-N.
        let s = world_layers([&"gfx/invobjs/hat-sprucecap"], true, 0);
        assert!(s[0].contains("/walking/sprucecap-"));
    }

    #[test]
    fn all_eight_octants_materialize_without_placeholders() {
        for def in PIECES {
            for octant in 0u8..8 {
                for moving in [false, true] {
                    for name in world_layers([&def.inv], moving, octant) {
                        assert!(!name.contains('{'), "{name} keeps a placeholder");
                        assert!(!name.contains('}'), "{name} keeps a placeholder");
                        assert!(
                            !name.contains("{pose}"),
                            "{name} keeps the pose placeholder"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn doll_set_is_standing_banzai_front_and_drops_idle_only_arms() {
        // Shirt: torso + BOTH banzai arms on the doll. Art octant 0 =
        // movement octant 1 (art_dir(1) = 0), the camera-facing front.
        let d = doll_layers([&"gfx/invobjs/linenshirt"]);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0], "gfx/borka/shirt-linen/standing/torso/male-0");
        assert!(d[1].contains("/arm/banzai/left-0"));
        assert!(d[2].contains("/arm/banzai/right-0"));
        // Sword ships idle arms only: nothing renders on the doll.
        assert!(doll_layers([&"gfx/invobjs/sword"]).is_empty());
        // Gloves ship banzai: both arms render.
        assert_eq!(doll_layers([&"gfx/invobjs/glove-poor"]).len(), 2);
        // Non-arm pieces render standing front layers.
        let p = doll_layers([&"gfx/invobjs/linenpants"]);
        assert_eq!(p, vec!["gfx/borka/pants-linen/standing/pants-0"]);
    }

    #[test]
    fn hand_side_split_matches_the_pack() {
        // Right-hand gear emits the right arm layer only. Movement
        // octant 4 renders art octant 3 (art_dir(4) = 3).
        let s = world_layers([&"gfx/invobjs/sword"], false, 4);
        assert_eq!(s.len(), 1);
        assert!(s[0].ends_with("/arm/idle/right-3"));
        // Wooden shield is the left-hand piece (movement 0 -> art 7).
        let sh = world_layers([&"gfx/invobjs/shield-wood"], false, 0);
        assert!(sh[0].ends_with("/arm/idle/left-7"));
    }

    #[test]
    fn unknown_and_non_wearable_resources_are_ignored() {
        assert!(world_layers([&"gfx/invobjs/meat", &"gfx/invobjs/axe"], false, 0).is_empty());
        assert!(doll_layers([&"gfx/invobjs/meat"]).is_empty());
    }

    #[test]
    fn stacking_multiple_pieces_extends_the_list_in_table_order() {
        let got = world_layers(
            [
                &"gfx/invobjs/linenshirt",
                &"gfx/invobjs/linenpants",
                &"gfx/invobjs/shat",
            ],
            false,
            1,
        );
        // 3 shirt + 1 pants + 1 hat.
        assert_eq!(got.len(), 5);
        assert!(got[0].contains("shirt-linen"));
        assert!(got[3].contains("pants-linen"));
        assert!(got[4].contains("hat-straw"));
    }

    #[test]
    fn every_listed_piece_has_a_served_borka_layer_file() {
        // The served pack must back every materialized name: this is the
        // inventory cross-check (scripts/inventory_clothes.py +
        // scripts/diff_clothes_poses.py walked the same tree; the test
        // pins it so a pack change fails here first).
        let dir = std::path::Path::new("../../gameres");
        if !dir.is_dir() {
            // The pack is not part of the test sandbox; the wire names
            // were verified against it during the session.
            return;
        }
        for def in PIECES {
            for octant in 0u8..8 {
                for (moving, pose) in [(false, "standing"), (true, "walking")] {
                    for name in world_layers([&def.inv], moving, octant) {
                        let rel = format!("{}/{name}.res", dir.display());
                        let path = std::path::Path::new(&rel);
                        assert!(path.is_file(), "missing {pose} file for {}: {rel}", def.inv);
                    }
                }
            }
        }
    }
}
