//! What the fleet manager is doing (docs/FLEET.md): its nodes, load,
//! spend and budget, and the audit log of every decision and provider
//! call. The panel's Fleet page reads these; the settings themselves are in
//! [`crate::fleet`].
//!
//! Amounts are decimal strings in the settings' currency, such as
//! `"12.3400"`. Times are Unix milliseconds.

use serde::{Deserialize, Serialize};

/// `GET /v1/fleet`: the fleet as of the last tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FleetState {
    /// Whether this control plane runs the fleet manager at all
    /// (`HALLOW_FLEET_FILE`). When `false` everything else is empty.
    pub enabled: bool,
    /// From the settings: decisions are carried out against dry run.
    pub dry_run: bool,
    /// From the settings: no decisions are made.
    pub paused: bool,
    pub currency: String,
    /// When the fleet manager last planned; `None` before its first tick.
    pub last_tick_ms: Option<u64>,
    /// Free game slots on ready and setting-up nodes.
    pub free_slots: u32,
    /// Free slots the fleet manager keeps, after budget tightening.
    pub headroom_target: u32,
    pub spend_today: String,
    pub spend_month: String,
    /// Month-end spend forecast.
    pub forecast: String,
    pub daily_cap: Option<String>,
    pub monthly_budget: Option<String>,
    /// Set while a budget or the node limit stops a buy the fleet manager
    /// wanted: players queue instead.
    pub capped: Option<FleetCap>,
    /// Why the fleet manager held back this tick, in words.
    pub holds: Vec<String>,
    /// The longest wait per game queue.
    pub queues: Vec<FleetQueue>,
    pub proxies: FleetProxies,
    /// Every node not yet cancelled, the floor first, then by creation.
    pub nodes: Vec<FleetNode>,
}

/// What stops buying.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapKind {
    DailyCap,
    MonthlyBudget,
    MaxNodes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetCap {
    pub kind: CapKind,
    /// The planner's reason, such as "at the daily cap: today would reach
    /// 10.4000 of 10.0000".
    pub reason: String,
    pub since_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetQueue {
    pub kind: String,
    pub players: u32,
    pub wait_secs: u64,
}

/// The proxy layer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetProxies {
    /// Registered proxies.
    pub count: u32,
    /// Proxies taking new players (not draining).
    pub active: u32,
    /// Proxies the players need, at least `min_proxies`.
    pub wanted: u32,
    /// Players on all proxies.
    pub players: u32,
    /// Proxies the fleet manager drained for being idle.
    pub drained: Vec<String>,
}

/// A node's place in the fleet manager's life cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetNodePhase {
    /// Bought; not taking games yet.
    SettingUp,
    Ready,
    /// No new games go here; cancelled once empty.
    Draining,
    /// Returned to the provider.
    Cancelled,
}

impl FleetNodePhase {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SettingUp => "setting_up",
            Self::Ready => "ready",
            Self::Draining => "draining",
            Self::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "setting_up" => Self::SettingUp,
            "ready" => Self::Ready,
            "draining" => Self::Draining,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// One node the fleet manager knows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetNode {
    /// Also the node's name in the orchestrator and the panel.
    pub name: String,
    pub provider: String,
    /// The provider's id for the machine; `None` for a floor node from the
    /// fleet file.
    pub provider_id: Option<String>,
    /// `base` or `burst`.
    pub kind: String,
    pub region: String,
    pub machine_type: String,
    pub phase: FleetNodePhase,
    /// Bought in dry run: no machine exists.
    pub dry_run: bool,
    /// Never drained or cancelled.
    pub floor: bool,
    pub slots: u32,
    /// Slots taken by its instances, at the last tick.
    pub used_slots: u32,
    pub players: u32,
    pub instances: u32,
    pub per_hour: String,
    /// `hour` or `month`.
    pub billing: String,
    /// What it cost this month so far.
    pub spend_month: String,
    pub created_ms: u64,
    pub ready_ms: Option<u64>,
    pub draining_since_ms: Option<u64>,
    pub cancelled_ms: Option<u64>,
    /// A base node the planner wants cancelled, waiting for staff.
    pub awaiting_approval: bool,
    pub approved_by: Option<String>,
}

/// What an audit log entry is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetEventKind {
    /// A planner decision: buy, reinstate, drain, cancel, hold.
    Decision,
    /// A call to a provider (or dry run in its place).
    Provider,
    /// A node changed phase on its own (became ready).
    Node,
    /// The proxy layer drained or gave back a proxy.
    Proxy,
    /// A staff action from the panel.
    Staff,
    /// Something staff should know: a cap reached or lifted, a failed call.
    Alert,
}

impl FleetEventKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Provider => "provider",
            Self::Node => "node",
            Self::Proxy => "proxy",
            Self::Staff => "staff",
            Self::Alert => "alert",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "decision" => Self::Decision,
            "provider" => Self::Provider,
            "node" => Self::Node,
            "proxy" => Self::Proxy,
            "staff" => Self::Staff,
            "alert" => Self::Alert,
            _ => return None,
        })
    }
}

/// One entry of `GET /v1/fleet/events`, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetEvent {
    pub id: i64,
    pub at_ms: u64,
    pub kind: FleetEventKind,
    /// `buy`, `drain`, `create`, `cancel`, `capped`, `dry_run`, ...
    pub action: String,
    pub node: Option<String>,
    /// Made while dry run was on (provider calls went to dry run).
    pub dry_run: bool,
    /// `false` for a provider call or action that failed.
    pub ok: bool,
    /// What happened, in words.
    pub message: String,
    /// Who, for staff actions.
    pub by: Option<String>,
    /// The decision or call as JSON text, for the curious.
    pub detail: Option<String>,
}

/// `PUT /v1/fleet/dry-run`: turn dry run on or off. Saved like any other
/// settings change, with its history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetDryRun {
    pub dry_run: bool,
    pub updated_by: String,
}

/// `POST /v1/fleet/nodes/{name}/drain`, `/reinstate` and `/approve-cancel`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetNodeAction {
    /// The staff member, for the audit log.
    pub by: String,
}
