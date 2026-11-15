# hallow-fleet

The core of Hallow's fleet manager ([Fleet](../../docs/FLEET.md)): the provider trait, the scaling planner and dry run. The control plane will run the loop. That wiring, the audit log and the panel settings aren't built yet.

- **`provider`**: the `Provider` trait (`create`, `status`, `cancel`, `list`, `prices`, all async) that each crate in [`providers/`](../providers) implements, and its types: `NodeSpec`, `Node`, `Cost` (an amount per billing step, hourly or monthly, with an optional monthly cap), `Prices` and `ProviderError`.
- **`planner`**: `plan(&Snapshot, &Policy, &Memory) -> Plan`. A pure function: no I/O and no clock (the time is in the snapshot). `Memory` holds what must carry between ticks (how long pressure or surplus has held, the last buy and drain), so the control plane can keep it in Redis.
- **`dry_run`**: `DryRun<P>` wraps a provider and never calls it. Machines it "creates" live in memory, priced from a price list it is given, and every create and cancel is logged with `tracing` and kept in `DryRun::log`. `DryRun::into_inner` turns dry run off.
- **`money`**: `Money`, in millionths of the network's currency, so hourly prices like €0.0083 add up exactly.

```rust
let plan = hallow_fleet::plan(&snapshot, &policy, &memory);
for decision in &plan.decisions {
    tracing::info!("{decision}"); // e.g. "buy cpx41 in fsn1 at 0.0500/h: 34 free slots, target 40"
}
memory = plan.memory;
```

## Planner rules

| Rule | How |
|---|---|
| Scale up | Free slots on ready and setting-up nodes below the headroom target, or any queue past its wait target, held for `scale_up_after`. Reinstates draining nodes first, then buys enough of the cheapest offer per slot to cover the shortfall (one node for a queue). |
| Scale down | Spare capacity for `scale_down_after`: the cheapest node to lose (burst before base, then fewest players, then most expensive) whose slots aren't needed to keep the headroom, one per cooldown. The floor nodes are never picked. |
| Drain, then cancel | A draining node is cancelled once it is empty or past `drain_deadline` (its players are moved first). An empty hourly node is kept until `billing_step_margin` before its next billing step, since that hour is already paid, and is reinstated for free if load comes back. Cancelling a base node needs approval. |
| Budgets | The month-end forecast is the larger of spend so far plus base nodes for the rest of the month (and burst nodes until midnight), and spend so far extrapolated (after the first day). Headroom shrinks linearly from `tighten_from` of the budget to zero at it. A buy is refused if it would take today's projection past the daily cap or the forecast past the budget. Budgets only ever stop buying; they never drain or cancel. |
| Limits | `max_nodes` counts every node, the floor included. A buy waits out `scale_up_cooldown`; a drain waits out `scale_down_cooldown` after any buy or drain. |
| Hold | Whenever the planner wanted to act but didn't, it says why (sustain time, cooldown, daily cap, monthly budget, node limit, billing step), for the audit log and the queue's "network is busy". |

Days and months are UTC.

## Tests

```sh
cargo test -p hallow-fleet
cargo test -p hallow-fleet --test simulated_peak -- --nocapture  # print the simulated day
```

- `src/planner_tests.rs`: one or more tests per rule.
- `tests/simulated_peak.rs`: a scripted day fed through the planner one minute at a time, with decisions carried out against a dry-run provider whose inner provider panics if called. It checks:
  - the buys at the evening rise, and none for a two-minute spike at noon;
  - nobody queues at the peak;
  - after the peak, one drain per cooldown, then each node cancelled empty just before its next billing hour;
  - the day stays inside its budget.

  A second run with a low daily cap checks that buying stops at the cap, players queue, and nothing is drained or cut while the cap holds.
