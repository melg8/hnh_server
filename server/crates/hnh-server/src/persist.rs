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

/// A serialized character snapshot (world position in subtiles).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// as empty strings. Kept additive so v1 files stay readable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inv_labels: Vec<String>,
}

/// Top-level save container. Bump VERSION on incompatible changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveData {
    pub version: u32,
    pub seed: u64,
    pub saved_at_unix: u64,
    pub players: Vec<SavedPlayer>,
}

impl SaveData {
    pub const VERSION: u32 = 1;

    pub fn new(seed: u64) -> Self {
        SaveData {
            version: Self::VERSION,
            seed,
            saved_at_unix: now_unix(),
            players: Vec::new(),
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
}

impl SaveStore {
    /// Load the store from `path`, returning snapshots of previously seen
    /// characters. A missing file is a fresh world; a corrupt file logs and
    /// starts fresh rather than wedging the server.
    pub fn load(path: &Path, expected_seed: u64) -> SaveStore {
        let players = match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<SaveData>(&bytes) {
                Ok(data) if data.seed == expected_seed => data
                    .players
                    .into_iter()
                    .map(|p| (p.name.clone(), p))
                    .collect(),
                Ok(data) => {
                    tracing::warn!(
                        path = %path.display(),
                        stored = data.seed,
                        expected = expected_seed,
                        "save seed mismatch: starting fresh characters"
                    );
                    HashMap::new()
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "unreadable save file: starting fresh");
                    HashMap::new()
                }
            },
            Err(_) => HashMap::new(),
        };
        tracing::info!(path = %path.display(), saved_chars = players.len(), "save store loaded");
        SaveStore {
            path: path.to_path_buf(),
            players,
        }
    }

    /// Snapshot one online player. `inv_named` carries the inventory already
    /// translated from process-local indices to resource names, with the
    /// display labels parallel to the stacks.
    pub fn snapshot(
        &mut self,
        p: &Player,
        pos: (i32, i32),
        inv_named: Vec<(String, u32, u8)>,
        inv_labels: Vec<String>,
    ) {
        self.players.insert(
            p.name.clone(),
            SavedPlayer {
                name: p.name.clone(),
                pos,
                hp: p.hp,
                energy: p.energy,
                stamina: p.stamina,
                lp: p.lp,
                attrs: p.attrs.clone(),
                inv: inv_named,
                inv_labels,
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
                name: "tester".to_owned(),
                gob: 1,
                session: 1,
                hp: 77,
                energy: 55,
                stamina: 99,
                lp: 12,
                attrs: HashMap::from([("str".to_owned(), 14)]),
                inv: Vec::new(),
                fep: crate::craft::FepState::default(),
                fight_target: None,
                atk_cd: 0,
            },
            (123, -456),
            vec![("gfx/invobjs/stone".to_owned(), 3, 7)],
            vec![String::new()],
        );
        store.flush(42).unwrap();

        let reloaded = SaveStore::load(&path, 42);
        let p = reloaded.players.get("tester").expect("snapshot persisted");
        assert_eq!(p.pos, (123, -456));
        assert_eq!(p.hp, 77);
        assert_eq!(p.lp, 12);
        assert_eq!(p.inv[0].0, "gfx/invobjs/stone");
        assert_eq!(p.inv_labels.len(), 1);

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
                name: "a".to_owned(),
                gob: 1,
                session: 1,
                hp: 10,
                energy: 10,
                stamina: 10,
                lp: 0,
                attrs: HashMap::new(),
                inv: Vec::new(),
                fep: crate::craft::FepState::default(),
                fight_target: None,
                atk_cd: 0,
            },
            (0, 0),
            Vec::new(),
            Vec::new(),
        );
        store.flush(42).unwrap();
        let reloaded = SaveStore::load(&path, 43);
        assert!(reloaded.players.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
