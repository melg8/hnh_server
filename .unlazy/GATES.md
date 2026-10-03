# GATES — Session 2: persistence, scale-out, mechanics

## Gates

### G1: One-command start with auto-generated TLS cert
Server starts with no `server/certs/authsrv.key.pem` present: the key and
certificate are generated at boot, auth listens on 1871, integration client
prints `WORLD ENTRY: OK`.
CHECK: bash server/scripts/verify_session2.sh cert-and-entry
EXPECT: ALL PASS

### G2: Zero-warning build
`cargo clippy --all-targets -- -D warnings` exits zero.
CHECK: cargo clippy --all-targets -- -D warnings
EXPECT: exit 0

### G3: Persistence roundtrip
Player character state (position, inventory, LP, attributes) survives a
server restart: save -> kill -> start -> login sees the saved state.
CHECK: bash server/scripts/verify_session2.sh persistence
EXPECT: ALL PASS

### G4: Multi-shard UDP socket scaling
`--shards N` opens N UDP sockets with SO_REUSEPORT; sessions distribute
across shards (log shows shard histogram); integration client still passes.
CHECK: bash server/scripts/verify_session2.sh shards
EXPECT: ALL PASS

### G5: Load test 1000 bots saturated world
`--bots 1000 --saturated --perf` runs: all bots enter, walk and fight;
steady-state tick_us < 100000.
CHECK: bash server/scripts/verify_session2.sh load
EXPECT: ALL PASS

### G6: Java client builds and connects
`ant jar` succeeds; jar contains local-dev defaults.
CHECK: test -f build/haven.jar
EXPECT: exit 0

### G7: Committed and pushed to master
All session work committed; `git status` clean; HEAD pushed to origin/master.
CHECK: bash server/scripts/verify_session2.sh committed
EXPECT: ALL PASS
