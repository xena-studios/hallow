# vine-plugin

`hallow-vine`, Hallow's Vine plugin (`hallow_vine.wasm`). It links each proxy to the [control plane](../control-plane/README.md), so several proxies run as one network.

- **Presence.** Every login asks the control plane first.
  - It refuses a second login for an account already online on another proxy, and refuses logins when the network is full.
  - Otherwise it allows the login and names the hub the player lands on.
  - Switches and leaves are reported back.
- **Global ping.** Server-list pings show the network's player count and limit.
- **One server list.** Servers registered with the control plane are mirrored into Vine's runtime registry.
  - Draining servers are left out, so nobody new is sent there.
  - Players already on them aren't affected.
- **Commands.** Sends, messages, broadcasts and kicks queued for this proxy are carried out.
- **Player commands** that work across proxies. The full table is in [`src/commands.rs`](src/commands.rs).
  - Everyone: `/msg` and `/r`, `/party` (`/p`), `/pc` for party chat, `/play <game>` and `/leavequeue`.
  - Staff: `/find`, `/alert` and `/gkick`, for players whose ranks grant `hallow.staff.find`, `.alert` or `.kick` (or a wildcard). The control plane sends each player's nodes with their login; the console may run everything.
- **Friends** (needs the control plane's database):
  - `/friend` (`/f`): add, accept, deny, remove, list and requests; `/friend <player>` adds.
  - The list shows who's online and on which server.
  - Friends are told when each other join or leave the network.
- **Moderation** (needs the control plane's database):
  - `/ban` and `/mute`, with an optional duration (`30m`, `2h`, `7d`, `1w`); `/unban` and `/unmute`; `/warn`; `/history`.
  - Permissions: `hallow.staff.ban`, `.mute`, `.warn` and `.history`.
  - A ban kicks the player wherever they are and refuses their logins.
  - A muted player's chat, `/msg`, `/r` and party chat are blocked until the mute ends.
- **Anticheat signals.** Every 30 s, one sample per player goes to the control plane (`POST /v1/signals`): chat and command counts and peak rates, plugin messages, server switches and time online. The list is in [`src/signals.rs`](src/signals.rs). Measurement only; nothing acts on them yet.
- **Recovery.**
  - If the control plane forgets the proxy, for example after it expired or Redis was reset, the plugin registers again and re-reports its players.
  - Admitted players who never became live sessions are reported gone after 10 s.

While the plugin isn't registered, logins are refused, since any of them could be a duplicate.

## How it runs

Vine plugin calls are synchronous: while one runs, the plugin handles nothing else. So the plugin never holds a request open.

- A repeating task, using the scheduler from our Vine fork, polls the control plane every `poll_ms` (default 250 ms) with `?wait=0`.
- It sends a heartbeat every 5 s.
- Every HTTP call has a 2 s timeout.

## Setup

```sh
cargo build --release --target wasm32-wasip2 -p hallow-vine
cp target/wasm32-wasip2/release/hallow_vine.wasm <vine>/plugins/
```

In `vine.toml`:

```toml
[plugins.permissions]
hallow_vine = ["http.outbound", "fs.read.data"]
```

Settings go in `plugins/data/hallow_vine/config.toml`. The format is at the top of [`src/config.rs`](src/config.rs). Secrets can sit in files next to it.
