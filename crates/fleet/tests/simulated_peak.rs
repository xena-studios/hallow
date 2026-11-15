//! Issue #41's goal: in dry run, a simulated peak logs the right buys and
//! cancels. A scripted day of player load goes through the planner one
//! minute at a time; decisions are carried out against a dry-run provider
//! (wrapping one that panics if called), players are placed on nodes, and
//! billing is charged the way an hourly provider would.
//!
//! Run with `--nocapture` to see the day's decisions.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeDelta, TimeZone, Utc};
use hallow_fleet::{
    Cost, Decision, DryRun, DryRunAction, Memory, Money, Node, NodeId, NodeKind, NodeLoad,
    NodePhase, NodeSpec, Offer, Policy, Price, Prices, Provider, ProviderError, QueueWait, Reason,
    Snapshot, Spend, plan,
};

thread_local! {
    // Simulated time for the dry-run provider's created-at stamps. Each
    // #[tokio::test] runs on its own single thread.
    static NOW: Cell<i64> = const { Cell::new(0) };
}

fn sim_clock() -> DateTime<Utc> {
    DateTime::from_timestamp(NOW.with(Cell::get), 0).unwrap()
}

/// Stands in for Hetzner: any call means dry run leaked through.
struct Unreachable;

impl Provider for Unreachable {
    fn name(&self) -> &str {
        "hetzner-cloud"
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Burst
    }
    async fn create(&self, _: &NodeSpec) -> Result<Node, ProviderError> {
        panic!("dry run called the real provider")
    }
    async fn status(&self, _: &NodeId) -> Result<Node, ProviderError> {
        panic!("dry run called the real provider")
    }
    async fn cancel(&self, _: &NodeId) -> Result<(), ProviderError> {
        panic!("dry run called the real provider")
    }
    async fn list(&self) -> Result<Vec<Node>, ProviderError> {
        panic!("dry run called the real provider")
    }
    async fn prices(&self) -> Result<Prices, ProviderError> {
        panic!("dry run called the real provider")
    }
}

const MIN: Duration = Duration::from_secs(60);
const SETUP: TimeDelta = TimeDelta::minutes(3);
const NODE_SLOTS: u32 = 100;

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 15, h, m, 0).unwrap()
}

fn burst_cost() -> Cost {
    Cost::hourly(Money::from_micros(50_000), Some(Money::from_units(30)))
}

fn floor_cost() -> Cost {
    Cost::monthly(Money::from_units(40))
}

fn policy() -> Policy {
    Policy {
        headroom_slots: 40,
        queue_wait_target: Duration::from_secs(60),
        scale_up_after: MIN * 3,
        scale_down_after: MIN * 10,
        scale_up_cooldown: MIN * 5,
        scale_down_cooldown: MIN * 15,
        drain_deadline: MIN * 20,
        billing_step_margin: MIN * 5,
        max_nodes: 10,
        monthly_budget: Money::from_units(500),
        daily_cap: Money::from_units(40),
        tighten_from: 0.8,
        floor_nodes: vec![NodeId::from("floor")],
        prefer_burst_on_scale_down: true,
        burst_offers: vec![Offer {
            region: "fsn1".into(),
            machine_type: "cpx41".into(),
            slots: NODE_SLOTS,
            cost: burst_cost(),
        }],
    }
}

/// Players online: a quiet morning, a two-minute spike at noon, an evening
/// rise to 330 by 20:00, a two-hour peak, and a fall to 20 by 23:00.
fn players_at(t: DateTime<Utc>) -> u32 {
    let m = (t - at(0, 0)).num_minutes();
    let v = match m {
        ..720 => 20,
        720..722 => 160,
        722..1020 => 30,
        1020..1200 => 30 + (m - 1020) * 300 / 180,
        1200..1320 => 330,
        1320..1380 => 330 - (m - 1320) * 310 / 60,
        _ => 15,
    };
    v as u32
}

struct SimNode {
    load: NodeLoad,
    ready_at: DateTime<Utc>,
}

struct Day {
    decisions: Vec<(DateTime<Utc>, Decision)>,
    log: Vec<DryRunAction>,
    queue_waits: Vec<(DateTime<Utc>, Duration)>,
    spend_by_day: BTreeMap<NaiveDate, Money>,
    month_spend: Money,
    created: BTreeMap<NodeId, DateTime<Utc>>,
    cancelled: BTreeMap<NodeId, (DateTime<Utc>, u32)>,
    nodes_left: Vec<NodeId>,
}

impl Day {
    fn decisions_matching(&self, f: impl Fn(&Decision) -> bool) -> Vec<(DateTime<Utc>, &Decision)> {
        self.decisions
            .iter()
            .filter(|(_, d)| f(d))
            .map(|(t, d)| (*t, d))
            .collect()
    }

    fn buys(&self) -> Vec<DateTime<Utc>> {
        self.decisions_matching(|d| matches!(d, Decision::Buy { .. }))
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }

    fn of_kind(&self, f: impl Fn(&Decision) -> bool) -> Vec<DateTime<Utc>> {
        self.decisions_matching(f)
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }

    fn max_queue_wait(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Duration {
        self.queue_waits
            .iter()
            .filter(|(t, _)| *t >= from && *t < to)
            .map(|(_, w)| *w)
            .max()
            .unwrap_or_default()
    }
}

async fn run_day(policy: &Policy, month_spend_before: Money) -> Day {
    let start = at(6, 0);
    let prices = Prices {
        currency: "EUR".into(),
        entries: vec![Price {
            machine_type: "cpx41".into(),
            region: "fsn1".into(),
            cost: burst_cost(),
        }],
    };
    let dry = DryRun::new(Unreachable, prices).with_clock(sim_clock);

    let mut nodes = vec![SimNode {
        load: NodeLoad {
            id: "floor".into(),
            kind: NodeKind::Base,
            region: "fsn1".into(),
            phase: NodePhase::Ready,
            slots: NODE_SLOTS,
            used_slots: 0,
            players: 0,
            cost: floor_cost(),
            created_at: Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            draining_since: None,
        },
        ready_at: start,
    }];
    let mut memory = Memory::default();
    let mut queued_since: Option<DateTime<Utc>> = None;
    let mut day = Day {
        decisions: vec![],
        log: vec![],
        queue_waits: vec![],
        spend_by_day: BTreeMap::from([(start.date_naive(), floor_cost().per_hour().scale(6.0))]),
        month_spend: month_spend_before,
        created: BTreeMap::new(),
        cancelled: BTreeMap::new(),
        nodes_left: vec![],
    };
    let mut bought = 0;
    let mut printed_holds: Vec<std::mem::Discriminant<Reason>> = vec![];

    for minute in 0..24 * 60 {
        let now = start + TimeDelta::minutes(minute);
        NOW.with(|c| c.set(now.timestamp()));

        for n in &mut nodes {
            if n.load.phase == NodePhase::SettingUp && now >= n.ready_at {
                n.load.phase = NodePhase::Ready;
            }
        }

        // Billing: a started hour for burst nodes, pro rata for the floor.
        let mut charge = Money::ZERO;
        for n in &nodes {
            charge += match n.load.kind {
                NodeKind::Burst if (now - n.load.created_at).num_minutes() % 60 == 0 => {
                    n.load.cost.amount
                }
                NodeKind::Burst => Money::ZERO,
                NodeKind::Base => n.load.cost.per_hour().scale(1.0 / 60.0),
            };
        }
        *day.spend_by_day.entry(now.date_naive()).or_default() += charge;
        day.month_spend += charge;

        // Players: games on draining nodes end over about five minutes and
        // their players come back to be placed; the rest fill ready nodes,
        // the floor first; whoever doesn't fit queues.
        let online = players_at(now);
        let mut on_draining = 0;
        for n in nodes
            .iter_mut()
            .filter(|n| n.load.phase == NodePhase::Draining)
        {
            let p = n.load.players;
            n.load.players = p
                .saturating_sub(p.div_ceil(5).max(1))
                .min(online - on_draining);
            on_draining += n.load.players;
        }
        let mut to_place = online - on_draining;
        for n in nodes.iter_mut() {
            match n.load.phase {
                NodePhase::Ready => {
                    let here = to_place.min(n.load.slots);
                    n.load.players = here;
                    to_place -= here;
                }
                NodePhase::SettingUp => n.load.players = 0,
                NodePhase::Draining => {}
            }
            n.load.used_slots = n.load.players;
        }
        let wait = if to_place > 0 {
            (now - *queued_since.get_or_insert(now)).to_std().unwrap()
        } else {
            queued_since = None;
            Duration::ZERO
        };
        day.queue_waits.push((now, wait));

        let snapshot = Snapshot {
            now,
            nodes: nodes.iter().map(|n| n.load.clone()).collect(),
            queues: if to_place > 0 {
                vec![QueueWait {
                    game: "skywars".into(),
                    wait,
                }]
            } else {
                vec![]
            },
            spend: Spend {
                today: day.spend_by_day[&now.date_naive()],
                month: day.month_spend,
            },
        };
        let p = plan(&snapshot, policy, &memory);
        memory = p.memory.clone();

        // Carry the decisions out, the way the control plane will.
        let mut holds_now = vec![];
        for d in p.decisions {
            match &d {
                Decision::Buy {
                    region,
                    machine_type,
                    slots,
                    ..
                } => {
                    bought += 1;
                    let spec = NodeSpec {
                        name: format!("hallow-burst-{bought}"),
                        region: region.clone(),
                        machine_type: machine_type.clone(),
                        image: "debian-13".into(),
                        user_data: "#cloud-config\n".into(),
                        labels: BTreeMap::from([("role".into(), "game".into())]),
                    };
                    let node = dry.create(&spec).await.unwrap();
                    assert_eq!(node.created_at, now);
                    day.created.insert(node.id.clone(), now);
                    nodes.push(SimNode {
                        load: NodeLoad {
                            id: node.id,
                            kind: node.kind,
                            region: node.region,
                            phase: NodePhase::SettingUp,
                            slots: *slots,
                            used_slots: 0,
                            players: 0,
                            cost: node.cost,
                            created_at: node.created_at,
                            draining_since: None,
                        },
                        ready_at: now + SETUP,
                    });
                }
                Decision::Drain { node, .. } => {
                    let n = nodes.iter_mut().find(|n| &n.load.id == node).unwrap();
                    n.load.phase = NodePhase::Draining;
                    n.load.draining_since = Some(now);
                }
                Decision::Reinstate { node, .. } => {
                    let n = nodes.iter_mut().find(|n| &n.load.id == node).unwrap();
                    n.load.phase = NodePhase::Ready;
                    n.load.draining_since = None;
                }
                Decision::Cancel { node, .. } => {
                    let i = nodes.iter().position(|n| &n.load.id == node).unwrap();
                    let players = nodes[i].load.players;
                    dry.cancel(node).await.unwrap();
                    nodes.remove(i);
                    day.cancelled.insert(node.clone(), (now, players));
                }
                Decision::Hold { .. } => {}
            }
            // Print the day's log: every action, and each hold once when it starts.
            let print = match &d {
                Decision::Hold {
                    reason: Reason::WaitingToScaleUp { .. } | Reason::WaitingToScaleDown { .. },
                    ..
                } => false,
                Decision::Hold { reason, .. } => {
                    let kind = std::mem::discriminant(reason);
                    holds_now.push(kind);
                    !printed_holds.contains(&kind)
                }
                _ => true,
            };
            if print {
                println!("{} {d}", now.format("%H:%M"));
            }
            day.decisions.push((now, d));
        }
        printed_holds = holds_now;
    }

    day.log = dry.log();
    day.nodes_left = nodes.into_iter().map(|n| n.load.id).collect();
    day
}

#[tokio::test]
async fn a_day_with_an_evening_peak() {
    let policy = policy();
    let day = run_day(&policy, Money::from_units(30)).await;

    // The noon spike lasts two minutes, shorter than the sustain time: no buy.
    let buys = day.buys();
    assert!(
        buys.iter().all(|t| *t >= at(17, 0) && *t < at(20, 0)),
        "every buy is during the evening rise: {buys:?}"
    );
    assert!(
        day.max_queue_wait(at(12, 0), at(12, 5)) > Duration::ZERO,
        "the spike did queue players"
    );

    // The rise: 330 players plus 40 headroom need the floor and three burst
    // nodes, bought ahead of demand, so nobody queues at the rise or peak.
    assert_eq!(buys.len(), 3, "{buys:?}");
    assert_eq!(day.max_queue_wait(at(17, 0), at(23, 0)), Duration::ZERO);

    // No thrashing: nothing reinstated, nothing bought once draining starts,
    // and every node drained exactly once.
    let drains = day.of_kind(|d| matches!(d, Decision::Drain { .. }));
    assert_eq!(drains.len(), 3, "{drains:?}");
    assert!(
        drains.iter().all(|t| *t >= at(22, 0)),
        "drains come after the peak: {drains:?}"
    );
    assert!(buys.iter().max() < drains.iter().min());
    assert!(
        day.of_kind(|d| matches!(d, Decision::Reinstate { .. }))
            .is_empty()
    );
    for pair in drains.windows(2) {
        assert!(
            pair[1] - pair[0] >= TimeDelta::minutes(15),
            "drains respect the cooldown"
        );
    }

    // After the peak every burst node is cancelled, empty, just before its
    // next billing step, and the network is back on the floor node alone.
    assert_eq!(day.nodes_left, vec![NodeId::from("floor")]);
    assert_eq!(day.cancelled.len(), 3);
    for (node, (when, players)) in &day.cancelled {
        assert_eq!(*players, 0, "{node} cancelled with players on it");
        let into_hour = (*when - day.created[node]).num_minutes() % 60;
        assert!(
            (55..60).contains(&into_hour),
            "{node} cancelled {into_hour} min into its hour"
        );
    }
    assert!(
        day.decisions_matching(|d| matches!(
            d,
            Decision::Cancel {
                reason: Reason::Empty,
                ..
            }
        ))
        .len()
            == 3
    );

    // The dry-run log: three creates, then three cancels of the same nodes.
    let created: Vec<_> = day
        .log
        .iter()
        .filter_map(|a| match a {
            DryRunAction::Create { node, spec } => {
                assert_eq!(
                    (spec.region.as_str(), spec.machine_type.as_str()),
                    ("fsn1", "cpx41")
                );
                Some(node.clone())
            }
            _ => None,
        })
        .collect();
    let cancelled: Vec<_> = day
        .log
        .iter()
        .filter_map(|a| match a {
            DryRunAction::Cancel { node } => Some(node.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(created.len(), 3);
    let mut sorted = cancelled.clone();
    sorted.sort();
    assert_eq!(sorted, created);
    assert!(matches!(
        day.log[..3],
        [
            DryRunAction::Create { .. },
            DryRunAction::Create { .. },
            DryRunAction::Create { .. }
        ]
    ));

    // Within budget.
    for (date, spent) in &day.spend_by_day {
        assert!(*spent <= policy.daily_cap, "{date}: {spent}");
    }
    assert!(day.month_spend <= policy.monthly_budget);
    assert!(
        day.decisions_matching(|d| matches!(
            d,
            Decision::Hold {
                reason: Reason::DailyCap { .. } | Reason::MonthlyBudget { .. },
                ..
            }
        ))
        .is_empty()
    );
}

#[tokio::test]
async fn at_the_daily_cap_players_queue_and_nothing_is_cut() {
    let mut policy = policy();
    // Enough for the floor and one burst node this evening, not two.
    policy.daily_cap = Money::from_micros(1_700_000);
    let day = run_day(&policy, Money::from_units(30)).await;

    // One burst node fits under the cap at the rise; after that the cap holds
    // every buy through the peak. (Late in the evening a node fits again,
    // since less of the day is left to pay for.)
    let buys = day.buys();
    let cap_holds = day.of_kind(|d| {
        matches!(
            d,
            Decision::Hold {
                reason: Reason::DailyCap { .. },
                ..
            }
        )
    });
    let first_hold = *cap_holds.first().expect("the cap stopped a buy");
    assert_eq!(buys[0], at(17, 22));
    assert!(
        buys.iter().all(|t| *t < first_hold || *t >= at(22, 0)),
        "no buys while at the cap during the peak: {buys:?}"
    );
    assert!(buys.len() < 3, "fewer nodes than without a cap: {buys:?}");

    // At the cap the peak queues players instead of buying...
    assert!(day.max_queue_wait(at(19, 0), at(22, 0)) > policy.queue_wait_target);

    // ...and nothing is drained or cut while it lasts, nor ever with players on it.
    let cuts = day.of_kind(|d| matches!(d, Decision::Drain { .. } | Decision::Cancel { .. }));
    assert!(
        cuts.iter().all(|t| *t > at(22, 0)),
        "only after the peak: {cuts:?}"
    );
    for (node, (_, players)) in &day.cancelled {
        assert_eq!(*players, 0, "{node} cancelled with players on it");
    }

    for (date, spent) in &day.spend_by_day {
        assert!(*spent <= policy.daily_cap, "{date}: {spent} over the cap");
    }
    assert_eq!(day.nodes_left, vec![NodeId::from("floor")]);
}
