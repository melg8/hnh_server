#!/usr/bin/env bash
# One-command launcher for local development.
set -euo pipefail
cd "$(dirname "$0")"
if [ ! -d gameres ]; then
    echo "Generating gameres/ from lib/haven-res.jar + res/compiled ..."
    mkdir -p gameres
    unzip -o -q ../lib/haven-res.jar 'res/*' -d /tmp/hnh-res-extract
    cp -rn /tmp/hnh-res-extract/res/* gameres/
    cp -r ../res/compiled/* gameres/
fi
SEED="${1:-42}"
cargo run --release -- --seed "$SEED"
