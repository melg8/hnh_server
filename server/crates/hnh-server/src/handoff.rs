//! Handoff protocol: keep LLM session context across restarts.
//!
//! `HANDOFF.md` in the repo root is the durable memory of the project:
//! architecture decisions, what is implemented and verified, what is next.
//! Every session appends a dated entry so the next session can continue
//! without losing context.

use std::io::Write;

pub fn refresh(seed: u64) {
    let path = repo_root().join("HANDOFF.md");
    let stamp = chrono_stamp();
    let mut f = match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return,
    };
    let _ = writeln!(f, "\n---\n\n## Session end {stamp}\n\n- Server exited cleanly (seed {seed}).\n- See HANDOFF.md top section for current state.\n");
}

fn chrono_stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            let secs = d.as_secs();
            format!("{secs}")
        })
        .unwrap_or_default()
}

fn repo_root() -> std::path::PathBuf {
    // server/crates/hnh-server/src -> up 4 levels.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}
