//! Hallow's control plane: the registry of servers and proxies, player
//! presence, and cross-proxy commands, served over the HTTP/JSON API in
//! `hallow-proto` ([Decisions] 12).
//!
//! Every piece of live state is in Redis ([`store`]), so any number of control
//! plane instances can run behind one address. Writes publish an event on a
//! Redis channel; each instance listens to it ([`events`]) and wakes the long
//! polls it holds, so a proxy polling one instance sees a change made through
//! another straight away.
//!
//! [Decisions]: https://github.com/xena-studios/hallow/blob/main/docs/DECISIONS.md

mod accounts;
mod agent;
mod api;
pub mod auth;
mod error;
pub mod events;
mod fleet;
mod friends;
pub mod instances;
mod kinds;
pub mod lobbies;
mod metrics;
mod nodes;
pub mod orchestrator;
pub mod panel;
mod punish;
mod ranks;
mod remote;
mod signals;
mod stats;
pub mod store;
pub mod versions;

use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use hallow_proto::routes::{HEARTBEAT_SECONDS, LONG_POLL_SECONDS};
use tokio::net::TcpListener;

/// What happens when a player logs in while already online on another live proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicateLogin {
    /// Refuse the new login; the player stays where they are.
    Deny,
    /// Kick the old session and let the new one in.
    KickOld,
}

/// Which orchestrator runs instances.
#[derive(Clone, Debug)]
pub enum OrchestratorChoice {
    /// The local Docker Engine (dev network).
    Docker(orchestrator::docker::DockerConfig),
    /// Calagopus Wings on the nodes that joined; needs the panel and the
    /// database.
    Wings,
}

/// Instance orchestration settings (docs/INSTANCES.md).
#[derive(Clone, Debug)]
pub struct InstancesSetup {
    pub orchestrator: OrchestratorChoice,
    /// `host:port` instances reach the control plane at.
    pub control_plane: String,
    /// Environment for every instance, such as `HALLOW_VINE_SECRET`.
    pub env: BTreeMap<String, String>,
    pub settings: instances::Settings,
    /// Kinds to register at start (`HALLOW_KINDS_FILE`).
    pub kinds_file: Option<PathBuf>,
}

impl InstancesSetup {
    #[must_use]
    pub fn new(orchestrator: OrchestratorChoice, control_plane: impl Into<String>) -> Self {
        Self {
            orchestrator,
            control_plane: control_plane.into(),
            env: BTreeMap::new(),
            settings: instances::Settings::default(),
            kinds_file: None,
        }
    }
}

/// Settings, read from the environment by the binary.
#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub redis_url: String,
    /// The shared bearer token, good for every route.
    pub token: String,
    /// Scoped tokens for servers the control plane doesn't start
    /// (`HALLOW_SERVER_TOKENS`, see [`auth`]).
    pub server_tokens: Vec<auth::StaticServerToken>,
    /// Prefix for every Redis key and channel, so several networks (or tests)
    /// can share one Redis.
    pub key_prefix: String,
    /// The server kind players land on after login, such as `hub`.
    pub hub_kind: String,
    /// The network's player limit, shown in the server list.
    pub max_players: u32,
    pub duplicate_login: DuplicateLogin,
    /// PostgreSQL for accounts, bans, ranks and sessions. Without it the
    /// control plane keeps only live state and nobody has permissions.
    pub database_url: Option<String>,
    /// Usernames given the `admin` rank (`*`) when they log in, to bootstrap
    /// a new network. Needs `database_url`.
    pub admins: Vec<String>,
    /// How long anticheat signal samples are kept.
    pub signal_retention_days: u32,
    /// The panel, for joining nodes. Without it node joins are off.
    pub panel: Option<panel::PanelConfig>,
    /// How long a node join token lasts unless the request says otherwise.
    pub join_token_ttl: Duration,
    /// Instance orchestration; `None` leaves servers to whoever starts them.
    /// Needs `database_url` (the kinds registry).
    pub instances: Option<InstancesSetup>,
    /// A proxy or server that hasn't heartbeated for this long is removed.
    pub expiry: Duration,
    /// How long a long poll is held before answering `unchanged`.
    pub long_poll: Duration,
    /// Which versions may go live (docs/SERVERS.md#versions).
    pub versions: versions::VersionsConfig,
}

impl Config {
    /// Defaults for everything but the Redis URL and token.
    #[must_use]
    pub fn new(redis_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 8080)),
            redis_url: redis_url.into(),
            token: token.into(),
            server_tokens: Vec::new(),
            key_prefix: "hallow".into(),
            hub_kind: "hub".into(),
            max_players: 1000,
            duplicate_login: DuplicateLogin::Deny,
            database_url: None,
            admins: Vec::new(),
            signal_retention_days: 14,
            panel: None,
            join_token_ttl: Duration::from_secs(24 * 3600),
            instances: None,
            // Three missed heartbeats.
            expiry: Duration::from_secs(HEARTBEAT_SECONDS * 3),
            long_poll: Duration::from_secs(LONG_POLL_SECONDS),
            versions: versions::VersionsConfig::default(),
        }
    }
}

/// Shared by every request handler.
pub struct AppState {
    pub config: Config,
    pub store: store::Store,
    pub events: events::Events,
    pub db: Option<hallow_db::PgPool>,
    /// The panel's admin API, built from `config.panel`.
    pub panel: Option<Arc<dyn panel::Panel>>,
    /// The instance lifecycle, when `config.instances` is set.
    pub instances: Option<Arc<instances::Manager>>,
}

/// Connects to Redis, starts the event listener and the reaper, and serves the
/// API on `listener` until the process ends.
pub async fn serve(
    listener: TcpListener,
    config: Config,
) -> Result<(), Box<dyn std::error::Error>> {
    serve_all(listener, None, config).await
}

/// [`serve`], plus HAProxy agent checks on `agent` (see the `agent` module).
pub async fn serve_all(
    listener: TcpListener,
    agent: Option<TcpListener>,
    config: Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let panel = match &config.panel {
        Some(panel_config) => {
            Some(Arc::new(panel::HttpPanel::new(panel_config)?) as Arc<dyn panel::Panel>)
        }
        None => None,
    };
    serve_with_panel(listener, agent, config, panel).await
}

/// [`serve_all`] with a given panel client instead of one built from
/// `config.panel` (which must still be set for node joins).
pub async fn serve_with_panel(
    listener: TcpListener,
    agent: Option<TcpListener>,
    config: Config,
    panel: Option<Arc<dyn panel::Panel>>,
) -> Result<(), Box<dyn std::error::Error>> {
    serve_with(listener, agent, config, panel, None).await
}

/// [`serve_with_panel`] with a given orchestrator instead of the one
/// `config.instances` names (which must still be set).
pub async fn serve_with(
    listener: TcpListener,
    agent: Option<TcpListener>,
    config: Config,
    panel: Option<Arc<dyn panel::Panel>>,
    orchestrator: Option<Arc<dyn orchestrator::Orchestrator>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = redis::Client::open(config.redis_url.as_str())?;
    let store = store::Store::new(&client, &config).await?;
    let events = events::Events::listen(client, store.keys().channel());
    let db = match &config.database_url {
        Some(url) => {
            let pool = hallow_db::connect(url).await?;
            hallow_db::migrate(&pool).await?;
            tracing::info!("database connected and migrated");
            Some(pool)
        }
        None => {
            tracing::warn!("no database: accounts, bans, ranks and sessions are off");
            None
        }
    };
    if panel.is_none() {
        tracing::warn!("no panel: node joins are off");
    }
    let instances = match &config.instances {
        Some(setup) => {
            Some(start_instances(setup, &config, db.as_ref(), panel.as_ref(), orchestrator).await?)
        }
        None => None,
    };
    let state = Arc::new(AppState {
        config,
        store,
        events,
        db,
        panel,
        instances,
    });
    tokio::spawn(reaper(state.clone()));
    tokio::spawn(matchmaker(state.clone()));
    if state.instances.is_some() {
        tokio::spawn(sweeper(state.clone()));
    }
    tokio::spawn(signals::retention(state.clone()));
    tokio::spawn(nodes::retention(state.clone()));
    if let Some(agent) = agent {
        tracing::info!(address = %agent.local_addr()?, "answering load balancer agent checks");
        tokio::spawn(agent::serve(agent, state.clone()));
    }
    tracing::info!(address = %listener.local_addr()?, "control plane listening");
    axum::serve(listener, api::router(state)).await?;
    Ok(())
}

/// Builds the instance lifecycle and registers the kinds file's kinds.
async fn start_instances(
    setup: &InstancesSetup,
    config: &Config,
    db: Option<&hallow_db::PgPool>,
    panel: Option<&Arc<dyn panel::Panel>>,
    given: Option<Arc<dyn orchestrator::Orchestrator>>,
) -> Result<Arc<instances::Manager>, Box<dyn std::error::Error>> {
    let db =
        db.ok_or("instances need the database (HALLOW_DATABASE_URL) for the kinds registry")?;
    let mut wings = None;
    let orchestrator: Arc<dyn orchestrator::Orchestrator> = match (given, &setup.orchestrator) {
        (Some(given), _) => given,
        (None, OrchestratorChoice::Docker(docker)) => Arc::new(
            orchestrator::docker::DockerOrchestrator::new(docker.clone()),
        ),
        (None, OrchestratorChoice::Wings) => {
            let panel = panel.ok_or("Wings instances need the panel (HALLOW_PANEL_URL)")?;
            let panel_config = config
                .panel
                .as_ref()
                .ok_or("Wings instances need the panel")?;
            let built = Arc::new(orchestrator::wings::WingsOrchestrator::new(
                db.clone(),
                panel.clone(),
                orchestrator::wings::WingsConfig {
                    wings_port: panel_config.wings_port,
                    game_ports: panel_config.game_ports,
                    owner: config.key_prefix.clone(),
                },
            )?);
            wings = Some(built.clone());
            built
        }
    };
    if let Some(path) = &setup.kinds_file {
        for (name, spec) in instances::read_kinds_file(path)? {
            hallow_db::register_kind(db, &name, &spec).await?;
            tracing::info!(kind = %name, image = %spec.image, "kind registered from the kinds file");
        }
    }
    tracing::info!(orchestrator = orchestrator.name(), "instances on");
    Ok(Arc::new(instances::Manager::new(
        orchestrator,
        wings,
        db.clone(),
        instances::InstancesConfig {
            settings: setup.settings,
            control_plane: setup.control_plane.clone(),
            env: setup.env.clone(),
        },
    )))
}

/// Removes proxies and servers whose heartbeats stopped. Every instance runs
/// one; removal is idempotent, so they don't step on each other.
async fn reaper(state: Arc<AppState>) {
    let every = (state.config.expiry / 3).max(Duration::from_millis(200));
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match state.store.reap().await {
            Ok(reaped) => {
                for name in &reaped.servers {
                    tracing::info!(server = %name, "server expired");
                }
                for id in &reaped.proxies {
                    tracing::info!(proxy = %id, "proxy expired");
                    if let Some(db) = &state.db
                        && let Err(err) = hallow_db::close_proxy_sessions(db, id).await
                    {
                        tracing::warn!(%err, proxy = %id, "couldn't close its sessions");
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "reaper couldn't reach Redis"),
        }
    }
}

/// Plans instances and places queued players every half second. Every
/// instance runs one; a lock in Redis lets only one pass run at a time.
async fn matchmaker(state: Arc<AppState>) {
    let instance = uuid::Uuid::new_v4().to_string();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match state.store.lock_matchmaker(&instance).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(err) => {
                tracing::warn!(%err, "matchmaker couldn't reach Redis");
                continue;
            }
        }
        let work = match &state.instances {
            Some(manager) => match manager.plan(&state.store).await {
                Ok(work) => work,
                Err(err) => {
                    tracing::warn!(%err, "couldn't plan instances");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        let placed = state.store.place_all().await;
        if let Err(err) = state.store.unlock_matchmaker(&instance).await {
            tracing::warn!(%err, "matchmaker couldn't unlock");
        }
        if let Some(manager) = &state.instances {
            manager.run(&state, work);
        }
        match placed {
            Ok(placements) => {
                for placed in placements {
                    tracing::info!(
                        kind = %placed.kind,
                        server = %placed.server,
                        players = placed.players.len(),
                        "matched"
                    );
                }
            }
            Err(err) => tracing::warn!(%err, "matchmaker couldn't reach Redis"),
        }
    }
}

/// Every 30 s, deletes instances the orchestrator runs that have no record.
async fn sweeper(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(manager) = &state.instances else {
            return;
        };
        if let Err(err) = manager.sweep(&state.store).await {
            tracing::warn!(%err, "sweep couldn't reach Redis");
        }
    }
}
