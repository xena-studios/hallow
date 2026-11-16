# Instances

The control plane runs every lobby and game instance itself ([Architecture](ARCHITECTURE.md#who-owns-what)): it keeps warm instances of each server kind, starts more when a queue needs them, and deletes a game's instance when the game is over. On real nodes it does this through Calagopus Wings; on the dev network, through the local Docker Engine. The panel never holds a record for an instance.

## Wings without a panel record per instance

Wings can't create a server from a configuration sent with the request. `POST /api/servers` takes only `{uuid, start_on_completion, skip_scripts}`; Wings then fetches the server's configuration from its **remote** (normally the panel) at `GET /api/remote/servers/{uuid}`, and fails the create if that fails (`upstream/wings/application/src/routes/api/servers/mod.rs`). It also asks the remote:

- again before every start (`server/mod.rs`; a failure falls back to the cached configuration, after about 14 s of retries);
- at boot, for the list of every server it should run (`GET /api/remote/servers?page=`). Servers the remote doesn't list aren't loaded, and their API routes answer `404`.

So the panel, or something that answers like it, has to know every server. There were two ways to do that:

1. **The control plane drives Wings directly** and answers Wings' remote calls for its own instances.
2. **The control plane creates short-lived servers through the panel's API**, so Wings finds them there like any other.

**Chosen: 1.** Each node's Wings uses the control plane as its remote. The control plane:

- answers for the instances it created, from their records in Redis:
  - `GET /api/remote/servers/{uuid}`: the instance's configuration (image, environment, port, memory);
  - the boot list: the panel's pages, with the node's instances added to the last page;
  - `GET`/`POST /api/remote/servers/{uuid}/install`: nothing to install;
- passes every other remote call through to the panel unchanged, with Wings' own credentials: the panel's own servers, SFTP logins, activity, backups, `servers/reset`. Nodes keep working with the panel exactly as before.

The code is `crates/control-plane/src/remote.rs`.

Why not 2: it is what [Decisions](DECISIONS.md) 10 rules out. Every game would be a panel row created and deleted within minutes, at hundreds at a time. That floods the panel, its database and its audit log, and puts the panel's API on the matchmaker's critical path for every match. Option 1 costs one passthrough route and three small answers, and the panel stays out of the hot path.

How it fits together:

- **Enrolling.** Nodes are enrolled with the control plane as their remote. The panel's enrollment code carries a `remote` URL that Wings is told to use (`POST /api/admin/nodes/{node}/enrollment {remote}`), and the node join already sends one. Setting `HALLOW_NODE_WINGS_REMOTE` makes it the control plane's URL instead of the panel's.
- **Calling Wings.** The control plane calls each node's Wings API (`create`, `power`, `delete`, `GET /api/servers`) with the node's token. It reads the token from the panel (`GET /api/admin/nodes/{node}/token`, admin permission `nodes.read-token`) and caches it. The token also authenticates Wings' remote calls: `Bearer <token_id>.<token>` must match a joined node before the control plane answers for an instance, and that instance must be on that node.
- **The container.** Wings runs the image's own `ENTRYPOINT` (the configuration's `entrypoint` is unset) and makes the rest of the filesystem read-only apart from the server's volume at `/home/container`. It binds the node's game port to the same port in the container. So instances get `HALLOW_SERVER_DIR=/home/container` (the base image copies `/server` there and runs from it) and `HALLOW_SERVER_PORT=<port>` (Pumpkin listens on it). Both default to the old behaviour, so compose and other uses are unchanged.
- **Ports.** Instances take the lowest free port in the node's game-port range (`HALLOW_NODE_GAME_PORTS`), tracked in the instance records. The panel's allocation records for that range stay as they are; panel-made servers should use other ports.

**What is verified, and what isn't.** `tools/wings-e2e/run.sh` runs this whole path against a real Wings (the pinned 1.2.10 release) on one machine: the real panel, a node joined with the real `install.sh` and `wings configure --enroll`, Wings fetching configurations from the control plane, pulling images from a registry and running instances as its own user on a read-only root, servers registering with their scoped tokens, a bot routed to a Wings instance through Vine, and deletion removing the container. The first run found three bugs, all fixed:

- Wings' boot call `POST /api/remote/servers/reset` got `405` from the control plane, because it matched the `GET servers/{uuid}` route, and Wings refused to start. Every method now reaches the handler, which passes anything that isn't a `GET` of its own instance to the panel (`tests/nodes.rs` checks it).
- Every Wings instance exited at once: the base image's `/server` was `0700` for the `hallow` user, and Wings runs containers as its own user, so the entrypoint's copy to `/home/container` failed. `/server` is now `0755`.
- With a node unreachable, the planner created a new lobby instance on every pass (twice a second) while failed ones waited out their retry time. A kind with a failed instance now waits for it to be cleaned up.

Still untested: the panel calling Wings (console, files) for a node joined this way, several nodes, x86_64, and a real machine's WireGuard and firewall ([tools/wings-e2e](../tools/wings-e2e/README.md#limits)).

## Kinds

A **kind** is a server repo the control plane may start: its live image and the parts of its `hallow.toml` that decide how it scales (role, `lobby`, `players`, `scaling`), plus optional extra environment and a memory limit. Kinds are kept in PostgreSQL (`hallow.server_kinds`), so the control plane needs its database to run instances. They can be registered two ways:

- **The API**, used by staff, the panel and deploys. Making a new image live is a `PUT` with the new `image`:
  - `PUT /v1/kinds/{name}` with a `KindSpec` (`hallow_proto::instance`), answered with `KindInfo`. An invalid spec gets a `400` whose `fields` names each problem.
  - `GET /v1/kinds` lists them; `GET` and `DELETE /v1/kinds/{name}` act on one.
- **A kinds file**, registered at every start (`HALLOW_KINDS_FILE`). The dev network uses one. Each `[[kind]]` names an image and the kind's `hallow.toml`, which the control plane reads with `hallow-manifest`, so the file can't drift from the repo:

  ```toml
  [[kind]]
  image = "hallow-test-game:${HALLOW_IMAGE_TAG:-dev}"
  manifest = "test-game/hallow.toml"   # relative to this file
  memory_mb = 1024
  ```

Running instances keep the image they started with. Deleting a kind starts no new instances of it and ends its unused ones; games already running finish.

## Lifecycle

Each instance is a record in Redis (`instances`, plus `instance-specs`: what it was created from, environment included, for Wings to fetch). `GET /v1/instances` lists them with their state and their server's player count. `DELETE /v1/instances/{name}` ends one (`202`).

| State | Meaning |
|---|---|
| `creating` | Decided on; the orchestrator is creating and starting it |
| `starting` | Running; waiting for its server to register (the server plugin registers about a second after Pumpkin is up) |
| `ready` | Its server registered under the instance's name and takes players |
| `ending` | Done: its game ended, it emptied, it isn't needed, or staff ended it. The matchmaker sends nobody to it. It is being stopped |
| `deleting` | Stopped; being deleted |
| `failed` | An orchestrator call failed (`reason` says why). It is cleaned up and retried after a few seconds |

The control plane decides every half second, under the matchmaker's Redis lock, just before each matchmaker pass. A pure planner (`crates/control-plane/src/instances/plan.rs`, unit tested like the fleet planner) reads the kinds, the instance records, the server list, the queues and the online count, and decides:

- **Queue demand, game kinds.** It places each queue entry the way the matchmaker would: the fullest ready server with room for the whole entry, then instances still on their way up. Entries that still don't fit need new instances, packed `players.max` to an instance.
- **Warm instances.** It keeps `scaling.warm` unused instances of each game kind while anyone is online. Unused instances beyond that are ended after `HALLOW_INSTANCE_IDLE_SECS`.
- **Lobbies and hubs.** At least `warm` instances of each lobby or hub kind, and enough that their players fill them to no more than 80 % on average, so a busy lobby gets another instance before it is full. Empty ones beyond that are ended after `HALLOW_INSTANCE_IDLE_SECS`. Players are spread over them by `POST /v1/lobbies/send`, which picks the least full ([Servers](SERVERS.md#hubs-and-lobbies)).
- **Limits.** Nothing goes past `scaling.max_instances`.
- **A game is over** when its server reports `draining` with nobody on it, when it has been empty for `HALLOW_INSTANCE_EMPTY_SECS` after having had players, or when its server unregistered.
- **Clean-up.** An instance whose server hasn't registered within `HALLOW_INSTANCE_START_TIMEOUT_SECS` is ended. So is one stuck in a task-owned state, because the control plane running the task died.
- **Placement.** A new instance goes on the node with the fewest instances, on its lowest free game port. Its name is `<kind>-<6 hex>`. Draining nodes come with the fleet manager.

The quick decisions are written to Redis under the lock: new records, `ready`, bookkeeping, `ending`. Because the matchmaker pass that follows skips instances that are `ending`, nobody is sent to an instance that's going away, and a placement it makes is counted by the next plan. Before an instance is stopped, anyone still on it is sent to its kind's `lobby` (or a hub), and the stop waits up to 5 s for them to leave; a game that ends normally has already sent its players on (the server plugin does that when the server turns `draining`). The slow calls (create, stop, delete) run on their own tasks outside the lock and move the records on with a compare-and-set on the state. A sweep every 30 s deletes containers the orchestrator runs for this network that have no record, such as ones left behind when Redis was reset.

Every instance gets the server plugin's settings in its environment:

- `HALLOW_SERVER_NAME` (the instance name);
- `HALLOW_CONTROL_PLANE` (`HALLOW_INSTANCE_CONTROL_PLANE`) and `HALLOW_CONTROL_PLANE_TOKEN`, the instance's own scoped token ([Credentials](#credentials));
- `HALLOW_SERVER_ADDRESS`, set by the orchestrator;
- the network's `HALLOW_INSTANCE_ENV`, such as the Vine forwarding secret;
- the kind's `env`.

## Credentials

An instance never gets the control plane's shared token (`HALLOW_TOKEN`), which can do anything: ban players, change kinds, end instances, issue node join tokens. Anyone who can inspect a container (`docker inspect`, the Wings API, a plugin with `sys.env`) can read its environment, so each instance gets a **scoped token** of its own instead (`crates/control-plane/src/auth.rs`):

- **Minted** when the instance is recorded: `hit_` and 32 random bytes in hex.
- **Stored hashed.** Redis keeps its SHA-256 with the instance's name, kind and creation time (`instance-tokens`, plus `instance-token-of` from the name back to the hash). The raw token is in the instance's spec (`instance-specs`), because that is the environment Wings fetches on every start ([above](#wings-without-a-panel-record-per-instance)); Redis is as trusted as the control plane itself.
- **Lives as long as the instance.** A token works only while its instance record exists. Deleting the record (the instance ended, or a stuck one was cleaned up) removes the token in the same Lua script, and every lookup also checks the record is still there.
- **Scoped to what the server plugin does** (audited from `crates/server-plugin`: register, heartbeat, player reports, stats, signals, unregister), for its own name only:

  | Route | Allowed when |
  |---|---|
  | `POST /v1/servers` | the body's `name` and `kind` are the instance's |
  | `POST /v1/servers/{name}/heartbeat` (state, players, roster) | `{name}` is the instance's |
  | `DELETE /v1/servers/{name}` | `{name}` is the instance's |
  | `POST`/`GET /v1/servers/{name}/players` | `{name}` is the instance's |
  | `POST /v1/stats` | the batch's `source` and `kind` are the instance's |
  | `POST /v1/signals` | the batch's `source` is the instance's |

  Everything else answers `403` (code `unauthorized`): other servers' routes, the server list, presence, commands, kinds, instances, punishments, ranks, fleet settings and node tokens. An unknown or revoked token answers `401`.

`tests/instances.rs` checks a scoped token against every one of those, and that it stops working once its instance is deleted.

**Servers the control plane doesn't start**, such as the dev network's compose hub (`lobby`), keep working with the shared token. To give one a scoped token instead, list it in `HALLOW_SERVER_TOKENS` as `name:kind:token` (comma-separated, tokens at least 16 characters) and pass that token as the server's `HALLOW_CONTROL_PLANE_TOKEN`. A static token has the same scope as an instance's and lives as long as the setting. The dev network still gives its hub the shared token.

Players queued for a kind that has no server yet are accepted when the kind is registered as a game; the queue then starts one. `players.min` isn't used yet: the matchmaker sends players as soon as there's room, and a game plugin decides when its match starts.

## Orchestrators

`crates/control-plane/src/orchestrator`: one trait (`nodes`, `create`, `start`, `stop`, `delete`, `status`, `list`) and three implementations, picked with `HALLOW_ORCHESTRATOR`:

| | Nodes | Address | Used for |
|---|---|---|---|
| `wings` | Every node that joined (`hallow.nodes`) | `<private IP>:<game port>` | Production |
| `docker` | One, `docker`, the Engine at `HALLOW_DOCKER_HOST` | `<instance name>:25565` on `HALLOW_DOCKER_NETWORK` | The dev network |
| mock | Given by the test | `<instance name>:25565` | Tests (`tests/instances.rs`) |

Docker containers are labelled `hallow.instance`, `hallow.kind` and `hallow.owner=<network>`. Two dev networks on one Docker host never touch each other's containers.

## Dev network

`deploy/compose/compose.instances.yaml`, over `compose.yaml` and `compose.hallow.yaml`, makes games dynamic:

- The control plane gets the Docker socket (dev only: that's root on the host), `HALLOW_ORCHESTRATOR=docker`, and a kinds file that registers `test-game` and `test-game-lobby` (one lobby instance always warm).
- `game-1` is built but not started.
- The hub (`lobby`) stays a static compose service: the network's floor, which compose keeps up.

See [Dev network](../deploy/compose/README.md#dynamic-games). The Phase 1 gate runs on it, with a bot joining through one of two proxies, queueing with `/play test-game` and landing in a fresh instance. So does the hub's path: hub, `/lobby test-game` to a lobby instance, queue, game, and back to the lobby when the match ends.

## Settings

| Variable | Default | |
|---|---|---|
| `HALLOW_ORCHESTRATOR` | `off` | `docker` or `wings` |
| `HALLOW_INSTANCE_CONTROL_PLANE` | required | `host:port` instances reach the control plane at |
| `HALLOW_INSTANCE_ENV` | none | `NAME=value,...` for every instance |
| `HALLOW_KINDS_FILE` | none | Kinds registered at start |
| `HALLOW_INSTANCE_START_TIMEOUT_SECS` | `120` | |
| `HALLOW_INSTANCE_EMPTY_SECS` | `30` | |
| `HALLOW_INSTANCE_IDLE_SECS` | `300` | |
| `HALLOW_DOCKER_HOST` | `unix:///var/run/docker.sock` | Docker only |
| `HALLOW_DOCKER_NETWORK` | required | Docker only |
| `HALLOW_DOCKER_OWNER` | the network | Docker only |
| `HALLOW_NODE_WINGS_REMOTE` | the panel's public URL | Wings only: the remote URL new nodes are enrolled with, the control plane's URL as nodes reach it |

## Still to do

- Run the Wings orchestrator on a real (cloud or dedicated) node; it has run end to end locally (`tools/wings-e2e`).
- Draining nodes: no new instances there (the fleet manager).
- `players.min` and match start, with `hallow-sdk`.
- Console and logs in the panel: the panel reads `GET /v1/instances` for each instance's node and Wings UUID, and talks to Wings with the node's token.
- Canary channels: a share of new instances on a second image.
