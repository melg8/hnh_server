# Gates: leaf-3 server-side follow-ups exposed by the probe

OWNS: server/crates/hnh-server/src/game.rs

Scope: conditional. If the leaf-2 probe exposes a server defect, this
leaf owns its fix. If the probe is green unchanged, this leaf ships a
single-line observability log for the charlist add so future user bug
reports carry layer names and versions in logs/server.log.

- [ ] G1: charlist add observability present in the login path
  CHECK: bash server/scripts/verify_charlist_log.sh
  CWD: ../../..
  EXPECT: CHARLIST LOG GATE: OK
