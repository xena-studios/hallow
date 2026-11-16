#!/bin/sh
# Builds every WASM plugin the dev network runs, and copies the in-tree
# servers' own plugins into their plugins/ folders, where their Dockerfiles
# and hallow.toml expect them. A server repo of its own does the same in its
# CI's `prepare` step. Run from anywhere:
#   servers/build-plugins.sh
set -eu
cd "$(dirname "$0")/.."
cargo build --release --locked --target wasm32-wasip2 \
  -p hallow-vine -p hallow-server -p hallow-hub -p hallow-test-game
out=target/wasm32-wasip2/release
cp "$out/hallow_hub.wasm" servers/hub/plugins/
cp "$out/hallow_test_game.wasm" servers/test-game/plugins/
echo "Built the plugins into $out, servers/hub/plugins and servers/test-game/plugins."
