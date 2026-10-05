# Gates: Session 26 — cursor item, ground drops, vis-scan cell check, accept throttle

OWNS: server/crates/hnh-server/src/game.rs, server/crates/hnh-server/src/visidx.rs, server/crates/hnh-server/src/net.rs, server/crates/hnh-server/src/state.rs, server/crates/hnh-server/src/resources.rs, scripts/jogl/agent/DriveAgent.java, scripts/jogl/run-real-client-e2e.sh, server/scripts/verify_session26.sh, HANDOFF.md

Scope: four handoff backlog items. (1) The held (cursor) stack becomes a
visible drag Item widget under the mouse. (2) MapView `drop` lands the
held stack on the ground as a gob — which surfaced a NEW defect: ground
drops rendered with the inventory icon resource (no `neg` layer) and
failed sprite init ("No negative found") — fixed with a two-resource
Drop kind (terobjs world shape + invobj pickup restore). (3) The
per-session vis skip-check stops iterating the whole dirty set and
probes the session's own view cells against the dirty HashSet. (4)
MSG_SESS accepts stop blocking the shard recv loop and are token-bucket
throttled against login storms.

- [x] G1: Zero-warning build, all unit tests green (no regressions).
      CHECK: bash -c "cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test"
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - fmt clean, clippy -D warnings clean, 127 tests green (11+108+8; 5 new: cursor wire proof, 2x map-drop, visidx cell probe, accept throttle bucket)

- [x] G2: Vis skip-check cost is O(view cells) not O(dirty cells):
      any_dirty_in_view enumerates the session's own cell range and
      probes the dirty set; a unit test pins worst-case behavior with a
      large dirty population far away (must stay cheap and correct).
      CHECK: cargo test -p hnh-server visidx::
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - skip_check_is_cell_probe_not_dirty_set_walk green (200 far movers keep dirty cells; a far-dirty world must not trip the skip; boundary tolerance still rescans); load check: 300 sessions mean tick ~6 ms, 1000 sessions connect 1000/1000 with ticks in budget

- [x] G3: Cursor wire proof: inv take creates a drag item widget (drag=1
      + drag Coord args), inv/epry drop and mapview consumption destroy
      it; the widget table never leaks the cursor wid.
      CHECK: cargo test -p hnh-server cursor
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - take_creates_and_drop_destroys_cursor_widget green: NEWWDG "item" with drag=1 + Coord arg on take, DSTWDG on drop, widget table clean; epry take/drop wired to the same sync; refresh_inventory skips the cursor wid so the sync helper's widget survives rebuilds

- [x] G4: Mapview drop: `wdgmsg("drop", modflags)` on mapview drops the
      held stack onto the ground near the player as a normal item gob
      (same despawn/pickup path as other drops); wire test pins the
      spawn and the cursor clear.
      CHECK: cargo test -p hnh-server map_drop
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - map_drop_spawns_ground_gob_and_clears_cursor (cursor cleared, Drop gob spawned, OBJDATA streamed) + map_drop_conserves_stack_contents (inv_res_idx == taken stack res; render res is a gfx/terobjs/items world shape). NEW DEFECT FOUND+FIXED: ground drops used the invobj icon resource (image+tooltip only, no neg layer) - the real client failed sprite init ("No negative found") so EVERY ground drop was invisible; Drop now carries inv_res_idx (pickup restores the exact icon) and renders via the terobjs/items twin (fallback: branch shape, still visible)

- [x] G5: Accept throttle: MSG_SESS handling no longer awaits the game
      task inline on the shard recv loop, and a per-shard token bucket
      bounds new-session rate (storm of N handshakes cannot queue
      unbounded accepts; unit/pinned test covers the bucket math).
      CHECK: cargo test -p hnh-server accept
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - accept_throttle_burst_then_refill green (burst 100, drain, 1 s refills exactly ACCEPT_RATE, cap not infinite credit); the shard loop's MSG_SESS arm is parse+validate+throttle+reply only, the game-task handshake runs in finish_accept off the recv-loop path with a pending-set that keeps duplicate handshakes idempotent; cookies are consumed only after the token check (a throttled handshake does not brick the login)

- [x] G6: Real GL client: taking an item from the inventory shows the
      held item under the mouse (screenshot read, not assumed); dropping
      it on the map spawns a visible ground gob; the item can be picked
      back up. Verdict lines CURSOR VERDICT and GROUNDDROP VERDICT.
      CHECK: bash server/scripts/verify_session26.sh
      EXPECT: exit 0
      CWD: .
      EVIDENCE: met - s26c/s26d/s26gate real-client runs: CURSOR DUMP: dragging=gfx/invobjs/branch; /tmp/client_cursor_item.png READ (branch sprite + tooltip "Branch, quality 10 / Resource: gfx/invobjs/branch" AT THE POINTER); GROUNDDROP DUMP: new-gob=gfx/terobjs/items/branch (the pre-fix run printed "No negative found" for the invobj gob - the fix landed); /tmp/client_grounddrop.png READ (the branch lies on the grass next to the player); PICKUP DUMP: gob-gone=true seed-in-inventory=true; CURSOR VERDICT: OK + GROUNDDROP VERDICT: OK; full regression green (PORTRAIT, MOVEMENT, NO TELEPORT, RAPID, 5x WALKDIR, EQUIP DOLL, ANIMALS, EQUIPVIS); verify_session26.sh e2e: E2E ALL PASS

- [x] G7: HANDOFF.md Session 26 entry written; worklog.md appended;
      commits pushed to origin/master.
      EVIDENCE: met - see HANDOFF.md Session 26; commits pushed (cursor/drop/vis/accept + gates)
