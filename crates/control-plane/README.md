# control-plane

Hallow's control plane: the registry of servers and proxies, player presence, and cross-proxy commands. It serves the HTTP/JSON API whose types and endpoint table are in [`hallow-proto`](../proto/src/routes.rs).

Live state is kept in Redis (Valkey works too), so any number of instances can run behind one address. Each write publishes a change event on a Redis channel; every instance listens and wakes the long polls it holds. A proxy polling one instance sees a change made through another within milliseconds. The keys are listed in [`src/store.rs`](src/store.rs).

## What it does

- **Callers** send `Authorization: Bearer <token>`: the shared token (`HALLOW_TOKEN`) can do anything; a server's scoped token (one per instance, or `HALLOW_SERVER_TOKENS`) can only register, heartbeat, report players, stats and signals, and unregister its own server ([Instances](../../docs/INSTANCES.md#credentials)).
- **Servers** register when they accept players and heartbeat every 5 s. The list revision changes when a server is added or removed, or changes state, but not on player counts alone. Proxies long-poll the list and mirror it into Vine.
- **Players per server.** Each server reports who is on it, independently of the proxies:
  - `POST /v1/servers/{name}/players` with who joined and left, as it happens;
  - every heartbeat carries the whole list (`roster`), which replaces the stored one, so a lost report is corrected within 5 s;
  - a player reported joining one server comes off any other server's list, and a late leave from the old server doesn't touch the new one;
  - a server that registers again (restarted), unregisters or expires takes its list with it;
  - `GET /v1/servers/{name}/players` lists them by username, for the panel.
- **Proxies** register on start and heartbeat. Re-registering after a restart drops the players the proxy had.
- **Expiry.** Anything that misses three heartbeats (15 s) is removed. A removed proxy takes its players and queued commands with it.
- **Login.** A proxy asks before a player reaches a server, and the answer is always somewhere to go ([Never dropped](../../docs/ARCHITECTURE.md#never-dropped)):
  - a hub: the emptiest ready lobby of `HALLOW_HUB_KIND`;
  - for a banned player, the emptiest banned lobby (`HALLOW_BANNED_KIND`, default `banned`), or limbo while none is up, with the ban (`ban`);
  - with the network full (`HALLOW_MAX_PLAYERS`) or no hub free, `wait`: limbo, with the reason and their place in line.
  - A duplicate login replaces the old session, whose proxy disconnects it. `HALLOW_DUPLICATE_LOGIN=deny` refuses the new one instead; a login from a draining proxy always replaces.
  - Two proxies racing the same login can't both win; the loser is the only other refusal.
- **Limbo** ([`src/limbo.rs`](src/limbo.rs)): limbo is each proxy's built-in server (`HALLOW_LIMBO_SERVER`, default `limbo`).
  - Players waiting there are kept oldest first. Every matchmaker pass (half a second, one instance at a time) sends each one on as soon as there's room: a hub, or a banned lobby for a banned player. Those still waiting are told their place every 30 s.
  - The instance planner counts waiting players towards the kind they wait for, so a hub or banned lobby starts for them.
  - `POST /v1/presence/fallback`: a proxy whose server kicked a player, crashed or stopped asks where they go. The answer is the game's lobby, another lobby of the same kind, a hub, or limbo (banned players: a banned lobby or limbo), with a message.
  - An ending instance with nowhere to send its players sends them to limbo.
  - `GET /metrics` shows `hallow_limbo_waiting` and `hallow_players_banned_online`.
- **Leaving.** A leave only counts from the proxy the player is on, so a late leave from an old session can't remove a newer login.
- **Draining.**
  - `POST /v1/proxies/{id}/drain` stops (or resumes) new players for a proxy.
  - The proxy refuses new logins as soon as it polls, and the request survives the proxy restarting.
  - `GET /v1/proxies` lists proxies with their player counts.
- **Load balancer agent checks.**
  - A plain TCP port (`HALLOW_AGENT_LISTEN`, default `0.0.0.0:8081`) answers HAProxy's `agent-check`.
  - HAProxy sends a proxy id; the agent replies `up ready`, `drain` (weight 0: no new players, existing ones stay) or `down`.
  - The port carries no token and changes nothing. Keep it on the private network.
- **Accounts, bans, ranks and sessions** (PostgreSQL through [`hallow-db`](../db/README.md), when `HALLOW_DATABASE_URL` is set):
  - a login upserts the account; an active ban sends the player to the banned lobby with no permissions (see Punishments);
  - an allowed login gets the player's permission nodes and an open session, which records the servers they visit and closes at logout (or when their proxy dies);
  - `HALLOW_ADMINS` gives the listed usernames the `admin` rank (`*`) at login, to bootstrap a network.

  With a database configured, logins fail closed when it's down: the proxy holds the player in limbo and asks again. Without one, these are all off and nobody has staff permissions.
- **Friends** (with the database):
  - `POST /v1/friends` takes one action (add, accept, deny, remove, list or requests), with notices to the other player on their proxy;
  - friends are told when each other join or leave the network.
- **Punishments** (with the database):
  - `POST /v1/punishments` issues a ban, mute, kick or warning by username (online or not; a kick only while online) and enforces it on the player's proxy. The issuer is an in-game staff player (`issuer`), a panel staff member (`staff: {name, panel_user}`) or the console; all three take the same path, so a ban from the panel acts on every proxy just like `/ban`;
  - a ban locks an online player in (`restricted` in Redis, `Restrict` to their proxy) and moves them to a banned lobby or limbo; `/lobby`, `/hub`, queues, party actions and sends are refused for them (`409`). When it ends (checked every matchmaker pass) or is lifted, they're unlocked and sent to a hub;
  - a kick never disconnects: the player waits in limbo for `HALLOW_KICK_HOLD_SECS` (10) with the reason as a title, then goes to a hub. `POST /v1/commands` with a `kick` does the same;
  - `GET /v1/bans/{uuid}` returns a player's active ban as the banned lobby shows it: the issuer as the panel names them, the end, the id and the appeal (`HALLOW_APPEAL_TEXT`, `HALLOW_APPEAL_URL`). A server's scoped token may ask only about its own players;
  - `POST /v1/punishments/revoke` lifts a ban or mute, with an optional reason;
  - `GET /v1/punishments?name=` lists a player's last 10;
  - `GET /v1/punishments/search?player=&kind=&active=&limit=&offset=` pages through every punishment, newest first, with the target, issuer and revocation (for the panel);
  - `GET`/`POST /v1/punishments/templates` and `PUT`/`DELETE /v1/punishments/templates/{id}` manage reason templates (name, kind, length, reason);
  - an active mute is sent with the login and pushed whenever it changes.
- **Ranks** (with the database), edited from the panel:
  - `GET /v1/ranks` lists ranks with their member counts; `PUT /v1/ranks/{name}` creates or replaces one (bad nodes get a `400` with `fields`); `DELETE /v1/ranks/{name}` deletes one, except the default;
  - `GET /v1/rank-grants?name=`, `POST /v1/rank-grants` and `POST /v1/rank-grants/revoke` show, give and take a player's ranks. Nodes reach the proxy with the login, so a change applies at the player's next join.
- **Anticheat signals** (with the database):
  - proxies and servers `POST /v1/signals` batches of per-player samples, each a window of named measurements, from 30 s of play;
  - they're kept in PostgreSQL for `HALLOW_SIGNAL_RETENTION_DAYS` (14);
  - `GET /v1/signals?name=` returns a player's last 200 for review (docs/ANTICHEAT.md, phase 1).
- **Fleet settings** (with the database): everything the fleet manager decides by ([Fleet](../../docs/FLEET.md#settings)), edited from the panel:
  - `GET /v1/fleet/settings` returns the settings in force with who changed them last and when, or the defaults if nobody has;
  - `PUT /v1/fleet/settings` with `{settings, updated_by}` replaces them; invalid settings get a `400` whose `fields` names each bad field;
  - every change goes in an append-only history, `GET /v1/fleet/settings/history?limit=` (newest first, 50 by default, at most 500).
- **Node joins** (with the database and the panel, `HALLOW_PANEL_*`; see [deploy/node](../../deploy/node/README.md)):
  - `POST /v1/nodes/join-tokens` issues a one-time token for a node's role, region and labels; only its SHA-256 is kept;
  - `POST /v1/nodes/join`, with the token as the bearer, creates the node and its game-port allocations in the panel and returns a Wings enrollment code; the token is claimed atomically and spent only when the join finishes, so a failed join can be retried;
  - `GET /v1/nodes` lists joined nodes.
- **Stats** (with the database):
  - servers `POST /v1/stats` batches of per-player increases for their kind (`play_time_s`, `joins`, and whatever a game counts, such as `kills`, `deaths` or `wins`);
  - they're added to running totals in `hallow.player_stats` in one upsert; increases for players who never joined are skipped;
  - stat names are 1 to 64 of `a-z 0-9 _ .`, increases can't be negative, and a batch carries at most 1000 players with 64 stats each; a batch breaking a rule is refused whole (`400`);
  - `GET /v1/stats/{player}` (UUID or username) returns every total, by kind then stat.
- **Metrics.** `GET /metrics` (no token) serves Prometheus' text format, read from live state:
  - players online and the limit;
  - players per proxy;
  - per-server players, limit and tick time;
  - queue sizes.
- **Lookups.** Online players can be looked up by UUID, or by username ignoring case, for `/find` and `/msg`.
- **Commands.**
  - Send, message and kick go to the proxy holding the player. Broadcast goes to every proxy.
  - Each proxy long-polls its own command queue. The last 1000 commands are kept for polls that fall behind.

- **Parties.**
  - `POST /v1/parties` takes one action (invite, accept, leave, kick, disband, warp, chat or list) and answers with a message for the player.
  - Other members are told through their proxies.
  - Parties live in Redis while their members are online:
    - leaving the network leaves the party;
    - a leaving leader hands over to the longest member;
    - a party ends when one member is left.
- **Queues and the matchmaker.**
  - `POST /v1/queues` queues a player, or the whole party if they lead one.
  - Every half second, one instance (under a Redis lock) puts each entry, oldest first, on the fullest ready game server of its kind with room for all of it.
  - A member going offline takes the party out of the queue.
  - Before each pass, under the same lock, the instance lifecycle starts instances when a queue needs them and keeps warm ones (see below).
- **Hubs and lobbies** ([Servers](../../docs/SERVERS.md#hubs-and-lobbies)):
  - `GET /v1/lobbies` lists the game modes (lobby kinds) with each one's players, in its lobbies and its games, and ready lobby instances, plus the network's player count, for the hub's game menu;
  - `POST /v1/lobbies/send` moves a player to the least full ready instance of a mode's lobby (by mode name, lobby kind, or a game kind's `lobby`), or to a hub; `/lobby`, `/hub`, the game menu and a finished game all use it. With `fallback_hub`, an unknown mode or a full one sends the player to a hub instead of refusing;
  - logins pick the emptiest ready hub, skipping instances that are ending.
- **Instances** ([Instances](../../docs/INSTANCES.md)), with `HALLOW_ORCHESTRATOR=docker` or `wings` and the database:
  - `PUT /v1/kinds/{name}` registers a server kind (live image, role, players, scaling); `GET /v1/kinds` lists them, `DELETE` removes one. A kinds file (`HALLOW_KINDS_FILE`) registers kinds at start from their `hallow.toml`;
  - `GET /v1/instances` lists every instance with its lifecycle state (`creating`, `starting`, `ready`, `ending`, `deleting`, `failed`); `DELETE /v1/instances/{name}` ends one;
  - a pure planner keeps `warm` unused instances while players are online, starts instances for queue entries that fit nowhere (up to `max_instances`), and deletes a game once it reports `draining` empty or has been empty for `HALLOW_INSTANCE_EMPTY_SECS` after having players. Lobby and hub kinds keep at least `warm` instances and add one when their players fill them past 80 % on average;
  - an instance that ends with players on it (staff ended it, a timeout) first sends them to its kind's `lobby` (or a hub) and waits up to 5 s for them to leave;
  - instances run through an orchestrator: Wings on joined nodes (the control plane answers Wings' panel remote API for its instances and passes the rest through to the panel), the local Docker Engine on the dev network, or a mock in tests.

## Running

```sh
docker run -d -p 127.0.0.1:6379:6379 valkey/valkey:9.1-alpine
HALLOW_TOKEN=$(openssl rand -hex 24) cargo run -p hallow-control-plane
```

Settings are environment variables; the table is at the top of [`src/main.rs`](src/main.rs). `GET /healthz` answers `204` without a token while Redis is reachable.

## Tests

The tests in `tests/network.rs` start two instances against one Redis and drive them as simulated proxies:

- both proxies see the same server list and player count;
- changes reach a waiting long poll on the other instance;
- duplicate logins, kicks, expiry and auth behave as described;
- nobody is turned away: players wait in limbo with no hub or a full network and are sent on when there's room; fallbacks pick a lobby, a hub or limbo; kicks hold players in limbo before a hub; bans move players to the banned lobby, lock them in, and release them when lifted or over.

```sh
HALLOW_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test -p hallow-control-plane
```

`tests/nodes.rs` also needs `HALLOW_TEST_DATABASE_URL`. It runs node joins against a mock of the panel's admin API. So does `tests/instances.rs`, which drives the instance lifecycle with the mock orchestrator and plays each instance's server: a queue starts a fresh instance, the player is sent there, the emptied game is deleted; warm pools, `max_instances`, failed creates and staff endings. `tests/lobbies.rs` does the same for hubs and lobbies: a warm lobby instance, the mode list, sending players by mode name and back to the hub, refusals, and an ending game sending its players to its lobby before it stops.

Without the variable they skip, except in CI, which provides Valkey.
