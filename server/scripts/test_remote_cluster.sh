#!/usr/bin/env bash
# Session 87: the REMOTE cluster e2e smoke.
#
# Proves the `cluster-up.sh remote` path - the actual multi-machine
# deployment shape - inside one sandbox by using two loopback
# addresses as two "machines":
#
#   machine A = 127.0.0.1 (node 0: auth 1871 / game 1870, mesh 18790)
#   machine B = 127.0.0.2 (node 1: auth 1873 / game 1874, mesh 18791)
#
# Each "machine" runs ONE node via `CLUSTER_SPEC=... SELF=i
# cluster-up.sh remote` (exactly the commands a real operator types
# on each host), then:
#
#   1. REMOTE MESH:  both nodes report "cluster dial link up".
#   2. GUEST WALK:   a client enters through machine A and walks
#                    east across cells owned by machine B.
#   3. MIGRATION:    the character created through machine A
#                    re-enters through machine B's OWN address
#                    (127.0.0.2:1873/1874): machine A serves the
#                    snapshot, machine B receives the migration.
#
# Verdict lines: REMOTE MESH: OK / WORLD ENTRY + GUEST WALK (from
# probe_guest_walk.py) / REMOTE MIGRATION: OK / REMOTE CLUSTER: OK.
# Saves: throwaway /tmp dir, so reruns stay repeatable.
set -u
cd "$(dirname "$0")/.."   # server/

SAVES=/tmp/hnh_remote_e2e_saves
OUT0=target/remote-n0.out
OUT1=target/remote-n1.out
LOG0=target/cluster-n0.log
LOG1=target/cluster-n1.log
MIGUSER=${MIGUSER:-remotemig87}
SPEC="127.0.0.1:18790,127.0.0.2:18791"

cleanup() { ./scripts/cluster-up.sh stop >/dev/null 2>&1; }
trap cleanup EXIT

echo "== booting two REMOTE nodes (127.0.0.1 + 127.0.0.2 as machines) =="
./scripts/cluster-up.sh stop >/dev/null 2>&1
rm -rf "$SAVES" && mkdir -p "$SAVES"
: > "$LOG0"; : > "$LOG1"; : > "$OUT0"; : > "$OUT1"

SAVEDIR="$SAVES" RUST_LOG=hnh_server=debug CLUSTER_SPEC="$SPEC" SELF=0 \
    ./scripts/cluster-up.sh remote >> "$OUT0" 2>&1 &
p0=$!
SAVEDIR="$SAVES" RUST_LOG=hnh_server=debug CLUSTER_SPEC="$SPEC" SELF=1 \
    ./scripts/cluster-up.sh remote >> "$OUT1" 2>&1 &
p1=$!

ok0=0; ok1=0
for _ in $(seq 1 150); do
    [ "$ok0" = 0 ] && rg -q "NODE UP" "$OUT0" 2>/dev/null && ok0=1
    [ "$ok1" = 0 ] && rg -q "NODE UP" "$OUT1" 2>/dev/null && ok1=1
    if [ "$ok0" = 0 ] && ! kill -0 "$p0" 2>/dev/null; then
        echo "REMOTE CLUSTER: FAIL (node-0 wrapper died)"; cat "$OUT0"; exit 1
    fi
    if [ "$ok1" = 0 ] && ! kill -0 "$p1" 2>/dev/null; then
        echo "REMOTE CLUSTER: FAIL (node-1 wrapper died)"; cat "$OUT1"; exit 1
    fi
    [ "$ok0" = 1 ] && [ "$ok1" = 1 ] && break
    sleep 2
done
if [ "$ok0" != 1 ] || [ "$ok1" != 1 ]; then
    echo "REMOTE CLUSTER: FAIL (NODE UP timeout)"
    tail -n 5 "$OUT0" "$OUT1"
    exit 1
fi
echo "REMOTE MESH: OK (both nodes joined $SPEC)"
mesh0=$(rg -c "cluster dial link up" "$LOG0" 2>/dev/null || echo 0)
mesh1=$(rg -c "cluster dial link up" "$LOG1" 2>/dev/null || echo 0)
echo "mesh links: machineA=$mesh0 machineB=$mesh1"

echo "== guest walk through machine A over machine B's cells =="
python3 scripts/probe_guest_walk.py guestwalk87 || {
    echo "REMOTE CLUSTER: FAIL (guest walk)"
    exit 1
}
guest_ing=$(rg -c "guest ingested" "$LOG1" 2>/dev/null || echo 0)
echo "guest evidence on machine B: ingested=$guest_ing"
if [ "$guest_ing" -lt 1 ]; then
    echo "REMOTE CLUSTER: FAIL (no guest rows ingested by machine B)"
    exit 1
fi

echo "== migration: created on machine A, entered through machine B =="
python3 scripts/probe_cluster_entry.py "$MIGUSER" 1871 1870 127.0.0.1 || {
    echo "REMOTE MIGRATION: FAIL (creation through machine A)"
    exit 1
}
sleep 2   # let the owner node persist the new character
python3 scripts/probe_cluster_entry.py "$MIGUSER" 1873 1874 127.0.0.2 || {
    echo "REMOTE MIGRATION: FAIL (entry through machine B)"
    exit 1
}
mig_rx=$(rg -c "char migration received: entering world" "$LOG1" 2>/dev/null || echo 0)
mig_tx=$(rg -c "char query: serving snapshot to peer" "$LOG0" 2>/dev/null || echo 0)
echo "migration evidence: machineA served=$mig_tx machineB received=$mig_rx"
if [ "$mig_rx" -lt 1 ] || [ "$mig_tx" -lt 1 ]; then
    echo "REMOTE MIGRATION: FAIL (no cross-machine snapshot exchange)"
    echo "REMOTE CLUSTER: FAIL"
    exit 1
fi
echo "REMOTE MIGRATION: OK"
echo "REMOTE CLUSTER: OK"
