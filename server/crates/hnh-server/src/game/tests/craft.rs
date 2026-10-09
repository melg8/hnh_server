//! Recipe registry consistency and the craft chains.
use super::super::*;
use super::common::*;

/// Session 66 refinement tier, end to end through craft_once: the
/// smelted cast-iron bar refines into wrought iron (the shipped
/// bloom2wrought pagina, the finery-forge stand-in), and the bar +
/// branch make the smithy's hammer (shammer pagina). Quality chain:
/// cast iron q40 -> wrought (10+40)/2 = 25; hammer (25 + 10)/2 = 17
/// -> softcap (10 + 17)/2 = 13.
#[tokio::test]
async fn metal_refinement_chain_crafts_bar_and_hammer() {
    let (mut g, _rx, _raw) = entered_game("metalref");
    set_inv(
        &mut g,
        &[
            ("gfx/invobjs/bar-castiron", 2, 40),
            ("gfx/invobjs/branch", 2, 10),
        ],
    );
    assert!(g.craft_once(1, "wroughtiron"), "cast iron refines");
    {
        let pidx = *g.world.by_session.get(&1).unwrap();
        let bar = g.world.res.intern("gfx/invobjs/bar-wroughtiron");
        let s = g.world.players[pidx]
            .inv
            .iter()
            .find(|s| s.res == bar)
            .expect("wrought-iron bar in inventory");
        assert_eq!(s.count, 1);
        assert_eq!(s.ql, 25, "cast iron q40 softcapped by str 10 -> 25");
    }
    assert!(g.craft_once(1, "shammer"), "bar + branch make the hammer");
    {
        let pidx = *g.world.by_session.get(&1).unwrap();
        let bar = g.world.res.intern("gfx/invobjs/bar-wroughtiron");
        let hammer = g.world.res.intern("gfx/invobjs/hammer-smithys");
        assert!(
            !g.world.players[pidx].inv.iter().any(|s| s.res == bar),
            "the bar is consumed"
        );
        let h = g.world.players[pidx]
            .inv
            .iter()
            .find(|s| s.res == hammer)
            .expect("hammer in inventory");
        assert_eq!(h.ql, 13, "hammer quality follows the weighted chain");
        assert_eq!(h.count, 1);
    }
}

/// Session 58 breadth batch. The leather chain: hides (animal loot)
/// tan into leather through the tanhide fork page, and the shipped
/// leather-tier pages consume it. Quality: 4 hides at q40 average the
/// type to 40, the sewing softcap (10) halves toward 25.
#[tokio::test]
async fn leather_chain_tans_and_consumes() {
    let (mut g, _rx, _raw) = entered_game("leather");
    set_inv(
        &mut g,
        &[
            ("gfx/invobjs/hide-raw-cow", 4, 40),
            ("gfx/invobjs/string", 1, 10),
        ],
    );
    assert!(g.craft_once(1, "tanhide"), "tanhide must succeed");
    {
        let pidx = *g.world.by_session.get(&1).unwrap();
        let leather = g.world.res.intern("gfx/invobjs/leather");
        let l = g.world.players[pidx]
            .inv
            .iter()
            .find(|s| s.res == leather)
            .expect("leather produced");
        assert_eq!(l.count, 1);
        assert_eq!(l.ql, 25, "(40 + 10)/2 with sewing softcap");
    }
    // The second tanhide consumes the remaining 2 hides (craft all
    // would continue; a single pass keeps the test tight).
    assert!(g.craft_once(1, "tanhide"), "second tanhide must succeed");
    assert!(g.craft_once(1, "lboots"), "lboots from leather + string");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let boots = g.world.res.intern("gfx/invobjs/lboots");
    let b = g.world.players[pidx]
        .inv
        .iter()
        .find(|s| s.res == boots)
        .expect("boots produced");
    assert_eq!(b.count, 1);
    // Leather q25 + string q10, type weights [2,1]: (25*2 + 10)/3 = 20,
    // softcap sewing=10: (20 + 10)/2 = 15.
    assert_eq!(b.ql, 15, "type-weighted boots quality");
}

/// String: the pack economy consumed string with no producer page;
/// the session-58 fork page closes it from flax fibres (the flax/hemp
/// early harvest).
#[tokio::test]
async fn string_spins_from_flax_fibres() {
    let (mut g, _rx, _raw) = entered_game("flaxspin");
    set_inv(&mut g, &[("gfx/invobjs/flaxfibre", 2, 10)]);
    assert!(g.craft_once(1, "string"), "string must succeed");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let str_gidx = g.world.res.intern("gfx/invobjs/string");
    let total: u32 = g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == str_gidx)
        .map(|s| s.count)
        .sum();
    assert_eq!(total, 1, "one new string stack unit");
}

/// The saw closes the session-46 carpentry loop: the bucket demanded a
/// saw that nothing produced. Now saw crafts from the starter kit and
/// the bucket follows - and the saw is CONSUMED-as-required, not lost.
#[tokio::test]
async fn saw_crafts_from_starter_and_unlocks_bucket() {
    let (mut g, _rx, _raw) = entered_game("carpentry");
    // Starter kit carries 10 branch + 6 stone: enough for saw (2+1)
    // and bucket (3 branches) with headroom.
    assert!(g.craft_once(1, "saw"), "saw must craft from the starter");
    {
        let pidx = *g.world.by_session.get(&1).unwrap();
        let saw = g.world.res.intern("gfx/invobjs/saw");
        assert!(
            g.world.players[pidx].inv.iter().any(|s| s.res == saw),
            "saw produced"
        );
    }
    assert!(g.craft_once(1, "bucket"), "bucket with the crafted saw");
}

/// Recipe registry hygiene: ids unique, paginae unique, quality
/// weights align with the input count (per-type weights are indexed
/// by input type; a shorter vector is legal, a bogus longer one is
/// a data bug).
#[test]
fn recipe_registry_is_consistent() {
    let mut ids: Vec<&str> = crate::craft::RECIPES.iter().map(|r| r.id).collect();
    ids.sort_unstable();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "recipe ids must be unique");
    let mut pages: Vec<&str> = crate::craft::RECIPES.iter().map(|r| r.pagina).collect();
    pages.sort_unstable();
    pages.dedup();
    assert_eq!(pages.len(), n, "recipe paginae must be unique");
    for r in crate::craft::RECIPES {
        assert!(
            r.q_weights.len() <= r.inputs.len(),
            "{}: more type weights than inputs",
            r.id
        );
        assert!(!r.inputs.is_empty(), "{}: no inputs", r.id);
        assert!(!r.outputs.is_empty(), "{}: no outputs", r.id);
    }
}

/// Wooden Bow quality follows the RoB type-weighted formula
/// `(qBranches + qString)/2` INDEPENDENT of the unit counts: 4
/// branches at q40 + 1 string at q10 average the TYPES to 25, then
/// the Marksmanship (ranged) softcap (10 here) halves it toward 17.
/// The pre-36 unit-weighted math would give 34 -> 22, so the assert
/// distinguishes the two models.
#[tokio::test]
async fn woodbow_quality_is_type_weighted() {
    let (mut g, _rx, _raw) = entered_game("bowq");
    set_inv(
        &mut g,
        &[("gfx/invobjs/branch", 4, 40), ("gfx/invobjs/string", 1, 10)],
    );
    assert!(g.craft_once(1, "woodbow"), "craft must succeed");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let bow_gidx = g.world.res.intern("gfx/invobjs/bow");
    let bow = g.world.players[pidx]
        .inv
        .iter()
        .find(|s| s.res == bow_gidx)
        .expect("bow produced");
    assert_eq!(bow.count, 1);
    // (40 + 10)/2 = 25, softcap ranged=10: (25 + 10)/2 = 17.
    assert_eq!(bow.ql, 17, "type-weighted quality with ranged softcap");
    // All inputs consumed.
    assert!(
        !g.world.players[pidx]
            .inv
            .iter()
            .any(|s| s.res == g.world.res.intern("gfx/invobjs/branch")),
        "branches fully consumed"
    );
}

/// Stone Arrows come out as ONE batch of ten per craft, and the
/// branch type weighs double the stone type (RoB Legacy:Quality
/// arrow example): stone q10 + branches q40 -> (10*1 + 40*2)/3 = 30,
/// softcap survive (unset -> 10): (30 + 10)/2 = 20.
#[tokio::test]
async fn stonearrow_bundles_ten_and_branch_weighs_double() {
    let (mut g, _rx, _raw) = entered_game("arrq");
    set_inv(
        &mut g,
        &[("gfx/invobjs/stone", 1, 10), ("gfx/invobjs/branch", 2, 40)],
    );
    assert!(g.craft_once(1, "stonearrow"), "craft must succeed");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let arr_gidx = g.world.res.intern("gfx/invobjs/arrow-stone");
    let arrows = g.world.players[pidx]
        .inv
        .iter()
        .find(|s| s.res == arr_gidx)
        .expect("stone arrows produced");
    assert_eq!(arrows.count, 10, "one craft yields a bundle of ten");
    // (10*1 + 40*2)/3 = 30, softcap survive=10: (30+10)/2 = 20.
    assert_eq!(arrows.ql, 20);
}

/// A Wooden Bow dropped into any equipment slot renders the dedicated
/// carrying layers (gfx/borka/eq-bow/.../arm/carrying/...) on the
/// world drawable AND on the paperdoll doll set.
#[tokio::test]
async fn bow_equip_renders_carrying_pose() {
    let (mut g, _rx, _raw) = entered_game("bowpose");
    set_inv(&mut g, &[("gfx/invobjs/bow", 1, 10)]);
    let pidx = *g.world.by_session.get(&1).unwrap();
    // Equip via the same slot the epry flow uses (slot 0).
    let stack = g.world.players[pidx].inv.pop().unwrap();
    g.world.players[pidx].equip[0] = Some(stack);
    let names: Vec<&'static str> = g.world.players[pidx]
        .equip
        .iter()
        .flatten()
        .filter_map(|s| g.world.res.name(s.res))
        .collect();
    let world = crate::equip::world_layers(names.iter(), false, 1);
    assert_eq!(world.len(), 2, "standing front: left + right carrying");
    assert!(world
        .iter()
        .all(|l| l.contains("eq-bow/standing/arm/carrying/")));
    let doll = crate::equip::doll_layers(names.iter());
    assert_eq!(doll.len(), 2, "doll renders the front carrying pair");
    assert!(
        doll.iter()
            .all(|l| l.contains("eq-bow/standing/arm/carrying/")),
        "doll layers: {doll:?}"
    );
    let walking = crate::equip::world_layers(names.iter(), true, 1);
    assert!(
        walking
            .iter()
            .all(|l| l.contains("eq-bow/walking/arm/carrying/")),
        "walking pose carries too"
    );
}

/// The bow chain must be craftable straight out of the starter kit
/// (the kit composition is the server policy that keeps the chain
/// playable with zero foraging).
#[tokio::test]
async fn starter_kit_covers_the_bow_chain() {
    let (mut g, _rx, _raw) = entered_game("bowkit");
    let pidx = *g.world.by_session.get(&1).unwrap();
    fn count(g: &mut Game, pidx: usize, res: &'static str) -> u32 {
        let gidx = g.world.res.intern(res);
        g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == gidx)
            .map(|s| s.count)
            .sum()
    }
    assert!(
        count(&mut g, pidx, "gfx/invobjs/branch") >= 4 + 2,
        "bow + arrow branches"
    );
    assert!(count(&mut g, pidx, "gfx/invobjs/string") >= 1, "bow string");
    assert!(count(&mut g, pidx, "gfx/invobjs/stone") >= 1, "arrow stone");
    // The menu must announce every new pagina (rendered MenuGrid).
    for page in [
        "paginae/craft/woodbow",
        "paginae/craft/stonearrow",
        "paginae/craft/bonearrow",
    ] {
        assert!(
            crate::craft::RECIPES.iter().any(|r| r.pagina == page),
            "{page} in RECIPES"
        );
    }
}

// ------------------------------------------------------------------
// Bow ranged combat (session 37, archery.rs)
// ------------------------------------------------------------------

/// The tool requirement (session 46): craft_once refuses a tool
/// recipe without the tool and crafts with it, never destroying the
/// ingredients on the refusal path.
#[tokio::test]
async fn bucket_craft_needs_the_saw() {
    let (mut g, _rx, _raw) = entered_game("sawbucket");
    let pidx = *g.world.by_session.get(&1).unwrap();
    let branch = g.world.res.intern("gfx/invobjs/branch");
    let saw = g.world.res.intern("gfx/invobjs/saw");
    let buckete = g.world.res.intern("gfx/invobjs/buckete");
    let give = |g: &mut Game, res: u16, n: u32| {
        g.world.players[pidx].inv.push(crate::state::InvStack {
            res,
            count: n,
            ql: 10,
            label: "",
        });
    };
    // Ingredients present, tool absent: refuse, nothing consumed
    // (the starter kit already carries branches - measure the
    // baseline and compare).
    give(&mut g, branch, 3);
    let branch_before = g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == branch)
        .map(|s| s.count)
        .sum::<u32>();
    assert!(branch_before >= 3, "branches were granted");
    assert!(!g.craft_once(1, "bucket"), "no saw -> no bucket");
    let branch_left = g.world.players[pidx]
        .inv
        .iter()
        .filter(|s| s.res == branch)
        .map(|s| s.count)
        .sum::<u32>();
    assert_eq!(
        branch_left, branch_before,
        "the refusal must not consume the ingredients"
    );
    // With the saw in the inventory the craft lands.
    give(&mut g, saw, 1);
    assert!(g.craft_once(1, "bucket"), "saw + branches -> bucket");
    let bucket = g.world.players[pidx]
        .inv
        .iter()
        .find(|s| s.res == buckete)
        .expect("bucket produced");
    assert_eq!(bucket.count, 1);
}

// ------------------------------------------------------------------
// Food Trough + feeding (session 48; animals-and-husbandry.md
// "Feeding: troughs and grazing").
// ------------------------------------------------------------------

// ------------------------------------------------------------------
// The sausage chain (session 77; crafting doc "Sausages" session note).
// ------------------------------------------------------------------

/// The wurst recipe set is internally consistent: every wurst id has a
/// meat-slot table entry and vice versa, the slot counts sum exactly to
/// the recipe's generic meat input, and every slot label is a real
/// fep.conf meat key (Fox Meat, Beef, ... - the same keys ROAST_MAP
/// rides on). The edible/inedible split is fep.conf's own: the five
/// labels the table carries eat; the other seven refuse (the table is
/// the truth, no invented numbers).
#[tokio::test]
async fn wurst_recipes_key_the_meat_slots_and_the_fep_table() {
    let (g, _rx, _raw) = entered_game("wursttable");
    let mut slot_ids: Vec<&str> = crate::craft::WURST_MEAT_SLOTS
        .iter()
        .map(|(id, _)| *id)
        .collect();
    slot_ids.sort_unstable();
    let mut recipe_ids: Vec<&str> = crate::craft::RECIPES
        .iter()
        .filter(|r| r.id.starts_with("wurst_"))
        .map(|r| r.id)
        .collect();
    recipe_ids.sort_unstable();
    assert_eq!(recipe_ids.len(), 12, "twelve of the thirteen wurst pages");
    assert_eq!(
        recipe_ids, slot_ids,
        "every wurst recipe has a meat-slot entry and nothing else does"
    );
    for (id, slots) in crate::craft::WURST_MEAT_SLOTS {
        let recipe = crate::craft::RECIPES.iter().find(|r| r.id == *id).unwrap();
        let meat_units: u32 = recipe
            .inputs
            .iter()
            .filter(|(res, _)| *res == crate::craft::MEAT_RES)
            .map(|(_, n)| n)
            .sum();
        let slot_sum: u32 = slots.iter().map(|(_, n)| n).sum();
        assert_eq!(
            meat_units, slot_sum,
            "{id}: the meat slots must replace the generic meat input exactly"
        );
        assert_eq!(recipe.outputs.len(), 1, "{id}: one wurst per craft");
        for (label, _) in slots.iter() {
            assert!(
                g.fep.get(label).is_some(),
                "{id}: slot label {label} must be a fep.conf meat key"
            );
        }
        // fep.conf carries a row for every implemented wurst (the
        // "Chicken Chorizo" and "Bierwurst" keys have no item resource
        // in the pack and stay unimplemented) - all twelve eat.
        assert!(
            g.fep.get(recipe.name).is_some(),
            "{}: the crafted label must resolve its fep.conf row",
            recipe.name
        );
    }
}

/// The label gate end to end through craft_once: Fox Wurst grinds Fox
/// Meat only - a Beef-carrying inventory refuses with the chat line,
/// and the correct meat crafts at the two-type quality average
/// (Fox Meat q30 + Intestines q20 -> 25, Perception-10 softcap -> 17).
#[tokio::test]
async fn fox_wurst_crafts_from_labeled_meat_and_refuses_the_wrong_species() {
    let (mut g, mut rx, _raw) = entered_game("wurstfox");
    let pidx = *g.world.by_session.get(&1).unwrap();
    // Beef instead of Fox Meat: the refusal names the missing label and
    // consumes nothing.
    set_inv_labeled(
        &mut g,
        &[
            ("gfx/invobjs/meat", 5, 40, "Beef"),
            ("gfx/invobjs/intestines", 1, 20, ""),
        ],
    );
    assert!(
        !g.craft_once(1, "wurst_fox"),
        "Beef must not pass the Fox Wurst meat gate"
    );
    let lines = drain_chat(&mut rx);
    assert!(
        lines.iter().any(|l| l.contains("You need the Fox Meat")),
        "the refusal names the missing meat label, got {lines:?}"
    );
    let meat = g.world.res.intern(crate::craft::MEAT_RES);
    assert_eq!(
        g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == meat)
            .map(|s| s.count)
            .sum::<u32>(),
        5,
        "the refusal must not consume the ingredients"
    );
    // The right labels craft: Fox Meat x2 + Intestines x1 -> wurst.
    set_inv_labeled(
        &mut g,
        &[
            ("gfx/invobjs/meat", 2, 30, "Fox Meat"),
            ("gfx/invobjs/meat", 5, 40, "Beef"),
            ("gfx/invobjs/intestines", 1, 20, ""),
        ],
    );
    assert!(g.craft_once(1, "wurst_fox"), "labeled meat + casing craft");
    let wurst = g.world.res.intern("gfx/invobjs/wurst-fox");
    let stack = g.world.players[pidx]
        .inv
        .iter()
        .find(|s| s.res == wurst)
        .expect("the wurst lands in the inventory");
    assert_eq!(stack.count, 1);
    assert_eq!(stack.ql, 17, "(30+20)/2 = 25 softcapped by per 10 -> 17");
    assert_eq!(
        stack.label, "Fox Wurst",
        "the crafted label carries the name"
    );
    // The Fox Meat slots are gone; the Beef stack survives untouched.
    assert_eq!(
        g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == meat && s.label == "Fox Meat")
            .map(|s| s.count)
            .sum::<u32>(),
        0,
        "the Fox Meat slots are consumed"
    );
    assert_eq!(
        g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == meat && s.label == "Beef")
            .map(|s| s.count)
            .sum::<u32>(),
        5,
        "the Beef stack is untouched"
    );
}

/// The session-77 butcher loot: Intestines follow the doc's table
/// verbatim (Aurochs/Cattle/Bear x4, Deer x3, Boar/Sheep x2, Fox x1,
/// mouflon policy 1; Wolf/Hare/Hen drop none per their rows), and the
/// two new species carry their doc yields (Bear Meat x8 + the bear
/// hide; Raw Chicken Meat + Chicken Feather x3).
#[test]
fn intestines_loot_follows_the_butcher_table() {
    use crate::state::Species;
    let expect: &[(Species, u32)] = &[
        (Species::Deer, 3),
        (Species::Aurochs, 4),
        (Species::Cow, 4),
        (Species::Boar, 2),
        (Species::Fox, 1),
        (Species::Wolf, 0),
        (Species::Hare, 0),
        (Species::Mouflon, 1),
        (Species::Sheep, 2),
        (Species::Bear, 4),
        (Species::Hen, 0),
    ];
    for (sp, want) in expect {
        let got = sp
            .loot()
            .iter()
            .filter(|(res, _, _)| *res == "gfx/invobjs/intestines")
            .map(|(_, n, _)| n)
            .sum::<u32>();
        assert_eq!(
            &got, want,
            "{sp:?}: the intestines yield must match the doc table"
        );
    }
    let bear = Species::Bear.loot();
    assert!(
        bear.iter()
            .any(|(res, n, _)| *res == "gfx/invobjs/meat" && *n == 8),
        "the bear mirrors its doc row: Meat x8"
    );
    assert!(
        bear.iter()
            .any(|(res, _, _)| *res == "gfx/invobjs/hide-raw-bear"),
        "the bear drops the raw bear hide"
    );
    assert_eq!(Species::Bear.meat_label(), "Bear Meat");
    assert_eq!(Species::Bear.name(), "Bear");
    let hen = Species::Hen.loot();
    assert!(
        hen.iter()
            .any(|(res, n, _)| *res == "gfx/invobjs/feather-chicken" && *n == 3),
        "the hen mirrors its doc row: Chicken Feather x3"
    );
    assert_eq!(Species::Hen.meat_label(), "Raw Chicken Meat");
    assert_eq!(Species::Hen.name(), "Hen");
    // The pose routing covers the whole roster (pose.rs arrays sized by
    // the enum count - an uncompiled addition would panic at runtime).
    assert_eq!(Species::ALL.len(), 11);
    assert_eq!(Species::Bear.index(), 9);
    assert_eq!(Species::Hen.index(), 10);
    assert_eq!(Species::from_index(9), Some(Species::Bear));
    assert_eq!(Species::from_index(10), Some(Species::Hen));
    assert_eq!(Species::from_index(11), None);
}
