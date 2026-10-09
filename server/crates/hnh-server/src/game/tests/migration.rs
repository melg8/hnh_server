//! Cross-node character migration (CharQuery/CharData/Nack).

use super::common::*;

#[tokio::test]
async fn char_query_migrates_the_offline_snapshot_to_the_peer() {
    let (mut holder, mut mesh_rx) = bare_clustered("migrate-holder", 0, 2);
    let key = crate::persist::save_key("acct", "Player");
    holder
        .save
        .players
        .insert(key.clone(), snapshot(&key, (500, 500)));
    holder.on_node_msg(crate::nodes::NodeMsg::CharQuery {
        from: 1,
        name: key.clone(),
    });
    // Two-phase: the holder KEEPS the snapshot until the ack, so a
    // lost reply can always be re-served by a query retry.
    assert!(
        holder.save.players.contains_key(&key),
        "the holder must keep the snapshot until CharAck"
    );
    let mut data = None;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        assert_eq!(peer, 1, "unicast to the requester");
        if let crate::nodes::NodeMsg::CharData {
            to,
            from,
            name,
            snap,
        } = msg
        {
            assert_eq!(to, 1, "routed to the requester node");
            assert_eq!(from, 0, "sent by the holder");
            assert_eq!(name, key);
            data = Some(snap);
        }
    }
    let snap = data.expect("CharData reply");
    assert_eq!(snap.pos, (500, 500));
    assert_eq!(snap.lp, 42);
    // Re-query before the ack: the snapshot is re-served (idempotent).
    holder.on_node_msg(crate::nodes::NodeMsg::CharQuery {
        from: 1,
        name: key.clone(),
    });
    let mut re_served = false;
    while let Ok((_, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::CharData { name, .. } = msg {
            assert_eq!(name, key);
            re_served = true;
        }
    }
    assert!(re_served, "retry must re-serve the snapshot");
    // The ack completes the migration: the copy leaves the holder.
    holder.on_node_msg(crate::nodes::NodeMsg::CharAck { name: key.clone() });
    assert!(
        !holder.save.players.contains_key(&key),
        "CharAck must drop the holder's copy"
    );
}

#[tokio::test]
async fn char_query_for_an_online_character_nacks_and_keeps_the_snapshot() {
    let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("migrate-online", 0, 2);
    let key = crate::persist::save_key("acct", &g.world.players[0].name);
    g.save.players.insert(key.clone(), snapshot(&key, (10, 10)));
    g.on_node_msg(crate::nodes::NodeMsg::CharQuery {
        from: 1,
        name: key.clone(),
    });
    let mut nacks = 0;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        assert_eq!(peer, 1);
        if let crate::nodes::NodeMsg::CharNack { to, from, name: n } = msg {
            assert_eq!(to, 1, "routed to the requester");
            assert_eq!(from, 0, "sent by the holder");
            assert_eq!(n, key);
            nacks += 1;
        }
    }
    assert_eq!(nacks, 1, "an online character is answered with a nack");
    assert!(
        g.save.players.contains_key(&key),
        "the live player's snapshot must not migrate"
    );
}

#[tokio::test]
async fn chardata_adopts_the_snapshot_and_enters_the_world() {
    let (mut g, mut mesh_rx) = bare_clustered("migrate-adopt", 1, 2);
    open_session_and_play(&mut g, 1, "acct", "Player");
    // The entry deferred: no player yet, one CharQuery on the mesh.
    assert!(!g.world.by_session.contains_key(&1), "entry must defer");
    let mut queried = None;
    while let Ok((_, msg)) = mesh_rx.try_recv() {
        if let crate::nodes::NodeMsg::CharQuery { from, name } = msg {
            assert_eq!(from, 1);
            queried = Some(name);
        }
    }
    let key = queried.expect("CharQuery broadcast");
    assert_eq!(key, crate::persist::save_key("acct", "Player"));
    // The holder answers; the requester adopts, acks back and enters.
    g.on_node_msg(crate::nodes::NodeMsg::CharData {
        to: 1,
        from: 0,
        name: key.clone(),
        snap: Box::new(snapshot(&key, (777, -777))),
    });
    let pidx = *g.world.by_session.get(&1).expect("entered via migration");
    let pgob = g.world.players[pidx].gob;
    let slot = g.world.gobs.get(pgob).expect("player slot");
    assert_eq!(g.world.gobs.pos[slot], (777, -777), "restored position");
    assert_eq!(g.world.players[pidx].lp, 42, "restored lp");
    assert_eq!(g.world.players[pidx].hp, 80, "restored hp");
    let mut acked = false;
    while let Ok((peer, msg)) = mesh_rx.try_recv() {
        assert_eq!(peer, 0, "ack unicast to the holder");
        if let crate::nodes::NodeMsg::CharAck { name } = msg {
            assert_eq!(name, key);
            acked = true;
        }
    }
    assert!(acked, "adoption must ack so the holder drops its copy");
}

#[tokio::test]
async fn char_nack_majority_enters_fresh_without_the_deadline() {
    let (mut g, _mesh_rx) = bare_clustered("migrate-nacks", 1, 3);
    open_session_and_play(&mut g, 1, "acct", "Player");
    assert!(!g.world.by_session.contains_key(&1));
    // One of two peers answered: still waiting.
    g.on_node_msg(crate::nodes::NodeMsg::CharNack {
        to: 1,
        from: 0,
        name: crate::persist::save_key("acct", "Player"),
    });
    assert!(
        !g.world.by_session.contains_key(&1),
        "entry waits for every peer"
    );
    g.on_node_msg(crate::nodes::NodeMsg::CharNack {
        to: 1,
        from: 2,
        name: crate::persist::save_key("acct", "Player"),
    });
    assert!(
        g.world.by_session.contains_key(&1),
        "the last nack completes the entry"
    );
}

#[tokio::test]
async fn accounts_hold_separate_characters_and_legacy_saves_are_adopted() {
    let (mut g, _mesh_rx) = bare_clustered("account-keys", 0, 1);
    // Legacy layout: one bare "Player" snapshot from an older server.
    g.save
        .players
        .insert("Player".to_owned(), snapshot("Player", (321, 123)));
    open_session_and_play(&mut g, 1, "alice", "Player");
    let pidx = *g.world.by_session.get(&1).expect("legacy adoption entered");
    let pgob = g.world.players[pidx].gob;
    let slot = g.world.gobs.get(pgob).expect("player slot");
    assert_eq!(
        g.world.gobs.pos[slot],
        (321, 123),
        "legacy snapshot restored"
    );
    // Adoption re-keyed the snapshot into the account namespace.
    assert!(
        g.save.players.contains_key("alice:Player"),
        "legacy snapshot re-keyed"
    );
    assert!(!g.save.players.contains_key("Player"), "bare key consumed");
    // A second account gets a FRESH character, not alice's.
    open_session_and_play(&mut g, 2, "bob", "Player");
    let pidx2 = *g.world.by_session.get(&2).expect("second account entered");
    assert_ne!(
        g.world.players[pidx].gob, g.world.players[pidx2].gob,
        "two live players"
    );
    assert_eq!(
        g.save.players.get("bob:Player").map(|s| s.pos),
        None,
        "bob starts with no snapshot"
    );
}

// ------------------------------------------------------------------
// Session 30: cursor pickup redirection + stack merging
// (items-and-quality.md: stacking policy is server policy)
// ------------------------------------------------------------------
