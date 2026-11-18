#!/bin/sh
# Never dropped: netcheck bots on the two-proxy dev network with dynamic
# instances check that the network moves players instead of disconnecting
# them (docs/ARCHITECTURE.md). Any disconnect fails the bot. Run from the
# repo root once the network is up:
#
#   export HALLOW_IMAGE_TAG=nd HALLOW_DOCKER_NETWORK=nd
#   files="-f deploy/compose/compose.yaml -f deploy/compose/compose.hallow.yaml -f deploy/compose/compose.instances.yaml"
#   TEST_GAME_MATCH_SECS=600 docker compose -p nd $files up --build -d --wait
#   deploy/compose/neverdrop.sh nd
#
# TEST_GAME_MATCH_SECS keeps the test game running long enough to be killed
# mid-match.
#
# The first argument is the compose project. NETCHECK (default
# target/debug/netcheck), PROXY (127.0.0.1:25566) and CONTROL_PLANE
# (127.0.0.1:18090) can be set, and EXTRA_FILES ("-f my-ports.yaml") if the
# network was started with more compose files; ONLY=banned (or crash, hubs,
# control-plane) runs one scenario.
set -eu
project=${1:?usage: neverdrop.sh <compose project>}
cd "$(dirname "$0")/../.."
files="-f deploy/compose/compose.yaml -f deploy/compose/compose.hallow.yaml -f deploy/compose/compose.instances.yaml ${EXTRA_FILES:-}"
compose="docker compose -p $project $files"
network=${HALLOW_DOCKER_NETWORK:-hallow-dev}
netcheck=${NETCHECK:-target/debug/netcheck}
cp=${CONTROL_PLANE:-127.0.0.1:18090}
token=deploy/compose/config/hallow/control-plane.token
only=${ONLY:-}

bot() {
  name=$1
  shift
  "$netcheck" --proxy "${PROXY:-127.0.0.1:25566}" --name "$name" \
    --control-plane "$cp" --token-file "$token" "$@"
}

wanted() { [ -z "$only" ] || [ "$only" = "$1" ]; }

if wanted banned; then
  echo "== A banned player lands in the banned lobby, can't leave, and goes to the hub when unbanned"
  bot banbot --route "@lobby,!ban 1h Testing the banned lobby,@banned,?Testing the banned lobby,?ends in,/hub,?banned,/server lobby,?banned,~2,@banned,^,@banned,?Testing the banned lobby,!unban,@lobby"
fi

if wanted crash; then
  echo "== Killing a game instance with a player in it moves them to the game's lobby"
  # Expanded when the step runs, once the game instance exists.
  kill_game='docker kill $(docker ps -q --filter label=hallow.owner='"$network"' --filter label=hallow.kind=test-game)'
  bot crashbot --route "@lobby,/play test-game,@test-game-??????,~2,\$$kill_game,@test-game-lobby,?went offline"
fi

if wanted hubs; then
  echo "== With every hub stopped, players wait in limbo and move on when one is back"
  bot limbobot --route "@lobby,\$$compose stop lobby,@limbo,?No hub,^,@limbo,\$$compose start lobby,@lobby"
fi

if wanted control-plane; then
  echo "== With the control plane down, new players wait in limbo and move on when it's back"
  bot cpbot --route "@lobby,\$$compose stop control-plane,^,?having trouble,\$$compose start control-plane,@lobby"
fi

echo "neverdrop: passed"
