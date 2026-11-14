#!/bin/sh
# Renders pumpkin.toml from the base config and the environment, then starts
# Pumpkin. Runs on every start, so a changed environment takes effect.
set -eu

: "${HALLOW_VINE_SECRET:?HALLOW_VINE_SECRET must be set}"
export HALLOW_SERVER_NAME="${HALLOW_SERVER_NAME:-hallow}"

envsubst '${HALLOW_VINE_SECRET} ${HALLOW_SERVER_NAME}' \
  < /etc/hallow/pumpkin.toml > /server/pumpkin.toml

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

exec pumpkin "$@"
