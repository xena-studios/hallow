#!/usr/bin/env bash
# Hallow on a real Calagopus Wings, end to end, on one machine: the panel, the
# control plane with HALLOW_ORCHESTRATOR=wings, a node joined with the real
# install.sh and `wings configure --enroll`, Wings running instances from a
# local registry, a bot routed to a Wings instance through Vine, and staff
# using a panel server's console, files and power actions through the panel.
# See tools/wings-e2e/README.md.
#
#   tools/wings-e2e/run.sh            # up, test, down
#   tools/wings-e2e/run.sh up         # bring it up and leave it running
#   tools/wings-e2e/run.sh test       # run the checks against what is up
#   tools/wings-e2e/run.sh down       # remove everything it made
#
# Needs Docker (OrbStack, or Docker Engine on Linux; privileged containers and
# host networking), curl, jq, nc, Node.js 22+, cargo with the wasm32-wasip2
# target, and the panel fork built for this machine (HALLOW_E2E_PANEL_BIN,
# default panel/calagopus/target/debug/panel-rs).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)

NAME=${HALLOW_E2E_NAME:-hwe2e}
TAG=${HALLOW_E2E_TAG:-wingse2e}
# A /24 nobody else uses, for the containers; .1 is the bridge's gateway.
NET=${HALLOW_E2E_NET:-10.213.0}
# The node's private IP: an address of the Docker host (where Wings runs) that
# this machine (the panel) and the containers (the control plane, Vine) both
# reach, as they reach a real node's WireGuard address. `infra` finds it (the
# Docker host's own source address), or set it.
NODE_IP=${HALLOW_E2E_NODE_IP:-}
PANEL_BIN=${HALLOW_E2E_PANEL_BIN:-$repo/panel/calagopus/target/debug/panel-rs}
# Host ports. Only 127.0.0.1 is bound, apart from the panel.
PANEL_PORT=${HALLOW_E2E_PANEL_PORT:-8100}
CP_PORT=${HALLOW_E2E_CP_PORT:-18490}
PROXY_PORT=${HALLOW_E2E_PROXY_PORT:-25590}
REGISTRY_PORT=${HALLOW_E2E_REGISTRY_PORT:-15000}
PG_PORT=${HALLOW_E2E_PG_PORT:-55490}
REDIS_PORT=${HALLOW_E2E_REDIS_PORT:-56390}
# Ports on the Docker host: Wings' API and SFTP, the game ports, and the panel
# server's port (outside the game ports, which belong to instances).
WINGS_PORT=${HALLOW_E2E_WINGS_PORT:-18480}
SFTP_PORT=${HALLOW_E2E_SFTP_PORT:-12022}
GAME_PORTS=${HALLOW_E2E_GAME_PORTS:-30100-30119}
STATIC_PORT=${HALLOW_E2E_STATIC_PORT:-30150}
# Where Wings keeps server files, on the Docker host. Wings hands these paths
# to the Docker Engine as bind mounts, so the node container mounts them at
# the same path.
WINGS_ROOT=/var/lib/hallow-wings-$NAME
WINGS_RUN=/run/hallow-wings-$NAME
NODE_IMAGE=${HALLOW_E2E_NODE_IMAGE:-debian:trixie}
SKIP_BUILD=${HALLOW_E2E_SKIP_BUILD:-0}
PROXY_IMAGE=${HALLOW_E2E_PROXY_IMAGE:-hallow-proxy:$TAG}

work=${HALLOW_E2E_WORK:-$repo/target/wings-e2e}
TOKEN=hallow-wings-e2e-token-0123456789
SECRET=hallow-wings-e2e-forwarding-secret
GATEWAY=$NET.1
CP_IP=$NET.10
REGISTRY=localhost:$REGISTRY_PORT
CP=http://127.0.0.1:$CP_PORT
PANEL=http://127.0.0.1:$PANEL_PORT
# How containers reach this machine (the panel runs here).
HOST_FROM_CONTAINERS=${HALLOW_E2E_HOST:-host.docker.internal}
# The panel server staff use through the panel: a console that echoes what it
# is sent, from a small image in the local registry.
CONSOLE_IMAGE=$REGISTRY/hallow-e2e-console:$TAG

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

# panel_api METHOD PATH [CURL ARGS]: the panel as staff, with the staff API key.
panel_api() {
	local method=$1 path=$2
	shift 2
	curl -fsS -X "$method" -H "Authorization: Bearer $(cat "$work/panel/staff-key")" \
		-H 'content-type: application/json' "$@" "$PANEL$path"
}

# panel_session METHOD PATH [CURL ARGS]: the panel as the logged-in admin.
panel_session() {
	local method=$1 path=$2
	shift 2
	curl -fsS -X "$method" -b "$work/panel/cookies" -H 'content-type: application/json' "$@" "$PANEL$path"
}

# The node's private IP: HALLOW_E2E_NODE_IP, or the one `infra` found.
load_node_ip() {
	if [[ -z $NODE_IP ]]; then
		[[ -s $work/node-ip ]] || die "no node IP yet: run the infra step first, or set HALLOW_E2E_NODE_IP"
		NODE_IP=$(cat "$work/node-ip")
	fi
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
	# The Vine plugin, the server plugin, and the hub's and test game's own
	# plugins, which their images copy from servers/*/plugins.
	"$repo/servers/build-plugins.sh"
	(cd "$repo" && cargo build --release -p netcheck)
	(cd "$repo" && docker buildx bake --load \
		--set "proxy.tags=hallow-proxy:$TAG" --set "server.tags=hallow-server-base:$TAG" \
		--set "hub.tags=hallow-hub:$TAG" --set "test-game.tags=hallow-test-game:$TAG" \
		--set "control-plane.tags=hallow-control-plane:$TAG" \
		proxy server hub test-game control-plane)
	# The panel server's image: Wings runs an egg's image with the startup
	# command in STARTUP, as Pterodactyl-style images expect.
	docker build -q -t "hallow-e2e-console:$TAG" - >/dev/null <<'DOCKERFILE'
FROM alpine:3
RUN printf '#!/bin/sh\ncd /home/container\nexec /bin/sh -c "$STARTUP"\n' >/entrypoint.sh \
    && chmod 0755 /entrypoint.sh
CMD ["/entrypoint.sh"]
DOCKERFILE
}

infra() {
	log "network $NAME ($NET.0/24), registry, Postgres, Valkey"
	docker network inspect "$NAME" >/dev/null 2>&1 ||
		docker network create --label "hallow.e2e=$NAME" --subnet "$NET.0/24" --gateway "$GATEWAY" "$NAME" >/dev/null
	container registry --network "$NAME" -p "127.0.0.1:$REGISTRY_PORT:5000" registry:3
	container postgres --network "$NAME" --ip "$NET.5" -p "127.0.0.1:$PG_PORT:5432" \
		-e POSTGRES_PASSWORD=hallow-e2e -e POSTGRES_DB=hallow postgres:18-alpine
	container valkey --network "$NAME" --ip "$NET.6" -p "127.0.0.1:$REDIS_PORT:6379" valkey/valkey:9.1-alpine
	wait_for "Postgres" 60 docker exec "$NAME-postgres" pg_isready -U postgres -d hallow
	wait_for "the registry" 30 curl -fsS "http://127.0.0.1:$REGISTRY_PORT/v2/"
	find_node_ip
}

# The node's private IP: the Docker host's own source address (on OrbStack,
# the VM's address on its link to the Mac; on Linux, the machine's), checked to
# be reachable from this machine on Wings' port, as the panel needs.
find_node_ip() {
	if [[ -z $NODE_IP ]]; then
		NODE_IP=$(docker run --rm --network host alpine:3 ip -4 route get 1.1.1.1 |
			sed -n 's/.* src \([0-9.]*\).*/\1/p')
		[[ -n $NODE_IP ]] || die "couldn't find the Docker host's address; set HALLOW_E2E_NODE_IP"
	fi
	docker rm -f "$NAME-probe" >/dev/null 2>&1 || true
	docker run -d --name "$NAME-probe" --label "hallow.e2e=$NAME" --network host alpine:3 \
		nc -lk -p "$WINGS_PORT" >/dev/null
	local reached=0 try
	for try in 1 2 3 4 5; do
		if nc -z -w 2 "$NODE_IP" "$WINGS_PORT" 2>/dev/null; then
			reached=1
			break
		fi
		sleep 1
	done
	docker rm -f "$NAME-probe" >/dev/null
	((reached)) || die "this machine can't reach $NODE_IP:$WINGS_PORT on the Docker host, so the panel couldn't reach Wings; set HALLOW_E2E_NODE_IP to an address of the Docker host that this machine reaches (Docker Desktop for Mac has none)"
	printf '%s\n' "$NODE_IP" >"$work/node-ip"
	ok "the node's private IP is $NODE_IP (try $try), reachable from this machine"
}

push() {
	log "pushing the server images to the registry, then dropping the local copies so Wings must pull"
	local image
	for image in hallow-hub hallow-test-game hallow-e2e-console; do
		docker tag "$image:$TAG" "$REGISTRY/$image:$TAG"
		docker push -q "$REGISTRY/$image:$TAG" >/dev/null
		docker rmi "$REGISTRY/$image:$TAG" >/dev/null
	done
	ok "pushed $REGISTRY/hallow-hub:$TAG, $REGISTRY/hallow-test-game:$TAG and $CONSOLE_IMAGE"
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
APP_ENABLE_WINGS_PROXY=true
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
		curl -fsS -c "$work/panel/cookies" -H 'content-type: application/json' \
			-d '{"user":"admin","password":"hallow-wings-e2e"}' "$PANEL/api/auth/login" >/dev/null
		# The permissions deploy/node/README.md lists, plus locations for this script.
		panel_session POST /api/client/account/api-keys -d '{
			"name": "hallow-control-plane", "allowed_ips": [], "user_permissions": [],
			"server_permissions": [], "expires": null,
			"admin_permissions": ["nodes.read", "nodes.create", "nodes.update", "nodes.allocations",
				"nodes.reset-token", "nodes.delete", "nodes.read-token",
				"locations.read", "locations.create"]}' | jq -r .key >"$work/panel/api-key"
		# What staff use in the checks: the node list, servers' consoles, files
		# and power, and the Hallow pages.
		panel_session POST /api/client/account/api-keys -d '{
			"name": "staff", "allowed_ips": [], "user_permissions": [], "expires": null,
			"server_permissions": ["control.read-console", "control.console", "control.start",
				"control.stop", "control.restart", "files.create", "files.read",
				"files.read-content", "files.update"],
			"admin_permissions": ["nodes.read", "nodes.read-token", "servers.read",
				"hallow.instances.read", "hallow.instances.console"]}' | jq -r .key >"$work/panel/staff-key"
		curl -fsS -H "Authorization: Bearer $(cat "$work/panel/api-key")" -H 'content-type: application/json' \
			-d '{"name": "local", "description": "Wings end to end", "flag": null, "backup_configuration_uuid": null}' \
			"$PANEL/api/admin/locations" | jq -r .location.uuid >"$work/panel/location"
		# The Hallow pages read the control plane (Admin > Extensions > Hallow).
		panel_session PUT /api/admin/extensions/dev.xenastudios.hallow/settings \
			-d "{\"control_plane_url\": \"$CP\", \"control_plane_token\": \"$TOKEN\"}" >/dev/null
	fi
	ok "API keys (control plane, staff), location $(cat "$work/panel/location"), Hallow settings"
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
		-e HALLOW_NODE_PUBLIC_URL="$PANEL/wings-proxy/{uuid}" \
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
	load_node_ip
	log "a Debian 13 node container with the real install.sh (host network, Docker socket), private IP $NODE_IP"
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
	# WireGuard: the node's private IP is an address of the Docker host.
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
	load_node_ip
	log "starting Wings on the node"
	docker restart -t 15 "$NAME-node" >/dev/null
	# Settings that differ from a real node only because this one shares the
	# Docker host with everything else: its own data directories, and the e2e
	# network for containers (Wings' default 172.18.0.0/16 may be taken).
	docker exec -d "$NAME-node" sh -c "exec env \
		CALAGOPUS_SYSTEM_ROOT_DIRECTORY=$WINGS_ROOT \
		CALAGOPUS_SYSTEM_PASSWD_DIRECTORY=$WINGS_RUN/etc \
		CALAGOPUS_DOCKER_NETWORK_NAME=$NAME CALAGOPUS_DOCKER_NETWORK_MODE=$NAME \
		CALAGOPUS_DOCKER_NETWORK_INTERFACE=$GATEWAY \
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

# A server made in the panel on the node, as staff make long-lived servers:
# a nest and egg for the echo console, an allocation outside the game ports,
# and the server. Wings fetches its configuration from its remote, the control
# plane, which passes the call to the panel.
panel_server() {
	load_node_ip
	log "a panel server on the node, on $NODE_IP:$STATIC_PORT"
	if [[ -s $work/panel/server ]]; then
		ok "already made: $(cat "$work/panel/server")"
		return
	fi
	local node nest egg allocation owner startup server
	node=$(panel_session GET '/api/admin/nodes?per_page=100' |
		jq -er '.nodes.data[] | select(.name == "e2e-node-1") | .uuid')
	nest=$(panel_session POST /api/admin/nests \
		-d '{"author": "hallow-e2e@example.com", "name": "Hallow e2e", "description": null}' | jq -er .nest.uuid)
	# shellcheck disable=SC2016 # expanded by the server's shell, not this one
	startup='echo console ready; while read -r line; do [ "$line" = stop ] && exit 0; echo "console got: $line"; done'
	egg=$(jq -n --arg image "$CONSOLE_IMAGE" --arg startup "$startup" '{
		author: "hallow-e2e@example.com", name: "Echo console", description: null,
		config_files: [], config_startup: {done: ["console ready"], strip_ansi: false},
		config_stop: {type: "command", value: "stop"},
		config_script: {container: "alpine:3", entrypoint: "sh", content: ""},
		startup_commands: {default: $startup}, force_outgoing_ip: false, separate_port: false,
		features: [], docker_images: {console: $image}, file_denylist: []}' |
		panel_session POST "/api/admin/nests/$nest/eggs" -d @- | jq -er .egg.uuid)
	panel_session POST "/api/admin/nodes/$node/allocations" \
		-d "{\"ip\": \"$NODE_IP\", \"ip_alias\": null, \"ports\": [$STATIC_PORT]}" >/dev/null
	allocation=$(panel_session GET "/api/admin/nodes/$node/allocations?per_page=100&search=$STATIC_PORT" |
		jq -er --argjson port "$STATIC_PORT" '[.. | objects | select(.port? == $port and .uuid? != null)][0].uuid')
	owner=$(panel_session GET /api/client/account | jq -er .user.uuid)
	server=$(jq -n --arg node "$node" --arg owner "$owner" --arg egg "$egg" --arg allocation "$allocation" \
		--arg startup "$startup" --arg image "$CONSOLE_IMAGE" '{
		node_uuid: $node, owner_uuid: $owner, egg_uuid: $egg, backup_configuration_uuid: null,
		allocation_uuid: $allocation, allocation_uuids: [], start_on_completion: true,
		skip_installer: true, external_id: null, name: "e2e-console",
		description: "Wings end to end: a panel server", pinned_cpus: [],
		limits: {cpu: 0, memory: 256, memory_overhead: 0, swap: 0, disk: 512, io_weight: null},
		startup: $startup, image: $image, timezone: null,
		hugepages_passthrough_enabled: false, kvm_passthrough_enabled: false,
		feature_limits: {allocations: 0, databases: 0, backups: 0, schedules: 0}, variables: []}' |
		panel_session POST /api/admin/servers -d @- | jq -er .server.uuid)
	printf '%s\n' "$server" >"$work/panel/server"
	ok "server $server (egg $egg, allocation $allocation)"
	wait_for "the panel server to run" 180 sh -c \
		"curl -fsS -H 'Authorization: Bearer $(cat "$work/panel/staff-key")' $PANEL/api/client/servers/$server/resources |
			jq -e '.. | objects | select(.state? == \"running\")'"
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
	panel_server
}

instances() { api GET /v1/instances; }

# The first instance of a kind in a state, by name.
instance_of() { instances | jq -er --arg k "$1" --arg s "$2" '[.[] | select(.kind == $k and .state == $s)][0].name'; }

# The panel shows the node online the way its node list does: the browser
# asks Wings' /api/system with the node's token, at the node's public URL (the
# panel's Wings proxy, which calls Wings at the private IP); and the panel
# itself reads Wings' system overview.
panel_node_online() {
	local node public token
	node=$(panel_api GET '/api/admin/nodes?per_page=100' | jq -c '.nodes.data[] | select(.name == "e2e-node-1")')
	[[ -n $node ]] || die "the panel has no node e2e-node-1"
	[[ $(jq -r .url <<<"$node") == "http://$NODE_IP:$WINGS_PORT/" ]] ||
		die "the panel calls Wings at $(jq -r .url <<<"$node"), not http://$NODE_IP:$WINGS_PORT/"
	public=$(jq -r '.publicUrl // .public_url // empty' <<<"$node")
	[[ $public == "$PANEL/wings-proxy/"* ]] || die "the node's public URL is '$public', not the panel's Wings proxy"
	ok "the panel has node e2e-node-1: Wings at $(jq -r .url <<<"$node"), browsers at $public"
	token=$(panel_api GET "/api/admin/nodes/$(jq -r .uuid <<<"$node")/token" | jq -er .token)
	curl -fsS -H "Authorization: Bearer $token" "$public/api/system" | jq -e .version >/dev/null ||
		die "Wings' /api/system through the panel's Wings proxy failed"
	ok "online in the node list: /api/system through the Wings proxy answers $(curl -fsS -H "Authorization: Bearer $token" "$public/api/system" | jq -r .version)"
	panel_api GET "/api/admin/nodes/$(jq -r .uuid <<<"$node")/system/overview" |
		jq -ce '.overview // . | {version, servers}' >&2 || die "the panel can't read the node's system overview"
	ok "the panel reads the node's system overview"
}

# Staff on a panel server: the console over the websocket the panel hands out
# (logs, a command), a command and a restart through the panel's API, and the
# file manager.
panel_server_checks() {
	log "staff on the panel server, through the panel"
	local server ws
	server=$(cat "$work/panel/server")
	panel_api GET "/api/client/servers/$server/resources" | jq -c '.. | objects | select(.state?) | {state}' >&2
	ws=$(panel_api GET "/api/client/servers/$server/websocket")
	[[ $(jq -r .url <<<"$ws") == "ws://127.0.0.1:$PANEL_PORT/wings-proxy/"* ]] ||
		die "the console's websocket is $(jq -r .url <<<"$ws"), not the panel's Wings proxy"
	ok "websocket token for $(jq -r .url <<<"$ws")"
	# Unique words, so the console's history from an earlier run can't match.
	local run=$RANDOM$RANDOM
	PANEL_API_KEY=$(cat "$work/panel/staff-key") command node "$here/console.mjs" "$ws" \
		'log=console ready' "command=socket-$run" "log=console got: socket-$run" \
		"http=POST,$PANEL/api/client/servers/$server/command,{\"command\": \"api-$run\"}" \
		"log=console got: api-$run" \
		"http=POST,$PANEL/api/client/servers/$server/power,{\"action\": \"restart\"}" \
		'status=stopping' 'status=running' 'wait=2' "command=restarted-$run" "log=console got: restarted-$run" ||
		die "the panel server's console"
	ok "console: history, a command over the socket and one through the API, a restart"

	local content
	content="written through the panel at $(date -u +%FT%TZ)"
	panel_api POST "/api/client/servers/$server/files/write?file=/hallow-e2e.txt" \
		-H 'content-type: text/plain' --data-binary "$content" >/dev/null || die "writing a file"
	panel_api GET "/api/client/servers/$server/files/list?directory=/" |
		jq -e '.. | objects | select(.name? == "hallow-e2e.txt")' >/dev/null || die "the file isn't listed"
	[[ $(panel_api GET "/api/client/servers/$server/files/contents?file=/hallow-e2e.txt") == "$content" ]] ||
		die "reading the file back gave something else"
	docker exec "$NAME-node" cat "$WINGS_ROOT/volumes/$server/hallow-e2e.txt" | grep -qxF "$content" ||
		die "the file isn't in the server's volume on the node"
	ok "files: written, listed and read back, and on the node in the server's volume"
}

# The panel's Instances page: the control plane's instances, read by the
# Hallow extension.
panel_instances() {
	log "the panel's Instances page"
	local hub
	hub=$(instance_of hub ready)
	panel_api GET /api/admin/hallow/instances | jq -e --arg n "$hub" '.instances[] | select(.name == $n)' >/dev/null ||
		die "the Instances page doesn't list $hub"
	ok "the Instances page lists $hub"

	log "an instance's console from the panel (the Hallow extension signs the Wings token)"
	local ws
	ws=$(panel_api GET "/api/admin/hallow/instances/$hub/websocket")
	[[ $(jq -r .url <<<"$ws") == "ws://127.0.0.1:$PANEL_PORT/wings-proxy/"*"/api/servers/$(jq -r .server_uuid <<<"$ws")/ws" ]] ||
		die "the instance console's websocket is $(jq -r .url <<<"$ws"), not the panel's Wings proxy"
	ok "websocket token for $hub on $(jq -r .node <<<"$ws")"
	# Pumpkin's console echoes what it is sent (with the cursor's escape codes
	# between the command and its argument).
	local run=$RANDOM$RANDOM
	PANEL_API_KEY=$(cat "$work/panel/staff-key") command node "$here/console.mjs" "$ws" \
		'log=Server is now running' "command=say panel-$run" "log=panel-$run" ||
		die "$hub's console through the panel"
	ok "console: $hub's history, and a command echoed live"
}

wings_containers() {
	docker ps -a --filter "label=hallow.owner=$TAG" --format '{{.Names}} {{.Image}} {{.Status}}'
}

test_run() {
	load_node_ip
	local netcheck=$repo/target/release/netcheck
	[[ -x $netcheck ]] || die "build netcheck first (cargo build --release -p netcheck)"

	log "the node joined: in the control plane and the panel"
	api GET /v1/nodes | jq -e --arg ip "$NODE_IP" '.[] | select(.name == "e2e-node-1" and .private_ip == $ip)' >/dev/null ||
		die "the node isn't in GET /v1/nodes"
	ok "GET /v1/nodes lists e2e-node-1 at $NODE_IP"
	panel_node_online

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

	panel_server_checks
	panel_instances
	log "all checks passed"
}

down() {
	log "removing everything named $NAME"
	if [[ -s $work/panel/pid ]]; then
		kill "$(cat "$work/panel/pid")" 2>/dev/null || true
		rm -f "$work/panel/pid"
	fi
	# Wings first, or it restarts the servers it sees die; then its servers
	# (instances carry the owner label; the panel server runs the console
	# image, and is on the e2e network once started), then the rest.
	docker rm -f "$NAME-node" >/dev/null 2>&1 || true
	local id
	{
		docker ps -aq --filter "label=hallow.owner=$TAG"
		docker ps -aq --filter "ancestor=$CONSOLE_IMAGE"
		docker ps -aq --filter "network=$NAME"
	} | sort -u | while read -r id; do docker rm -f "$id" >/dev/null; done
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
panel-server) mkdir -p "$work" && panel_server ;;
control-plane) mkdir -p "$work" && control_plane ;;
test) test_run ;;
down) down ;;
all)
	# A failed run stops here and is left up for a look.
	up
	test_run
	[[ ${HALLOW_E2E_KEEP:-0} == 1 ]] || down
	;;
*) die "usage: run.sh [up|test|down|all], or one step of up: build infra push panel control-plane node wings proxy panel-server" ;;
esac
