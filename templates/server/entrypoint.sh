#!/bin/sh
# Renders pumpkin.toml from the base config and the environment, applies the
# server repo's hallow.toml if the image has one, then starts Pumpkin. Runs on
# every start, so a changed environment takes effect.
set -eu

: "${HALLOW_VINE_SECRET:?HALLOW_VINE_SECRET must be set}"
export HALLOW_SERVER_NAME="${HALLOW_SERVER_NAME:-hallow}"

envsubst '${HALLOW_VINE_SECRET} ${HALLOW_SERVER_NAME}' \
  < /etc/hallow/pumpkin.toml > /server/pumpkin.toml

# A server repo's image adds /server/hallow.toml. hallow-manifest checks it
# against this base image's version, merges its [pumpkin] table, players.max,
# world and plugin permissions into pumpkin.toml, copies its plugins into
# plugins/, and prints the environment it implies for Hallow's server plugin
# (HALLOW_SERVER_KIND, HALLOW_SERVER_ROLE, HALLOW_MAX_PLAYERS,
# HALLOW_RETURN_LOBBY) plus its [env] table. Variables the container already
# sets win. Without a hallow.toml the base image runs as a plain server.
manifest=/server/hallow.toml
if [ -f "$manifest" ]; then
  hallow-manifest apply "$manifest" --pumpkin /server/pumpkin.toml \
    --plugins /server/plugins --base-version "$(cat /etc/hallow/base-version)" \
    > /tmp/hallow-env.sh
  . /tmp/hallow-env.sh
fi

# The multiversion plugin lets Java clients older than 26.3 join. It's off by
# default: with it loaded, Pumpkin fires a blocking plugin event for every
# outgoing packet from the tick, and the server deadlocks at about 50 moving
# players (see docs/VALIDATION.md). Set HALLOW_MULTIVERSION=1 to load it.
multiversion=/server/plugins/pumpkin_java_multiversion.wasm
if [ "${HALLOW_MULTIVERSION:-0}" = "1" ]; then
  cp /opt/hallow/plugins/pumpkin_java_multiversion.wasm "$multiversion"
else
  rm -f "$multiversion"
fi

# Hallow's server plugin links the server to the control plane. It's always
# loaded and does nothing unless HALLOW_CONTROL_PLANE (or its config.toml) is
# set; see crates/server-plugin. Copied on every start so a newer image (or a
# dev build mounted over /opt/hallow/plugins/hallow_server.wasm) takes effect.
cp /opt/hallow/plugins/hallow_server.wasm /server/plugins/hallow_server.wasm

exec pumpkin "$@"
