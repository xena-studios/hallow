# Fleet

Hallow buys, sets up and returns machines on its own as player load changes, from a single machine when nobody is online up to as many as the budget allows. The fleet manager in the control plane talks to provider APIs (Hetzner and OVHcloud first) to add a node when capacity runs low, installs Wings on it, and drains and cancels it when it is no longer needed.

```mermaid
flowchart LR
    metrics["Load signals<br/>free slots · queue wait · CPU"] --> fm["Fleet manager<br/>(control plane)"]
    fm -- "create / cancel" --> prov["Provider APIs<br/>Hetzner Cloud · Hetzner Robot<br/>OVHcloud Public Cloud · OVHcloud dedicated"]
    prov --> node["New machine"]
    node -- "cloud-init: Docker, Wings,<br/>private network, firewall" --> ready["Node ready"]
    ready -- "registers" --> panel["Hallow Panel"]
    fm -- "drain, then cancel" --> node
```

## From one machine up

With nobody online, the whole network runs on one machine: the **floor node**. Everything else is added as players arrive and removed when they leave.

| Players online | Machines | What runs |
|---|---|---|
| 0 | 1, the floor node | Load balancer, control plane, Redis, PostgreSQL, panel, one Vine, one lobby. No game instances. |
| A few | 1 | Game instances start on the floor node as players queue. |
| The floor node fills up | 1 + burst nodes | New game instances, then extra lobbies and proxies, go to burst nodes. |
| Peak | Floor + base + burst | Proxies, lobbies and game instances spread over every node. |

Every layer scales on its own signal, down to its minimum:

| Layer | Scales on | Minimum |
|---|---|---|
| Game instances | Players queued, warm-pool target per game | 0 |
| Lobbies | Players per lobby | 1 |
| Proxies | Players and CPU per proxy | 1 |
| Nodes | Free slots left after the layers above | 1, the floor node |

When the last player leaves, games end, warm pools shrink to zero after an idle timeout, extra lobbies and proxies stop, and burst nodes drain and are cancelled. The floor node never stops, so the first player always gets in straight away. The first game after a quiet spell is a cold start on the floor node; Phase 0 measures how long that takes, and a warm pool of one for the most popular game can be kept when the budget allows.

The floor is configurable. One node is the cheapest. Two or more put the platform services (PostgreSQL, Redis, control plane) on separate machines so losing one doesn't take the network down; that's the setting for after launch.

## Budgets

Budgets stop growth; they never cut a node that has players on it.

- **Limits:** a monthly budget and a daily cap for the whole network, optional per-region caps, and a maximum node count. All are set in the panel; every change is in the audit log.
- **Forecast:** the fleet manager knows the running cost of every node and forecasts month-end spend. As the forecast nears the budget it keeps less spare capacity, then stops buying.
- **At the cap:** no new nodes. The matchmaker packs games tighter, queues get longer, players in a queue see that the network is busy, and staff are alerted. Staff can raise the cap from the panel.
- **The floor node** always counts against the budget and is never cut.

## Two kinds of capacity

| Kind | Providers | Billing | Ready in | Used for |
|---|---|---|---|---|
| **Base** | Hetzner Robot dedicated, OVHcloud dedicated | Monthly | Hours to days | The floor node and the load we always have. Cheapest per core. |
| **Burst** | Hetzner Cloud, OVHcloud Public Cloud | Hourly | Minutes | Peaks: evenings, weekends, events, launches. Returned when the peak ends. |

Burst nodes are added and removed automatically. Base nodes are ordered automatically only when burst capacity has stayed high for a configured time, and only after a staff member approves the order in the panel, because a monthly order can't be undone in an hour.

Provider billing details and delivery times change, so they live in each provider's config, not in code, and Phase 0 checks them.

## Scaling

**Scale up** when either holds for a few minutes:

- free game slots across all nodes, after warm instance pools, drop below the target headroom
- queue wait for any game passes its target

**Scale down** when free capacity stays above the headroom for a while:

1. Pick the node that is cheapest to lose: burst before base, then the fewest players.
2. Mark it draining: the matchmaker stops placing games there.
3. Wait for its games to end, or until a deadline, then move what is left.
4. Delete its servers through Wings, remove it from the panel, cancel it with the provider. A burst node is kept until just before its next billing step when that is cheaper than buying again soon.

Every threshold, wait time and cooldown is configured per region in the panel.

## Setting up a new node

1. The fleet manager creates the machine with a cloud-init script (cloud) or an install image and script (dedicated).
2. The script installs Docker and Calagopus Wings, joins the private network (WireGuard across providers; Hetzner vSwitch or OVHcloud vRack inside one), and locks the firewall to game ports and the private network.
3. Wings starts with a one-time join token, and the node registers itself in the panel.
4. The node takes server templates and starts getting games.

The same steps set up a machine staff add by hand.

## Safety

- **Budgets:** see [Budgets](#budgets). The fleet manager refuses to buy past them and alerts.
- **Approval** in the panel for every monthly order and every cancellation of a base node.
- **Cooldowns** between scale actions so a short spike can't loop buy and cancel.
- **Dry run:** log what the fleet manager would do without calling providers. The default until each provider is trusted.
- **Credentials** for provider APIs live in the secrets store, never in config or the panel database. Each provider gets an API key limited to what the fleet manager needs.
- **Audit log:** every order, setup, drain and cancellation shows in the panel with its reason and cost.

## Providers

The fleet manager talks to providers through one Rust trait (`create`, `status`, `cancel`, `list`, `prices`), so a provider is one crate in `crates/providers/`.

| Provider | API | First |
|---|---|---|
| Hetzner Cloud | Hetzner Cloud API | Phase 2 |
| Hetzner Robot (dedicated) | Hetzner Robot API | Phase 3 |
| OVHcloud Public Cloud | OVHcloud API / OpenStack | Phase 3 |
| OVHcloud dedicated | OVHcloud API (order flow) | Phase 3 |

More providers can follow the same trait.

## In the panel

A Fleet page shows every node with its provider, region, kind, cost so far this month, players and state (setting up, ready, draining, cancelled). Staff can add, drain or cancel a node, approve orders, set scaling targets and caps, and switch dry run on or off.
