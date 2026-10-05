# session23 gate evidence

## G1 server-builds-clean — PASS

cargo fmt --all (reformatted the two touched files); clippy
--workspace --all-targets -- -D warnings finished clean; cargo test
--workspace: 11 + 81 + 8 = 100 passed, 0 failed (94 prior + 6 new
grid_owner tests).

## G2 rendezvous-owner-unit-tested — PASS

cargo test grid_owner: 6 passed, 0 failed.
- owner_is_deterministic_and_in_range (N in {1,2,3,4,7,16} over a
  16x16 lattice, stability per cell)
- one_node_owns_everything
- scale_out_migrates_only_the_new_nodes_share (4->5 nodes over 1024
  cells: every mover lands on the joiner, migration share in the
  0.05-0.35 band; rendezvous guarantee pinned in code)
- lattice_spreads_across_nodes (no starved/omnivorous node; <= 2x fair
  share at N in {2,4,8})
- partition_covers_every_item_exactly_once (+ every entry belongs to
  its partition's owner)
- partition_preserves_input_order_within_a_partition (ranked items,
  strictly ascending ranks per partition)

## G3 tick-partitioned-in-code — PASS

rg "partition_by_owner" game.rs -> tick_animals (per-owner intent
partitions) + update_visibility phase A2 (scan indices grouped by the
session's VisIndex-cell owner, results reordered back into to_scan
order before the serial apply). Dead/gone ids drop out in the pure
intent filter; the serial apply phase is unchanged.

## G4 load-budget-holds — PASS

Release build, --seed 42 --bots 300 --perf --workers 4:

players=300 animals=33 tick_us=4373 mean_tick_us=4537 max_tick_us=15120
sessions=300 gobs=823 vis_cells=134-137
players=300 tick_us=4575 mean_tick_us=4787
players=300 tick_us=5283 mean_tick_us=4886
players=300 tick_us=3858 mean_tick_us=4455
players=300 tick_us=4823 mean_tick_us=4450
players=300 tick_us=4527 mean_tick_us=4633

Mean tick ~4.4-4.9 ms, budget 100 ms - no regression vs the session
21-22 baseline (~4.5-4.8 ms); the partition grouping (cell-owner
buckets) matches the count-chunking performance while carrying the
multi-node ownership contract.

## G5 real-client-regression — PASS

s23a (workers=4, partitioned fan-out): PORTRAIT LAYERS standing set
(hair-0, arm/idle/right-0, arm/idle/left-0, head-0, torso/male-0,
legs-0 = art sprite 0 front view); MOVEMENT MOVED 555,555 -> 575,575;
MOVEMENT2 MOVED; WALKDIR EAST/NORTH/SOUTH/UP/LEFT all ARRIVED attempt
0; EQUIP DOLL avagob=65536 ava-rend=OK; ANIMALS SCREENSHOT boar
mid-walk (gfx/kritter/boar/body/walking/walking-2) in viewport.

s23b (default serial path): same verdicts - PORTRAIT LAYERS standing
set; MOVEMENT/MOVEMENT2 MOVED; five WALKDIR legs ARRIVED attempt 0;
EQUIP DOLL ava-rend=OK; ANIMALS hunt fallback (no predator in viewport
at hunt time - placement-dependent, wire spawn coverage remains pinned
by session 21 G6 and the unit suite).

Runner improvement: the e2e run log now greps the full verdict set
(WALKDIR/EQUIP/PORTRAIT/ANIMALS) instead of only AGENT/MOVEMENT lines.

## Follow-up (same session): armor class end to end

- armor.rs: per-piece base def/abs for the 14 armor pieces the shipped
  pack contains; quality scaling sqrt(q/10) pinned to the documented
  anchors (Q10 tusk helm 1/7, Q160 4/28); tooltip line "Armor class: D/A"
  composed into the epry "set" sync ( Equipory.calcAC sums it); combat
  applications: hurt_player damage shrinks by abs
  (dmg*50/(50+abs)), animal bites chip the player's defence bar through
  defense_chip (chip*50/(50+def)).
- craft.rs: recipe "hcloak" (Hide cloak from 2 raw cow hides,
  paginae/craft/hcloak, dex softcap) - the armor economy entry point;
  resources verified against lib/haven-res.jar (invobjs/cloak-hide,
  hide-raw-cow, paginae/craft/hcloak).
- 105 unit tests green (5 new armor tests), fmt+clippy -D warnings
  clean; real-client e2e (s23c, workers=4) green: portrait, movement x2,
  five walkdir legs, equip doll ava-rend=OK.
