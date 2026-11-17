# Multi-proxy

Hallow runs any number of Vine proxies as one network from the first release. Vine has no shared state between instances, so the Hallow Vine plugins and the control plane provide it, the same job RedisBungee does for BungeeCord and Velocity networks.

| Need | How Hallow does it |
|---|---|
| Spread players over proxies | A TCP load balancer (HAProxy, or a DDoS-protection provider) in front. Vine already reads PROXY protocol (`src/network/proxy_protocol.rs`), so real IPs survive. |
| Global player count and list | Each proxy reports joins and leaves to the control plane, kept in Redis. A Vine plugin answers pings with the network-wide count. |
| One login per account | On login, the presence plugin asks the control plane; a second login replaces the old session, which its proxy disconnects (`HALLOW_DUPLICATE_LOGIN=deny` refuses the new one instead). |
| The same server list everywhere | The control plane pushes registry changes to every proxy through the runtime registry added in our Vine fork. |
| Cross-proxy actions | Party warp, `/find`, private messages, staff broadcasts and bans go to the control plane, which tells the proxy holding the player to act (a command long poll per proxy over HTTP, Redis pub/sub between control plane instances). |
| Updating or losing a proxy | Draining: the proxy stops taking logins and the load balancer health check drops it. With `transfer_to` set, the plugin hands players on lobbies and in limbo to the load balancer with the Transfer packet (1.20.5+); players in a game go when they leave it, and older clients stay until they leave. On shutdown Vine transfers who it can (`routing.shutdown_transfer`) and asks the rest to reconnect. A dead proxy's players reconnect through the load balancer. |
| The control plane is down | Proxies keep their server list and players. New logins wait in Vine's limbo, unverified, and are sent on when it answers again ([Never dropped](ARCHITECTURE.md#never-dropped)). |
| Scaling | A proxy is a Wings server template, so the control plane can add proxies as load rises. |

## Bedrock

Bedrock is UDP. With the native route, Bedrock players skip Vine and the backends report presence directly. With Geyser, several Geyser instances sit behind a UDP load balancer and their players count like any other Java player.

## What we build

- Vine fork: `register-server` and `unregister-server` in `wit/proxy.wit`, a mutable server map, a drain mode, a built-in limbo, and a `server-kick` event so plugins pick where a dropped player goes.
- Vine plugins: presence, ping, duplicate login, cross-proxy commands.
- Control plane: proxy registry, presence store, command routing, health.
