//! Session-local resource id tables. RESIDs are session-local by protocol
//! (session-lifecycle.md, reconnection semantics); each session announces
//! its own id -> (name, version) mappings before first use.

use std::collections::HashMap;

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
    pub fn pending_announce(&self, wire_idx: u16) -> Option<(&'static str, u16)> {
        if self.announced.contains(&wire_idx) {
            return None;
        }
        self.wire.get(wire_idx as usize)?;
        let name = self.wire_names.get(&wire_idx)?;
        Some((name, 1))
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

    /// RMSG_PAGINAE add entries.
    pub fn paginae_add(names: &[&str]) -> Vec<u8> {
        let mut m = MessageBuf::new();
        m.uint8(RMSG_PAGINAE);
        for n in names {
            m.uint8(b'+').string(n).uint16(1);
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
