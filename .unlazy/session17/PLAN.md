# PLAN — session 17

Original request: the standing full-implementation prompt (see AGENTS.md /
HANDOFF.md), resent with two user-visible login-screen defects:

1. "При логине не видно лица персонажа" - the character portrait on the
   character-selection screen renders no face/avatar layers.
2. "После выбора персонажа и нажатии на вход в игру по нажатию кнопки
   ничего не происходит, клиент словно завис" - clicking the Play button
   appears to do nothing.

Session-16 audit findings that define this plan:

- The charlist wire encoding (typed list tags 0/1/2/3/6) matches the
  client decoder byte for byte; body/head/hair .res exist in gameres;
  the server announces real file versions (session-16 fix). The
  server-side path is formally correct but NOT empirically proven:
  UiProbe waits for the Charlist widget yet never asserts
  Charlist.chars non-empty, never resolves the ava layers, and sends
  "play" via sess.queuemsg, bypassing the real Button.click() chain.
- windows/run-client.bat builds the client jar ONLY when
  build/haven.jar is absent. After `git pull` the user keeps running a
  stale jar with pre-fix client code (encodeurl, res-version, OD_LAYERS
  fixes all shipped in client sources) - by itself sufficient to make
  the reported symptoms appear unchanged no matter what the server
  sends. This is the highest-probability root cause.

Depth note: `tree 99` was requested; the honest decomposition is a
depth-2 tree (root -> node-1 -> three leaves). Stated per the skill
contract.

## Contract inventory (independently omittable outcomes)

| Id | Outcome | Tier | Owns | Needs |
| --- | --- | --- | --- | --- |
| leaf-1 | Stale-jar-proof client launch: run-client.bat rebuilds the jar whenever the checked-out HEAD moved (rev stamp in build/.clientrev) or the jar is missing; prints the rev it runs; start-server.bat similarly stamps its log. A pulled tree can no longer silently run old client code. | mechanical | windows/run-client.bat, windows/start-server.bat, windows/README.md | session 16 master (b72be4f) |
| leaf-2 | Charlist proof: UiProbe mode "charlist" asserts Charlist.chars >= 1, resolves all ava layer resources client-side within timeout, composites >0 image layers headlessly, then enters the world through the REAL Button.click() chain (not raw queuemsg) and asserts mapview+slen. verify_ui_probe.sh charlist gate runs it against a live server. | judgment | client-probe/UiProbe.java, server/scripts/verify_ui_probe.sh | session 16 master (b72be4f) |
| leaf-3 | Whatever the leaf-2 probe exposes as broken server-side is fixed; if (and only if) the probe is green unchanged, leaf-3 instead adds a one-line server observability log (charlist add: layer names+versions) so future user reports carry the evidence. | judgment | server/crates/hnh-server/src/game.rs | leaf-2 verified |
| node-1 | Integration: cargo test + fmt + clippy green, e2e battery incl. the new charlist gate in one server generation, HANDOFF.md session entry, committed and pushed to master. | judgment | HANDOFF.md | leaf-1..3 settled |

OWNS overlap note: leaves 1 and 2 are disjoint (windows/ vs
client-probe/+scripts); leaf-3 touches game.rs only after leaf-2 is
parent-verified; node-1 is the single writer for HANDOFF.md.

## Interfaces and shared assumptions

- The client source tree in src/haven is authoritative for widget
  semantics (Charlist.uimsg "add" = name + ava layer resids; Button
  click chain: Button.click -> wdgmsg("activate") -> Charlist.wdgmsg ->
  "play").
- The probe stays headless: GL-dependent texel output cannot be
  asserted; layer resolution + image-layer inventory is the deepest
  honest headless proof of the portrait data path.
- The rev-stamp approach must not require new tools: cmd + git + ant
  only, same as the existing bats.

## Waves

Wave 1: leaf-1 + leaf-2 in parallel (disjoint ownership).
Wave 2: leaf-3 after leaf-2 evidence.
Wave 3: node-1 integration + push.
