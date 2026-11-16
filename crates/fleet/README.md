# hallow-fleet

The core of Hallow's fleet manager ([Fleet](../../docs/FLEET.md)): the provider trait, the scaling planner, dry run, and the planner's policy from the [fleet settings](../../docs/FLEET.md#settings). The settings are kept in PostgreSQL and served by the control plane (`GET`/`PUT /v1/fleet/settings`); the control plane will run the loop. That loop and the audit log of decisions aren't built yet.

- **`provider`**: the `Provider` trait (`create`, `status`, `cancel`, `list`, `prices`, all async) that each crate in [`providers/`](../providers) implements, and its types: `NodeSpec`, `Node`, `Cost` (an amount per billing step, hourly or monthly, with an optional monthly cap), `Prices` and `ProviderError`.
- **`planner`**: `plan(&Snapshot, &Policy, &Memory) -> Plan`. A pure function: no I/O and no clock (the time is in the snapshot). `Memory` holds what must carry between ticks (how long pressure or surplus has held, the last buy and drain), so the control plane can keep it in Redis.
- **`dry_run`**: `DryRun<P>` wraps a provider and never calls it. Machines it "creates" live in memory, priced from a price list it is given, and every create and cancel is logged with `tracing` and kept in `DryRun::log`. `DryRun::into_inner` turns dry run off.
- **`settings`**: `Policy::from_settings(&FleetSettings, floor_nodes, offers)` turns the settings the panel edits (`hallow_proto::fleet::FleetSettings`) into the planner's `Policy`, refusing settings that don't validate. `allowed_offers` keeps the offers of an enabled, listed provider in its allowed regions and machine types, and `floor_nodes` picks the oldest base nodes as the floor. `dry_run` and `paused` aren't planner inputs: the loop doesn't plan while paused, and carries out decisions through `DryRun` in dry run.
- **`bootstrap`**: `render_cloud_init(&NodeBootstrap)` fills [`deploy/node/cloud-init.yaml`](../../deploy/node/cloud-init.yaml) with a node's settings and [`install.sh`](../../deploy/node/install.sh), both compiled in. The result goes into `NodeSpec::user_data`. It checks every value against what the script accepts and refuses anything over 32 KiB. See [deploy/node](../../deploy/node/README.md).
- **`money`**: `Money`, in millionths of the network's currency, so hourly prices like €0.0083 add up exactly.

```rust
let offers = hallow_fleet::allowed_offers(&settings, "hetzner-cloud", offers);
let floor = hallow_fleet::floor_nodes(settings.floor_nodes, &snapshot.nodes);
let policy = Policy::from_settings(&settings, floor, offers)?;
let plan = hallow_fleet::plan(&snapshot, &policy, &memory);
for decision in &plan.decisions {
    tracing::info!("{decision}"); // e.g. "buy cpx41 in fsn1 at 0.0500/h: 34 free slots, target 40"
}
memory = plan.memory;
```

## Planner rules

Every name in backticks is a setting (its `_secs` form in the settings); the defaults are in [Fleet](../../docs/FLEET.md#settings).

| Rule | How |
|---|---|
| Scale up | Free slots on ready and setting-up nodes below the headroom target (`headroom_slots`), or any queue past `queue_wait_target`, held for `scale_up_after`. Reinstates draining nodes first, then buys enough of the cheapest offer per slot to cover the shortfall (one node for a queue). |
| Scale down | Spare capacity for `scale_down_after`: the cheapest node to lose (burst before base, then fewest players, then most expensive) whose slots aren't needed to keep the headroom, one per cooldown. Burst before base only with `prefer_burst_on_scale_down`. The `floor_nodes` are never picked. |
| Drain, then cancel | A draining node is cancelled once it is empty or past `drain_deadline` (its players are moved first). An empty hourly node is kept until `billing_step_margin` before its next billing step, since that hour is already paid, and is reinstated for free if load comes back. Cancelling a base node needs approval while `base_drain_needs_approval` is on. |
| Budgets | The month-end forecast is the larger of spend so far plus base nodes for the rest of the month (and burst nodes until midnight), and spend so far extrapolated (after the first day). Headroom shrinks linearly from `tighten_from` of the budget to zero at it. A buy is refused if it would take today's projection past `daily_cap` or the forecast past `monthly_budget`; either can be unset for no limit. Budgets only ever stop buying; they never drain or cancel. |
| Limits | `max_nodes` counts every node, the floor included. A buy waits out `scale_up_cooldown`; a drain waits out `scale_down_cooldown` after any buy or drain. |
| Hold | Whenever the planner wanted to act but didn't, it says why (sustain time, cooldown, daily cap, monthly budget, node limit, billing step), for the audit log and the queue's "network is busy". |

Days and months start at midnight in the `timezone` setting (an IANA zone, `UTC` by default), clock changes included, so "today" for the daily cap and "this month" for the budget match the bills the owner reads.

## Tests

```sh
cargo test -p hallow-fleet
cargo test -p hallow-fleet --test simulated_peak -- --nocapture  # print the simulated day
cargo test -p hallow-fleet --test bootstrap_dry_run              # rendered cloud-init drives install.sh --dry-run
```

- `src/planner_tests.rs`: one or more tests per rule, and day and month boundaries in a time zone other than UTC.
- `src/settings.rs`: settings to policy, and the offer filters.
- `tests/simulated_peak.rs`: a scripted day fed through the planner one minute at a time, with decisions carried out against a dry-run provider whose inner provider panics if called. It checks:
  - the buys at the evening rise, and none for a two-minute spike at noon;
  - nobody queues at the peak;
  - after the peak, one drain per cooldown, then each node cancelled empty just before its next billing hour;
  - the day stays inside its budget.

  A second run with a low daily cap checks that buying stops at the cap, players queue, and nothing is drained or cut while the cap holds.
