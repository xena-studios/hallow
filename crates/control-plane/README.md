# control-plane

Hallow's control plane: the registry of servers and proxies, player presence, and cross-proxy commands. It serves the HTTP/JSON API whose types and endpoint table are in [`hallow-proto`](../proto/src/routes.rs).

Live state is kept in Redis (Valkey works too), so any number of instances can run behind one address. Each write publishes a change event on a Redis channel; every instance listens and wakes the long polls it holds. A proxy polling one instance sees a change made through another within milliseconds. The keys are listed in [`src/store.rs`](src/store.rs).

## What it does

- **Servers** register when they accept players and heartbeat every 5 s. The list revision changes when a server is added or removed, or changes state, but not on player counts alone. Proxies long-poll the list and mirror it into Vine.
- **Proxies** register on start and heartbeat. Re-registering after a restart drops the players the proxy had.
- **Expiry.** Anything that misses three heartbeats (15 s) is removed. A removed proxy takes its players and queued commands with it.
- **Login.**
  - A proxy asks before a player reaches a server. The answer is either a hub (the emptiest ready lobby of `HALLOW_HUB_KIND`) or a refusal.
  - Refusals cover a full network, no free hub, or a duplicate login. With `HALLOW_DUPLICATE_LOGIN=kick_old`, a duplicate login kicks the old session instead.
  - Two proxies racing the same login can't both win.
- **Leaving.** A leave only counts from the proxy the player is on, so a late leave from an old session can't remove a newer login.
- **Draining.**
  - `POST /v1/proxies/{id}/drain` stops (or resumes) new players for a proxy.
  - The proxy refuses new logins as soon as it polls, and the request survives the proxy restarting.
  - `GET /v1/proxies` lists proxies with their player counts.
- **Load balancer agent checks.**
  - A plain TCP port (`HALLOW_AGENT_LISTEN`, default `0.0.0.0:8081`) answers HAProxy's `agent-check`.
  - HAProxy sends a proxy id; the agent replies `up ready`, `drain` (weight 0: no new players, existing ones stay) or `down`.
  - The port carries no token and changes nothing. Keep it on the private network.
- **Lookups.** Online players can be looked up by UUID, or by username ignoring case, for `/find` and `/msg`.
- **Commands.**
  - Send, message and kick go to the proxy holding the player. Broadcast goes to every proxy.
  - Each proxy long-polls its own command queue. The last 1000 commands are kept for polls that fall behind.

Queues and the matchmaker come later.

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
- duplicate logins, kicks, expiry and auth behave as described.

```sh
HALLOW_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test -p hallow-control-plane
```

Without the variable they skip, except in CI, which provides Valkey.
