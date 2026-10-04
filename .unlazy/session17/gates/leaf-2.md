# Gates: leaf-2 charlist portrait + real click path proof

OWNS: client-probe/UiProbe.java, server/scripts/verify_ui_probe.sh

Scope: the headless probe must prove the character-selection screen
data path end to end: at least one char in the list, every ava layer
resource resolves client-side, the composited layer inventory is
non-empty, and world entry works through the REAL Button.click() chain.

- [ ] G1: probe compiles together with the client tree
  CHECK: bash server/scripts/verify_ui_probe.sh compile
  CWD: ../../..
  EXPECT: UI PROBE COMPILE: OK
- [ ] G2: full charlist probe against a live server: chars>=1, all ava
  layers resolve, >0 image layers, real-click play -> mapview+slen
  CHECK: bash server/scripts/verify_ui_probe.sh charlist
  CWD: ../../..
  EXPECT: UI PROBE CHARLIST: OK
