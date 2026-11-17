//! The fleet manager wired into the control plane (docs/FLEET.md), in dry
//! run, with a provider that records any call made to it.
//!
//! - Two simulated days run the real [`FleetManager`] tick by tick (one
//!   per simulated minute) against PostgreSQL and Redis, with a simulated
//!   network standing in for live load. They check the audit log: the buys
//!   at the evening peak and the cancels after it, and, with a budget too
//!   small for the peak, buying stopping at the cap while players queue.
//! - A live control plane, with the mock orchestrator, plays scale to one:
//!   a queue fills the floor node, a dry-run node is bought and takes a
//!   game, the players leave, the node is drained and cancelled, and a new
//!   player still gets a game on the floor node. It also checks the API.
//!
//! Needs `HALLOW_TEST_REDIS_URL` and `HALLOW_TEST_DATABASE_URL`, like
//! `instances.rs`; without them the tests pass without doing anything
//! (except in CI, where they fail). Run with `--nocapture` to see the
//! simulated days' audit logs.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use hallow_control_plane::{
    Config, InstancesSetup, OrchestratorChoice, Overrides,
    fleet_manager::{
        FleetConfig, FleetManager, FleetSetup, Load, NodeUsage, ProxyLoad, QueueLoad, World,
        WorldFuture,
    },
    instances::Settings,
    orchestrator::{
        Orchestrator,
        docker::{DockerConfig, Endpoint},
        mock::MockOrchestrator,
    },
    serve_custom,
    store::Store,
};
use hallow_db::sqlx::{self, Executor, postgres::PgPoolOptions};
use hallow_fleet::{Node, NodeId, NodeKind, NodeSpec, Prices, Provider, ProviderError};
use hallow_proto::{
    ApiError, ErrorCode, Poll,
    command::ProxyCommand,
    fleet::{FleetSettings, FleetSettingsRecord},
    fleet_state::{
        CapKind, FleetEvent, FleetEventKind, FleetNodeAction, FleetNodePhase, FleetState, SetDryRun,
    },
    instance::{InstanceInfo, InstanceState, KindPlayers, KindRole, KindScaling, KindSpec},
    presence::{LoginDecision, LoginRequest},
    proxy::RegisterProxy,
    queue::{JoinQueue, QueueStatus},
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerPlayer, ServerRole, ServerState},
};
use reqwest::StatusCode;
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

const TOKEN: &str = "test-token-0123456789";

fn env_or_skip(var: &str) -> Option<String> {
    let value = std::env::var(var).ok();
    if value.is_none() {
        assert!(std::env::var_os("CI").is_none(), "{var} isn't set in CI");
        eprintln!("{var} isn't set; skipping");
    }
    value
}

/// Stands in for Hetzner: counts every call and refuses it. In dry run the
/// count must stay at zero.
#[derive(Default)]
struct Recording {
    calls: AtomicU32,
}

impl Recording {
    fn refuse<T>(&self) -> Result<T, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Unauthorized)
    }
}

impl Provider for Recording {
    fn name(&self) -> &str {
        "hetzner-cloud"
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Burst
    }
    async fn create(&self, _: &NodeSpec) -> Result<Node, ProviderError> {
        self.refuse()
    }
    async fn status(&self, _: &NodeId) -> Result<Node, ProviderError> {
        self.refuse()
    }
    async fn cancel(&self, _: &NodeId) -> Result<(), ProviderError> {
        self.refuse()
    }
    async fn list(&self) -> Result<Vec<Node>, ProviderError> {
        self.refuse()
    }
    async fn prices(&self) -> Result<Prices, ProviderError> {
        self.refuse()
    }
}

/// A fresh database, dropped at the end.
struct Db {
    pool: hallow_db::PgPool,
    url: String,
    admin: hallow_db::PgPool,
    name: String,
}

impl Db {
    async fn create() -> Option<Self> {
        let db_url = env_or_skip("HALLOW_TEST_DATABASE_URL")?;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
            .unwrap();
        let name = format!("hallow_fleet_test_{}", Uuid::new_v4().simple());
        admin
            .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .await
            .unwrap();
        let (base, _) = db_url.rsplit_once('/').unwrap();
        let url = format!("{base}/{name}");
        let pool = hallow_db::connect(&url).await.unwrap();
        hallow_db::migrate(&pool).await.unwrap();
        Some(Self {
            pool,
            url,
            admin,
            name,
        })
    }

    async fn drop(self) {
        self.pool.close().await;
        self.admin
            .execute(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE {} WITH (FORCE)",
                self.name
            )))
            .await
            .unwrap();
    }

    async fn events(&self) -> Vec<hallow_db::FleetEvent> {
        let mut events = hallow_db::fleet_events(&self.pool, 100_000, None, None)
            .await
            .unwrap();
        events.reverse();
        events
    }
}

fn fleet_file(floor_slots: u32, machine_slots: u32, setup_secs: u64) -> FleetConfig {
    FleetConfig::parse(&format!(
        r#"
        tick_secs = 1
        dry_run_setup_secs = {setup_secs}
        [[floor]]
        name = "floor"
        region = "fsn1"
        slots = {floor_slots}
        price = "40.00"
        [[machine]]
        type = "cpx41"
        region = "fsn1"
        slots = {machine_slots}
        per_hour = "0.0500"
        monthly_cap = "30.00"
        "#
    ))
    .unwrap()
}

// The simulated days

fn at(d: u32, h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, d, h, m, 0).unwrap()
}

/// Players online: a quiet morning, a two-minute spike at noon, an evening
/// rise to 330 by 20:00, a two-hour peak, and a fall to 15 by 23:00.
fn players_at(t: DateTime<Utc>) -> u32 {
    let m = (t - at(15, 0, 0)).num_minutes();
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

/// The network as the fleet manager sees it: players fill ready nodes, the
/// floor first; games on a draining node end over a few minutes; whoever
/// doesn't fit queues.
struct SimWorld {
    db: hallow_db::PgPool,
    now: Mutex<DateTime<Utc>>,
    state: Mutex<SimState>,
}

#[derive(Default)]
struct SimState {
    players: BTreeMap<String, u32>,
    queued_since: Option<DateTime<Utc>>,
    waits: Vec<(DateTime<Utc>, Duration)>,
    told_busy: u32,
    moved: u32,
}

impl SimWorld {
    fn set_now(&self, now: DateTime<Utc>) {
        *self.now.lock().unwrap() = now;
    }

    fn max_wait(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Duration {
        self.state
            .lock()
            .unwrap()
            .waits
            .iter()
            .filter(|(t, _)| *t >= from && *t < to)
            .map(|(_, w)| *w)
            .max()
            .unwrap_or_default()
    }
}

impl World for SimWorld {
    fn load(&self) -> WorldFuture<'_, Load> {
        Box::pin(async move {
            let now = *self.now.lock().unwrap();
            let rows = hallow_db::fleet_nodes(&self.db, now - TimeDelta::days(60))
                .await
                .map_err(|e| e.to_string())?;
            let mut state = self.state.lock().unwrap();
            let online = players_at(now);
            let mut on_draining = 0;
            for row in rows.iter().filter(|r| r.phase == "draining") {
                let p = state.players.get(&row.name).copied().unwrap_or(0);
                let left = p
                    .saturating_sub(p.div_ceil(5).max(1))
                    .min(online - on_draining);
                state.players.insert(row.name.clone(), left);
                on_draining += left;
            }
            let mut to_place = online - on_draining;
            for row in &rows {
                match row.phase.as_str() {
                    "ready" => {
                        let here = to_place.min(u32::try_from(row.slots).unwrap());
                        state.players.insert(row.name.clone(), here);
                        to_place -= here;
                    }
                    "draining" => {}
                    _ => {
                        state.players.remove(&row.name);
                    }
                }
            }
            let wait = if to_place > 0 {
                (now - *state.queued_since.get_or_insert(now))
                    .to_std()
                    .unwrap()
            } else {
                state.queued_since = None;
                Duration::ZERO
            };
            state.waits.push((now, wait));
            Ok(Load {
                nodes: state
                    .players
                    .iter()
                    .map(|(name, p)| {
                        (
                            name.clone(),
                            NodeUsage {
                                instances: p.div_ceil(10),
                                used_slots: *p,
                                players: *p,
                            },
                        )
                    })
                    .collect(),
                queues: if to_place > 0 {
                    vec![QueueLoad {
                        kind: "skywars".into(),
                        players: to_place,
                        wait,
                    }]
                } else {
                    vec![]
                },
                proxies: vec![ProxyLoad {
                    id: "proxy-1".into(),
                    players: online,
                    draining: false,
                }],
            })
        })
    }

    fn joined_nodes(&self) -> WorldFuture<'_, BTreeSet<String>> {
        Box::pin(async { Ok(BTreeSet::new()) })
    }

    fn clear_node<'a>(&'a self, node: &'a str, occupied_too: bool) -> WorldFuture<'a, u32> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if !occupied_too {
                return Ok(0);
            }
            let moved = state.players.remove(node).unwrap_or(0);
            state.moved += moved;
            Ok(moved.div_ceil(10))
        })
    }

    fn set_proxy_draining<'a>(&'a self, _: &'a str, _: bool) -> WorldFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }

    fn notify_queued<'a>(&'a self, _: &'a str) -> WorldFuture<'a, ()> {
        Box::pin(async move {
            self.state.lock().unwrap().told_busy += 1;
            Ok(())
        })
    }

    fn join_token<'a>(&'a self, _: &'a str, _: &'a str) -> WorldFuture<'a, String> {
        Box::pin(async { panic!("dry run asked for a join token") })
    }
}

struct Day {
    db: Db,
    world: Arc<SimWorld>,
    provider: Arc<Recording>,
    states: Vec<(DateTime<Utc>, FleetState)>,
}

impl Day {
    fn at_kind<'a>(
        events: &'a [hallow_db::FleetEvent],
        kind: &str,
        action: &str,
    ) -> Vec<&'a hallow_db::FleetEvent> {
        events
            .iter()
            .filter(|e| e.kind == kind && e.action == action)
            .collect()
    }
}

/// Runs a day from 06:00 to 06:00, one tick a minute, under `settings`.
async fn run_day(settings: FleetSettings) -> Option<Day> {
    let redis = env_or_skip("HALLOW_TEST_REDIS_URL")?;
    let db = Db::create().await?;
    hallow_db::save_fleet_settings(&db.pool, &settings, "test")
        .await
        .unwrap();
    let mut config = Config::new(redis.clone(), TOKEN);
    config.key_prefix = format!("test-{}", Uuid::new_v4());
    let client = redis::Client::open(redis.as_str()).unwrap();
    let store = Store::new(&client, &config).await.unwrap();
    let world = Arc::new(SimWorld {
        db: db.pool.clone(),
        now: Mutex::new(at(15, 6, 0)),
        state: Mutex::default(),
    });
    let provider = Arc::new(Recording::default());
    let manager = FleetManager::new(
        db.pool.clone(),
        store,
        world.clone(),
        provider.clone(),
        fleet_file(100, 100, 180),
    );
    // The floor node has run since the month began.
    manager.seed(at(1, 0, 0)).await.unwrap();
    let mut states = Vec::new();
    for minute in 0..24 * 60 {
        let now = at(15, 6, 0) + TimeDelta::minutes(minute);
        world.set_now(now);
        let report = manager.tick(now).await.unwrap();
        states.push((now, report.state));
    }
    let events = db.events().await;
    for e in &events {
        if e.kind != "node" {
            println!(
                "{} {:<8} {:<10} {}",
                e.at.format("%H:%M"),
                e.kind,
                e.action,
                e.message
            );
        }
    }
    Some(Day {
        db,
        world,
        provider,
        states,
    })
}

#[tokio::test]
async fn a_simulated_peak_in_dry_run_logs_the_right_buys_and_cancels() {
    let Some(day) = run_day(FleetSettings {
        monthly_budget: Some("500.00".into()),
        daily_cap: Some("40.00".into()),
        players_per_proxy: 0,
        ..FleetSettings::default()
    })
    .await
    else {
        return;
    };
    let events = day.db.events().await;
    assert_eq!(
        day.provider.calls.load(Ordering::SeqCst),
        0,
        "dry run never calls the provider"
    );
    assert!(events.iter().all(|e| e.dry_run), "every entry is dry run");

    // Three buys during the evening rise, none for the two-minute spike.
    let buys = Day::at_kind(&events, "decision", "buy");
    let creates = Day::at_kind(&events, "provider", "create");
    assert_eq!(buys.len(), 3, "{buys:?}");
    assert_eq!(creates.len(), 3);
    assert!(creates.iter().all(|e| e.ok));
    for e in &buys {
        assert!(e.at >= at(15, 17, 0) && e.at < at(15, 20, 0), "{e:?}");
        assert!(
            e.message.starts_with("buy cpx41 in fsn1 at 0.0500/h"),
            "{}",
            e.message
        );
    }
    assert!(day.world.max_wait(at(15, 12, 0), at(15, 12, 5)) > Duration::ZERO);
    assert_eq!(
        day.world.max_wait(at(15, 17, 0), at(15, 23, 0)),
        Duration::ZERO
    );

    // After the peak: one drain per cooldown, each node cancelled empty
    // just before its next billing hour, through dry run.
    let drains = Day::at_kind(&events, "decision", "drain");
    assert_eq!(drains.len(), 3);
    assert!(drains.iter().all(|e| e.at >= at(15, 22, 0)));
    for pair in drains.windows(2) {
        assert!(pair[1].at - pair[0].at >= TimeDelta::minutes(15));
    }
    let cancels = Day::at_kind(&events, "provider", "cancel");
    assert_eq!(cancels.len(), 3);
    let created: BTreeSet<_> = creates.iter().map(|e| e.node.clone().unwrap()).collect();
    let cancelled: BTreeSet<_> = cancels.iter().map(|e| e.node.clone().unwrap()).collect();
    assert_eq!(created, cancelled);
    for cancel in &cancels {
        assert!(cancel.ok && cancel.message.starts_with("dry run: cancelled"));
        let node = cancel.node.as_deref().unwrap();
        let bought = creates
            .iter()
            .find(|e| e.node.as_deref() == Some(node))
            .unwrap();
        let into_hour = (cancel.at - bought.at).num_minutes() % 60;
        assert!(
            (55..60).contains(&into_hour),
            "{node} {into_hour} min into its hour"
        );
    }
    assert_eq!(
        day.world.state.lock().unwrap().moved,
        0,
        "nobody had to be moved"
    );
    assert!(Day::at_kind(&events, "decision", "reinstate").is_empty());

    // Holds are logged once when they start, not every minute.
    let holds = Day::at_kind(&events, "decision", "hold");
    assert!(
        !holds.is_empty() && holds.len() < 60,
        "{} holds",
        holds.len()
    );
    assert!(Day::at_kind(&events, "alert", "capped").is_empty());

    // Back on the floor node alone, within budget.
    let nodes = hallow_db::fleet_nodes(&day.db.pool, at(15, 0, 0))
        .await
        .unwrap();
    let live: Vec<_> = nodes.iter().filter(|n| n.phase != "cancelled").collect();
    assert_eq!(live.len(), 1);
    assert!(live[0].floor);
    let (_, last) = day.states.last().unwrap();
    assert_eq!(last.nodes.len(), 1);
    assert!(last.capped.is_none());
    for (t, state) in &day.states {
        let today = hallow_fleet::Money::parse(&state.spend_today).unwrap();
        assert!(today <= hallow_fleet::Money::from_units(40), "{t}: {today}");
    }
    day.db.drop().await;
}

#[tokio::test]
async fn past_the_daily_cap_buying_stops_and_players_queue() {
    // Enough for the floor and one burst node this evening, not more.
    let cap = "1.70";
    let Some(day) = run_day(FleetSettings {
        monthly_budget: Some("500.00".into()),
        daily_cap: Some(cap.into()),
        players_per_proxy: 0,
        ..FleetSettings::default()
    })
    .await
    else {
        return;
    };
    let events = day.db.events().await;
    assert_eq!(day.provider.calls.load(Ordering::SeqCst), 0);

    // One node fits under the cap at the rise; then the cap holds buying
    // through the peak, and staff are alerted.
    let buys = Day::at_kind(&events, "decision", "buy");
    assert!(!buys.is_empty() && buys.len() < 3, "{buys:?}");
    assert_eq!(buys[0].at, at(15, 17, 22));
    let capped = Day::at_kind(&events, "alert", "capped");
    assert!(!capped.is_empty(), "the cap raised an alert");
    let first_cap = capped[0].at;
    assert!(
        capped[0].message.contains("daily cap"),
        "{}",
        capped[0].message
    );
    assert!(
        buys.iter()
            .all(|e| e.at < first_cap || e.at >= at(15, 22, 0)),
        "no buys while capped during the peak"
    );
    let peak = day
        .states
        .iter()
        .find(|(t, _)| *t == at(15, 20, 30))
        .map(|(_, s)| s)
        .unwrap();
    let cap_state = peak.capped.as_ref().expect("capped at the peak");
    assert_eq!(cap_state.kind, CapKind::DailyCap);
    assert!(
        peak.holds.iter().any(|h| h.contains("daily cap")),
        "{:?}",
        peak.holds
    );

    // Players queue instead, and are told the network is busy.
    assert!(day.world.max_wait(at(15, 19, 0), at(15, 22, 0)) > Duration::from_secs(60));
    assert!(day.world.state.lock().unwrap().told_busy >= 1);
    assert!(peak.queues.iter().any(|q| q.players > 0));

    // Nothing is drained or cut while capped, nor ever with players on it.
    for e in Day::at_kind(&events, "decision", "drain")
        .into_iter()
        .chain(Day::at_kind(&events, "decision", "cancel"))
    {
        assert!(e.at > at(15, 22, 0), "{e:?}");
    }
    assert_eq!(day.world.state.lock().unwrap().moved, 0);

    // The cap is kept, and lifted once the peak is over.
    let cap = hallow_fleet::Money::parse(cap).unwrap();
    for (t, state) in &day.states {
        let today = hallow_fleet::Money::parse(&state.spend_today).unwrap();
        assert!(today <= cap, "{t}: {today} over the cap");
    }
    assert!(!Day::at_kind(&events, "alert", "uncapped").is_empty());
    let (_, last) = day.states.last().unwrap();
    assert_eq!(last.nodes.len(), 1, "back on the floor node");
    day.db.drop().await;
}

#[tokio::test]
async fn past_the_monthly_budget_buying_stops_and_players_queue() {
    // The floor node alone forecasts about 39.45 this month.
    let Some(day) = run_day(FleetSettings {
        monthly_budget: Some("40.00".into()),
        daily_cap: None,
        players_per_proxy: 0,
        ..FleetSettings::default()
    })
    .await
    else {
        return;
    };
    let events = day.db.events().await;
    assert_eq!(day.provider.calls.load(Ordering::SeqCst), 0);
    let capped = Day::at_kind(&events, "alert", "capped");
    assert!(!capped.is_empty());
    assert!(
        capped[0].message.contains("monthly budget"),
        "{}",
        capped[0].message
    );
    assert!(day.world.max_wait(at(15, 19, 0), at(15, 22, 0)) > Duration::from_secs(60));
    // Every buy was made with the month-end forecast, the node included,
    // inside the budget; while the forecast is at the budget nothing is
    // bought.
    let budget = hallow_fleet::Money::from_units(40);
    let buys = Day::at_kind(&events, "decision", "buy");
    assert!(!buys.is_empty());
    for buy in &buys {
        let (_, state) = day.states.iter().find(|(t, _)| *t == buy.at).unwrap();
        let forecast = hallow_fleet::Money::parse(&state.forecast).unwrap();
        assert!(
            forecast < budget,
            "bought at {} with forecast {forecast}",
            buy.at
        );
        assert!(state.capped.is_none() || state.capped.as_ref().unwrap().kind != CapKind::DailyCap);
    }
    for (t, state) in &day.states {
        let forecast = hallow_fleet::Money::parse(&state.forecast).unwrap();
        if forecast >= budget {
            assert!(buys.iter().all(|b| b.at != *t), "{t}: bought at the budget");
        }
    }
    let capped_ticks = day
        .states
        .iter()
        .filter(|(_, s)| {
            s.capped
                .as_ref()
                .is_some_and(|c| c.kind == CapKind::MonthlyBudget)
        })
        .count();
    assert!(capped_ticks > 60, "capped for {capped_ticks} minutes");
    let (_, last) = day.states.last().unwrap();
    assert_eq!(last.nodes.len(), 1);
    day.db.drop().await;
}

// Scale to one, live

struct Live {
    base: String,
    http: reqwest::Client,
    mock: Arc<MockOrchestrator>,
    provider: Arc<Recording>,
    db: Db,
}

impl Live {
    async fn start() -> Option<Self> {
        let redis = env_or_skip("HALLOW_TEST_REDIS_URL")?;
        let db = Db::create().await?;
        let mut config = Config::new(redis, TOKEN);
        config.key_prefix = format!("test-{}", Uuid::new_v4());
        config.database_url = Some(db.url.clone());
        config.expiry = Duration::from_secs(120);
        let mut setup = InstancesSetup::new(
            OrchestratorChoice::Docker(DockerConfig::new(
                Endpoint::Unix("/nonexistent".into()),
                "unused",
            )),
            "control-plane:8080",
        );
        setup.settings = Settings {
            start_timeout_ms: 30_000,
            empty_grace_ms: 300,
            idle_ms: 300,
            retry_ms: 300,
            stuck_ms: 30_000,
        };
        config.instances = Some(setup);
        // The floor node holds two games of four.
        let mut fleet = fleet_file(8, 8, 1);
        fleet.floor[0].name = "dock".into();
        config.fleet = Some(FleetSetup::new(fleet));
        let mock = Arc::new(MockOrchestrator::new(&["dock"]));
        let provider = Arc::new(Recording::default());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let orchestrator: Arc<dyn Orchestrator> = mock.clone();
        let real: Arc<dyn hallow_control_plane::fleet_manager::DynProvider> = provider.clone();
        tokio::spawn(async move {
            serve_custom(
                listener,
                None,
                config,
                Overrides {
                    panel: None,
                    orchestrator: Some(orchestrator),
                    provider: Some(real),
                },
            )
            .await
            .unwrap();
        });
        let http = reqwest::Client::new();
        for _ in 0..100 {
            if http
                .get(format!("{base}/healthz"))
                .send()
                .await
                .is_ok_and(|r| r.status() == StatusCode::NO_CONTENT)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Some(Self {
            base,
            http,
            mock,
            provider,
            db,
        })
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> reqwest::Response {
        req.bearer_auth(TOKEN).send().await.unwrap()
    }

    async fn post<B: Serialize>(&self, path: &str, body: &B) -> reqwest::Response {
        self.send(self.http.post(format!("{}{path}", self.base)).json(body))
            .await
    }

    async fn put<B: Serialize>(&self, path: &str, body: &B) -> reqwest::Response {
        self.send(self.http.put(format!("{}{path}", self.base)).json(body))
            .await
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> T {
        let res = self
            .send(self.http.get(format!("{}{path}", self.base)))
            .await;
        assert!(res.status().is_success(), "GET {path}: {}", res.status());
        res.json().await.unwrap()
    }

    async fn instances(&self) -> Vec<InstanceInfo> {
        self.get_json(routes::INSTANCES).await
    }

    async fn fleet(&self) -> FleetState {
        self.get_json(routes::FLEET).await
    }

    async fn wait_instances(
        &self,
        what: &str,
        check: impl Fn(&[InstanceInfo]) -> bool,
    ) -> Vec<InstanceInfo> {
        for _ in 0..200 {
            let list = self.instances().await;
            if check(&list) {
                return list;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting for {what}: {:?}", self.instances().await);
    }

    async fn wait_fleet(&self, what: &str, check: impl Fn(&FleetState) -> bool) -> FleetState {
        for _ in 0..200 {
            let state = self.fleet().await;
            if check(&state) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting for {what}: {:?}", self.fleet().await);
    }

    async fn online_player(&self) -> Uuid {
        let uuid = Uuid::new_v4();
        let res = self
            .post(
                routes::PRESENCE_LOGIN,
                &LoginRequest {
                    uuid,
                    username: format!("p{}", &uuid.simple().to_string()[..6]),
                    proxy: "proxy-1".into(),
                    protocol: Some(777),
                    ip: None,
                },
            )
            .await;
        let decision: LoginDecision = res.json().await.unwrap();
        assert!(
            matches!(decision, LoginDecision::Allow { .. }),
            "{decision:?}"
        );
        uuid
    }

    async fn queue(&self, player: Uuid) -> QueueStatus {
        let res = self
            .post(
                routes::QUEUES,
                &JoinQueue {
                    kind: "test-game".into(),
                    player,
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        res.json().await.unwrap()
    }

    async fn boot(&self, name: &str) {
        let res = self
            .post(
                routes::SERVERS,
                &RegisterServer {
                    name: name.into(),
                    kind: "test-game".into(),
                    version: "dev".into(),
                    role: ServerRole::Game,
                    address: format!("{name}:25565"),
                    max_players: 4,
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    async fn heartbeat(&self, name: &str, players: &[Uuid], state: ServerState) {
        let beat = ServerHeartbeat {
            players: u32::try_from(players.len()).unwrap(),
            state,
            tick_ms: None,
            roster: Some(
                players
                    .iter()
                    .map(|uuid| ServerPlayer {
                        uuid: *uuid,
                        username: format!("p{}", &uuid.simple().to_string()[..6]),
                    })
                    .collect(),
            ),
        };
        let res = self.post(&routes::server_heartbeat(name), &beat).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    /// Where the matchmaker sent each player, from the proxy's commands.
    async fn sends(&self) -> BTreeMap<Uuid, String> {
        let poll: Poll<Vec<ProxyCommand>> = self
            .get_json(&routes::since_wait(
                &routes::proxy_commands("proxy-1"),
                0,
                0,
            ))
            .await;
        let mut sent = BTreeMap::new();
        if let Poll::Changed { data, .. } = poll {
            for command in data {
                if let ProxyCommand::Send { player, server } = command {
                    sent.insert(player, server);
                }
            }
        }
        sent
    }

    async fn events(&self, kind: &str) -> Vec<FleetEvent> {
        self.get_json(&format!("{}&kind={kind}", routes::fleet_events(500)))
            .await
    }
}

/// #50's goal: after the last player leaves, the network settles on the
/// floor node alone, and a new player can still join and get a game.
#[tokio::test]
async fn after_the_last_player_leaves_the_floor_node_alone_still_gets_a_game() {
    let Some(env) = Live::start().await else {
        return;
    };
    // Fast timers; no budget in the way; proxies left alone.
    let res = env
        .put(
            routes::FLEET_SETTINGS,
            &serde_json::json!({
                "updated_by": "test",
                "settings": {
                    "headroom_slots": 4,
                    "scale_up_after_secs": 0,
                    "scale_down_after_secs": 1,
                    "scale_up_cooldown_secs": 0,
                    "scale_down_cooldown_secs": 0,
                    "billing_step_margin_secs": 3599,
                    "monthly_budget": null,
                    "daily_cap": null,
                    "players_per_proxy": 0
                }
            }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = env
        .put(
            &routes::kind("test-game"),
            &KindSpec {
                image: "hallow-test-game:dev".into(),
                role: KindRole::Game,
                lobby: Some("hub".into()),
                players: KindPlayers { min: 1, max: 4 },
                scaling: KindScaling {
                    warm: 0,
                    max_instances: Some(10),
                },
                env: BTreeMap::new(),
                memory_mb: None,
                updated_by: None,
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = env
        .post(
            routes::PROXIES,
            &RegisterProxy {
                id: "proxy-1".into(),
                address: "proxy-1:25565".into(),
                version: "test".into(),
            },
        )
        .await;
    assert!(res.status().is_success());
    let res = env
        .post(
            routes::SERVERS,
            &RegisterServer {
                name: "lobby".into(),
                kind: "hub".into(),
                version: "dev".into(),
                role: ServerRole::Lobby,
                address: "lobby:25565".into(),
                max_players: 100,
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Quiet: the floor node alone, nothing bought.
    let quiet = env
        .wait_fleet("the first tick", |s| s.last_tick_ms.is_some())
        .await;
    assert!(quiet.enabled && quiet.dry_run);
    assert_eq!(quiet.nodes.len(), 1);
    assert!(quiet.nodes[0].floor && quiet.nodes[0].name == "dock");
    assert_eq!(quiet.nodes[0].phase, FleetNodePhase::Ready);
    // The instance lifecycle caches its node list for two seconds.
    tokio::time::sleep(Duration::from_millis(2500)).await;

    // Twelve players queue for three games of four. The floor node holds
    // two; the fleet buys a dry-run node for the third.
    let mut players = Vec::new();
    for _ in 0..12 {
        let player = env.online_player().await;
        env.queue(player).await;
        players.push(player);
    }
    let three = env
        .wait_instances("three instances", |list| list.len() == 3)
        .await;
    let on_floor = three.iter().filter(|i| i.node == "dock").count();
    assert_eq!(on_floor, 2, "{three:?}");
    let burst = three
        .iter()
        .find(|i| i.node != "dock")
        .unwrap()
        .node
        .clone();
    assert!(burst.starts_with("hallow-burst-"), "{burst}");
    let fleet = env.fleet().await;
    let node = fleet.nodes.iter().find(|n| n.name == burst).unwrap();
    assert!(node.dry_run && node.kind == "burst" && node.slots == 8);
    assert_eq!(node.per_hour, "0.0500");

    // Each game boots and gets its four players.
    for instance in &three {
        env.boot(&instance.name).await;
    }
    let mut sent = BTreeMap::new();
    for _ in 0..100 {
        sent = env.sends().await;
        if sent.len() == 12 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(sent.len(), 12, "every player got a game");
    for instance in &three {
        let here: Vec<Uuid> = sent
            .iter()
            .filter(|(_, s)| **s == instance.name)
            .map(|(p, _)| *p)
            .collect();
        assert_eq!(here.len(), 4);
        env.heartbeat(&instance.name, &here, ServerState::InGame)
            .await;
    }
    env.wait_instances("every game to count its players", |list| {
        list.iter().all(|i| i.had_players)
    })
    .await;

    // The games end and everyone leaves.
    for instance in &three {
        env.heartbeat(&instance.name, &[], ServerState::InGame)
            .await;
    }
    for player in &players {
        let res = env
            .post(
                routes::PRESENCE_LEAVE,
                &hallow_proto::presence::PlayerLeft {
                    uuid: *player,
                    proxy: "proxy-1".into(),
                },
            )
            .await;
        assert!(res.status().is_success());
    }
    env.wait_instances("every game deleted", <[InstanceInfo]>::is_empty)
        .await;

    // The burst node is drained and cancelled: the floor node alone.
    let settled = env
        .wait_fleet("the floor node alone", |s| s.nodes.len() == 1)
        .await;
    assert_eq!(settled.nodes[0].name, "dock");
    let decisions = env.events("decision").await;
    let actions: Vec<&str> = decisions
        .iter()
        .rev()
        .filter(|e| e.action != "hold")
        .map(|e| e.action.as_str())
        .collect();
    assert_eq!(actions, ["buy", "drain", "cancel"], "{decisions:?}");
    let calls = env.events("provider").await;
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls.iter().all(|e| e.dry_run && e.ok));
    assert_eq!(calls[0].action, "cancel");
    assert_eq!(calls[1].action, "create");
    assert_eq!(calls[0].node.as_deref(), Some(burst.as_str()));
    let rows = hallow_db::fleet_nodes(&env.db.pool, Utc::now() - TimeDelta::days(1))
        .await
        .unwrap();
    let row = rows.iter().find(|r| r.name == burst).unwrap();
    assert_eq!(row.phase, "cancelled");

    // A new player arrives: a game starts on the floor node and they get it.
    let player = env.online_player().await;
    env.queue(player).await;
    let one = env
        .wait_instances("a game for the new player", |list| list.len() == 1)
        .await;
    assert_eq!(one[0].node, "dock");
    env.boot(&one[0].name).await;
    let mut got = None;
    for _ in 0..100 {
        got = env.sends().await.get(&player).cloned();
        if got.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(got.as_deref(), Some(one[0].name.as_str()));
    let still = env.fleet().await;
    assert_eq!(still.nodes.len(), 1, "no new node for one player");
    assert_eq!(
        env.provider.calls.load(Ordering::SeqCst),
        0,
        "dry run never called the provider"
    );
    assert!(
        env.mock
            .calls()
            .iter()
            .filter(|c| matches!(c, hallow_control_plane::orchestrator::mock::Call::Create { node, .. } if *node == burst))
            .count()
            == 1
    );

    // The API: staff can't drain the floor; unknown nodes are 404; dry run
    // toggles through the settings, with its history and the audit log.
    let res = env
        .post(
            &routes::fleet_node_drain("dock"),
            &FleetNodeAction { by: "staff".into() },
        )
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.code, ErrorCode::Conflict);
    let res = env
        .post(
            &routes::fleet_node_drain("nope"),
            &FleetNodeAction { by: "staff".into() },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = env
        .post(
            &routes::fleet_node_reinstate(&burst),
            &FleetNodeAction { by: "staff".into() },
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "cancelled nodes stay cancelled"
    );
    let res = env
        .put(
            routes::FLEET_DRY_RUN,
            &SetDryRun {
                dry_run: false,
                updated_by: "staff".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let record: FleetSettingsRecord = res.json().await.unwrap();
    assert!(!record.settings.dry_run);
    assert_eq!(record.settings.headroom_slots, 4, "other settings kept");
    let res = env
        .put(
            routes::FLEET_DRY_RUN,
            &SetDryRun {
                dry_run: true,
                updated_by: "staff".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let staff = env.events("staff").await;
    assert_eq!(staff.len(), 2);
    assert_eq!(staff[0].by.as_deref(), Some("staff"));
    assert_eq!(
        staff[1].message,
        "staff turned dry run off: providers will be called"
    );
    let history: Vec<hallow_proto::fleet::FleetSettingsChange> =
        env.get_json(&routes::fleet_settings_history(10)).await;
    assert_eq!(history.len(), 3);
    let res = env
        .send(env.http.get(format!(
            "{}{}&kind=bogus",
            env.base,
            routes::fleet_events(5)
        )))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let metrics = env
        .http
        .get(format!("{}/metrics", env.base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("hallow_fleet_nodes{kind=\"base\",phase=\"ready\"} 1"),
        "{metrics}"
    );
    assert!(metrics.contains("hallow_fleet_dry_run "));
    let _ = (InstanceState::Ready, FleetEventKind::Staff);
    env.db.drop().await;
}
