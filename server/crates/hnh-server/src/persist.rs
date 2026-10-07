//! Persistence: player character snapshots survive restarts.
//!
//! Design: a single JSON save file holding per-character snapshots. The
//! world itself is seed-deterministic (pure function of `--seed`), so only
//! mutable player-owned state needs to be stored. Item resources are saved
//! by name, not by the process-local resource index, so ids stay stable
//! across runs. Writes are atomic (tmp file + rename) so a crash mid-write
//! cannot corrupt the save.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::Player;

/// Full save key of a character: `<account>:<charname>`, with `:`
/// stripped from the account so the separator stays unambiguous.
/// The account is the authenticated login user (one character per
/// account), which keeps two accounts' characters from colliding in the
/// store and makes cluster character migration queries exact.
pub fn save_key(account: &str, charname: &str) -> String {
    let account = account.replace(':', "_");
    format!("{account}:{charname}")
}

/// A serialized character snapshot (world position in subtiles).
/// `name` holds the full save key (`save_key`), not the display name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedPlayer {
    pub name: String,
    pub pos: (i32, i32),
    pub hp: i32,
    pub energy: i32,
    pub stamina: i32,
    pub lp: i32,
    pub attrs: HashMap<String, i32>,
    /// Inventory stacks as (resource name, count, quality).
    pub inv: Vec<(String, u32, u8)>,
    /// Parallel display labels for `inv` (server-sent food names). Entries
    /// may be shorter than `inv` or absent (v1 saves): missing labels load
    /// as empty strings. NOTE: `skip_serializing_if` is intentionally NOT
    /// used here - NodeMsg ships this struct over bincode, which is not
    /// self-describing; omitted fields break positional deserialization.
    #[serde(default)]
    pub inv_labels: Vec<String>,
    /// Purchased non-incrementable skills as `gfx/hud/skills/` basenames
    /// (v2, additive; absent in v1 saves -> empty). Incrementable skill
    /// values live in `attrs`. Bincode-safe: `default` only, never skip.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Equipped paperdoll items as (slot 0..15, resource name, count,
    /// quality, label); only occupied slots are stored (v4, additive;
    /// absent in older saves -> everything unequipped). Bincode-safe.
    #[serde(default)]
    pub equip: Vec<(usize, String, u32, u8, String)>,
    /// Criminal-flag expiry in world ms (v5, additive; absent in older
    /// saves -> clean record). None while the record is clean.
    #[serde(default)]
    pub criminal_until_ms: Option<u64>,
}

/// Top-level save container. Bump VERSION on incompatible changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveData {
    pub version: u32,
    pub seed: u64,
    pub saved_at_unix: u64,
    pub players: Vec<SavedPlayer>,
    /// Persisted growing crops (v2, additive): gob resource name + state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crops: Vec<SavedCrop>,
    /// Persisted furrowed tiles: (tile x, tile y) -> decay deadline unix-ms
    /// (0 = planted). v2, additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tilth: Vec<((i32, i32), u64)>,
    /// Persisted tile overrides (terraforming): (tx, ty) -> tile id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tile_overrides: Vec<((i32, i32), u8)>,
    /// Persisted construction plans (v3, additive): half-built sites keep
    /// their credited materials across restarts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plans: Vec<SavedPlan>,
    /// Persisted finished structures and stations (v3, additive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structures: Vec<SavedStructure>,
    /// Persisted tamed animals (session 47, additive): tameness +
    /// production meters survive restarts (v6; absent in older saves).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animals: Vec<SavedAnimal>,
}

/// A persisted construction plan. Credited materials are saved by item
/// resource name with the snapshotted delivery qualities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPlan {
    /// Index into `build::BUILDABLES` (registry order is stable).
    pub spec: u8,
    /// Tile coordinates (11x11 map units per tile).
    pub tile: (i32, i32),
    /// Credited deliveries: (item resource name, units, quality sum).
    pub credited: Vec<(String, u32, u64)>,
}

/// A persisted finished structure (station or plain).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedStructure {
    pub spec: u8,
    pub tile: (i32, i32),
    /// Structure quality snapshotted at completion.
    pub quality: u8,
    /// Station fields (zero for plain structures).
    #[serde(default)]
    pub fuel: u32,
    #[serde(default)]
    pub fuel_ql_sum: u64,
    #[serde(default)]
    pub fuel_seen: u64,
    /// Loaded station input: (item resource name, quality, label).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<(String, u8, String)>,
    #[serde(default)]
    pub progress: u32,
    /// Food Trough fodder store (session 48, additive; zero for every
    /// other structure). Quality history: sum and count of the units
    /// ever placed (the running average the doc's q5+q12+q16 -> q11).
    #[serde(default)]
    pub fodder_units: u32,
    #[serde(default)]
    pub fodder_ql_sum: u64,
    #[serde(default)]
    pub fodder_seen: u64,
}

/// A persisted growing crop. Resource names keep the entry stable across
/// process-local resource renumbering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCrop {
    /// Planted gob resource (`gfx/terobjs/plants/...`).
    pub res: String,
    /// Tile coordinates (11x11 map units per tile).
    pub tile: (i32, i32),
    /// Crop spec index into `farm::CROPS` (registry order is stable).
    pub spec: u8,
    pub stage: u8,
    pub seed_ql: u8,
    pub soil_ql: u8,
    /// Absolute unix-ms deadline of the next stage advance.
    pub next_stage_at: u64,
}

/// A persisted tamed animal (session 47; additive). Spawned wildlife is
/// seed-regenerated and never saved; tamed animals carry runtime state
/// the doc requires to survive restarts: tameness, the production
/// meters and the domestic morph (the saved species IS the morph).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedAnimal {
    /// Species index (state::Species::index; the append-only order).
    pub species: u8,
    /// Tile coordinates (11x11 map units per tile).
    pub tile: (i32, i32),
    /// Current hit points (clamped to the species max on load).
    pub hp: i32,
    /// Accumulated tameness (0..=100; only rows with tameness > 0 save).
    pub tameness: i32,
    /// The tamer's character save key (`account:name`). Empty when the
    /// tamer is unknown (offline at save time): the binding
    /// re-establishes on the next quell, which overwrites the row.
    #[serde(default)]
    pub tamer_key: String,
    /// Stored milk in 0.01 L units (cows).
    #[serde(default)]
    pub milk_units: u32,
    /// Stored wool count (sheep).
    #[serde(default)]
    pub wool: u8,
    /// Production accumulator (quantity-ticks toward the next unit).
    #[serde(default)]
    pub prod_acc: u32,
    /// Trough-feeding accumulator in nano-units (session 48, additive):
    /// the fractional part of consumption between whole fodder units.
    #[serde(default)]
    pub feed_acc_nano: u64,
    /// Consecutive unfed ticks (session 48 starvation timer).
    #[serde(default)]
    pub hunger: u64,
}

impl SaveData {
    /// v7: trough fodder + animal feeding/starvation fields (session
    /// 48). Additive only - older files load through the per-field
    /// serde defaults.
    pub const VERSION: u32 = 7;

    pub fn new(seed: u64) -> Self {
        SaveData {
            version: Self::VERSION,
            seed,
            saved_at_unix: now_unix(),
            players: Vec::new(),
            crops: Vec::new(),
            tilth: Vec::new(),
            tile_overrides: Vec::new(),
            plans: Vec::new(),
            structures: Vec::new(),
            animals: Vec::new(),
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Store that owns the save path and the authoritative in-memory snapshots.
pub struct SaveStore {
    path: PathBuf,
    /// Name -> latest snapshot (online players overwrite on autosave).
    pub players: HashMap<String, SavedPlayer>,
    /// World-state snapshot taken at last flush (crops + tilth).
    pub world_state: WorldState,
}

/// World-level persisted state gathered by the game task at flush time.
#[derive(Default)]
pub struct WorldState {
    pub crops: Vec<SavedCrop>,
    pub tilth: Vec<((i32, i32), u64)>,
    pub tile_overrides: Vec<((i32, i32), u8)>,
    pub plans: Vec<SavedPlan>,
    pub structures: Vec<SavedStructure>,
    /// Tamed animals (session 47).
    pub animals: Vec<SavedAnimal>,
}

impl SaveStore {
    /// Load the store from `path`, returning snapshots of previously seen
    /// characters. A missing file is a fresh world; a corrupt file logs and
    /// starts fresh rather than wedging the server.
    pub fn load(path: &Path, expected_seed: u64) -> SaveStore {
        let (players, world_state) = match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<SaveData>(&bytes) {
                Ok(data) if data.seed == expected_seed => {
                    let ws = WorldState {
                        crops: data.crops.clone(),
                        tilth: data.tilth.clone(),
                        tile_overrides: data.tile_overrides.clone(),
                        plans: data.plans.clone(),
                        structures: data.structures.clone(),
                        animals: data.animals.clone(),
                    };
                    (
                        data.players
                            .into_iter()
                            .map(|p| (p.name.clone(), p))
                            .collect(),
                        ws,
                    )
                }
                Ok(data) => {
                    tracing::warn!(
                        path = %path.display(),
                        stored = data.seed,
                        expected = expected_seed,
                        "save seed mismatch: starting fresh characters"
                    );
                    (HashMap::new(), WorldState::default())
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "unreadable save file: starting fresh");
                    (HashMap::new(), WorldState::default())
                }
            },
            Err(_) => (HashMap::new(), WorldState::default()),
        };
        tracing::info!(
            path = %path.display(),
            saved_chars = players.len(),
            saved_crops = world_state.crops.len(),
            "save store loaded"
        );
        SaveStore {
            path: path.to_path_buf(),
            players,
            world_state,
        }
    }

    /// Snapshot one online player under its account save key. `inv_named`
    /// carries the inventory already translated from process-local indices
    /// to resource names, with the display labels parallel to the stacks.
    /// `equip_named` carries the occupied paperdoll slots as
    /// (slot, resname, count, ql, label).
    pub fn snapshot(
        &mut self,
        p: &Player,
        pos: (i32, i32),
        inv_named: Vec<(String, u32, u8)>,
        inv_labels: Vec<String>,
        equip_named: Vec<(usize, String, u32, u8, String)>,
    ) {
        let key = save_key(&p.account, &p.name);
        self.players.insert(
            key.clone(),
            SavedPlayer {
                name: key,
                pos,
                hp: p.hp,
                energy: p.energy,
                stamina: p.stamina,
                lp: p.lp,
                attrs: p.attrs.clone(),
                inv: inv_named,
                inv_labels,
                skills: p.skills.iter().map(|s| s.to_string()).collect(),
                equip: equip_named,
                criminal_until_ms: p.criminal_until_ms,
            },
        );
    }

    /// Atomically write the current snapshots to disk.
    pub fn flush(&self, seed: u64) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut data = SaveData::new(seed);
        data.players = self.players.values().cloned().collect();
        data.players.sort_by(|a, b| a.name.cmp(&b.name));
        data.crops = self.world_state.crops.clone();
        data.tilth = self.world_state.tilth.clone();
        data.plans = self.world_state.plans.clone();
        data.structures = self.world_state.structures.clone();
        // Session 47: animals + the tile_overrides fixup. flush()
        // previously dropped tile_overrides (furrows reverted on every
        // restart even though world_state carried them) - both fields
        // now round-trip.
        data.tile_overrides = self.world_state.tile_overrides.clone();
        data.animals = self.world_state.animals.clone();
        let bytes = serde_json::to_vec(&data)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_players() {
        let dir = std::env::temp_dir().join(format!("hnh-persist-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("world.json");

        let mut store = SaveStore::load(&path, 42);
        store.snapshot(
            &Player {
                account: "tester".to_owned(),
                name: "tester".to_owned(),
                gob: 1,
                equip: Vec::new(),
                session: 1,
                hp: 77,
                energy: 55,
                stamina: 99,
                lp: 12,
                criminal_until_ms: None,
                lp_carry_ms: 0,
                gait: 1,
                skills: std::collections::HashSet::new(),
                attrs: HashMap::from([("str".to_owned(), 14)]),
                inv: Vec::new(),
                fep: crate::craft::FepState::default(),
                fight_target: None,
                atk_cd: 0,
                aim: None,
            },
            (123, -456),
            vec![("gfx/invobjs/stone".to_owned(), 3, 7)],
            vec![String::new()],
            Vec::new(),
        );
        store.flush(42).unwrap();

        let reloaded = SaveStore::load(&path, 42);
        let p = reloaded
            .players
            .get(&save_key("tester", "tester"))
            .expect("snapshot persisted");
        assert_eq!(p.pos, (123, -456));
        assert_eq!(p.hp, 77);
        assert_eq!(p.lp, 12);
        assert_eq!(p.inv[0].0, "gfx/invobjs/stone");
        assert_eq!(p.inv_labels.len(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_key_separates_accounts_and_neutralizes_colons() {
        assert_eq!(save_key("alice", "Player"), "alice:Player");
        assert_eq!(save_key("a:b", "Player"), "a_b:Player");
        assert_ne!(save_key("a:b", "Player"), save_key("a", "b:Player"));
    }

    #[test]
    fn seed_mismatch_starts_fresh() {
        let dir = std::env::temp_dir().join(format!("hnh-persist-seed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("world.json");
        let mut store = SaveStore::load(&path, 42);
        store.snapshot(
            &Player {
                account: "a".to_owned(),
                name: "a".to_owned(),
                equip: Vec::new(),
                gob: 1,
                session: 1,
                hp: 10,
                energy: 10,
                stamina: 10,
                lp: 0,
                gait: 1,
                criminal_until_ms: None,
                lp_carry_ms: 0,
                skills: std::collections::HashSet::new(),
                attrs: HashMap::new(),
                inv: Vec::new(),
                fep: crate::craft::FepState::default(),
                fight_target: None,
                atk_cd: 0,
                aim: None,
            },
            (0, 0),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        store.flush(42).unwrap();
        let reloaded = SaveStore::load(&path, 43);
        assert!(reloaded.players.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
