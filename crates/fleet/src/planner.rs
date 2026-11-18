//! The scaling planner: a pure function from a snapshot of the fleet and a
//! policy to decisions. No I/O, no clock (the time is in the snapshot), so
//! every rule can be tested tick by tick.
//!
//! What the planner has to remember between ticks (how long pressure has
//! held, when it last bought or drained) is in [`Memory`], which goes in and
//! comes back out, so the control plane can keep it in Redis.

use std::cmp::Reverse;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, NaiveTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::money::Money;
use crate::provider::{BillingStep, Cost, NodeId, NodeKind};

/// A node's place in Hallow's life cycle (not the provider's state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodePhase {
    /// Bought; cloud-init and Wings are still setting it up. Its slots count
    /// as free, since they are coming, so the planner doesn't buy twice.
    SettingUp,
    /// Registered in the panel and taking games.
    Ready,
    /// The matchmaker places no new games here.
    Draining,
}

/// One node as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLoad {
    pub id: NodeId,
    pub kind: NodeKind,
    pub region: String,
    pub phase: NodePhase,
    /// Game slots this node can hold.
    pub slots: u32,
    /// Slots taken, warm instance pools included.
    pub used_slots: u32,
    pub players: u32,
    pub cost: Cost,
    pub created_at: DateTime<Utc>,
    /// When it started draining; `None` unless `phase` is `Draining`.
    pub draining_since: Option<DateTime<Utc>>,
}

impl NodeLoad {
    /// Slots new games can use: none while draining, all while setting up.
    pub fn free_slots(&self) -> u32 {
        match self.phase {
            NodePhase::SettingUp => self.slots,
            NodePhase::Ready => self.slots.saturating_sub(self.used_slots),
            NodePhase::Draining => 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.players == 0 && self.used_slots == 0
    }
}

/// How long the longest-waiting player of one game kind has queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueWait {
    pub game: String,
    pub wait: Duration,
}

/// Spend so far, from the billing records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Spend {
    pub today: Money,
    pub month: Money,
}

/// The fleet at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub now: DateTime<Utc>,
    pub nodes: Vec<NodeLoad>,
    pub queues: Vec<QueueWait>,
    pub spend: Spend,
}

/// A machine the planner may buy for burst capacity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub region: String,
    pub machine_type: String,
    /// Game slots one such machine holds.
    pub slots: u32,
    pub cost: Cost,
}

/// Scaling targets, limits and budgets. Built from the fleet settings the
/// panel edits with [`Policy::from_settings`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    /// Where "today" and "this month" start for the daily cap and the
    /// monthly budget.
    pub timezone: Tz,
    /// Free game slots to keep across all nodes.
    pub headroom_slots: u32,
    /// Scale up when a queue waits longer than this.
    pub queue_wait_target: Duration,
    /// Pressure must hold this long before buying.
    pub scale_up_after: Duration,
    /// Surplus must hold this long before draining.
    pub scale_down_after: Duration,
    /// After a buy, no further buy for this long.
    pub scale_up_cooldown: Duration,
    /// After a buy or a drain, no drain for this long.
    pub scale_down_cooldown: Duration,
    /// A draining node is cancelled after this even with players left (they
    /// are moved first).
    pub drain_deadline: Duration,
    /// An empty hourly node is cancelled this long before its next billing
    /// step, and kept until then.
    pub billing_step_margin: Duration,
    /// Nodes of every kind and phase, the floor included.
    pub max_nodes: u32,
    /// `None` for no limit.
    pub monthly_budget: Option<Money>,
    /// `None` for no limit.
    pub daily_cap: Option<Money>,
    /// When the month-end forecast passes this fraction of the monthly
    /// budget, headroom shrinks linearly to zero at the budget.
    pub tighten_from: f64,
    /// Never drained or cancelled.
    pub floor_nodes: Vec<NodeId>,
    /// Drain burst nodes before base nodes.
    pub prefer_burst_on_scale_down: bool,
    /// Cancelling a draining base node waits for a staff member's approval.
    pub base_drain_needs_approval: bool,
    /// What to buy when scaling up. The cheapest per slot is picked.
    pub burst_offers: Vec<Offer>,
}

/// Why the planner decided something. Its `Display` is the audit-log text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    LowHeadroom {
        free: u32,
        target: u32,
    },
    QueueWait {
        game: String,
        wait: Duration,
        target: Duration,
    },
    Surplus {
        free_after: u32,
        target: u32,
    },
    Empty,
    DrainDeadline {
        players: u32,
    },
    KeepUntilBillingStep {
        cancel_at: DateTime<Utc>,
    },
    WaitingToScaleUp {
        since: DateTime<Utc>,
        needed: Duration,
    },
    WaitingToScaleDown {
        since: DateTime<Utc>,
        needed: Duration,
    },
    Cooldown {
        until: DateTime<Utc>,
    },
    DailyCap {
        projected: Money,
        cap: Money,
    },
    MonthlyBudget {
        forecast: Money,
        budget: Money,
    },
    MaxNodes {
        max: u32,
    },
    NoOffer,
    FloorNode,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::LowHeadroom { free, target } => write!(f, "{free} free slots, target {target}"),
            Reason::QueueWait { game, wait, target } => write!(
                f,
                "{game} queue waits {}s, target {}s",
                wait.as_secs(),
                target.as_secs()
            ),
            Reason::Surplus { free_after, target } => {
                write!(
                    f,
                    "{free_after} slots stay free without it, target {target}"
                )
            }
            Reason::Empty => f.write_str("empty"),
            Reason::DrainDeadline { players } => {
                write!(
                    f,
                    "drain deadline passed with {players} players left; move them first"
                )
            }
            Reason::KeepUntilBillingStep { cancel_at } => {
                write!(f, "already paid for; keeping it until {cancel_at}")
            }
            Reason::WaitingToScaleUp { since, needed } => {
                write!(
                    f,
                    "short of capacity since {since}; buying after {}s",
                    needed.as_secs()
                )
            }
            Reason::WaitingToScaleDown { since, needed } => {
                write!(
                    f,
                    "spare capacity since {since}; draining after {}s",
                    needed.as_secs()
                )
            }
            Reason::Cooldown { until } => write!(f, "cooling down until {until}"),
            Reason::DailyCap { projected, cap } => {
                write!(
                    f,
                    "at the daily cap: today would reach {projected} of {cap}"
                )
            }
            Reason::MonthlyBudget { forecast, budget } => {
                write!(
                    f,
                    "at the monthly budget: month-end forecast {forecast} of {budget}"
                )
            }
            Reason::MaxNodes { max } => write!(f, "at the maximum of {max} nodes"),
            Reason::NoOffer => f.write_str("no machine to buy is configured"),
            Reason::FloorNode => f.write_str("floor node, never cancelled"),
        }
    }
}

/// What the fleet manager should do this tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Decision {
    Buy {
        region: String,
        machine_type: String,
        slots: u32,
        cost: Cost,
        reason: Reason,
    },
    /// Stop draining a node and give it games again: cheaper than buying.
    Reinstate {
        node: NodeId,
        reason: Reason,
    },
    Drain {
        node: NodeId,
        reason: Reason,
    },
    Cancel {
        node: NodeId,
        reason: Reason,
        /// Base nodes need a staff member's approval in the panel.
        needs_approval: bool,
    },
    /// Something the planner wanted to do but didn't, and why.
    Hold {
        node: Option<NodeId>,
        reason: Reason,
    },
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Decision::Buy {
                region,
                machine_type,
                cost,
                reason,
                ..
            } => write!(
                f,
                "buy {machine_type} in {region} at {}/h: {reason}",
                cost.per_hour()
            ),
            Decision::Reinstate { node, reason } => write!(f, "reinstate {node}: {reason}"),
            Decision::Drain { node, reason } => write!(f, "drain {node}: {reason}"),
            Decision::Cancel {
                node,
                reason,
                needs_approval,
            } => {
                write!(f, "cancel {node}: {reason}")?;
                if *needs_approval {
                    f.write_str(" (needs approval)")?;
                }
                Ok(())
            }
            Decision::Hold {
                node: Some(node),
                reason,
            } => write!(f, "hold {node}: {reason}"),
            Decision::Hold { node: None, reason } => write!(f, "hold: {reason}"),
        }
    }
}

/// What the planner keeps between ticks.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Memory {
    pub pressure_since: Option<DateTime<Utc>>,
    pub surplus_since: Option<DateTime<Utc>>,
    pub last_buy: Option<DateTime<Utc>>,
    pub last_drain: Option<DateTime<Utc>>,
}

/// The planner's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub decisions: Vec<Decision>,
    /// Pass this to the next tick.
    pub memory: Memory,
    /// Free slots across ready and setting-up nodes.
    pub free_slots: u32,
    /// Headroom after budget tightening.
    pub headroom_target: u32,
    /// Month-end spend forecast.
    pub forecast: Money,
}

fn td(d: Duration) -> TimeDelta {
    TimeDelta::from_std(d).unwrap_or(TimeDelta::MAX)
}

fn hours(d: TimeDelta) -> f64 {
    d.num_seconds().max(0) as f64 / 3600.0
}

/// What a newly bought node is assumed to cost: burst nodes until the end of
/// the day (at least one billing step), monthly nodes a full month.
fn buy_cost(cost: &Cost, hours_left_today: f64) -> Money {
    match cost.step {
        BillingStep::Hour => cost.amount.scale(hours_left_today.ceil().max(1.0)),
        BillingStep::Month => cost.amount,
    }
}

/// When `date` starts in `tz`. Where a clock change skips midnight, the
/// first moment of the day that exists.
pub(crate) fn start_of_day(tz: Tz, date: NaiveDate) -> DateTime<Utc> {
    let midnight = date.and_time(NaiveTime::MIN);
    (0..=3)
        .find_map(|h| {
            tz.from_local_datetime(&(midnight + TimeDelta::hours(h)))
                .earliest()
        })
        .map_or_else(|| midnight.and_utc(), |t| t.with_timezone(&Utc))
}

/// Spend projections. Days and months are in the policy's time zone.
struct Budget {
    forecast: Money,
    projected_today: Money,
    hours_left_today: f64,
}

impl Budget {
    fn new(snapshot: &Snapshot, tz: Tz) -> Budget {
        let now = snapshot.now;
        let today = now.with_timezone(&tz).date_naive();
        let midnight = start_of_day(tz, today + Days::new(1));
        let first = today.with_day(1).unwrap_or(today);
        let month_start = start_of_day(tz, first);
        let month_end = start_of_day(tz, first + Months::new(1));
        let hours_left_today = hours(midnight - now);
        let hours_left_month = hours(month_end - now);

        let rate = |burst: bool| -> Money {
            snapshot
                .nodes
                .iter()
                .filter(|n| (n.kind == NodeKind::Burst) == burst)
                .map(|n| n.cost.per_hour())
                .sum()
        };
        let (committed, burst) = (rate(false), rate(true));

        // Base nodes run all month; burst nodes running now are assumed to
        // run until the end of the day.
        let from_fleet = snapshot.spend.month
            + committed.scale(hours_left_month)
            + burst.scale(hours_left_today);
        // Spend so far extrapolated to month end catches a month of busy
        // evenings that the running fleet alone doesn't show. Only once a
        // day has passed, so one evening doesn't decide the month.
        let elapsed = (now - month_start).num_seconds() as f64;
        let length = (month_end - month_start).num_seconds() as f64;
        let from_trend = if elapsed >= 86_400.0 {
            snapshot.spend.month.scale(length / elapsed)
        } else {
            Money::ZERO
        };

        Budget {
            forecast: from_fleet.max(from_trend),
            projected_today: snapshot.spend.today + (committed + burst).scale(hours_left_today),
            hours_left_today,
        }
    }

    /// Headroom shrinks linearly from `tighten_from` of the budget to zero
    /// at the budget.
    fn headroom(&self, policy: &Policy) -> u32 {
        let Some(budget) = policy.monthly_budget else {
            return policy.headroom_slots;
        };
        if budget <= Money::ZERO {
            return 0;
        }
        let ratio = self.forecast.as_f64() / budget.as_f64();
        let from = policy.tighten_from.clamp(0.0, 0.999);
        let factor = if ratio <= from {
            1.0
        } else if ratio >= 1.0 {
            0.0
        } else {
            (1.0 - ratio) / (1.0 - from)
        };
        (f64::from(policy.headroom_slots) * factor).ceil() as u32
    }
}

fn cooldown_until(
    last: Option<DateTime<Utc>>,
    cooldown: Duration,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let until = last? + td(cooldown);
    (until > now).then_some(until)
}

/// Decides what to buy, reinstate, drain and cancel.
pub fn plan(snapshot: &Snapshot, policy: &Policy, memory: &Memory) -> Plan {
    let now = snapshot.now;
    let mut memory = memory.clone();
    let mut decisions = Vec::new();
    let is_floor = |id: &NodeId| policy.floor_nodes.contains(id);

    let budget = Budget::new(snapshot, policy.timezone);
    let headroom_target = budget.headroom(policy);
    let mut free_slots: u32 = snapshot.nodes.iter().map(NodeLoad::free_slots).sum();
    let free_before = free_slots;

    // Signals.
    let low = free_slots < headroom_target;
    let worst_queue = snapshot
        .queues
        .iter()
        .filter(|q| q.wait > policy.queue_wait_target)
        .max_by_key(|q| q.wait);
    let pressure = low || worst_queue.is_some();

    // The node cheapest to lose: burst before base, then fewest players, then
    // the most expensive, among those whose slots aren't needed.
    let mut candidates: Vec<&NodeLoad> = snapshot
        .nodes
        .iter()
        .filter(|n| n.phase == NodePhase::Ready && !is_floor(&n.id))
        .collect();
    candidates.sort_by_key(|n| {
        let kind_rank = match (policy.prefer_burst_on_scale_down, n.kind) {
            (true, NodeKind::Burst) => 0,
            (true, NodeKind::Base) => 1,
            (false, _) => 0,
        };
        (
            kind_rank,
            n.players,
            Reverse(n.cost.per_hour()),
            n.id.clone(),
        )
    });
    let surplus_node = candidates
        .into_iter()
        .find(|n| free_slots.saturating_sub(n.slots) >= headroom_target && free_slots >= n.slots);
    let surplus = !pressure && surplus_node.is_some();

    memory.pressure_since = if pressure {
        memory.pressure_since.or(Some(now))
    } else {
        None
    };
    memory.surplus_since = if surplus {
        memory.surplus_since.or(Some(now))
    } else {
        None
    };

    // Scale up: reinstate draining nodes first, then buy.
    let mut reinstated: Vec<NodeId> = Vec::new();
    if pressure {
        let reason = match worst_queue {
            Some(q) if !low => Reason::QueueWait {
                game: q.game.clone(),
                wait: q.wait,
                target: policy.queue_wait_target,
            },
            _ => Reason::LowHeadroom {
                free: free_slots,
                target: headroom_target,
            },
        };
        let since = memory.pressure_since.unwrap_or(now);
        if now - since < td(policy.scale_up_after) {
            decisions.push(Decision::Hold {
                node: None,
                reason: Reason::WaitingToScaleUp {
                    since,
                    needed: policy.scale_up_after,
                },
            });
        } else {
            // A queue over target with enough headroom still asks for one
            // node's worth.
            let mut deficit = if low { headroom_target - free_slots } else { 1 };

            let mut draining: Vec<&NodeLoad> = snapshot
                .nodes
                .iter()
                .filter(|n| n.phase == NodePhase::Draining && n.slots > n.used_slots)
                .collect();
            draining.sort_by_key(|n| (Reverse(n.slots - n.used_slots), n.id.clone()));
            for node in draining {
                if deficit == 0 {
                    break;
                }
                let gained = node.slots - node.used_slots;
                deficit = deficit.saturating_sub(gained);
                free_slots += gained;
                reinstated.push(node.id.clone());
                decisions.push(Decision::Reinstate {
                    node: node.id.clone(),
                    reason: reason.clone(),
                });
            }

            if deficit > 0 {
                if let Some(until) = cooldown_until(memory.last_buy, policy.scale_up_cooldown, now)
                {
                    decisions.push(Decision::Hold {
                        node: None,
                        reason: Reason::Cooldown { until },
                    });
                } else {
                    buy(
                        policy,
                        snapshot,
                        &budget,
                        deficit,
                        &reason,
                        &mut decisions,
                        &mut memory,
                    );
                }
            }
            if decisions
                .iter()
                .any(|d| matches!(d, Decision::Buy { .. } | Decision::Reinstate { .. }))
            {
                memory.pressure_since = None;
            }
        }
    }

    // Draining nodes: cancel once empty or past the deadline, keeping an
    // empty hourly node until just before its next billing step.
    for node in snapshot
        .nodes
        .iter()
        .filter(|n| n.phase == NodePhase::Draining)
    {
        if reinstated.contains(&node.id) {
            continue;
        }
        if is_floor(&node.id) {
            decisions.push(Decision::Hold {
                node: Some(node.id.clone()),
                reason: Reason::FloorNode,
            });
            continue;
        }
        let since = node.draining_since.unwrap_or(now);
        let overdue = now - since >= td(policy.drain_deadline);
        if !node.is_empty() && !overdue {
            continue;
        }
        if node.is_empty() && node.cost.step == BillingStep::Hour {
            let cancel_at =
                node.cost.next_billing_step(node.created_at, now) - td(policy.billing_step_margin);
            if now < cancel_at {
                decisions.push(Decision::Hold {
                    node: Some(node.id.clone()),
                    reason: Reason::KeepUntilBillingStep { cancel_at },
                });
                continue;
            }
        }
        decisions.push(Decision::Cancel {
            node: node.id.clone(),
            reason: if node.is_empty() {
                Reason::Empty
            } else {
                Reason::DrainDeadline {
                    players: node.players,
                }
            },
            needs_approval: policy.base_drain_needs_approval && node.kind == NodeKind::Base,
        });
    }

    // Scale down: one node per cooldown.
    if let (true, Some(node)) = (surplus, surplus_node) {
        let since = memory.surplus_since.unwrap_or(now);
        if now - since < td(policy.scale_down_after) {
            decisions.push(Decision::Hold {
                node: Some(node.id.clone()),
                reason: Reason::WaitingToScaleDown {
                    since,
                    needed: policy.scale_down_after,
                },
            });
        } else if let Some(until) = cooldown_until(memory.last_buy, policy.scale_down_cooldown, now)
            .max(cooldown_until(
                memory.last_drain,
                policy.scale_down_cooldown,
                now,
            ))
        {
            decisions.push(Decision::Hold {
                node: Some(node.id.clone()),
                reason: Reason::Cooldown { until },
            });
        } else {
            decisions.push(Decision::Drain {
                node: node.id.clone(),
                reason: Reason::Surplus {
                    free_after: free_slots - node.slots,
                    target: headroom_target,
                },
            });
            memory.last_drain = Some(now);
            memory.surplus_since = None;
        }
    }

    Plan {
        decisions,
        memory,
        free_slots: free_before,
        headroom_target,
        forecast: budget.forecast,
    }
}

/// Buys enough nodes to cover `deficit` slots, within the node limit and the
/// budgets. Budgets only ever stop buying here; nothing else reads them.
fn buy(
    policy: &Policy,
    snapshot: &Snapshot,
    budget: &Budget,
    deficit: u32,
    reason: &Reason,
    decisions: &mut Vec<Decision>,
    memory: &mut Memory,
) {
    let Some(offer) = policy
        .burst_offers
        .iter()
        .filter(|o| o.slots > 0)
        .min_by(|a, b| {
            let per_slot = |o: &Offer| o.cost.per_hour().as_f64() / f64::from(o.slots);
            per_slot(a).total_cmp(&per_slot(b))
        })
    else {
        decisions.push(Decision::Hold {
            node: None,
            reason: Reason::NoOffer,
        });
        return;
    };

    let wanted = deficit.div_ceil(offer.slots);
    let existing = snapshot.nodes.len() as u32;
    let mut today = budget.projected_today;
    let mut forecast = budget.forecast;
    let cost = buy_cost(&offer.cost, budget.hours_left_today);
    let mut bought = false;
    for i in 0..wanted {
        let projected = today + cost;
        let month_end = forecast + cost;
        let hold = if existing + i >= policy.max_nodes {
            Some(Reason::MaxNodes {
                max: policy.max_nodes,
            })
        } else if let Some(cap) = policy.daily_cap.filter(|&cap| projected > cap) {
            Some(Reason::DailyCap { projected, cap })
        } else {
            policy
                .monthly_budget
                .filter(|&budget| month_end > budget)
                .map(|budget| Reason::MonthlyBudget {
                    forecast: month_end,
                    budget,
                })
        };
        if let Some(reason) = hold {
            decisions.push(Decision::Hold { node: None, reason });
            break;
        }
        decisions.push(Decision::Buy {
            region: offer.region.clone(),
            machine_type: offer.machine_type.clone(),
            slots: offer.slots,
            cost: offer.cost,
            reason: reason.clone(),
        });
        today += cost;
        forecast += cost;
        bought = true;
    }
    if bought {
        memory.last_buy = Some(snapshot.now);
    }
}

#[cfg(test)]
#[path = "planner_tests.rs"]
mod tests;
