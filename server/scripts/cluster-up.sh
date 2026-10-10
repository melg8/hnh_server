#!/usr/bin/env bash
# Session 86: the multi-machine cluster profile, one command.
#
#   cluster-up.sh up      - boot a local N-node cluster (default 2) with
#                           the SAME port profile as windows/start-cluster.bat:
#                           node i = auth 1871+2i, game 1870+4i, res 1872+4i,
#                           mesh 18790+i. Node 0 keeps the client-facing
#                           default ports, so run-client.bat / test_client.py
#                           work unchanged; later nodes are reachable through
#                           their own auth port (see CLUSTER ENTRY below).
#   cluster-up.sh remote  - boot ONLY this machine's node from $CLUSTER_SPEC
#                           ("hostA:mesh,hostB:mesh,...") and $SELF (node
#                           index). Run one copy per machine: every node
#                           binds its own mesh address from the spec, dials
#                           the peers, and the grid ownership splits by the
#                           stable hash - the multi-machine profile proper.
#   cluster-up.sh stop    - kill the cluster (local mode) or this node.
#   cluster-up.sh status  - pids, ports, mesh-link state.
#
# Client ports in remote mode: $AUTH_PORT/$GAME_PORT/$RES_PORT (defaults
# follow the node index formula above). Saves: repo save/cluster_n<i>.json
# (one shard per node, stable across restarts). Logs append to
# target/cluster-n<i>.log, RUST_LOG controlled by $RUST_LOG (default info).
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

BIN=target/release/hnh-server
CMD=${1:-up}
LOGLEVEL=${RUST_LOG:-hnh_server=info}
DIR=target
PIDF="$DIR/cluster.pid"
# One save shard per node (same layout main.rs defaults to). Keep it
# explicit so local and remote modes stay byte-identical.
SAVEDIR=${SAVEDIR:-../save}

node_ports() {  # $1 = node index -> prints "auth game res mesh"
    local i=$1
    echo $((1871 + 2 * i)) $((1870 + 4 * i)) $((1872 + 4 * i)) $((18790 + i))
}

die() { echo "cluster-up: $*" >&2; exit 1; }

cmd_stop() {
    if [ -f "$PIDF" ]; then
        while read -r p; do
            [ -n "$p" ] && kill "$p" 2>/dev/null && echo "stopped pid $p"
        done < "$PIDF"
        sleep 1
        while read -r p; do
            [ -n "$p" ] && kill -9 "$p" 2>/dev/null
        done < "$PIDF"
        rm -f "$PIDF"
    fi
    # Remote mode leaves a per-node pid file too.
    for f in "$DIR"/cluster-n*.pid; do
        [ -f "$f" ] || continue
        read -r p < "$f"
        [ -n "$p" ] && kill "$p" 2>/dev/null && echo "stopped pid $p ($(basename "$f"))"
        rm -f "$f"
    done
}

wait_node_boot() {  # $1 log file, $2 seconds
    # "sessions=" only appears once clients connect; readiness is the
    # UDP game shard accepting - the node is fully up at that point.
    local log=$1 secs=$2
    for _ in $(seq 1 "$secs"); do
        rg -q "game server \(UDP\) shard listening" "$log" 2>/dev/null && return 0
        sleep 1
    done
    return 1
}

case "$CMD" in
up)
    N=${N:-2}
    [ "$N" -ge 2 ] && [ "$N" -le 4 ] || die "N must be 2..4"
    [ -x "$BIN" ] || die "release binary missing: cargo build --release"
    [ -d ../gameres ] || die "gameres missing: run scripts/make-gameres.sh"
    cmd_stop
    SPEC=""
    for ((i = 0; i < N; i++)); do
        read -r _ _ _ mesh < <(node_ports "$i")
        SPEC+="${SPEC:+,}127.0.0.1:$mesh"
    done
    : > "$PIDF"
    for ((i = 0; i < N; i++)); do
        read -r auth game res mesh < <(node_ports "$i")
        LOG="$DIR/cluster-n$i.log"
        ARGS="--seed 42 --cluster $SPEC --node $i"
        [ "$i" -gt 0 ] && ARGS+=" --auth-port $auth --game-port $game --res-port $res"
        HNH_SAVE_FILE="$SAVEDIR/cluster_n$i.json" \
            RUST_LOG=$LOGLEVEL "$BIN" $ARGS >> "$LOG" 2>&1 &
        echo $! >> "$PIDF"
        echo "node $i: pid $! auth $auth game $game res $res mesh $mesh save $SAVEDIR/cluster_n$i.json (log $LOG)"
    done
    echo "waiting for the mesh..."
    ok=0
    for _ in $(seq 1 120); do
        links=0
        for ((i = 0; i < N; i++)); do
            rg -q "cluster dial link up" "$DIR/cluster-n$i.log" 2>/dev/null && links=$((links + 1))
        done
        # 2 nodes: one link each. 3+: node 0 dials everyone, so count
        # its links generously - any node reporting a link means the
        # mesh formed.
        [ "$links" -gt 0 ] && { ok=1; break; }
        sleep 1
    done
    [ "$ok" = 1 ] || { echo "cluster-up: mesh did not form in 120 s"; cmd_stop; exit 1; }
    for ((i = 0; i < N; i++)); do
        wait_node_boot "$DIR/cluster-n$i.log" 60 \
            || die "node $i did not reach live sessions"
    done
    echo "CLUSTER UP: $N nodes, mesh formed, all nodes live."
    echo "CLUSTER ENTRY (node 0): default ports auth 1871 / game 1870."
    [ "$N" -gt 1 ] && echo "CLUSTER ENTRY (node 1): auth 1873 / game 1874."
    ;;
remote)
    [ -n "${CLUSTER_SPEC:-}" ] || die "remote mode needs CLUSTER_SPEC=host:mesh,host:mesh,..."
    SELF=${SELF:-0}
    [ -x "$BIN" ] || die "release binary missing: cargo build --release"
    [ -d ../gameres ] || die "gameres missing: run scripts/make-gameres.sh"
    read -r auth game res mesh < <(node_ports "$SELF")
    AUTH_PORT=${AUTH_PORT:-$auth}
    GAME_PORT=${GAME_PORT:-$game}
    RES_PORT=${RES_PORT:-$res}
    LOG="$DIR/cluster-n$SELF.log"
    HNH_SAVE_FILE="$SAVEDIR/cluster_n$SELF.json" \
        RUST_LOG=$LOGLEVEL "$BIN" --seed 42 --cluster "$CLUSTER_SPEC" --node "$SELF" \
        --auth-port "$AUTH_PORT" --game-port "$GAME_PORT" --res-port "$RES_PORT" \
        >> "$LOG" 2>&1 &
    echo $! > "$DIR/cluster-n$SELF.pid"
    echo "node $SELF: pid $! auth $AUTH_PORT game $GAME_PORT res $RES_PORT"
    echo "waiting for the mesh..."
    ok=0
    for _ in $(seq 1 120); do
        rg -q "cluster dial link up" "$LOG" 2>/dev/null && { ok=1; break; }
        sleep 1
    done
    [ "$ok" = 1 ] || { echo "cluster-up: no mesh link in 120 s (peers up? firewall?)"; exit 1; }
    wait_node_boot "$LOG" 60 || die "node $SELF did not reach live sessions"
    echo "NODE UP: node $SELF joined the cluster at $CLUSTER_SPEC."
    ;;
stop) cmd_stop ;;
status)
    for ((i = 0; i < 4; i++)); do
        LOG="$DIR/cluster-n$i.log"
        [ -f "$LOG" ] || continue
        f=$(tail -n 40 "$LOG" | rg -c "cluster dial link up" 2>/dev/null || echo 0)
        s=$(rg -o "sessions=[0-9]+" "$LOG" 2>/dev/null | tail -1)
        echo "node $i: log=$LOG meshlinks_last40=$f ${s:-no-sessions-yet}"
    done
    ;;
*) die "usage: cluster-up.sh up|remote|stop|status" ;;
esac
