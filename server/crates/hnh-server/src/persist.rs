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

use crate::state::{Player, TroughState};

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
    /// The lifted Food Trough's fodder store (v7, additive; absent in
    /// older saves -> nothing carried). Persisted so a lifted trough
    /// with its fodder survives restarts with the character.
    #[serde(default)]
    pub carried_trough: Option<SavedTrough>,
}

/// A carried (lifted) Food Trough's fodder store (session 62). The
/// placement is NOT carried - a placed-back trough picks up the tile
/// it lands on; only the fodder state rides the character.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct SavedTrough {
    pub units: u32,
    pub ql_sum: u64,
    pub ql_seen: u64,
}

impl From<TroughState> for SavedTrough {
    fn from(t: TroughState) -> Self {
        SavedTrough {
            units: t.units,
            ql_sum: t.ql_sum,
            ql_seen: t.ql_seen,
        }
    }
}

impl From<SavedTrough> for TroughState {
    fn from(t: SavedTrough) -> Self {
        TroughState {
            units: t.units,
            ql_sum: t.ql_sum,
            ql_seen: t.ql_seen,
        }
    }
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
    /// Alloying Crucible aux slot (session 66, additive): the tin bar
    /// beside the copper input. None for every other station kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aux: Option<(String, u8, String)>,
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
                carried_trough: p.carried_trough.map(SavedTrough::from),
            },
        );
    }

    /// Snapshot the current state into an owned `SaveData` (the clone
    /// phase of a flush). Measured (S75 bench): 8 ms at 1k players, 55 ms
    /// at 10k - the only part of a save the game loop ever pays.
    pub fn snapshot_data(&self, seed: u64) -> SaveData {
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
        data
    }

    /// Pure blocking writer: serialize + atomic tmp+rename. Measured
    /// (S75 bench): 87 ms at 1k players, 464 ms at 10k - the 86%+ bulk
    /// of every save. Must run on a blocking thread (see
    /// `flush_background`), never on the game loop. A unique tmp suffix
    /// keeps overlapping background writes from sharing a partial file;
    /// the final rename is still atomic.
    pub fn write_file(path: &Path, data: &SaveData) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec(data)?;
        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tmp = PathBuf::from(format!(
            "{}.tmp{}",
            path.display(),
            TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Atomically write the current snapshots to disk (blocking). The
    /// shutdown-path and test entry point: the process is going away, so
    /// the write must be synchronous. On the live game loop use
    /// `flush_background` instead.
    pub fn flush(&self, seed: u64) -> anyhow::Result<()> {
        let data = self.snapshot_data(seed);
        Self::write_file(&self.path, &data)
    }

    /// flush() for the live game loop: the snapshot clone happens inline
    /// (cheap, measured above), then the serialize+write tail - the bulk
    /// of the stall - runs on a blocking thread. Without this the 30 s
    /// autosave froze every tick for the full serialize+write time (S75
    /// bench: 95 ms per save at 1k players, 519 ms at 10k, against the
    /// 100 ms tick budget). Errors are logged inside the blocking task.
    pub fn flush_background(&self, seed: u64) -> tokio::task::JoinHandle<()> {
        let data = self.snapshot_data(seed);
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = Self::write_file(&path, &data) {
                tracing::warn!(error = %e, "background save write failed");
            }
        })
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
                carried_trough: None,
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

    /// A lifted Food Trough (session 62) rides the character through
    /// the save round trip: the fodder store (units + quality history)
    /// must come back byte-identical.
    #[test]
    fn roundtrip_preserves_a_carried_trough() {
        let dir = std::env::temp_dir().join(format!("hnh-trough-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("world.json");

        let mut store = SaveStore::load(&path, 42);
        store.snapshot(
            &Player {
                account: "carrier".to_owned(),
                name: "carrier".to_owned(),
                gob: 1,
                equip: Vec::new(),
                session: 1,
                hp: 100,
                energy: 100,
                stamina: 100,
                lp: 0,
                criminal_until_ms: None,
                lp_carry_ms: 0,
                gait: 1,
                skills: std::collections::HashSet::new(),
                attrs: HashMap::new(),
                inv: Vec::new(),
                fep: crate::craft::FepState::default(),
                fight_target: None,
                atk_cd: 0,
                aim: None,
                carried_trough: Some(crate::state::TroughState {
                    units: 37,
                    ql_sum: 555,
                    ql_seen: 45,
                }),
            },
            (10, 20),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        store.flush(42).unwrap();

        let reloaded = SaveStore::load(&path, 42);
        let p = reloaded
            .players
            .get(&save_key("carrier", "carrier"))
            .expect("the carrying character persisted");
        let t = p.carried_trough.expect("the carried trough persisted");
        assert_eq!(t.units, 37);
        assert_eq!(t.ql_sum, 555);
        assert_eq!(t.ql_seen, 45);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_key_separates_accounts_and_neutralizes_colons() {
        assert_eq!(save_key("alice", "Player"), "alice:Player");
        assert_eq!(save_key("a:b", "Player"), "a_b:Player");
        assert_ne!(save_key("a:b", "Player"), save_key("a", "b:Player"));
    }

    /// Session-75 pin: the game-loop save path. The blocking-thread
    /// write must produce exactly what the synchronous flush produces
    /// (same file content contract, atomic rename included).
    #[tokio::test]
    async fn flush_background_matches_flush() {
        let dir = std::env::temp_dir().join(format!("hnh-persist-bg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("world.json");
        let mut store = SaveStore::load(&path, 42);
        store.snapshot(
            &Player {
                account: "bg".to_owned(),
                name: "bg".to_owned(),
                equip: Vec::new(),
                gob: 1,
                session: 1,
                hp: 42,
                energy: 10,
                stamina: 10,
                lp: 5,
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
                carried_trough: None,
            },
            (7, 9),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        store.flush_background(42).await.unwrap();
        let reloaded = SaveStore::load(&path, 42);
        assert_eq!(reloaded.players.len(), 1);
        let p = reloaded
            .players
            .get(&save_key("bg", "bg"))
            .expect("background-written character persisted");
        assert_eq!(p.pos, (7, 9));
        // No tmp residue from the unique-suffix rename.
        let residue: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.unwrap().file_name().into_string().ok())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(residue.is_empty(), "tmp files left behind: {residue:?}");
        std::fs::remove_dir_all(&dir).unwrap();
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
                carried_trough: None,
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

    /// Session-75 architecture bench (explicit run, never in the gate):
    /// how expensive is the blocking world flush against the 10k-session
    /// target. Builds synthetic saves at the 1k and 10k player marks with
    /// realistic per-character volume (12 inventory stacks + labels, 20
    /// skills, 8 attrs, 3 equipped slots) and a fixed world-state volume,
    /// then times the three flush phases separately (the value clone, the
    /// JSON serialize, the file write), the full flush() as a checksum and
    /// the full load() read-back. The game loop currently runs flush()
    /// inline every 30 s, so the total is the per-30 s tick stall.
    ///   cargo test -p hnh-server --bin hnh-server persist_flush_phase_bench \
    ///     -- --ignored --nocapture
    #[test]
    #[ignore]
    fn persist_flush_phase_bench_scales_to_10k() {
        fn synth_player(i: usize) -> SavedPlayer {
            SavedPlayer {
                name: save_key(&format!("user{i}"), "Player"),
                pos: (i as i32 * 11, (i as i32 % 97) * 11),
                hp: 100,
                energy: 90,
                stamina: 80,
                lp: 1000 + i as i32,
                attrs: (0..8).map(|k| (format!("attr{k}"), 10 + k)).collect(),
                inv: (0..12)
                    .map(|k| {
                        (
                            format!("gfx/invobjs/bench-item-{}", k % 6),
                            10 + k as u32,
                            10 + k as u8,
                        )
                    })
                    .collect(),
                inv_labels: (0..12).map(|k| format!("Bench Item {k}")).collect(),
                skills: (0..20).map(|k| format!("skill-{k}")).collect(),
                equip: (0..3)
                    .map(|k| {
                        (
                            k,
                            format!("gfx/armors/bench-{}", k),
                            1,
                            10,
                            format!("Bench {k}"),
                        )
                    })
                    .collect(),
                criminal_until_ms: None,
                carried_trough: None,
            }
        }

        fn synth_world_state() -> WorldState {
            WorldState {
                crops: (0..5_000)
                    .map(|i| SavedCrop {
                        res: format!("gfx/terobjs/plants/crop-{}", i % 7),
                        tile: (i % 300, i / 300),
                        spec: (i % 5) as u8,
                        stage: (i % 4) as u8,
                        seed_ql: 10,
                        soil_ql: 10,
                        next_stage_at: i as u64,
                    })
                    .collect(),
                tilth: (0..3_000).map(|i| ((i % 300, i / 300), i as u64)).collect(),
                tile_overrides: (0..5_000)
                    .map(|i| ((i % 300, i / 300), (i % 8) as u8))
                    .collect(),
                plans: (0..1_000)
                    .map(|i| SavedPlan {
                        spec: (i % 8) as u8,
                        tile: (i % 300, i / 300),
                        credited: (0..4)
                            .map(|k| (format!("gfx/invobjs/mat-{}", k), 10, 100))
                            .collect(),
                    })
                    .collect(),
                structures: (0..800)
                    .map(|i| SavedStructure {
                        spec: (i % 8) as u8,
                        tile: (i % 300, i / 300),
                        quality: 10,
                        fuel: 5,
                        fuel_ql_sum: 50,
                        fuel_seen: 5,
                        input: Some(("gfx/invobjs/input".into(), 10, "Input".into())),
                        aux: None,
                        progress: 100,
                        fodder_units: 0,
                        fodder_ql_sum: 0,
                        fodder_seen: 0,
                    })
                    .collect(),
                animals: (0..2_000)
                    .map(|i| SavedAnimal {
                        species: (i % 10) as u8,
                        tile: (i % 300, i / 300),
                        hp: 50,
                        tameness: 40,
                        tamer_key: save_key(&format!("user{}", i % 900), "Player"),
                        milk_units: 0,
                        wool: 0,
                        prod_acc: 0,
                        feed_acc_nano: 0,
                        hunger: 0,
                    })
                    .collect(),
            }
        }

        fn timed<T>(label: &str, f: impl FnOnce() -> T) -> (T, u128) {
            let t = std::time::Instant::now();
            let out = f();
            let ms = t.elapsed().as_millis();
            println!("    {label}: {ms} ms");
            (out, ms)
        }

        let dir = std::env::temp_dir().join(format!("hnh-persist-bench-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("world.json");

        for count in [1_000usize, 10_000] {
            println!("  players = {count} (+ fixed world-state volume)");
            let store = SaveStore {
                path: path.clone(),
                players: (0..count)
                    .map(synth_player)
                    .map(|p| (p.name.clone(), p))
                    .collect(),
                world_state: synth_world_state(),
            };
            // Phase decomposition mirrors flush()'s own steps.
            let mut data = SaveData::new(42);
            let clone_ms = timed("clone (players+world_state -> SaveData)", || {
                data.players = store.players.values().cloned().collect();
                data.players.sort_by(|a, b| a.name.cmp(&b.name));
                data.crops = store.world_state.crops.clone();
                data.tilth = store.world_state.tilth.clone();
                data.plans = store.world_state.plans.clone();
                data.structures = store.world_state.structures.clone();
                data.tile_overrides = store.world_state.tile_overrides.clone();
                data.animals = store.world_state.animals.clone();
            })
            .1;
            let (bytes, ser_ms) = timed("serialize (serde_json::to_vec)", || {
                serde_json::to_vec(&data).unwrap()
            });
            let size_mb = bytes.len() as f64 / (1024.0 * 1024.0);
            println!("    file size: {size_mb:.1} MB");
            let write_ms = timed("write (tmp + rename)", || {
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, &bytes).unwrap();
                std::fs::rename(&tmp, &path).unwrap();
            })
            .1;
            println!(
                "    total stall (clone+ser+write): {} ms",
                clone_ms + ser_ms + write_ms
            );
            let flush_ms = timed("flush() checksum", || store.flush(42).unwrap()).1;
            assert!(
                flush_ms <= clone_ms + ser_ms + write_ms + 40,
                "flush must not be slower than its phases"
            );
            let load_ms = timed("load() read-back", || {
                let st = SaveStore::load(&path, 42);
                assert_eq!(st.players.len(), count);
            })
            .1;
            assert!(
                load_ms < 5_000,
                "load must stay well under any startup budget"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
