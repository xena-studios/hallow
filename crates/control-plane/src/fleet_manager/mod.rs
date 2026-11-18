//! The fleet manager loop (docs/FLEET.md): buys, drains and cancels nodes
//! from live load, within the budgets, and keeps an audit log of every
//! decision and provider call in PostgreSQL.
//!
//! Every tick (`tick_secs` in the fleet file, 30 s by default), the control
//! plane instance holding the `fleet-lock` in Redis:
//!
//! 1. reads the [fleet settings](crate::fleet) and its nodes
//!    (`hallow.fleet_nodes`), and marks bought nodes ready once they joined
//!    (or, in dry run, once `dry_run_setup_secs` have passed);
//! 2. reads live load from the [`World`]: slots and players per node from
//!    the instances on it, queue waits, players per proxy;
//! 3. works out spend today and this month from every node's price and life
//!    ([`hallow_fleet::billing`]) and runs the pure planner
//!    ([`hallow_fleet::plan`]);
//! 4. carries the decisions out: buys and cancels through the provider, or
//!    through [`DryRun`] while `dry_run` is on (always for nodes bought in
//!    dry run); drains and reinstates nodes; ends idle instances on draining
//!    nodes so they empty; moves players off a node before cancelling it;
//! 5. drains or gives back proxies ([`proxies`]);
//! 6. notices a budget or the node limit stopping a buy: the fleet is
//!    *capped*, staff are alerted (log, metrics, audit log) and queued
//!    players are told the network is busy;
//! 7. writes the audit log, its status (`GET /v1/fleet`) and the node view
//!    the instance lifecycle places instances by ([`NodesView`]).
//!
//! Holds (what the planner wanted but didn't do) go in the audit log once
//! when they start, not every tick.

pub(crate) mod api;
pub mod provider;
pub mod proxies;
pub mod world;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;
use hallow_db::{FleetNode as NodeRow, NewFleetEvent, NewFleetNode, PgPool};
use hallow_fleet::{
    Billed, BillingStep, Cost, Decision, DryRun, Memory, Money, NodeBootstrap, NodeId, NodeKind,
    NodeLoad, NodePhase, NodeSpec, Offer, Policy, Price, Prices, QueueWait, Reason, Snapshot,
    WingsBuild, WireGuard, billing,
};
use hallow_proto::{
    fleet::FleetSettings,
    fleet_state::{
        CapKind, FleetCap, FleetEventKind, FleetNode, FleetNodePhase, FleetProxies, FleetQueue,
        FleetState,
    },
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    orchestrator::NodeSlot,
    store::{Store, StoreError},
};
pub use provider::{DynProvider, Shared, Unconfigured};
pub use proxies::ProxyLoad;
pub use world::{LiveWorld, Load, NodeUsage, QueueLoad, World, WorldFuture};

/// What queued players are told when the fleet is capped.
pub const BUSY_TEXT: &str =
    "§eThe network is busy right now. You'll join a game as soon as one has room.";

/// How long the real provider's price list is kept.
const PRICES_FOR: TimeDelta = TimeDelta::hours(1);

// The fleet file

fn default_tick() -> u64 {
    30
}
fn default_provider() -> String {
    "hetzner-cloud".into()
}
fn default_image() -> String {
    "debian-13".into()
}
fn default_setup() -> u64 {
    180
}
fn default_region() -> String {
    "local".into()
}
fn default_floor_type() -> String {
    "floor".into()
}

/// How a node is billed, in the fleet file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Billing {
    Hour,
    #[default]
    Month,
}

/// A floor node: a machine that is always there (`[[floor]]`). Its name is
/// the orchestrator's name for it (`docker` on the dev network, the
/// panel's node name with Wings).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FloorConfig {
    pub name: String,
    #[serde(default = "default_region")]
    pub region: String,
    #[serde(default = "default_floor_type")]
    pub machine_type: String,
    /// Player slots its instances may take.
    pub slots: u32,
    /// Price per billing step, such as `"40.00"`.
    pub price: String,
    #[serde(default)]
    pub billing: Billing,
}

/// A machine the fleet manager may buy (`[[machine]]`): its size in slots
/// and the price dry run budgets with. With dry run off, the provider's own
/// price is used when it lists one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    #[serde(rename = "type")]
    pub machine_type: String,
    pub region: String,
    pub slots: u32,
    /// Such as `"0.0500"`.
    pub per_hour: String,
    /// The provider's monthly price, the most an hourly machine costs in a
    /// month.
    #[serde(default)]
    pub monthly_cap: Option<String>,
}

/// The parts of a new node's cloud-init that are the same for every node
/// (`[bootstrap]`); the join token, role and region are filled in per
/// node. Only needed with dry run off. See `hallow_fleet::NodeBootstrap`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapTemplate {
    pub control_plane_url: String,
    #[serde(default)]
    pub wireguard: Option<WireGuard>,
    #[serde(default)]
    pub private_interfaces: Vec<String>,
    #[serde(default)]
    pub ssh_allow: Vec<String>,
    #[serde(default)]
    pub wings_allow: Vec<String>,
    #[serde(default)]
    pub game_ports: Option<(u16, u16)>,
    #[serde(default)]
    pub wings: Option<WingsBuild>,
    #[serde(default)]
    pub ssh_authorized_keys: Vec<String>,
}

/// The fleet file (`HALLOW_FLEET_FILE`): what the fleet manager runs on.
/// Everything it decides by is in the [settings](crate::fleet) instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetConfig {
    /// Seconds between ticks. Default 30.
    #[serde(default = "default_tick")]
    pub tick_secs: u64,
    /// The provider bought from. Default `hetzner-cloud`.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// OS image for new nodes. Default `debian-13`.
    #[serde(default = "default_image")]
    pub image: String,
    /// A dry-run node counts as set up this long after it was "bought".
    /// Default 180.
    #[serde(default = "default_setup")]
    pub dry_run_setup_secs: u64,
    #[serde(default)]
    pub floor: Vec<FloorConfig>,
    #[serde(default, rename = "machine")]
    pub machines: Vec<MachineConfig>,
    #[serde(default)]
    pub bootstrap: Option<BootstrapTemplate>,
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn amount(field: &str, s: &str) -> Result<Money, String> {
    Money::parse(s)
        .filter(|m| *m >= Money::ZERO)
        .ok_or_else(|| format!("{field}: {s:?} isn't an amount"))
}

impl FleetConfig {
    /// Reads and checks a fleet file's TOML.
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        config.check()?;
        Ok(config)
    }

    /// Checks what serde can't.
    pub fn check(&self) -> Result<(), String> {
        if self.tick_secs == 0 || self.tick_secs > 3600 {
            return Err("tick_secs must be 1 to 3600".into());
        }
        if self.floor.is_empty() {
            return Err("the fleet file needs at least one [[floor]] node".into());
        }
        let mut names = BTreeSet::new();
        for floor in &self.floor {
            if !is_name(&floor.name) || !is_name(&floor.region) || !is_name(&floor.machine_type) {
                return Err(format!(
                    "floor {:?}: names are 1 to 64 letters, digits, '.', '_' or '-'",
                    floor.name
                ));
            }
            if !names.insert(floor.name.clone()) {
                return Err(format!("floor {} is listed twice", floor.name));
            }
            amount(&format!("floor {} price", floor.name), &floor.price)?;
        }
        for machine in &self.machines {
            if !is_name(&machine.machine_type) || !is_name(&machine.region) {
                return Err(format!(
                    "machine {:?}: bad type or region",
                    machine.machine_type
                ));
            }
            if machine.slots == 0 {
                return Err(format!(
                    "machine {}: slots must be at least 1",
                    machine.machine_type
                ));
            }
            amount(
                &format!("machine {} per_hour", machine.machine_type),
                &machine.per_hour,
            )?;
            if let Some(cap) = &machine.monthly_cap {
                amount(
                    &format!("machine {} monthly_cap", machine.machine_type),
                    cap,
                )?;
            }
        }
        Ok(())
    }

    /// The machines' prices, for dry run.
    #[must_use]
    pub fn prices(&self, currency: &str) -> Prices {
        Prices {
            currency: currency.to_owned(),
            entries: self
                .machines
                .iter()
                .map(|m| Price {
                    machine_type: m.machine_type.clone(),
                    region: m.region.clone(),
                    cost: Cost::hourly(
                        Money::parse(&m.per_hour).unwrap_or_default(),
                        m.monthly_cap.as_deref().and_then(Money::parse),
                    ),
                })
                .collect(),
        }
    }
}

/// The fleet manager's setup: the fleet file, and where the provider's
/// credentials come from.
#[derive(Clone, Debug)]
pub struct FleetSetup {
    pub config: FleetConfig,
    /// Hetzner Cloud API token (`HALLOW_HETZNER_TOKEN` or `_FILE`). Without
    /// one, every real provider call fails, so only dry run works.
    pub hetzner_token: Option<String>,
    /// Hetzner API base URL, for a mock (`HALLOW_HETZNER_API_URL`).
    pub hetzner_api_url: Option<String>,
}

impl FleetSetup {
    #[must_use]
    pub fn new(config: FleetConfig) -> Self {
        Self {
            config,
            hetzner_token: None,
            hetzner_api_url: None,
        }
    }

    /// The real provider this setup names.
    pub fn provider(&self) -> Result<Arc<dyn DynProvider>, String> {
        match (&self.hetzner_token, self.config.provider.as_str()) {
            (Some(token), "hetzner-cloud") => {
                let mut config = hallow_provider_hetzner_cloud::Config::default();
                if let Some(url) = &self.hetzner_api_url {
                    config.base_url.clone_from(url);
                }
                let hetzner = hallow_provider_hetzner_cloud::HetznerCloud::new(
                    hallow_provider_hetzner_cloud::ApiToken::new(token.clone()),
                    config,
                )
                .map_err(|e| e.to_string())?;
                Ok(Arc::new(hetzner))
            }
            (Some(_), other) => Err(format!("no provider named {other}")),
            (None, name) => Ok(Arc::new(Unconfigured {
                name: name.to_owned(),
            })),
        }
    }
}

// The node view the instance lifecycle reads

/// Each node's slots and whether it takes new instances, written by the
/// fleet manager every tick and read by the instance lifecycle
/// ([`crate::instances`]), which may run in another control plane instance.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodesView {
    pub at_ms: u64,
    pub nodes: Vec<ViewNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewNode {
    pub name: String,
    pub slots: u32,
    /// Ready: not setting up, not draining.
    pub takes_instances: bool,
    /// Bought in dry run: no machine exists.
    pub dry_run: bool,
}

impl NodesView {
    pub async fn read(store: &Store) -> Result<Option<Self>, StoreError> {
        match store.fleet_value("view").await? {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    /// The orchestrator's nodes as the fleet sees them: those it is
    /// draining or setting up left out, the rest sized. Nodes it doesn't
    /// know are left as they are. With an orchestrator that
    /// [places anywhere](crate::orchestrator::Orchestrator::places_anywhere),
    /// ready dry-run nodes are added, so a dry run plays out on one machine.
    #[must_use]
    pub fn apply(&self, nodes: Vec<NodeSlot>, places_anywhere: bool) -> Vec<NodeSlot> {
        let by_name: BTreeMap<&str, &ViewNode> =
            self.nodes.iter().map(|n| (n.name.as_str(), n)).collect();
        let mut out: Vec<NodeSlot> = nodes
            .into_iter()
            .filter_map(|mut node| match by_name.get(node.name.as_str()) {
                Some(view) if !view.takes_instances => None,
                Some(view) => {
                    node.slots = Some(view.slots);
                    Some(node)
                }
                None => Some(node),
            })
            .collect();
        if places_anywhere {
            for view in &self.nodes {
                if view.takes_instances && view.dry_run && !out.iter().any(|n| n.name == view.name)
                {
                    out.push(NodeSlot {
                        name: view.name.clone(),
                        ports: None,
                        slots: Some(view.slots),
                    });
                }
            }
        }
        out
    }
}

/// The fleet's status from the last tick, if any.
pub async fn last_state(store: &Store) -> Result<Option<FleetState>, StoreError> {
    match store.fleet_value("status").await? {
        Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        None => Ok(None),
    }
}

/// What a queued player is told while the fleet is capped.
pub async fn busy(store: &Store) -> Option<String> {
    match last_state(store).await {
        Ok(Some(state)) if state.capped.is_some() => Some(BUSY_TEXT.to_owned()),
        _ => None,
    }
}

// The loop

#[derive(Debug, Default, Serialize, Deserialize)]
struct FleetMemory {
    #[serde(default)]
    planner: Memory,
    /// Holds logged and still holding, by key.
    #[serde(default)]
    holds: BTreeSet<String>,
    #[serde(default)]
    capped: Option<FleetCap>,
    #[serde(default)]
    proxies: proxies::ProxyMemory,
    #[serde(default)]
    proxies_missing: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum TickError {
    #[error("database: {0}")]
    Database(#[from] hallow_db::sqlx::Error),
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    World(String),
    #[error("{0}")]
    Settings(#[from] hallow_fleet::InvalidSettings),
}

impl From<serde_json::Error> for TickError {
    fn from(err: serde_json::Error) -> Self {
        Self::Store(StoreError::from(err))
    }
}

/// What one tick decided, for tests and logs.
#[derive(Clone, Debug)]
pub struct TickReport {
    pub decisions: Vec<Decision>,
    pub state: FleetState,
}

/// Runs the fleet. See the module docs.
pub struct FleetManager {
    db: PgPool,
    store: Store,
    world: Arc<dyn World>,
    real: Arc<dyn DynProvider>,
    dry: DryRun<Shared>,
    config: FleetConfig,
    prices: Mutex<Option<(DateTime<Utc>, Prices)>>,
    seeded: AtomicBool,
}

fn ms(at: DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

fn cost_of(row: &NodeRow) -> Cost {
    Cost {
        amount: Money::from_micros(row.cost_micros),
        step: if row.billing_step == "hour" {
            BillingStep::Hour
        } else {
            BillingStep::Month
        },
        monthly_cap: row.monthly_cap_micros.map(Money::from_micros),
    }
}

fn kind_of(row: &NodeRow) -> NodeKind {
    if row.kind == "base" {
        NodeKind::Base
    } else {
        NodeKind::Burst
    }
}

fn step_name(step: BillingStep) -> &'static str {
    match step {
        BillingStep::Hour => "hour",
        BillingStep::Month => "month",
    }
}

fn reason_kind(reason: &Reason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_owned))
        .unwrap_or_default()
}

fn decision_action(decision: &Decision) -> &'static str {
    match decision {
        Decision::Buy { .. } => "buy",
        Decision::Reinstate { .. } => "reinstate",
        Decision::Drain { .. } => "drain",
        Decision::Cancel { .. } => "cancel",
        Decision::Hold { .. } => "hold",
    }
}

fn decision_node(decision: &Decision) -> Option<String> {
    match decision {
        Decision::Buy { .. } => None,
        Decision::Reinstate { node, .. }
        | Decision::Drain { node, .. }
        | Decision::Cancel { node, .. } => Some(node.0.clone()),
        Decision::Hold { node, .. } => node.as_ref().map(|n| n.0.clone()),
    }
}

/// Builds audit log entries for one tick.
struct Log {
    now: DateTime<Utc>,
    dry_run: bool,
    events: Vec<NewFleetEvent>,
}

impl Log {
    fn add(
        &mut self,
        kind: FleetEventKind,
        action: &str,
        node: Option<String>,
        ok: bool,
        message: String,
        detail: Option<serde_json::Value>,
    ) {
        self.events.push(NewFleetEvent {
            at: self.now,
            kind: kind.as_str(),
            action: action.to_owned(),
            node,
            dry_run: self.dry_run,
            ok,
            message,
            by: None,
            detail,
        });
    }
}

/// A hold to log once, when it starts.
struct Hold {
    node: Option<String>,
    message: String,
    detail: Option<serde_json::Value>,
}

impl FleetManager {
    /// `real` is the provider the fleet buys from with dry run off; it is
    /// never called while dry run is on, nor for nodes bought in dry run.
    #[must_use]
    pub fn new(
        db: PgPool,
        store: Store,
        world: Arc<dyn World>,
        real: Arc<dyn DynProvider>,
        config: FleetConfig,
    ) -> Self {
        let dry = DryRun::new(Shared(real.clone()), config.prices("EUR"));
        Self {
            db,
            store,
            world,
            real,
            dry,
            config,
            prices: Mutex::new(None),
            seeded: AtomicBool::new(false),
        }
    }

    #[must_use]
    pub fn config(&self) -> &FleetConfig {
        &self.config
    }

    /// Records the fleet file's floor nodes.
    pub async fn seed(&self, now: DateTime<Utc>) -> Result<(), TickError> {
        for floor in &self.config.floor {
            let price = Money::parse(&floor.price).unwrap_or_default();
            hallow_db::put_floor_node(
                &self.db,
                &NewFleetNode {
                    name: &floor.name,
                    provider: "floor",
                    provider_id: None,
                    kind: "base",
                    region: &floor.region,
                    machine_type: &floor.machine_type,
                    slots: i32::try_from(floor.slots).unwrap_or(i32::MAX),
                    cost_micros: price.micros(),
                    billing_step: match floor.billing {
                        Billing::Hour => "hour",
                        Billing::Month => "month",
                    },
                    monthly_cap_micros: None,
                    phase: "ready",
                    dry_run: false,
                    floor: true,
                    created_at: now,
                },
            )
            .await?;
        }
        self.seeded.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Plans and acts once, as of `now`. See the module docs.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<TickReport, TickError> {
        if !self.seeded.load(Ordering::Relaxed) {
            self.seed(now).await?;
        }
        let settings = crate::fleet::current(&self.db).await?.settings;
        let tz: Tz = settings.timezone.parse().unwrap_or(chrono_tz::UTC);
        let mut memory: FleetMemory = match self.store.fleet_value("memory").await? {
            Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            None => FleetMemory::default(),
        };
        let mut log = Log {
            now,
            dry_run: settings.dry_run,
            events: Vec::new(),
        };
        let mut holds: BTreeMap<String, Hold> = BTreeMap::new();
        let (month_start, _) = billing::periods(now, tz);

        // 1. Bought nodes become ready.
        let rows = hallow_db::fleet_nodes(&self.db, month_start).await?;
        let setting_up: Vec<&NodeRow> = rows.iter().filter(|r| r.phase == "setting_up").collect();
        let joined = if setting_up.iter().any(|r| !r.dry_run) {
            self.world.joined_nodes().await.map_err(TickError::World)?
        } else {
            BTreeSet::new()
        };
        let setup = TimeDelta::seconds(i64::try_from(self.config.dry_run_setup_secs).unwrap_or(0));
        for row in setting_up {
            let ready = if row.dry_run {
                now >= row.created_at + setup
            } else {
                joined.contains(&row.name)
            };
            if ready
                && hallow_db::set_fleet_node_phase(
                    &self.db,
                    &row.name,
                    &["setting_up"],
                    "ready",
                    now,
                )
                .await?
                .is_some()
            {
                tracing::info!(node = %row.name, dry_run = row.dry_run, "fleet node ready");
                log.add(
                    FleetEventKind::Node,
                    "ready",
                    Some(row.name.clone()),
                    true,
                    if row.dry_run {
                        format!("{} is set up (dry run)", row.name)
                    } else {
                        format!("{} joined and takes games", row.name)
                    },
                    None,
                );
            }
        }

        // 2. Live load.
        let load = self.world.load().await.map_err(TickError::World)?;
        let rows = hallow_db::fleet_nodes(&self.db, month_start).await?;
        let mut decisions = Vec::new();
        let mut plan_info = (0u32, 0u32, Money::ZERO);
        let spend = billing::spend(
            &rows
                .iter()
                .map(|r| Billed {
                    cost: cost_of(r),
                    created_at: r.created_at,
                    ended_at: r.cancelled_at,
                })
                .collect::<Vec<_>>(),
            now,
            tz,
        );
        let mut cap: Option<(CapKind, String)> = None;
        let mut proxies_status = FleetProxies::default();

        if settings.paused {
            holds.insert(
                "-:paused".into(),
                Hold {
                    node: None,
                    message: "paused: making no decisions".into(),
                    detail: None,
                },
            );
        } else {
            // 3. Plan.
            let live: Vec<&NodeRow> = rows.iter().filter(|r| r.phase != "cancelled").collect();
            let nodes: Vec<NodeLoad> = live
                .iter()
                .map(|r| {
                    let usage = load.nodes.get(&r.name).copied().unwrap_or_default();
                    NodeLoad {
                        id: NodeId(r.name.clone()),
                        kind: kind_of(r),
                        region: r.region.clone(),
                        phase: match r.phase.as_str() {
                            "setting_up" => NodePhase::SettingUp,
                            "draining" => NodePhase::Draining,
                            _ => NodePhase::Ready,
                        },
                        slots: u32::try_from(r.slots).unwrap_or(0),
                        used_slots: usage.used_slots,
                        players: usage.players,
                        cost: cost_of(r),
                        created_at: r.created_at,
                        draining_since: r.draining_since,
                    }
                })
                .collect();
            let snapshot = Snapshot {
                now,
                nodes,
                queues: load
                    .queues
                    .iter()
                    .map(|q| QueueWait {
                        game: q.kind.clone(),
                        wait: q.wait,
                    })
                    .collect(),
                spend,
            };
            let mut floor = hallow_fleet::floor_nodes(settings.floor_nodes, &snapshot.nodes);
            for row in live.iter().filter(|r| r.floor) {
                let id = NodeId(row.name.clone());
                if !floor.contains(&id) {
                    floor.push(id);
                }
            }
            let offers = self.offers(&settings, now, &mut log).await;
            let policy = Policy::from_settings(&settings, floor, offers)?;
            let plan = hallow_fleet::plan(&snapshot, &policy, &memory.planner);
            memory.planner = plan.memory.clone();
            plan_info = (plan.free_slots, plan.headroom_target, plan.forecast);

            // 4. Carry it out.
            for decision in &plan.decisions {
                self.carry_out(decision, &settings, &rows, &load, now, &mut log, &mut holds)
                    .await?;
                if let Decision::Hold { reason, .. } = decision {
                    let kind = match reason {
                        Reason::DailyCap { .. } => Some(CapKind::DailyCap),
                        Reason::MonthlyBudget { .. } => Some(CapKind::MonthlyBudget),
                        Reason::MaxNodes { .. } => Some(CapKind::MaxNodes),
                        _ => None,
                    };
                    if let (Some(kind), None) = (kind, &cap) {
                        cap = Some((kind, reason.to_string()));
                    }
                }
            }
            decisions = plan.decisions;

            // 5. Proxies.
            proxies_status = self
                .proxies(&settings, &load, now, &mut memory, &mut log)
                .await;
        }

        // Idle instances on draining nodes end, so the nodes empty.
        for row in rows.iter().filter(|r| r.phase == "draining") {
            if load.nodes.get(&row.name).is_none_or(|u| u.instances == 0) {
                continue;
            }
            match self.world.clear_node(&row.name, false).await {
                Ok(0) => {}
                Ok(ended) => log.add(
                    FleetEventKind::Node,
                    "clear",
                    Some(row.name.clone()),
                    true,
                    format!("ended {ended} idle instances on draining {}", row.name),
                    None,
                ),
                Err(err) => {
                    tracing::warn!(node = %row.name, %err, "couldn't clear a draining node")
                }
            }
        }

        // 6. Capped?
        match (&cap, &memory.capped) {
            (Some((kind, reason)), None) => {
                tracing::warn!(%reason, "fleet capped: players queue instead of new nodes");
                log.add(
                    FleetEventKind::Alert,
                    "capped",
                    None,
                    true,
                    format!("capped: {reason}; players queue instead"),
                    None,
                );
                memory.capped = Some(FleetCap {
                    kind: *kind,
                    reason: reason.clone(),
                    since_ms: ms(now),
                });
                if let Err(err) = self.world.notify_queued(BUSY_TEXT).await {
                    tracing::warn!(%err, "couldn't tell queued players the network is busy");
                }
            }
            (Some((kind, reason)), Some(_)) => {
                if let Some(capped) = &mut memory.capped {
                    capped.kind = *kind;
                    capped.reason.clone_from(reason);
                }
            }
            (None, Some(was)) => {
                tracing::info!(reason = %was.reason, "fleet no longer capped");
                log.add(
                    FleetEventKind::Alert,
                    "uncapped",
                    None,
                    true,
                    format!("no longer capped (was: {})", was.reason),
                    None,
                );
                memory.capped = None;
            }
            (None, None) => {}
        }

        // Holds go in the log once, when they start.
        for (key, hold) in &holds {
            if !memory.holds.contains(key) {
                log.add(
                    FleetEventKind::Decision,
                    "hold",
                    hold.node.clone(),
                    true,
                    hold.message.clone(),
                    hold.detail.clone(),
                );
            }
        }
        memory.holds = holds.keys().cloned().collect();

        // 7. Write it all down.
        hallow_db::add_fleet_events(&self.db, &log.events).await?;
        let rows = hallow_db::fleet_nodes(&self.db, month_start).await?;
        let view = NodesView {
            at_ms: ms(now),
            nodes: rows
                .iter()
                .filter(|r| r.phase != "cancelled")
                .map(|r| ViewNode {
                    name: r.name.clone(),
                    slots: u32::try_from(r.slots).unwrap_or(0),
                    takes_instances: r.phase == "ready",
                    dry_run: r.dry_run,
                })
                .collect(),
        };
        let state = FleetState {
            enabled: true,
            dry_run: settings.dry_run,
            paused: settings.paused,
            currency: settings.currency.clone(),
            last_tick_ms: Some(ms(now)),
            free_slots: plan_info.0,
            headroom_target: plan_info.1,
            spend_today: spend.today.to_string(),
            spend_month: spend.month.to_string(),
            forecast: plan_info.2.to_string(),
            daily_cap: settings.daily_cap.clone(),
            monthly_budget: settings.monthly_budget.clone(),
            capped: memory.capped.clone(),
            holds: holds.values().map(|h| h.message.clone()).collect(),
            queues: load
                .queues
                .iter()
                .map(|q| FleetQueue {
                    kind: q.kind.clone(),
                    players: q.players,
                    wait_secs: q.wait.as_secs(),
                })
                .collect(),
            proxies: proxies_status,
            nodes: rows
                .iter()
                .filter(|r| r.phase != "cancelled")
                .map(|r| node_info(r, load.nodes.get(&r.name), now, tz))
                .collect(),
        };
        self.store
            .set_fleet_value("view", &serde_json::to_string(&view)?)
            .await?;
        self.store
            .set_fleet_value("status", &serde_json::to_string(&state)?)
            .await?;
        self.store
            .set_fleet_value("memory", &serde_json::to_string(&memory)?)
            .await?;
        Ok(TickReport { decisions, state })
    }

    /// The machines the settings allow, priced: by the config in dry run,
    /// by the provider (cached for an hour) otherwise.
    async fn offers(
        &self,
        settings: &FleetSettings,
        now: DateTime<Utc>,
        log: &mut Log,
    ) -> Vec<Offer> {
        let ours = self.config.prices(&settings.currency);
        let theirs = if settings.dry_run {
            None
        } else {
            let cached = self
                .prices
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .filter(|(at, _)| now - *at < PRICES_FOR);
            match cached {
                Some((_, prices)) => Some(prices),
                None => match self.real.prices().await {
                    Ok(prices) => {
                        log.add(
                            FleetEventKind::Provider,
                            "prices",
                            None,
                            true,
                            format!(
                                "{} prices: {} entries",
                                self.real.name(),
                                prices.entries.len()
                            ),
                            None,
                        );
                        *self
                            .prices
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some((now, prices.clone()));
                        Some(prices)
                    }
                    Err(err) => {
                        tracing::warn!(%err, "couldn't read the provider's prices; using the fleet file's");
                        log.add(
                            FleetEventKind::Provider,
                            "prices",
                            None,
                            false,
                            format!("{} prices failed: {err}", self.real.name()),
                            None,
                        );
                        None
                    }
                },
            }
        };
        let offers = self.config.machines.iter().map(|m| {
            let cost = theirs
                .as_ref()
                .and_then(|p| p.find(&m.machine_type, &m.region))
                .or_else(|| ours.find(&m.machine_type, &m.region))
                .map_or_else(|| Cost::hourly(Money::ZERO, None), |p| p.cost);
            Offer {
                region: m.region.clone(),
                machine_type: m.machine_type.clone(),
                slots: m.slots,
                cost,
            }
        });
        hallow_fleet::allowed_offers(settings, &self.config.provider, offers)
    }

    #[allow(clippy::too_many_arguments)]
    async fn carry_out(
        &self,
        decision: &Decision,
        settings: &FleetSettings,
        rows: &[NodeRow],
        load: &Load,
        now: DateTime<Utc>,
        log: &mut Log,
        holds: &mut BTreeMap<String, Hold>,
    ) -> Result<(), TickError> {
        let detail = serde_json::to_value(decision).ok();
        let node = decision_node(decision);
        match decision {
            Decision::Hold { reason, .. } => {
                holds.insert(
                    format!("{}:{}", node.as_deref().unwrap_or("-"), reason_kind(reason)),
                    Hold {
                        node,
                        message: decision.to_string(),
                        detail,
                    },
                );
                return Ok(());
            }
            Decision::Cancel {
                node: id,
                needs_approval,
                ..
            } => {
                let Some(row) = rows.iter().find(|r| r.name == id.0) else {
                    return Ok(());
                };
                if *needs_approval && row.approved_by.is_none() {
                    if hallow_db::await_fleet_approval(&self.db, &row.name).await? {
                        log.add(
                            FleetEventKind::Decision,
                            "cancel",
                            node.clone(),
                            true,
                            decision.to_string(),
                            detail,
                        );
                        log.add(
                            FleetEventKind::Alert,
                            "awaiting_approval",
                            node,
                            true,
                            format!("{} waits for staff to approve its cancellation", row.name),
                            None,
                        );
                    }
                    return Ok(());
                }
                // Move anyone still on it first; cancel on a later tick.
                let usage = load.nodes.get(&row.name).copied().unwrap_or_default();
                if usage.instances > 0 {
                    let ended = self
                        .world
                        .clear_node(&row.name, true)
                        .await
                        .map_err(TickError::World)?;
                    holds.insert(
                        format!("{}:moving", row.name),
                        Hold {
                            node,
                            message: format!(
                                "cancel {}: moving {} players off {} instances first ({ended} ending)",
                                row.name, usage.players, usage.instances
                            ),
                            detail,
                        },
                    );
                    return Ok(());
                }
                log.add(
                    FleetEventKind::Decision,
                    "cancel",
                    node,
                    true,
                    decision.to_string(),
                    detail,
                );
                self.cancel(row, now, log).await?;
                return Ok(());
            }
            _ => {}
        }
        log.add(
            FleetEventKind::Decision,
            decision_action(decision),
            node,
            true,
            decision.to_string(),
            detail,
        );
        match decision {
            Decision::Buy {
                region,
                machine_type,
                slots,
                ..
            } => {
                self.buy(region, machine_type, *slots, settings, now, log)
                    .await?
            }
            Decision::Reinstate { node, .. } => {
                hallow_db::set_fleet_node_phase(&self.db, &node.0, &["draining"], "ready", now)
                    .await?;
                tracing::info!(%node, "fleet node reinstated");
            }
            Decision::Drain { node, .. } => {
                hallow_db::set_fleet_node_phase(&self.db, &node.0, &["ready"], "draining", now)
                    .await?;
                tracing::info!(%node, "fleet node draining");
            }
            Decision::Cancel { .. } | Decision::Hold { .. } => {}
        }
        Ok(())
    }

    async fn buy(
        &self,
        region: &str,
        machine_type: &str,
        slots: u32,
        settings: &FleetSettings,
        now: DateTime<Utc>,
        log: &mut Log,
    ) -> Result<(), TickError> {
        let name = format!("hallow-burst-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let mut spec = NodeSpec {
            name: name.clone(),
            region: region.to_owned(),
            machine_type: machine_type.to_owned(),
            image: self.config.image.clone(),
            user_data: "#cloud-config\n# dry run: no machine is made\n".into(),
            labels: [("hallow-role".to_owned(), "game".to_owned())].into(),
        };
        // The spec as logged: never the user data, which holds the join token.
        let logged = serde_json::json!({
            "name": spec.name, "region": spec.region, "machine_type": spec.machine_type,
            "image": spec.image, "labels": spec.labels,
        });
        let dry_run = settings.dry_run;
        let created = if dry_run {
            hallow_fleet::Provider::create(&self.dry, &spec).await
        } else {
            match self.user_data(&name, region).await {
                Ok(user_data) => {
                    spec.user_data = user_data;
                    self.real.create(&spec).await
                }
                Err(err) => {
                    tracing::error!(%err, node = %name, "can't set up a node to buy");
                    log.add(
                        FleetEventKind::Alert,
                        "create",
                        Some(name),
                        false,
                        format!("not bought: {err}"),
                        Some(logged),
                    );
                    return Ok(());
                }
            }
        };
        let provider = self.real.name().to_owned();
        match created {
            Ok(node) => {
                tracing::info!(node = %name, id = %node.id, dry_run, region, machine_type, "fleet node bought");
                log.add(
                    FleetEventKind::Provider,
                    "create",
                    Some(name.clone()),
                    true,
                    format!(
                        "{}created {name} ({machine_type} in {region}) at {}/h, id {}",
                        if dry_run { "dry run: " } else { "" },
                        node.cost.per_hour(),
                        node.id
                    ),
                    Some(logged),
                );
                hallow_db::add_fleet_node(
                    &self.db,
                    &NewFleetNode {
                        name: &name,
                        provider: &provider,
                        provider_id: Some(&node.id.0),
                        kind: match node.kind {
                            NodeKind::Base => "base",
                            NodeKind::Burst => "burst",
                        },
                        region,
                        machine_type,
                        slots: i32::try_from(slots).unwrap_or(i32::MAX),
                        cost_micros: node.cost.amount.micros(),
                        billing_step: step_name(node.cost.step),
                        monthly_cap_micros: node.cost.monthly_cap.map(Money::micros),
                        phase: "setting_up",
                        dry_run,
                        floor: false,
                        created_at: now,
                    },
                )
                .await?;
            }
            Err(err) => {
                tracing::error!(%err, node = %name, dry_run, "couldn't buy a node");
                log.add(
                    FleetEventKind::Provider,
                    "create",
                    Some(name.clone()),
                    false,
                    format!("create {name} failed: {err}"),
                    Some(logged),
                );
                log.add(
                    FleetEventKind::Alert,
                    "create_failed",
                    Some(name),
                    false,
                    format!("{provider} refused a node: {err}"),
                    None,
                );
            }
        }
        Ok(())
    }

    /// Cloud-init for a real node: a join token and the fleet file's
    /// `[bootstrap]`.
    async fn user_data(&self, name: &str, region: &str) -> Result<String, String> {
        let template = self
            .config
            .bootstrap
            .as_ref()
            .ok_or("the fleet file has no [bootstrap] for real nodes")?;
        let token = self.world.join_token(name, region).await?;
        hallow_fleet::render_cloud_init(&NodeBootstrap {
            control_plane_url: template.control_plane_url.clone(),
            join_token: token,
            role: NodeKind::Burst,
            region: region.to_owned(),
            labels: BTreeMap::new(),
            wireguard: template.wireguard.clone(),
            private_interfaces: template.private_interfaces.clone(),
            ssh_allow: template.ssh_allow.clone(),
            wings_allow: template.wings_allow.clone(),
            game_ports: template.game_ports,
            wings: template.wings.clone(),
            ssh_authorized_keys: template.ssh_authorized_keys.clone(),
        })
        .map_err(|e| e.to_string())
    }

    async fn cancel(
        &self,
        row: &NodeRow,
        now: DateTime<Utc>,
        log: &mut Log,
    ) -> Result<(), TickError> {
        let Some(id) = &row.provider_id else {
            return Ok(());
        };
        let id = NodeId(id.clone());
        let result = if row.dry_run {
            hallow_fleet::Provider::cancel(&self.dry, &id).await
        } else {
            self.real.cancel(&id).await
        };
        let was_dry = log.dry_run;
        log.dry_run = row.dry_run;
        match result {
            Ok(()) => {
                hallow_db::set_fleet_node_phase(
                    &self.db,
                    &row.name,
                    &["draining", "ready", "setting_up"],
                    "cancelled",
                    now,
                )
                .await?;
                tracing::info!(node = %row.name, dry_run = row.dry_run, "fleet node cancelled");
                log.add(
                    FleetEventKind::Provider,
                    "cancel",
                    Some(row.name.clone()),
                    true,
                    format!(
                        "{}cancelled {} (id {id})",
                        if row.dry_run { "dry run: " } else { "" },
                        row.name
                    ),
                    None,
                );
            }
            Err(err) => {
                tracing::error!(%err, node = %row.name, "couldn't cancel a node");
                log.add(
                    FleetEventKind::Provider,
                    "cancel",
                    Some(row.name.clone()),
                    false,
                    format!("cancel {} failed: {err}", row.name),
                    None,
                );
                log.add(
                    FleetEventKind::Alert,
                    "cancel_failed",
                    Some(row.name.clone()),
                    false,
                    format!("{} wasn't cancelled: {err}; it still costs money", row.name),
                    None,
                );
            }
        }
        log.dry_run = was_dry;
        Ok(())
    }

    async fn proxies(
        &self,
        settings: &FleetSettings,
        load: &Load,
        now: DateTime<Utc>,
        memory: &mut FleetMemory,
        log: &mut Log,
    ) -> FleetProxies {
        let plan = proxies::plan(
            &load.proxies,
            settings.players_per_proxy,
            settings.min_proxies,
            Duration::from_secs(u64::try_from(settings.scale_down_after_secs).unwrap_or(0)),
            now,
            &mut memory.proxies,
        );
        for (ids, draining) in [(&plan.drain, true), (&plan.undrain, false)] {
            for id in ids {
                let ok = match self.world.set_proxy_draining(id, draining).await {
                    Ok(found) => found,
                    Err(err) => {
                        tracing::warn!(proxy = %id, %err, "couldn't set proxy drain");
                        false
                    }
                };
                let what = if draining { "drain" } else { "undrain" };
                tracing::info!(proxy = %id, ok, "proxy {what}");
                log.add(
                    FleetEventKind::Proxy,
                    what,
                    Some(id.clone()),
                    ok,
                    if draining {
                        format!(
                            "drained proxy {id}: {} proxies carry the players",
                            plan.wanted
                        )
                    } else {
                        format!("gave proxy {id} back: the players need {}", plan.wanted)
                    },
                    None,
                );
            }
        }
        if plan.missing > 0 && memory.proxies_missing == 0 {
            tracing::warn!(
                missing = plan.missing,
                "the players need more proxies than run"
            );
            log.add(
                FleetEventKind::Alert,
                "proxies_short",
                None,
                true,
                format!(
                    "the players need {} proxies, {} more than run; start more",
                    plan.wanted, plan.missing
                ),
                None,
            );
        }
        memory.proxies_missing = plan.missing;
        let draining_now: BTreeSet<&str> = plan.drain.iter().map(String::as_str).collect();
        let undrained: BTreeSet<&str> = plan.undrain.iter().map(String::as_str).collect();
        FleetProxies {
            count: load.proxies.len() as u32,
            active: load
                .proxies
                .iter()
                .filter(|p| {
                    (!p.draining || undrained.contains(p.id.as_str()))
                        && !draining_now.contains(p.id.as_str())
                })
                .count() as u32,
            wanted: plan.wanted,
            players: load.proxies.iter().map(|p| p.players).sum(),
            drained: memory.proxies.drained.iter().cloned().collect(),
        }
    }
}

/// A node for the API.
pub(crate) fn node_info(
    row: &NodeRow,
    usage: Option<&NodeUsage>,
    now: DateTime<Utc>,
    tz: Tz,
) -> FleetNode {
    let usage = usage.copied().unwrap_or_default();
    let cost = cost_of(row);
    let month = billing::node_month(
        &Billed {
            cost,
            created_at: row.created_at,
            ended_at: row.cancelled_at,
        },
        now,
        tz,
    );
    FleetNode {
        name: row.name.clone(),
        provider: row.provider.clone(),
        provider_id: row.provider_id.clone(),
        kind: row.kind.clone(),
        region: row.region.clone(),
        machine_type: row.machine_type.clone(),
        phase: FleetNodePhase::parse(&row.phase).unwrap_or(FleetNodePhase::Ready),
        dry_run: row.dry_run,
        floor: row.floor,
        slots: u32::try_from(row.slots).unwrap_or(0),
        used_slots: usage.used_slots,
        players: usage.players,
        instances: usage.instances,
        per_hour: cost.per_hour().to_string(),
        billing: step_name(cost.step).into(),
        spend_month: month.to_string(),
        created_ms: ms(row.created_at),
        ready_ms: row.ready_at.map(ms),
        draining_since_ms: row.draining_since.map(ms),
        cancelled_ms: row.cancelled_at.map(ms),
        awaiting_approval: row.awaiting_approval,
        approved_by: row.approved_by.clone(),
    }
}

/// Runs [`FleetManager::tick`] every `tick_secs` while this instance holds
/// the fleet lock.
pub async fn run(manager: FleetManager) {
    let holder = Uuid::new_v4().to_string();
    let every = Duration::from_secs(manager.config.tick_secs);
    let ttl = u64::try_from((every * 3).as_millis()).unwrap_or(u64::MAX);
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match manager.store.lock_fleet(&holder, ttl).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(err) => {
                tracing::warn!(%err, "fleet manager couldn't reach Redis");
                continue;
            }
        }
        match manager.tick(Utc::now()).await {
            Ok(report) => {
                for decision in report
                    .decisions
                    .iter()
                    .filter(|d| !matches!(d, Decision::Hold { .. }))
                {
                    tracing::debug!(%decision, "fleet decision");
                }
            }
            Err(err) => tracing::warn!(%err, "fleet tick failed"),
        }
    }
}

/// The fleet's metrics, from the last tick's status.
pub async fn metrics(store: &Store, out: &mut String) -> Result<(), StoreError> {
    use std::fmt::Write as _;
    let Some(state) = last_state(store).await? else {
        return Ok(());
    };
    let amount = |s: &str| Money::parse(s).map_or(0.0, Money::as_f64);
    let mut gauge = |name: &str, help: &str, lines: Vec<(String, f64)>| {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} gauge");
        for (labels, value) in lines {
            let _ = writeln!(out, "{name}{labels} {value}");
        }
    };
    gauge(
        "hallow_fleet_dry_run",
        "1 while the fleet manager carries decisions out in dry run.",
        vec![(String::new(), f64::from(u8::from(state.dry_run)))],
    );
    let mut phases: BTreeMap<(String, &str), u32> = BTreeMap::new();
    for node in &state.nodes {
        *phases
            .entry((node.kind.clone(), node.phase.as_str()))
            .or_default() += 1;
    }
    gauge(
        "hallow_fleet_nodes",
        "Fleet nodes by kind and phase.",
        phases
            .into_iter()
            .map(|((kind, phase), n)| {
                (
                    format!("{{kind=\"{kind}\",phase=\"{phase}\"}}"),
                    f64::from(n),
                )
            })
            .collect(),
    );
    gauge(
        "hallow_fleet_free_slots",
        "Free game slots on ready and setting-up nodes.",
        vec![(String::new(), f64::from(state.free_slots))],
    );
    gauge(
        "hallow_fleet_headroom_target",
        "Free slots the fleet manager keeps, after budget tightening.",
        vec![(String::new(), f64::from(state.headroom_target))],
    );
    gauge(
        "hallow_fleet_spend",
        "Spend so far, in the settings' currency.",
        vec![
            ("{period=\"today\"}".into(), amount(&state.spend_today)),
            ("{period=\"month\"}".into(), amount(&state.spend_month)),
        ],
    );
    gauge(
        "hallow_fleet_forecast",
        "Month-end spend forecast.",
        vec![(String::new(), amount(&state.forecast))],
    );
    let mut limits = Vec::new();
    if let Some(cap) = &state.daily_cap {
        limits.push(("{limit=\"daily_cap\"}".into(), amount(cap)));
    }
    if let Some(budget) = &state.monthly_budget {
        limits.push(("{limit=\"monthly_budget\"}".into(), amount(budget)));
    }
    gauge(
        "hallow_fleet_budget",
        "The daily cap and monthly budget.",
        limits,
    );
    gauge(
        "hallow_fleet_capped",
        "1 while a budget or the node limit stops a buy and players queue instead.",
        [CapKind::DailyCap, CapKind::MonthlyBudget, CapKind::MaxNodes]
            .into_iter()
            .map(|kind| {
                let name = match kind {
                    CapKind::DailyCap => "daily_cap",
                    CapKind::MonthlyBudget => "monthly_budget",
                    CapKind::MaxNodes => "max_nodes",
                };
                let on = state.capped.as_ref().is_some_and(|c| c.kind == kind);
                (format!("{{reason=\"{name}\"}}"), f64::from(u8::from(on)))
            })
            .collect(),
    );
    gauge(
        "hallow_fleet_proxies",
        "Proxies registered, taking players, and wanted.",
        vec![
            (
                "{state=\"registered\"}".into(),
                f64::from(state.proxies.count),
            ),
            ("{state=\"active\"}".into(), f64::from(state.proxies.active)),
            ("{state=\"wanted\"}".into(), f64::from(state.proxies.wanted)),
        ],
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(name: &str) -> NodeSlot {
        NodeSlot {
            name: name.into(),
            ports: None,
            slots: None,
        }
    }

    fn view(name: &str, slots: u32, takes: bool, dry_run: bool) -> ViewNode {
        ViewNode {
            name: name.into(),
            slots,
            takes_instances: takes,
            dry_run,
        }
    }

    #[test]
    fn the_view_sizes_nodes_and_leaves_out_draining_ones() {
        let v = NodesView {
            at_ms: 0,
            nodes: vec![
                view("floor", 40, true, false),
                view("burst-a", 100, false, false),
                view("dry-b", 100, true, true),
                view("dry-c", 100, false, true),
            ],
        };
        let nodes = vec![slot("floor"), slot("burst-a"), slot("manual")];
        let out = v.apply(nodes.clone(), false);
        assert_eq!(
            out.iter()
                .map(|n| (n.name.as_str(), n.slots))
                .collect::<Vec<_>>(),
            [("floor", Some(40)), ("manual", None)]
        );
        let out = v.apply(nodes, true);
        assert_eq!(
            out.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
            ["floor", "manual", "dry-b"]
        );
    }

    #[test]
    fn fleet_files_are_checked() {
        let good = r#"
            tick_secs = 10
            [[floor]]
            name = "docker"
            slots = 64
            price = "40.00"
            [[machine]]
            type = "cpx41"
            region = "fsn1"
            slots = 100
            per_hour = "0.0500"
            monthly_cap = "30.00"
        "#;
        let config = FleetConfig::parse(good).unwrap();
        assert_eq!(config.floor[0].billing, Billing::Month);
        assert_eq!(config.dry_run_setup_secs, 180);
        let prices = config.prices("EUR");
        assert_eq!(
            prices.find("cpx41", "fsn1").unwrap().cost,
            Cost::hourly(Money::from_cents(5), Some(Money::from_units(30)))
        );
        assert!(
            FleetConfig::parse("tick_secs = 10")
                .unwrap_err()
                .contains("[[floor]]")
        );
        assert!(FleetConfig::parse(&good.replace("\"0.0500\"", "\"cheap\"")).is_err());
        assert!(FleetConfig::parse(&good.replace("slots = 100", "slots = 0")).is_err());
        assert!(FleetConfig::parse(&format!("{good}\nbogus = 1")).is_err());
    }

    #[test]
    fn the_dev_networks_fleet_file_parses() {
        let config = FleetConfig::parse(include_str!(
            "../../../../deploy/compose/config/hallow/fleet.toml"
        ))
        .unwrap();
        assert_eq!(config.floor[0].name, crate::orchestrator::docker::NODE);
        assert!(config.bootstrap.is_none());
    }
}
