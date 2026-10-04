//! Party bookkeeping (docs/mechanics/network/communication.md).
//!
//! Pure state and transitions. Wire broadcasts and session lookups live in
//! `game.rs`; this module is deliberately free of I/O so the membership
//! rules are unit-testable in isolation.

use crate::state::GobId;

/// Hard cap on party size. Legacy limited parties to ten members; the
/// number is not visible in this client, so it is server policy recorded
/// in the communication doc.
pub const MAX_MEMBERS: usize = 10;

/// Marker colors assigned per member index (`PD_MEMBER` color). The
/// client draws minimap markers and map lines in this color; the palette
/// is server policy (legacy colors are not recoverable from the client).
pub const MEMBER_COLORS: [(u8, u8, u8); MAX_MEMBERS] = [
    (255, 64, 64),
    (64, 128, 255),
    (64, 255, 64),
    (255, 255, 64),
    (255, 64, 255),
    (64, 255, 255),
    (255, 128, 0),
    (128, 64, 255),
    (255, 255, 255),
    (0, 0, 0),
];

/// Marker color for a member at `index` (wraps on overflow so a full
/// party never panics on an off-by-one).
pub fn color_for(index: usize) -> (u8, u8, u8) {
    MEMBER_COLORS[index % MEMBER_COLORS.len()]
}

/// Why an invite or join is refused (rendered as chat system lines).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartyError {
    Full,
    AlreadyMember,
}

/// What an open player flower menu arms (SessionOut.player_menu payload).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerMenu {
    /// The clicker's confirm menu on `target` ("Invite to party").
    InviteTarget(GobId),
    /// The invitee's consent menu ("Join <leader>'s party").
    JoinParty { leader: GobId },
}

/// Outcome of removing a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    /// The party dropped to one member and was disbanded.
    Disbanded,
    /// The leader left (or was removed); leadership moved to `new_leader`.
    LeaderChanged { new_leader: GobId },
    /// A non-leader left; the party continues unchanged.
    Removed,
}

/// One formed party. Membership is a small ordered vec (cap 10), which is
/// cache-friendly and keeps member index == palette index stable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyState {
    pub members: Vec<GobId>,
    pub leader: GobId,
}

impl PartyState {
    /// A fresh party with `leader` as its only member.
    pub fn new(leader: GobId) -> Self {
        PartyState {
            members: vec![leader],
            leader,
        }
    }

    pub fn contains(&self, gob: GobId) -> bool {
        self.members.contains(&gob)
    }

    /// Palette index of a member (stable as long as no one leaves).
    pub fn member_index(&self, gob: GobId) -> Option<usize> {
        self.members.iter().position(|m| *m == gob)
    }

    /// Add a member. Refuses a full party and duplicates.
    pub fn add(&mut self, gob: GobId) -> Result<(), PartyError> {
        if self.contains(gob) {
            return Err(PartyError::AlreadyMember);
        }
        if self.members.len() >= MAX_MEMBERS {
            return Err(PartyError::Full);
        }
        self.members.push(gob);
        Ok(())
    }

    /// Remove a member and settle leadership.
    pub fn remove(&mut self, gob: GobId) -> Removal {
        let Some(idx) = self.member_index(gob) else {
            // Removing a stranger is a no-op, not a state change; callers
            // treat it as Removed (the broadcast is idempotent anyway).
            return Removal::Removed;
        };
        self.members.remove(idx);
        if self.members.len() <= 1 {
            return Removal::Disbanded;
        }
        if self.leader == gob {
            // Leadership passes to the first remaining member; the vec is
            // ordered by join time, so this is the earliest joiner.
            let new_leader = self.members[0];
            self.leader = new_leader;
            return Removal::LeaderChanged { new_leader };
        }
        Removal::Removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_party_starts_with_the_leader_alone() {
        let p = PartyState::new(7);
        assert_eq!(p.members, vec![7]);
        assert_eq!(p.leader, 7);
        assert!(p.contains(7));
        assert!(!p.contains(8));
    }

    #[test]
    fn add_refuses_duplicates_and_full_parties() {
        let mut p = PartyState::new(1);
        assert!(p.member_index(1) == Some(0));
        assert_eq!(p.add(1), Err(PartyError::AlreadyMember));
        for g in 2..=MAX_MEMBERS as i32 {
            assert_eq!(p.add(g), Ok(()));
        }
        assert_eq!(p.add(11), Err(PartyError::Full));
        assert_eq!(p.members.len(), MAX_MEMBERS);
    }

    #[test]
    fn removing_a_nonleader_keeps_leadership() {
        let mut p = PartyState::new(1);
        p.add(2).unwrap();
        p.add(3).unwrap();
        assert_eq!(p.remove(3), Removal::Removed);
        assert_eq!(p.leader, 1);
        assert!(!p.contains(3));
    }

    #[test]
    fn removing_the_leader_transfers_to_the_earliest_joiner() {
        let mut p = PartyState::new(1);
        p.add(2).unwrap();
        p.add(3).unwrap();
        assert_eq!(p.remove(1), Removal::LeaderChanged { new_leader: 2 });
        assert_eq!(p.leader, 2);
        assert_eq!(p.members, vec![2, 3]);
    }

    #[test]
    fn dropping_to_one_member_disbands() {
        let mut p = PartyState::new(1);
        p.add(2).unwrap();
        assert_eq!(p.remove(2), Removal::Disbanded);
        assert_eq!(p.remove(1), Removal::Disbanded);
    }

    #[test]
    fn removing_a_stranger_is_a_noop() {
        let mut p = PartyState::new(1);
        p.add(2).unwrap();
        assert_eq!(p.remove(99), Removal::Removed);
        assert_eq!(p.members, vec![1, 2]);
    }

    #[test]
    fn colors_are_distinct_across_a_full_party() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..MAX_MEMBERS {
            assert!(seen.insert(color_for(i)), "palette color duplicated");
        }
        // Overflow wraps instead of panicking.
        let _ = color_for(MAX_MEMBERS + 3);
    }
}
