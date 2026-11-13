#!/bin/sh
# Renders pumpkin.toml from the base config and the environment, then starts
# Pumpkin. Runs on every start, so a changed environment takes effect.
set -eu

: "${HALLOW_VINE_SECRET:?HALLOW_VINE_SECRET must be set}"
export HALLOW_SERVER_NAME="${HALLOW_SERVER_NAME:-hallow}"

envsubst '${HALLOW_VINE_SECRET} ${HALLOW_SERVER_NAME}' \
  < /etc/hallow/pumpkin.toml > /server/pumpkin.toml

exec pumpkin "$@"
