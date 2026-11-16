#!/usr/bin/env bash
# Hallow on a real Calagopus Wings, end to end, on one machine: the panel, the
# control plane with HALLOW_ORCHESTRATOR=wings, a node joined with the real
# install.sh and `wings configure --enroll`, Wings running instances from a
# local registry, and a bot routed to a Wings instance through Vine. See
# tools/wings-e2e/README.md.
#
#   tools/wings-e2e/run.sh            # up, test, down
#   tools/wings-e2e/run.sh up         # bring it up and leave it running
#   tools/wings-e2e/run.sh test       # run the checks against what is up
#   tools/wings-e2e/run.sh down       # remove everything it made
#
# Needs Docker (OrbStack or Docker Desktop; privileged containers and host
# networking), curl, jq, cargo with the wasm32-wasip2 target, and the panel
# fork built for this machine (HALLOW_E2E_PANEL_BIN, default
# panel/calagopus/target/debug/panel-rs).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)

NAME=${HALLOW_E2E_NAME:-hwe2e}
TAG=${HALLOW_E2E_TAG:-wingse2e}
# A /24 nobody else uses; .1 is the node's private IP (the bridge's gateway,
# which is an address of the Docker host, where Wings runs).
NET=${HALLOW_E2E_NET:-10.213.0}
PANEL_BIN=${HALLOW_E2E_PANEL_BIN:-$repo/panel/calagopus/target/debug/panel-rs}
# Host ports. Only 127.0.0.1 is bound, apart from the panel.
PANEL_PORT=${HALLOW_E2E_PANEL_PORT:-8100}
CP_PORT=${HALLOW_E2E_CP_PORT:-18490}
PROXY_PORT=${HALLOW_E2E_PROXY_PORT:-25590}
REGISTRY_PORT=${HALLOW_E2E_REGISTRY_PORT:-15000}
PG_PORT=${HALLOW_E2E_PG_PORT:-55490}
REDIS_PORT=${HALLOW_E2E_REDIS_PORT:-56390}
# Ports on the Docker host: Wings' API and SFTP, and the game ports.
WINGS_PORT=${HALLOW_E2E_WINGS_PORT:-18480}
SFTP_PORT=${HALLOW_E2E_SFTP_PORT:-12022}
GAME_PORTS=${HALLOW_E2E_GAME_PORTS:-30100-30119}
# Where Wings keeps server files, on the Docker host. Wings hands these paths
# to the Docker Engine as bind mounts, so the node container mounts them at
# the same path.
WINGS_ROOT=/var/lib/hallow-wings-e2e
WINGS_RUN=/run/hallow-wings-e2e
NODE_IMAGE=${HALLOW_E2E_NODE_IMAGE:-debian:trixie}
SKIP_BUILD=${HALLOW_E2E_SKIP_BUILD:-0}
PROXY_IMAGE=${HALLOW_E2E_PROXY_IMAGE:-hallow-proxy:$TAG}

work=${HALLOW_E2E_WORK:-$repo/target/wings-e2e}
TOKEN=hallow-wings-e2e-token-0123456789
SECRET=hallow-wings-e2e-forwarding-secret
NODE_IP=$NET.1
CP_IP=$NET.10
REGISTRY=localhost:$REGISTRY_PORT
CP=http://127.0.0.1:$CP_PORT
PANEL=http://127.0.0.1:$PANEL_PORT
# How containers reach this machine (the panel runs here).
HOST_FROM_CONTAINERS=${HALLOW_E2E_HOST:-host.docker.internal}

log() { printf '\n==> %s\n' "$*" >&2; }
ok() { printf '  ok  %s\n' "$*" >&2; }
die() { printf 'run.sh: error: %s\n' "$*" >&2; exit 1; }

# api METHOD PATH [BODY]: the control plane with the shared token.
api() {
	local method=$1 path=$2
	shift 2
	curl -fsS -X "$method" -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
		"$@" "$CP$path"
}

# wait_for WHAT SECONDS COMMAND...: retries COMMAND every 2 s.
wait_for() {
	local what=$1 secs=$2 start=$SECONDS
	shift 2
	until "$@" >/dev/null 2>&1; do
		((SECONDS - start < secs)) || die "timed out after ${secs}s waiting for $what"
		sleep 2
	done
	ok "$what ($((SECONDS - start)) s)"
}

container() {
	local name=$1
	shift
	docker rm -f "$NAME-$name" >/dev/null 2>&1 || true
	docker run -d --name "$NAME-$name" --label "hallow.e2e=$NAME" "$@" >/dev/null
}

build() {
	if [[ $SKIP_BUILD == 1 ]]; then
		log "build: skipped (HALLOW_E2E_SKIP_BUILD=1)"
		return
	fi
	log "building the Vine plugin, netcheck and the images (tag $TAG)"
	(cd "$repo" && cargo build --release --target wasm32-wasip2 -p hallow-vine)
	(cd "$repo" && cargo build --release -p netcheck)
	(cd "$repo" && docker buildx bake --load \
		--set "proxy.tags=hallow-proxy:$TAG" --set "server.tags=hallow-server-base:$TAG" \
		--set "hub.tags=hallow-hub:$TAG" --set "test-game.tags=hallow-test-game:$TAG" \
		--set "control-plane.tags=hallow-control-plane:$TAG" \
		proxy server hub test-game control-plane)
}

infra() {
	log "network $NAME ($NET.0/24), registry, Postgres, Valkey"
	docker network inspect "$NAME" >/dev/null 2>&1 ||
		docker network create --label "hallow.e2e=$NAME" --subnet "$NET.0/24" --gateway "$NODE_IP" "$NAME" >/dev/null
	container registry --network "$NAME" -p "127.0.0.1:$REGISTRY_PORT:5000" registry:3
	container postgres --network "$NAME" --ip "$NET.5" -p "127.0.0.1:$PG_PORT:5432" \
		-e POSTGRES_PASSWORD=hallow-e2e -e POSTGRES_DB=hallow postgres:18-alpine
	container valkey --network "$NAME" --ip "$NET.6" -p "127.0.0.1:$REDIS_PORT:6379" valkey/valkey:9.1-alpine
	wait_for "Postgres" 60 docker exec "$NAME-postgres" pg_isready -U postgres -d hallow
	wait_for "the registry" 30 curl -fsS "http://127.0.0.1:$REGISTRY_PORT/v2/"
}

push() {
	log "pushing the server images to the registry, then dropping the local copies so Wings must pull"
	local image
	for image in hallow-hub hallow-test-game; do
		docker tag "$image:$TAG" "$REGISTRY/$image:$TAG"
		docker push -q "$REGISTRY/$image:$TAG" >/dev/null
		docker rmi "$REGISTRY/$image:$TAG" >/dev/null
	done
	ok "pushed $REGISTRY/hallow-hub:$TAG and $REGISTRY/hallow-test-game:$TAG"
}

panel() {
	log "the panel on :$PANEL_PORT"
	[[ -x $PANEL_BIN ]] || die "no panel binary at $PANEL_BIN (build panel/calagopus, or set HALLOW_E2E_PANEL_BIN)"
	docker exec "$NAME-postgres" psql -qU postgres -c 'CREATE DATABASE panel' 2>/dev/null || true
	mkdir -p "$work/panel"
	[[ -s $work/panel/key ]] || openssl rand -hex 32 >"$work/panel/key"
	cat >"$work/panel/env" <<EOF
DATABASE_URL=postgresql://postgres:hallow-e2e@127.0.0.1:$PG_PORT/panel
REDIS_URL=redis://127.0.0.1:$REDIS_PORT/1
APP_ENCRYPTION_KEY=$(cat "$work/panel/key")
DATABASE_MIGRATE=true
BIND=0.0.0.0
PORT=$PANEL_PORT
APP_LOG_DIRECTORY=$work/panel/logs
EOF
	if [[ -s $work/panel/pid ]] && kill -0 "$(cat "$work/panel/pid")" 2>/dev/null; then
		ok "already running"
	else
		curl -fsS -o /dev/null "$PANEL/api/settings" 2>/dev/null && die "something else answers on :$PANEL_PORT"
		(
			cd "$work/panel"
			set -a
			# shellcheck disable=SC1091
			. ./env
			set +a
			exec "$PANEL_BIN" >"$work/panel/panel.log" 2>&1
		) &
		echo $! >"$work/panel/pid"
	fi
	wait_for "the panel" 120 curl -fsS -o /dev/null "$PANEL/api/settings"

	if [[ ! -s $work/panel/api-key ]]; then
		(
			set -a
			# shellcheck disable=SC1091
			. "$work/panel/env"
			set +a
			"$PANEL_BIN" users create --username admin --email admin@example.com --name-first Hallow \
				--name-last Admin --password hallow-wings-e2e --admin true --json >/dev/null 2>&1
		)
		local jar=$work/panel/cookies
		curl -fsS -c "$jar" -H 'content-type: application/json' \
			-d '{"user":"admin","password":"hallow-wings-e2e"}' "$PANEL/api/auth/login" >/dev/null
		# The permissions deploy/node/README.md lists, plus locations for this script.
		curl -fsS -b "$jar" -H 'content-type: application/json' -d '{
			"name": "hallow-control-plane", "allowed_ips": [], "user_permissions": [],
			"server_permissions": [], "expires": null,
			"admin_permissions": ["nodes.read", "nodes.create", "nodes.allocations",
				"nodes.reset-token", "nodes.delete", "nodes.read-token",
				"locations.read", "locations.create"]}' \
			"$PANEL/api/client/account/api-keys" | jq -r .key >"$work/panel/api-key"
		curl -fsS -H "Authorization: Bearer $(cat "$work/panel/api-key")" -H 'content-type: application/json' \
			-d '{"name": "local", "description": "Wings end to end", "flag": null, "backup_configuration_uuid": null}' \
			"$PANEL/api/admin/locations" | jq -r .location.uuid >"$work/panel/location"
	fi
	ok "admin API key and location $(cat "$work/panel/location")"
}

control_plane() {
	log "the control plane at $CP_IP:8080 (HALLOW_ORCHESTRATOR=wings), API on 127.0.0.1:$CP_PORT"
	mkdir -p "$work/kinds/hub" "$work/kinds/test-game"
	cp "$repo/servers/hub/hallow.toml" "$work/kinds/hub/hallow.toml"
	cp "$repo/servers/test-game/hallow.toml" "$work/kinds/test-game/hallow.toml"
	cat >"$work/kinds/kinds.toml" <<EOF
[[kind]]
image = "$REGISTRY/hallow-hub:$TAG"
manifest = "hub/hallow.toml"
memory_mb = 1024

[[kind]]
image = "$REGISTRY/hallow-test-game:$TAG"
manifest = "test-game/hallow.toml"
memory_mb = 1024
EOF
	container control-plane --network "$NAME" --ip "$CP_IP" -p "127.0.0.1:$CP_PORT:8080" \
		-v "$work/kinds:/etc/hallow/kinds:ro" \
		-e HALLOW_TOKEN="$TOKEN" \
		-e HALLOW_KEY_PREFIX="$TAG" \
		-e HALLOW_REDIS_URL="redis://$NET.6:6379" \
		-e HALLOW_DATABASE_URL="postgres://postgres:hallow-e2e@$NET.5:5432/hallow" \
		-e HALLOW_PANEL_URL="http://$HOST_FROM_CONTAINERS:$PANEL_PORT" \
		-e HALLOW_PANEL_API_KEY="$(cat "$work/panel/api-key")" \
		-e HALLOW_PANEL_LOCATIONS="*=$(cat "$work/panel/location")" \
		-e HALLOW_NODE_WINGS_REMOTE="http://$CP_IP:8080" \
		-e HALLOW_NODE_WINGS_PORT="$WINGS_PORT" \
		-e HALLOW_NODE_SFTP_PORT="$SFTP_PORT" \
		-e HALLOW_NODE_GAME_PORTS="$GAME_PORTS" \
		-e HALLOW_ORCHESTRATOR=wings \
		-e HALLOW_INSTANCE_CONTROL_PLANE="$CP_IP:8080" \
		-e HALLOW_INSTANCE_ENV="HALLOW_VINE_SECRET=$SECRET" \
		-e HALLOW_KINDS_FILE=/etc/hallow/kinds/kinds.toml \
		-e HALLOW_INSTANCE_START_TIMEOUT_SECS=240 \
		-e HALLOW_INSTANCE_EMPTY_SECS=20 \
		-e RUST_LOG=info,hallow_control_plane=debug \
		"hallow-control-plane:$TAG"
	wait_for "the control plane" 60 curl -fsS "$CP/healthz"
}

node() {
	log "a Debian 13 node container with the real install.sh (host network, Docker socket)"
	local reply join_token
	reply=$(api POST /v1/nodes/join-tokens -d '{"role": "burst", "region": "local", "name": "e2e-node-1", "labels": {}}')
	join_token=$(jq -r .token <<<"$reply")
	ok "join token $(jq -r .id <<<"$reply")"
	container node --privileged --network host --pid host --cgroupns host \
		-v /var/run/docker.sock:/var/run/docker.sock \
		-v "$WINGS_ROOT:$WINGS_ROOT" -v "$WINGS_RUN:$WINGS_RUN" \
		--hostname e2e-node-1 "$NODE_IMAGE" sleep infinity
	docker cp "$repo/deploy/node/install.sh" "$NAME-node:/root/install.sh"
	# Docker comes from the host's socket, there is no systemd, the firewall
	# and kernel settings belong to the Docker host, and there is no
	# WireGuard: the node's private IP is the e2e bridge's gateway.
	docker exec "$NAME-node" bash /root/install.sh \
		--control-plane "http://$CP_IP:8080" --join-token "$join_token" \
		--role burst --region local --private-ip "$NODE_IP" --game-ports "$GAME_PORTS" \
		--no-firewall --skip-docker --skip-sysctl --no-systemd 2>&1 | sed 's/^/  | /'
	docker exec "$NAME-node" test -s /etc/calagopus-wings/config.yml || die "Wings has no config after install.sh"
	ok "Wings enrolled: $(docker exec "$NAME-node" wings version 2>/dev/null | head -n 1)"
	wings
}

# (Re)starts Wings in the node container.
wings() {
	log "starting Wings on the node"
	docker restart -t 15 "$NAME-node" >/dev/null
	# Settings that differ from a real node only because this one shares the
	# Docker host with everything else: its own data directories, and the e2e
	# network for containers (Wings' default 172.18.0.0/16 may be taken).
	docker exec -d "$NAME-node" sh -c "exec env \
		CALAGOPUS_SYSTEM_ROOT_DIRECTORY=$WINGS_ROOT \
		CALAGOPUS_SYSTEM_PASSWD_DIRECTORY=$WINGS_RUN/etc \
		CALAGOPUS_DOCKER_NETWORK_NAME=$NAME CALAGOPUS_DOCKER_NETWORK_MODE=$NAME \
		CALAGOPUS_DOCKER_NETWORK_INTERFACE=$NODE_IP \
		wings >>/var/log/hallow-wings.log 2>&1"
	# Any HTTP answer will do (`/` is a 404); the control plane calls it here.
	wait_for "Wings' API on $NODE_IP:$WINGS_PORT" 90 \
		docker exec "$NAME-control-plane" curl -sS -o /dev/null "http://$NODE_IP:$WINGS_PORT/"
}

proxy() {
	log "a Vine proxy with the hallow_vine plugin on 127.0.0.1:$PROXY_PORT"
	local dir=$work/proxy
	mkdir -p "$dir"
	cp "$repo/deploy/compose/config/hallow/vine.toml" "$dir/vine.toml"
	cp "$repo/target/wasm32-wasip2/release/hallow_vine.wasm" "$dir/hallow_vine.wasm"
	printf '%s\n' "$TOKEN" >"$dir/token"
	printf '%s\n' "$SECRET" >"$dir/forwarding.secret"
	cat >"$dir/config.toml" <<EOF
control_plane = "$CP_IP:8080"
token_file = "token"
proxy_id = "e2e-proxy-1"
address = "$NET.20:25565"
forwarding = "vine"
forwarding_secret_file = "forwarding.secret"
poll_ms = 250
EOF
	container proxy --network "$NAME" --ip "$NET.20" -p "127.0.0.1:$PROXY_PORT:25565" \
		-v "$dir/vine.toml:/proxy/vine.toml:ro" \
		-v "$dir/hallow_vine.wasm:/proxy/plugins/hallow_vine.wasm:ro" \
		-v "$dir/token:/proxy/plugins/data/hallow_vine/token:ro" \
		-v "$dir/forwarding.secret:/proxy/plugins/data/hallow_vine/forwarding.secret:ro" \
		-v "$dir/config.toml:/proxy/plugins/data/hallow_vine/config.toml:ro" \
		"$PROXY_IMAGE"
	wait_for "the proxy to register" 60 sh -c "curl -fsS -H 'Authorization: Bearer $TOKEN' $CP/v1/proxies | jq -e 'length == 1'"
}

up() {
	mkdir -p "$work"
	build
	infra
	push
	panel
	control_plane
	node
	proxy
}

instances() { api GET /v1/instances; }

# The first instance of a kind in a state, by name.
instance_of() { instances | jq -er --arg k "$1" --arg s "$2" '[.[] | select(.kind == $k and .state == $s)][0].name'; }

wings_containers() {
	docker ps -a --filter "label=hallow.owner=$TAG" --format '{{.Names}} {{.Image}} {{.Status}}'
}

test_run() {
	local netcheck=$repo/target/release/netcheck
	[[ -x $netcheck ]] || die "build netcheck first (cargo build --release -p netcheck)"

	log "the node joined: in the control plane and the panel"
	api GET /v1/nodes | jq -e --arg ip "$NODE_IP" '.[] | select(.name == "e2e-node-1" and .private_ip == $ip)' >/dev/null ||
		die "the node isn't in GET /v1/nodes"
	ok "GET /v1/nodes lists e2e-node-1 at $NODE_IP"
	curl -fsS -H "Authorization: Bearer $(cat "$work/panel/api-key")" "$PANEL/api/admin/nodes" |
		jq -e '.nodes.data[] | select(.name == "e2e-node-1")' >/dev/null || die "the panel has no node e2e-node-1"
	ok "the panel has node e2e-node-1"

	log "the hub instance on Wings: created, image pulled, started, registered"
	wait_for "a ready hub instance" 300 instance_of hub ready
	local hub
	hub=$(instance_of hub ready)
	instances | jq -c --arg n "$hub" '.[] | select(.name == $n) | {name, node, address, backend_id, image}' >&2
	wings_containers | grep -q . || die "no container labelled hallow.owner=$TAG"
	ok "Wings runs: $(wings_containers | tr '\n' ';')"
	local hub_uuid scoped
	hub_uuid=$(instances | jq -r --arg n "$hub" '.[] | select(.name == $n) | .backend_id')
	scoped=$(docker inspect "$hub_uuid" --format '{{range .Config.Env}}{{println .}}{{end}}' |
		sed -n 's/^HALLOW_CONTROL_PLANE_TOKEN=//p')
	[[ $scoped == hit_* && $scoped != "$TOKEN" ]] || die "the instance doesn't have its own scoped token"
	local status
	status=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $scoped" "$CP/v1/instances")
	[[ $status == 403 ]] || die "the instance's token got $status from GET /v1/instances, not 403"
	ok "the container carries its own scoped token (hit_...), refused on admin routes"

	log "a bot joins through Vine, lands on the hub, and queues for test-game"
	"$netcheck" --proxy "127.0.0.1:$PROXY_PORT" --name wingsbot --route "/play test-game" --stay 90 \
		>"$work/netcheck-1.log" 2>&1 &
	local bot=$!
	wait_for "wingsbot on a test-game instance" 240 sh -c \
		"curl -fsS -H 'Authorization: Bearer $TOKEN' '$CP/v1/players?name=wingsbot' | jq -er '.server | startswith(\"test-game-\")'"
	local game
	game=$(api GET '/v1/players?name=wingsbot' | jq -r .server)
	instances | jq -c --arg n "$game" '.[] | select(.name == $n) | {name, node, state, address, players}' >&2
	ok "wingsbot was sent to $game, a Wings instance"
	kill "$bot" 2>/dev/null || true
	wait "$bot" 2>/dev/null || true
	sed 's/^/  | /' "$work/netcheck-1.log" >&2

	log "a second bot switches to the game instance by name (/server)"
	"$netcheck" --proxy "127.0.0.1:$PROXY_PORT" --name wingsbot2 --route "$game,$hub" 2>&1 | sed 's/^/  | /' >&2
	ok "Vine routed wingsbot2 to $game and back to $hub"

	log "deleting the game instance removes its Wings server and container"
	local uuid
	uuid=$(instances | jq -r --arg n "$game" '.[] | select(.name == $n) | .backend_id')
	docker ps -a --format '{{.Names}}' | grep -qx "$uuid" || die "no container named $uuid"
	api DELETE "/v1/instances/$game" -o /dev/null
	wait_for "$game to leave the instance list" 120 sh -c \
		"curl -fsS -H 'Authorization: Bearer $TOKEN' $CP/v1/instances | jq -e --arg n '$game' 'all(.[]; .name != \$n)'"
	wait_for "container $uuid to be gone" 60 sh -c "! docker ps -a --format '{{.Names}}' | grep -qx '$uuid'"
	log "all checks passed"
}

down() {
	log "removing everything named $NAME"
	if [[ -s $work/panel/pid ]]; then
		kill "$(cat "$work/panel/pid")" 2>/dev/null || true
		rm -f "$work/panel/pid"
	fi
	# Wings first, or it restarts the servers it sees die; then its servers
	# (they carry the owner label), then the rest.
	docker rm -f "$NAME-node" >/dev/null 2>&1 || true
	local id
	docker ps -aq --filter "label=hallow.owner=$TAG" | while read -r id; do docker rm -f "$id" >/dev/null; done
	docker ps -aq --filter "label=hallow.e2e=$NAME" | while read -r id; do docker rm -f "$id" >/dev/null; done
	docker network rm "$NAME" >/dev/null 2>&1 || true
	# Wings' files on the Docker host.
	docker run --rm -v "$(dirname "$WINGS_ROOT"):/host-lib" -v "$(dirname "$WINGS_RUN"):/host-run" \
		alpine:3 rm -rf "/host-lib/$(basename "$WINGS_ROOT")" "/host-run/$(basename "$WINGS_RUN")"
	rm -rf "$work"
	ok "clean"
}

case ${1:-all} in
up) up ;;
build | infra | push | panel | node | wings | proxy) mkdir -p "$work" && "$1" ;;
control-plane) mkdir -p "$work" && control_plane ;;
test) test_run ;;
down) down ;;
all)
	# A failed run is left up for a look unless HALLOW_E2E_KEEP=0.
	up
	test_run
	[[ ${HALLOW_E2E_KEEP:-0} == 1 ]] || down
	;;
*) die "usage: run.sh [up|test|down|all], or one step of up: build infra push panel control-plane node wings proxy" ;;
esac
