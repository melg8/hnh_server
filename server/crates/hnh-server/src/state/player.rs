//! Player character state: vitals, inventory and equipment
//! stacks.

use std::collections::{HashMap, HashSet};

use super::{GobId, SessionId, TroughState};

/// One inventory stack. `label` is the server-sent display name (item
/// widget tooltip, Item.name() precedence in food-and-fep.md); empty for
/// non-food items whose resource tooltip the client resolves locally.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InvStack {
    pub res: u16,
    pub count: u32,
    pub ql: u8,
    pub label: &'static str,
}

impl InvStack {
    /// Merge `added` into `self` (same resource only, caller checks).
    /// Counts add; quality re-averages as a count-weighted integer
    /// average, the same convention as craft output quality
    /// (crafting-and-building.md, loftar sum(q*w)/sum(w)). The stacking
    /// policy itself is server policy per items-and-quality.md; this is
    /// the chosen policy and the counts are conserved exactly.
    pub fn absorb(&mut self, added: &InvStack) {
        debug_assert_eq!(self.res, added.res, "absorb requires same resource");
        let total = self.count + added.count;
        if total == 0 {
            return; // nothing to average; keep stacks as-is
        }
        let qsum = self.ql as u32 * self.count + added.ql as u32 * added.count;
        self.ql = ((qsum / total).clamp(1, 255)) as u8;
        self.count = total;
    }
}

/// Character vitals and attributes (docs/mechanics/character/*).
#[derive(Debug, Clone)]
pub struct Player {
    /// In-world display name (the charlist entry the client picked).
    pub name: String,
    /// Authenticated login account. Save keys are `account:name`, so two
    /// accounts never share a character even though the legacy client's
    /// single charlist entry is always called "Player".
    pub account: String,
    pub gob: GobId,
    pub session: SessionId,
    /// Health 0..100 (SHP); reaching 0 knocks out then kills.
    pub hp: i32,
    /// Energy / hunger pool 0..100 ("hngr" meter).
    pub energy: i32,
    /// Stamina 0..100, drained by sprint/fight, refilled by energy.
    pub stamina: i32,
    /// Learning points (LP currency).
    pub lp: i32,
    /// Wall-clock world time (`world.now_ms`) until which the player is
    /// flagged CRIMINAL for a PvP knockout (the assault policy,
    /// combat-system.md "PvP knockout consequences"). None = clean
    /// record. Persisted; refreshed by every new knockout.
    pub criminal_until_ms: Option<u64>,
    /// Selected movement gait (speedget index; GAIT_SPEEDS entry).
    pub gait: u8,
    /// Fractional LP accrual carry (ms toward the next point; see
    /// `skills::accrue`).
    pub lp_carry_ms: u64,
    /// Purchased non-incrementable skills (`gfx/hud/skills/` basenames).
    pub skills: HashSet<&'static str>,
    /// Base attributes (docs: str/agi/int/vit/con/psy/emp...).
    pub attrs: HashMap<String, i32>,
    /// Inventory stacks.
    pub inv: Vec<InvStack>,
    /// Equipment (the Equipory paperdoll, widget type "epry"): exactly 16
    /// slots indexed by the wire slot index 0..15. Slot semantics are
    /// server-side (docs/mechanics/items/items-and-quality.md); the client
    /// addresses slots purely by index. Occupied slots hold one stack.
    pub equip: Vec<Option<InvStack>>,
    /// Food Event Point accumulators (integer tenths per attribute).
    pub fep: crate::craft::FepState,
    /// Currently open fight target (gob id) or none.
    pub fight_target: Option<GobId>,
    /// Attack cooldown in ticks.
    pub atk_cd: i32,
    /// Active ranged aim (bow equipped; see archery.rs). Mutually
    /// exclusive with `fight_target`.
    pub aim: Option<crate::archery::RangedAim>,
    /// The Food Trough this player lifted (animals-and-husbandry.md
    /// "Feeding: troughs and grazing"): a lifted trough leaves the world
    /// with its fodder store intact and rides the player until a map
    /// click places it back down or a click on another trough transfers
    /// the fodder "like a liquid". Persisted per character.
    pub carried_trough: Option<TroughState>,
}
