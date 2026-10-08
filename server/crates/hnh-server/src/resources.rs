//! Session-local resource id tables. RESIDs are session-local by protocol
//! (session-lifecycle.md, reconnection semantics); each session announces
//! its own id -> (name, version) mappings before first use.

use std::collections::HashMap;
use std::path::PathBuf;

/// Served resource directory (gameres/), set once at startup; the file
/// version reader needs it to inspect actual .res headers. Crate-visible
/// so tests can detect whether a pack was locatable (never required).
pub(crate) static RES_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static VER_CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, u16>>> =
    std::sync::OnceLock::new();

/// Point the version reader at the directory the res HTTP server serves.
pub fn init_res_dir(dir: PathBuf) {
    let _ = RES_DIR.set(dir);
    let _ = VER_CACHE.set(std::sync::Mutex::new(HashMap::new()));
}

/// Whether the served pack carries `<name>` (flat or nested layout).
/// Ground-drop rendering needs this probe: inventory item resources
/// carry no `neg` layer and cannot render as world gobs, so the drop
/// spawner checks for the item's gfx/terobjs/items/<base> world shape
/// before falling back to a generic visible shape.
pub fn served(name: &str) -> bool {
    let Some(dir) = RES_DIR.get() else {
        return false;
    };
    resolve_res_file(dir, name).exists()
}

/// Resolve a resource name to its file on disk, exactly like res_http
/// serves it: flat (`<dir>/<name>.res`) first, then the nested layout
/// (`<dir>/<name>/<base>.res`) that the shipped pack stores base
/// tilesets in (`gfx/tiles/water` -> `gfx/tiles/water/water.res`).
/// The reader and the HTTP source MUST agree or the client gets a
/// version announce for a different file than it downloads and dies
/// with "Wrong res version" (the real client resolves both layouts
/// too, so this mirrors its forking source).
pub fn resolve_res_file(dir: &std::path::Path, name: &str) -> PathBuf {
    let primary = dir.join(format!("{name}.res"));
    if primary.exists() {
        return primary;
    }
    if let Some(base) = name.rsplit('/').next() {
        if !base.is_empty() {
            let nested = dir.join(format!("{name}/{base}.res"));
            if nested.exists() {
                return nested;
            }
        }
    }
    primary
}

/// True resource version, parsed from the served `<name>.res` header
/// ("Haven Resource 1\n" + LE u16 version). The client rejects any
/// announce whose version differs from the file it loads
/// ("Wrong res version" LoadException), which used to leave every
/// hard-coded-version resource broken client-side: missing tilesets and
/// avatar layers render as a black screen with an invisible character.
/// Falls back to 1 when the file is unreadable.
pub fn file_version(name: &str) -> u16 {
    if let Some(cache) = VER_CACHE.get() {
        if let Ok(map) = cache.lock() {
            if let Some(&v) = map.get(name) {
                return v;
            }
        }
    }
    let ver = RES_DIR
        .get()
        .map(|dir| file_version_in(dir, name))
        .unwrap_or(1);
    if let Some(cache) = VER_CACHE.get() {
        if let Ok(mut map) = cache.lock() {
            map.insert(name.to_owned(), ver);
        }
    }
    ver
}

/// Pure (directory-parameterized) form of [`file_version`]; the global
/// wrapper exists so every announce site shares one version cache.
pub fn file_version_in(dir: &std::path::Path, name: &str) -> u16 {
    const SIG: &[u8] = b"Haven Resource 1";
    std::fs::read(resolve_res_file(dir, name))
        .ok()
        .filter(|bytes| bytes.len() >= SIG.len() + 2 && &bytes[..SIG.len()] == SIG)
        .map(|bytes| u16::from_le_bytes([bytes[SIG.len()], bytes[SIG.len() + 1]]))
        .unwrap_or(1)
}

pub struct ResTable {
    /// game-global name -> game-global index.
    by_name: HashMap<&'static str, u16>,
    names: Vec<&'static str>,
    /// session-local wire id -> game-global index.
    wire: Vec<u16>,
    /// game-global index -> session-local wire id.
    inv: HashMap<u16, u16>,
    /// session-local wire id -> resource name (populated at wire allocation;
    /// the global table lives on `World`, the session table must be able to
    /// announce names on its own).
    wire_names: HashMap<u16, &'static str>,
    /// wire ids whose RMSG_RESID was already queued this session.
    announced: std::collections::HashSet<u16>,
}

impl Default for ResTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ResTable {
    pub fn new() -> Self {
        ResTable {
            by_name: HashMap::new(),
            names: Vec::new(),
            wire: Vec::new(),
            inv: HashMap::new(),
            wire_names: HashMap::new(),
            announced: std::collections::HashSet::new(),
        }
    }

    /// Register a resource in the game-global table, returning its index.
    pub fn intern(&mut self, name: &'static str) -> u16 {
        if let Some(&i) = self.by_name.get(name) {
            return i;
        }
        let i = self.names.len() as u16;
        self.names.push(name);
        self.by_name.insert(name, i);
        i
    }

    pub fn name(&self, idx: u16) -> Option<&'static str> {
        self.names.get(idx as usize).copied()
    }

    /// Session-local wire id count (for bulk announcement sweeps).
    pub fn wire_count(&self) -> usize {
        self.wire.len()
    }

    /// Whether a wire id was allocated by THIS session (i.e. it indexes
    /// the session table, not the game-global placeholder space). The
    /// batch fan-out test asserts every streamed wire id passes this.
    #[cfg(test)]
    pub fn wire_is_local(&self, w: u16) -> bool {
        (w as usize) < self.wire.len()
    }

    /// Allocate (or fetch) the session-local wire id for a game-global
    /// index, remembering the resource name for RMSG_RESID announcements.
    pub fn wire_named(&mut self, global: u16, name: &'static str) -> u16 {
        if let Some(&w) = self.inv.get(&global) {
            return w;
        }
        let w = self.wire.len() as u16;
        self.wire.push(global);
        self.inv.insert(global, w);
        self.wire_names.insert(w, name);
        w
    }

    /// Mark a wire id as announced (call after queueing RMSG_RESID).
    pub fn mark_announced(&mut self, wire_idx: u16) {
        self.announced.insert(wire_idx);
    }

    /// Whether this session-local wire id still needs an RMSG_RESID push.
    /// The announced version is the real file version so the client's own
    /// version check accepts the resource it downloads or loads locally.
    pub fn pending_announce(&self, wire_idx: u16) -> Option<(&'static str, u16)> {
        if self.announced.contains(&wire_idx) {
            return None;
        }
        self.wire.get(wire_idx as usize)?;
        let name = self.wire_names.get(&wire_idx)?;
        Some((name, file_version(name)))
    }
}

/// Builder helpers for the widget protocol (RMSG_* payloads).
pub mod wdg {
    use hnh_proto::{consts::*, MessageBuf};

    /// RMSG_NEWWDG with typed args list.
    pub fn new_wdg(id: u16, ty: &str, x: i32, y: i32, parent: u16, args: &[ListVal]) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_NEWWDG)
            .uint16(id)
            .string(ty)
            .coord(x, y)
            .uint16(parent);
        push_args(&mut m, args);
        m.finish()
    }

    /// RMSG_NEWWDG for resource-defined widgets (type contains '/').
    #[allow(dead_code)] // wire surface for mechanics landing this session
    pub fn new_wdg_res(
        id: u16,
        resname: &str,
        x: i32,
        y: i32,
        parent: u16,
        args: &[ListVal],
    ) -> Vec<u8> {
        new_wdg(id, resname, x, y, parent, args)
    }

    /// RMSG_WDGMSG server -> client.
    pub fn wdgmsg(id: u16, name: &str, args: &[ListVal]) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_WDGMSG).uint16(id).string(name);
        push_args(&mut m, args);
        m.finish()
    }

    /// RMSG_DSTWDG.
    pub fn dst_wdg(id: u16) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_DSTWDG).uint16(id);
        m.finish()
    }

    /// RMSG_RESID announcement.
    pub fn resid(wire_idx: u16, name: &str, ver: u16) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_RESID)
            .uint16(wire_idx)
            .string(name)
            .uint16(ver);
        m.finish()
    }

    /// RMSG_TILES tileset mapping.
    pub fn tiles(id: u8, name: &str, ver: u16) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_TILES).uint8(id).string(name).uint16(ver);
        m.finish()
    }

    /// RMSG_GLOBLOB TIME+ASTRO(+LIGHT) blob.
    pub fn globlob(
        unix: i32,
        dt: i32,
        mp: i32,
        yt: i32,
        light: Option<(u8, u8, u8, u8)>,
    ) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_GLOBLOB);
        m.uint8(GMSG_TIME).int32(unix);
        m.uint8(GMSG_ASTRO).int32(dt).int32(mp).int32(yt);
        if let Some((r, g, b, a)) = light {
            m.uint8(GMSG_LIGHT).color(r, g, b, a);
        }
        m.finish()
    }

    /// RMSG_CATTR attributes.
    pub fn cattr(entries: &[(&str, i32, i32)]) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_CATTR);
        for (nm, base, comp) in entries {
            m.string(nm).int32(*base).int32(*comp);
        }
        m.finish()
    }

    /// RMSG_PAGINAE add entries. The per-pagina version must be the real
    /// file version - a mismatch makes the client's MenuGrid throw
    /// PaginaException, which kills the UI receive thread.
    pub fn paginae_add(names: &[&str]) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_PAGINAE);
        for n in names {
            m.uint8(b'+').string(n).uint16(super::file_version(n));
        }
        m.finish()
    }

    /// RMSG_BUFF set.
    #[allow(dead_code)] // wire surface for mechanics landing this session
    #[allow(clippy::too_many_arguments)] // mirrors the RMSG_BUFF wire layout
    pub fn buff_set(
        id: i32,
        resid: u16,
        tt: &str,
        ameter: i32,
        nmeter: i32,
        cmeter: i32,
        cticks: i32,
        major: u8,
    ) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_BUFF)
            .string("set")
            .int32(id)
            .uint16(resid)
            .string(tt)
            .int32(ameter)
            .int32(nmeter)
            .int32(cmeter)
            .int32(cticks)
            .uint8(major);
        m.finish()
    }

    /// RMSG_BUFF rm.
    #[allow(dead_code)] // wire surface for mechanics landing this session
    pub fn buff_rm(id: i32) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_BUFF).string("rm").int32(id);
        m.finish()
    }

    /// RMSG_SFX.
    #[allow(dead_code)] // wire surface for mechanics landing this session
    pub fn sfx(wire_idx: u16) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_SFX).uint16(wire_idx);
        m.finish()
    }

    /// RMSG_MAPIV mode 0: invalidate one grid.
    #[allow(dead_code)] // wire surface for mechanics landing this session
    pub fn mapiv_grid(gc: (i32, i32)) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_MAPIV).uint8(0).coord(gc.0, gc.1);
        m.finish()
    }

    /// RMSG_PARTY records (src/haven/Party.java tags).
    pub enum PartyRec<'a> {
        /// PD_LIST: the full member list, wire-terminated by int32 -1.
        List(&'a [i32]),
        /// PD_LEADER: gob id of the party leader.
        Leader(i32),
        /// PD_MEMBER: marker color + last known position (`None` streams
        /// the invisible flag and the client falls back to gob lookup).
        Member {
            gob: i32,
            pos: Option<(i32, i32)>,
            color: (u8, u8, u8),
        },
    }

    /// RMSG_PARTY state stream. An empty record list still emits a valid
    /// (empty) PD_LIST so a disband clears client state (Party.msg clears
    /// memb when the list has no ids).
    pub fn party(records: &[PartyRec]) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_PARTY);
        for r in records {
            match r {
                PartyRec::List(ids) => {
                    m.uint8(0);
                    for id in ids.iter() {
                        m.int32(*id);
                    }
                    m.int32(-1);
                }
                PartyRec::Leader(gob) => {
                    m.uint8(1).int32(*gob);
                }
                PartyRec::Member { gob, pos, color } => {
                    m.uint8(2)
                        .int32(*gob)
                        .uint8(if pos.is_some() { 1 } else { 0 });
                    if let Some((x, y)) = pos {
                        m.coord(*x, *y);
                    }
                    m.color(color.0, color.1, color.2, 255);
                }
            }
        }
        m.finish()
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum ListVal {
        I(i32),
        S(String),
        C(i32, i32),
        /// RGBA color list element (CharWnd.FoodMeter consumes these).
        Col(u8, u8, u8, u8),
    }

    fn push_args(m: &mut MessageBuf, args: &[ListVal]) {
        for a in args {
            match a {
                ListVal::I(v) => {
                    m.lint(*v);
                }
                ListVal::S(s) => {
                    m.lstr(s);
                }
                ListVal::C(x, y) => {
                    m.lcoord(*x, *y);
                }
                ListVal::Col(r, g, b, a) => {
                    m.lcolor(*r, *g, *b, *a);
                }
            }
        }
        m.lend();
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn resolve_prefers_flat_then_nested() {
        let dir = std::env::temp_dir().join(format!("hnh-res-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("gfx/tiles/water")).unwrap();
        // Flat candidate only.
        std::fs::write(dir.join("gfx/tiles/water.res"), b"flat").unwrap();
        assert_eq!(
            resolve_res_file(&dir, "gfx/tiles/water"),
            dir.join("gfx/tiles/water.res")
        );
        // Remove flat, nested candidate must win.
        std::fs::remove_file(dir.join("gfx/tiles/water.res")).unwrap();
        std::fs::write(dir.join("gfx/tiles/water/water.res"), b"nested").unwrap();
        assert_eq!(
            resolve_res_file(&dir, "gfx/tiles/water"),
            dir.join("gfx/tiles/water/water.res")
        );
        // Neither exists: the (missing) flat path is returned and the
        // caller's read fails, falling back to version 1.
        std::fs::remove_file(dir.join("gfx/tiles/water/water.res")).unwrap();
        assert_eq!(
            resolve_res_file(&dir, "gfx/tiles/water"),
            dir.join("gfx/tiles/water.res")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_version_reads_nested_tileset_headers() {
        let dir = std::env::temp_dir().join(format!("hnh-ver-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("gfx/tiles/water")).unwrap();
        let mut data = b"Haven Resource 1".to_vec();
        data.extend_from_slice(&6u16.to_le_bytes());
        std::fs::write(dir.join("gfx/tiles/water/water.res"), &data).unwrap();
        // Pure form: no global state involved, safe under parallel tests.
        assert_eq!(file_version_in(&dir, "gfx/tiles/water"), 6);
        // Flat layout reads identically.
        let mut flat = b"Haven Resource 1".to_vec();
        flat.extend_from_slice(&9u16.to_le_bytes());
        std::fs::create_dir_all(dir.join("gfx/hud")).unwrap();
        std::fs::write(dir.join("gfx/hud/vilind.res"), &flat).unwrap();
        assert_eq!(file_version_in(&dir, "gfx/hud/vilind"), 9);
        // Missing file falls back to 1 (the protocol floor).
        assert_eq!(file_version_in(&dir, "gfx/no/such"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
