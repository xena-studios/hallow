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
}

/// Connects to Redis, starts the event listener and the reaper, and serves the
/// API on `listener` until the process ends.
pub async fn serve(
    listener: TcpListener,
    config: Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = redis::Client::open(config.redis_url.as_str())?;
    let store = store::Store::new(&client, &config).await?;
    let events = events::Events::listen(client, store.keys().channel());
    let state = Arc::new(AppState {
        config,
        store,
        events,
    });
    tokio::spawn(reaper(state.clone()));
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
                }
            }
            Err(err) => tracing::warn!(%err, "reaper couldn't reach Redis"),
        }
    }
}
