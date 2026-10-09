//! Avatar and kritter pose layer tables + the movement-octant math.
//!
//! The tables materialize ONCE as leaked 'static strings (data-oriented:
//! flat fixed-size arrays indexed by pose/dir/species) so the stream
//! paths compose layer names with zero per-tick allocations.
//!
//! Pure move out of game.rs (session 70 split). `move_dir` / `art_dir`
//! / `PoseTable` are re-exported from `game` so the `game::move_dir`
//! doc references in state.rs and nodes.rs keep resolving.

use super::*;

/// Avatar part resource templates. `{pose}` is `standing` or `walking`;
/// `{dir}` is the art pack's 8-direction sprite index (0..7), produced
/// from a movement octant by `art_dir` (NOT the raw octant: the art ring
/// is rotated one octant against the movement ring - see `art_dir`).
/// Every directional resource embeds its full animation client-side
/// (standing = 1 frame, walking = 8 frames @100 ms through the
/// resource's own `anim` layer), so the server selects ONE direction
/// set per pose and NEVER streams frames: cycling the direction sets in
/// sequence is what made the avatar spin around its own axis (session
/// 21 defect).
const AVATAR_PART_TEMPLATES: [&str; 6] = [
    "gfx/borka/body/{pose}/legs-{dir}",
    "gfx/borka/body/{pose}/torso/male-{dir}",
    "gfx/borka/body/{pose}/head-{dir}",
    "gfx/borka/body/{pose}/arm/idle/left-{dir}",
    "gfx/borka/body/{pose}/arm/idle/right-{dir}",
    "gfx/borka/hair-karin/{pose}/hair-{dir}",
];

/// Equipment-window doll pose: standing with banzai arms (spread),
/// sprite index 0 = the art pack's full front view (the +x+y octant,
/// straight at the camera). Drawn by `Equipory.cdraw` from the gob's
/// `Avatar` attribute (OD_AVATAR), which is distinct from the world
/// drawable (OD_LAYERS) - the doll keeps the spread-arms pose while the
/// world avatar walks.
const AVATAR_DOLL_TEMPLATES: [&str; 6] = [
    "gfx/borka/body/standing/legs-0",
    "gfx/borka/body/standing/torso/male-0",
    "gfx/borka/body/standing/head-0",
    "gfx/borka/body/standing/arm/banzai/left-0",
    "gfx/borka/body/standing/arm/banzai/right-0",
    "gfx/borka/hair-karin/standing/hair-0",
];

/// The avatar base resource every OD_LAYERS player block references (a
/// load gate client-side: it carries only the plalay router the fork
/// client drops, and is never sprite-created).
pub(crate) const AVATAR_BASE: &str = "gfx/borka/body";

/// Quantize a movement vector into the movement octant (0..7).
/// Pure, deterministic, unit-tested: dir 0 = +x, dir 2 = +y, dir 4 = -x,
/// dir 6 = -y (counterclockwise atan2 octants). These are MOVEMENT
/// octants, not sprite indices: the directional art resources are
/// indexed by a ring rotated one octant against this one - convert with
/// `art_dir` before composing layer names (session 22 defect: feeding
/// the octant straight into the sprite index shifted every walk
/// animation one octant clockwise on screen).
pub fn move_dir((sx, sy): (i32, i32), (tx, ty): (i32, i32)) -> u8 {
    let (dx, dy) = (tx - sx, ty - sy);
    if dx == 0 && dy == 0 {
        return 0;
    }
    let deg = (dy as f64).atan2(dx as f64).to_degrees();
    // Euclidean division keeps the wraparound exact at both ends of the
    // -180..180 range: -180 deg lands on dir 4, +180 deg on dir 4 too.
    let octant = ((deg + 22.5).div_euclid(45.0)) as i32;
    octant.rem_euclid(8) as u8
}

/// Map a movement octant (`move_dir`) to the art pack's directional
/// sprite index. The art ring is rotated one octant against the movement
/// ring: sprite 0 is the full FRONT view (the +x+y camera-facing octant),
/// sprite 4 the full back, the pure left/right profiles sit at sprites
/// 2/6, and the walking-into-frame 3/4 views fill the odd slots.
/// Verified by decoding the fox standing sprites (art 0 = head-on front,
/// art 1 = down-left 3/4, art 2 = pure left profile, art 3 = up-left
/// 3/4, art 4 = back, art 5 = up-right 3/4, art 6 = pure right profile,
/// art 7 = down-right 3/4; scripts/dump_directions.py) and by the user
/// report this fixes: walking up (octant 5) showed the up-right set
/// (sprite 5 = octant 6), walking left (octant 3) showed the up-left
/// set (sprite 3 = octant 4) - i.e. sprite N always depicts octant N+1,
/// so displaying octant D needs sprite (D - 1) mod 8.
#[inline]
pub fn art_dir(octant: u8) -> u8 {
    (octant.wrapping_add(7)) & 7
}

/// All concrete pose layer names, materialized ONCE as leaked 'static
/// strings (bounded set: 2 poses x 8 dirs x 6 avatar parts, 1 doll set,
/// 7 species x 2 poses x 8 dirs) so ResTable interns by reference and
/// every stream path composes layers with zero allocations (data-
/// oriented: flat fixed-size tables indexed by pose/dir/species).
pub(crate) struct PoseTable {
    /// [pose][dir][part]: pose 0 = standing, 1 = walking.
    avatar: [[[&'static str; 6]; 8]; 2],
    /// The equipment-window doll set (banzai arms, camera facing).
    doll: [&'static str; 6],
    /// [species][pose][dir], one body part per kritter pose.
    kritter: [[[&'static str; 8]; 2]; 9],
    /// [species] pose-router base resources.
    kritter_base: [&'static str; 9],
}

static POSES: std::sync::OnceLock<PoseTable> = std::sync::OnceLock::new();

/// Species index order must mirror the enum declaration order (state.rs).
/// Session 46 appends the mufflon (the pack's directory spelling) and
/// the sheep - both ship body pose directories like the original seven.
const SPECIES_FOLDERS: [&str; 9] = [
    "deer", "fox", "wolf", "boar", "cow", "hare", "aurochs", "mufflon", "sheep",
];

impl PoseTable {
    fn build() -> PoseTable {
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        let mut avatar: [[[&'static str; 6]; 8]; 2] = Default::default();
        for (pi, pose) in ["standing", "walking"].into_iter().enumerate() {
            for d in 0u8..8 {
                // Table stays indexed by movement octant; the emitted
                // resource name carries the art sprite index.
                let dir_char = (b'0' + art_dir(d)) as char;
                for (ti, t) in AVATAR_PART_TEMPLATES.into_iter().enumerate() {
                    avatar[pi][d as usize][ti] = leak(
                        t.replace("{pose}", pose)
                            .replace("{dir}", &dir_char.to_string()),
                    );
                }
            }
        }
        let mut kritter: [[[&'static str; 8]; 2]; 9] = Default::default();
        for (si, sp) in SPECIES_FOLDERS.into_iter().enumerate() {
            for (pi, pose) in ["standing/standing", "walking/walking"]
                .into_iter()
                .enumerate()
            {
                for d in 0u8..8 {
                    let art = art_dir(d);
                    kritter[si][pi][d as usize] =
                        leak(format!("gfx/kritter/{sp}/body/{pose}-{art}"));
                }
            }
        }
        PoseTable {
            avatar,
            doll: AVATAR_DOLL_TEMPLATES,
            kritter,
            kritter_base: [
                "gfx/kritter/deer/body",
                "gfx/kritter/fox/body",
                "gfx/kritter/wolf/body",
                "gfx/kritter/boar/body",
                "gfx/kritter/cow/body",
                "gfx/kritter/hare/body",
                "gfx/kritter/aurochs/body",
                "gfx/kritter/mufflon/body",
                "gfx/kritter/sheep/body",
            ],
        }
    }
}

pub(crate) fn poses() -> &'static PoseTable {
    POSES.get_or_init(PoseTable::build)
}

/// Concrete avatar layer names for one pose + direction.
pub(crate) fn avatar_pose_layers(moving: bool, dir: u8) -> &'static [&'static str; 6] {
    &poses().avatar[usize::from(moving)][(dir & 7) as usize]
}

/// Equipment doll layer names (banzai pose, camera facing).
pub(crate) fn avatar_doll_layers() -> &'static [&'static str; 6] {
    &poses().doll
}

/// Kritter pose-router base per species (a load gate client-side, never
/// sprite-created: it carries only the plalay router layer).
pub(crate) fn kritter_base(sp: Species) -> &'static str {
    poses().kritter_base[sp as usize]
}

/// The one kritter body pose part for a species + pose + direction. Each
/// directional resource embeds its animation (standing = 1 frame, walking
/// = 8 frames @50 ms), so a single layer carries the whole pose and the
/// client animates it natively.
pub(crate) fn kritter_pose_layer(sp: Species, moving: bool, dir: u8) -> &'static str {
    poses().kritter[sp as usize][usize::from(moving)][(dir & 7) as usize]
}
