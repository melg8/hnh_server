//! Area chat relay (docs/mechanics/network/communication.md).
//!
//! Pure helpers only: text sanitization, the radius filter, and the
//! system-line color. The wire delivery lives in `game.rs` where the
//! session table and player positions are at hand.

use crate::state::VIEW_RADIUS;

/// Area chat range. Server policy: you hear what you can see — the relay
/// shares the view radius (state.rs, ~45 tiles at 500 subtiles).
pub const AREA_CHAT_RADIUS: i32 = VIEW_RADIUS;

/// Hard cap on a relayed line. The client `Textlog` renders long lines on
/// one row; anything longer is refused rather than wrapped server-side.
pub const MAX_LINE_CHARS: usize = 240;

/// System notification color (soft red) for server-to-one-player lines
/// (skill gate refusals, party notices, invite prompts).
pub const SYSTEM_COLOR: (u8, u8, u8) = (255, 128, 128);

/// Validate a chat line typed by a player.
///
/// Returns the trimmed line, or `None` when the line is empty after
/// trimming or exceeds [`MAX_LINE_CHARS`] characters (chars, not bytes, so
/// the limit is locale-independent).
pub fn sanitize(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_LINE_CHARS {
        return None;
    }
    Some(trimmed)
}

/// Euclidean distance test in map subtiles. Coordinates widen to i128
/// before the deltas and products (an i32 delta overflows on extreme
/// inputs and even an i64 square overflows at i32 extremes); the
/// comparison stays squared to avoid `hypot`. Chat relays are rare
/// events, so the wider arithmetic costs nothing measurable.
pub fn within_radius(a: (i32, i32), b: (i32, i32), radius: i32) -> bool {
    let dx = a.0 as i128 - b.0 as i128;
    let dy = a.1 as i128 - b.1 as i128;
    let r = radius as i128;
    dx * dx + dy * dy <= r * r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_accepts_trimmed_nonempty_lines() {
        assert_eq!(sanitize("  hello world \n"), Some("hello world"));
        assert_eq!(sanitize("x"), Some("x"));
    }

    #[test]
    fn sanitize_rejects_empty_and_oversized() {
        assert_eq!(sanitize("   "), None);
        assert_eq!(sanitize(""), None);
        let long = "a".repeat(MAX_LINE_CHARS + 1);
        assert_eq!(sanitize(&long), None);
        // Exactly at the cap passes; chars, not bytes (the 240 'ä' are 480
        // bytes but 240 chars).
        let at_cap = "ä".repeat(MAX_LINE_CHARS);
        assert_eq!(sanitize(&at_cap), Some(at_cap.as_str()));
    }

    #[test]
    fn radius_includes_edge_and_excludes_beyond() {
        // Same point is in range.
        assert!(within_radius((0, 0), (0, 0), AREA_CHAT_RADIUS));
        // Exactly on the circle (3-4-5 triangle at radius 50).
        assert!(within_radius((0, 0), (30, 40), 50));
        // One subtile beyond the circle is out.
        assert!(!within_radius((0, 0), (31, 40), 50));
        // Large coordinates must not overflow the squared comparison.
        assert!(!within_radius(
            (2_000_000_000, 0),
            (-2_000_000_000, 0),
            AREA_CHAT_RADIUS
        ));
    }

    #[test]
    fn area_radius_matches_the_view_radius() {
        // The documented policy: hear what you can see.
        assert_eq!(AREA_CHAT_RADIUS, VIEW_RADIUS);
    }
}
