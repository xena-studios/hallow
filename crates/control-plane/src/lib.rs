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
mod error;
pub mod events;
pub mod store;

use std::{net::SocketAddr, sync::Arc, time::Duration};

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

/// Settings, read from the environment by the binary.
#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub redis_url: String,
    /// The bearer token every request must carry.
    pub token: String,
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
    /// A proxy or server that hasn't heartbeated for this long is removed.
    pub expiry: Duration,
    /// How long a long poll is held before answering `unchanged`.
    pub long_poll: Duration,
}

impl Config {
    /// Defaults for everything but the Redis URL and token.
    #[must_use]
    pub fn new(redis_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 8080)),
            redis_url: redis_url.into(),
            token: token.into(),
            key_prefix: "hallow".into(),
            hub_kind: "hub".into(),
            max_players: 1000,
            duplicate_login: DuplicateLogin::Deny,
            database_url: None,
            admins: Vec::new(),
            // Three missed heartbeats.
            expiry: Duration::from_secs(HEARTBEAT_SECONDS * 3),
            long_poll: Duration::from_secs(LONG_POLL_SECONDS),
        }
    }
}

/// Shared by every request handler.
pub struct AppState {
    pub config: Config,
    pub store: store::Store,
    pub events: events::Events,
    pub db: Option<hallow_db::PgPool>,
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
    let state = Arc::new(AppState {
        config,
        store,
        events,
        db,
    });
    tokio::spawn(reaper(state.clone()));
    tokio::spawn(matchmaker(state.clone()));
    if let Some(agent) = agent {
        tracing::info!(address = %agent.local_addr()?, "answering load balancer agent checks");
        tokio::spawn(agent::serve(agent, state.clone()));
    }
    tracing::info!(address = %listener.local_addr()?, "control plane listening");
    axum::serve(listener, api::router(state)).await?;
    Ok(())
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

/// Places queued players every half second. Every instance runs one; a lock
/// in Redis lets only one pass run at a time.
async fn matchmaker(state: Arc<AppState>) {
    let instance = uuid::Uuid::new_v4().to_string();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match state.store.match_pass(&instance).await {
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
