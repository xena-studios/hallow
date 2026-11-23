#!/bin/sh
# Builds every WASM plugin the dev network runs, and copies the in-tree
# servers' own plugins into their plugins/ folders, where their Dockerfiles
# and hallow.toml expect them. A server repo of its own does the same in its
# CI's `prepare` step. Run from anywhere:
#   servers/build-plugins.sh
set -eu
cd "$(dirname "$0")/.."
cargo build --release --locked --target wasm32-wasip2 \
  -p hallow-vine -p hallow-server -p hallow-hub -p hallow-test-game \
  -p hallow-eggwars -p hallow-skywars -p hallow-game-lobby
out=target/wasm32-wasip2/release
cp "$out/hallow_hub.wasm" servers/hub/plugins/
cp "$out/hallow_test_game.wasm" servers/test-game/plugins/
cp "$out/hallow_eggwars.wasm" servers/eggwars/plugins/
cp "$out/hallow_skywars.wasm" servers/skywars/plugins/
cp "$out/hallow_game_lobby.wasm" servers/eggwars-lobby/plugins/
cp "$out/hallow_game_lobby.wasm" servers/skywars-lobby/plugins/
echo "Built the plugins into $out and the servers' plugins/ folders."
