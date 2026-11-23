//! The public API (`hallow_proto::public`): read-only player profiles,
//! search and leaderboards, with no token, for the player website and
//! anyone else.
//!
//! - **Rate limited** per client address (IPv6 by `/64`): a token bucket of
//!   [`PublicConfig::burst`] requests refilled at
//!   [`PublicConfig::per_minute`]. Over it, `429` with `Retry-After`. Each
//!   control plane instance keeps its own buckets.
//! - **Client address.** The peer's, unless the peer is one of
//!   [`PublicConfig::trusted_proxies`] (such as the player website), whose
//!   `X-Forwarded-For` is followed back to the first address that isn't.
//! - **Cached** for [`CACHE_FOR`]: answers are kept per path and query, so a
//!   busy page reads PostgreSQL once every few seconds.
//! - **Open to browsers**: `Access-Control-Allow-Origin: *`.
//!
//! It shows nothing a player couldn't see in game: no addresses, sessions,
//! punishments or which server someone is on.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use hallow_proto::{
    ErrorCode,
    public::{
        self, BoardKind, NetworkSummary, PlayerMatch, PlayerProfile, RECENT_GAMES, RecentGame,
        SEARCH_MAX,
    },
    stats::Window,
};
use ipnet::IpNet;
use serde::Deserialize;
use uuid::Uuid;

use crate::{AppState, accounts::resolve, error::Failure, stats};

/// How long an answer is served from the cache.
pub const CACHE_FOR: Duration = Duration::from_secs(10);
/// Past this many cached answers or buckets, the stale ones are dropped.
const MAX_ENTRIES: usize = 10_000;

/// Public API settings.
#[derive(Clone, Debug)]
pub struct PublicConfig {
    /// Off answers `404` on every public route.
    pub enabled: bool,
    /// Requests a client may make per minute, on average.
    pub per_minute: u32,
    /// Requests a client may make at once.
    pub burst: u32,
    /// Peers whose `X-Forwarded-For` is believed.
    pub trusted_proxies: Vec<IpNet>,
}

impl Default for PublicConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            per_minute: 120,
            burst: 40,
            trusted_proxies: Vec::new(),
        }
    }
}

/// Parses `HALLOW_PUBLIC_TRUSTED_PROXIES`: comma-separated addresses or
/// CIDR ranges.
pub fn parse_trusted(list: &str) -> Result<Vec<IpNet>, String> {
    list.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|entry| {
            entry
                .parse::<IpNet>()
                .or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| format!("{entry} isn't an address or CIDR range"))
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Buckets and cached answers, shared by the public routes.
pub struct Public {
    config: PublicConfig,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
    cache: Mutex<HashMap<String, (Instant, Bytes)>>,
}

impl Public {
    #[must_use]
    pub fn new(config: PublicConfig) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Takes a request from `client`'s bucket; how long to wait if it's
    /// empty.
    fn take(&self, client: IpAddr, now: Instant) -> Result<(), Duration> {
        let burst = f64::from(self.config.burst.max(1));
        let rate = f64::from(self.config.per_minute.max(1)) / 60.0;
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() > MAX_ENTRIES {
            // Buckets that would be full again are no different from new.
            buckets.retain(|_, b| b.tokens + now.duration_since(b.at).as_secs_f64() * rate < burst);
        }
        let bucket = buckets.entry(client).or_insert(Bucket {
            tokens: burst,
            at: now,
        });
        bucket.tokens =
            (bucket.tokens + now.duration_since(bucket.at).as_secs_f64() * rate).min(burst);
        bucket.at = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / rate))
        }
    }

    fn cached(&self, key: &str, now: Instant) -> Option<Bytes> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(key)
            .filter(|(at, _)| now.duration_since(*at) < CACHE_FOR)
            .map(|(_, body)| body.clone())
    }

    fn keep(&self, key: String, body: Bytes, now: Instant) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= MAX_ENTRIES {
            cache.retain(|_, (at, _)| now.duration_since(*at) < CACHE_FOR);
            if cache.len() >= MAX_ENTRIES {
                cache.clear();
            }
        }
        cache.insert(key, (now, body));
    }

    /// Who is asking: the peer, or for a trusted peer, the last address in
    /// `X-Forwarded-For` that isn't trusted. IPv6 by `/64`.
    fn client(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let trusted = |ip: &IpAddr| self.config.trusted_proxies.iter().any(|n| n.contains(ip));
        let mut client = peer;
        if trusted(&peer) {
            let forwarded = headers
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(','))
                .filter_map(|v| v.trim().parse::<IpAddr>().ok())
                .collect::<Vec<_>>();
            for ip in forwarded.into_iter().rev() {
                client = ip;
                if !trusted(&ip) {
                    break;
                }
            }
        }
        match client {
            IpAddr::V6(v6) => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
            v4 => v4,
        }
    }
}

/// The public routes, under `/public/v1`.
pub fn router(state: &Arc<AppState>) -> Router<Arc<AppState>> {
    let public = Arc::new(Public::new(state.config.public.clone()));
    Router::new()
        .route(public::NETWORK, get(network))
        .route(public::SEARCH, get(search))
        .route("/public/v1/players/{player}", get(profile))
        .route(public::LEADERBOARDS, get(boards))
        .route("/public/v1/leaderboards/{kind}/{stat}", get(leaderboard))
        .route_layer(middleware::from_fn_with_state(public, guard))
}

/// Rate limits, serves from the cache, and marks answers public.
async fn guard(State(public): State<Arc<Public>>, request: Request, next: Next) -> Response {
    if !public.config.enabled {
        return Failure::not_found("the public API is off").into_response();
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::from([0, 0, 0, 0]), |c| c.0.ip());
    let client = public.client(peer, request.headers());
    let now = Instant::now();
    if let Err(wait) = public.take(client, now) {
        let seconds = wait.as_secs().max(1);
        let mut response =
            Failure::new(ErrorCode::RateLimited, "too many requests; slow down").into_response();
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        return finish(response);
    }
    let key = request
        .uri()
        .path_and_query()
        .map_or_else(String::new, |p| p.as_str().to_owned());
    if let Some(body) = public.cached(&key, now) {
        let response = (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            body,
        )
            .into_response();
        return finish(response);
    }
    let response = next.run(request).await;
    if response.status() != StatusCode::OK {
        return finish(response);
    }
    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 4 * 1024 * 1024).await else {
        return Failure::new(ErrorCode::Unavailable, "answer too large").into_response();
    };
    public.keep(key, bytes.clone(), now);
    finish(Response::from_parts(parts, Body::from(bytes)))
}

fn finish(mut response: Response) -> Response {
    let ok = response.status() == StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    if ok {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=10"),
        );
    }
    response
}

type Reply<T> = Result<Json<T>, Failure>;

fn database(state: &AppState) -> Result<&hallow_db::PgPool, Failure> {
    state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "stats need a database (HALLOW_DATABASE_URL)",
        )
    })
}

async fn network(State(state): State<Arc<AppState>>) -> Reply<NetworkSummary> {
    let db = database(&state)?;
    let online = state.store.online().await?;
    let (players, games) = hallow_db::network_totals(db).await?;
    Ok(Json(NetworkSummary {
        online,
        players: u64::try_from(players).unwrap_or(0),
        games: u64::try_from(games).unwrap_or(0),
    }))
}

#[derive(Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
}

async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> Reply<Vec<PlayerMatch>> {
    let db = database(&state)?;
    let q = params.q.trim();
    if q.is_empty() || q.chars().count() > 32 {
        return Err(Failure::bad_request("search for 1 to 32 characters"));
    }
    #[allow(clippy::cast_possible_truncation)]
    let found = hallow_db::search_accounts(db, q, SEARCH_MAX as u32).await?;
    Ok(Json(
        found
            .into_iter()
            .map(|(uuid, username)| PlayerMatch { uuid, username })
            .collect(),
    ))
}

async fn profile(
    State(state): State<Arc<AppState>>,
    Path(player): Path<String>,
) -> Reply<PlayerProfile> {
    let db = database(&state)?;
    let uuid = match Uuid::parse_str(&player) {
        Ok(uuid) => Some(uuid),
        Err(_) if player.is_empty() || player.chars().count() > 32 => None,
        Err(_) => resolve(db, &state.store, &player)
            .await?
            .map(|(uuid, _)| uuid),
    };
    let account = match uuid {
        Some(uuid) => hallow_db::get_account(db, uuid).await?,
        None => None,
    };
    let Some(account) = account else {
        return Err(Failure::not_found("nobody by that name or UUID has joined"));
    };
    let shown = stats::display(db, &[account.uuid]).await?;
    let (rank, prefix) = shown
        .into_iter()
        .next()
        .map(|d| (d.rank, d.prefix))
        .unwrap_or_default();
    let mut kinds: std::collections::BTreeMap<String, std::collections::BTreeMap<String, i64>> =
        std::collections::BTreeMap::new();
    for total in hallow_db::stats_of(db, account.uuid).await? {
        kinds
            .entry(total.kind)
            .or_default()
            .insert(total.stat, total.value);
    }
    let recent_games = hallow_db::recent_games(db, account.uuid, RECENT_GAMES)
        .await?
        .into_iter()
        .map(|g| RecentGame {
            id: g.id,
            kind: g.kind,
            map: g.map,
            started_ms: stats::ms_of(g.started_at),
            ended_ms: stats::ms_of(g.ended_at),
            players: u32::try_from(g.players).unwrap_or(0),
            won: g.won,
            placement: g.placement.and_then(|p| u32::try_from(p).ok()),
            team: g.team,
            stats: g.stats.0,
        })
        .collect();
    let online = state.store.player(&account.uuid).await?.is_some();
    Ok(Json(PlayerProfile {
        uuid: account.uuid,
        username: account.username,
        edition: account.edition.as_str().to_owned(),
        first_seen_ms: stats::ms_of(account.first_seen),
        last_seen_ms: stats::ms_of(account.last_seen),
        online,
        rank,
        prefix,
        kinds,
        recent_games,
    }))
}

async fn boards(State(state): State<Arc<AppState>>) -> Reply<Vec<BoardKind>> {
    let db = database(&state)?;
    Ok(Json(stats::kinds(db).await?))
}

#[derive(Deserialize)]
pub(crate) struct BoardParams {
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl BoardParams {
    pub(crate) fn window(&self) -> Result<Window, Failure> {
        match &self.window {
            None => Ok(Window::All),
            Some(text) => Window::parse(text)
                .ok_or_else(|| Failure::bad_request("window is all, week or month")),
        }
    }
}

async fn leaderboard(
    State(state): State<Arc<AppState>>,
    Path((kind, stat)): Path<(String, String)>,
    Query(params): Query<BoardParams>,
) -> Reply<hallow_proto::stats::Leaderboard> {
    let db = database(&state)?;
    let window = params.window()?;
    Ok(Json(
        stats::leaderboard(db, &kind, &stat, window, params.limit).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(trusted: &str) -> Public {
        Public::new(PublicConfig {
            per_minute: 60,
            burst: 2,
            trusted_proxies: parse_trusted(trusted).unwrap(),
            ..PublicConfig::default()
        })
    }

    #[test]
    fn buckets_refill() {
        let public = public("");
        let ip = IpAddr::from([203, 0, 113, 7]);
        let other = IpAddr::from([203, 0, 113, 8]);
        let start = Instant::now();
        assert!(public.take(ip, start).is_ok());
        assert!(public.take(ip, start).is_ok());
        let wait = public.take(ip, start).unwrap_err();
        assert!(wait <= Duration::from_secs(1), "{wait:?}");
        assert!(public.take(other, start).is_ok());
        assert!(public.take(ip, start + Duration::from_secs(1)).is_ok());
        assert!(public.take(ip, start + Duration::from_secs(1)).is_err());
    }

    #[test]
    fn forwarded_for_only_from_trusted_peers() {
        let public = public("10.0.0.0/8, 192.0.2.1");
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.1, 203.0.113.9, 10.1.2.3"),
        );
        let site = IpAddr::from([10, 0, 0, 5]);
        assert_eq!(
            public.client(site, &headers),
            IpAddr::from([203, 0, 113, 9])
        );
        let stranger = IpAddr::from([198, 51, 100, 77]);
        assert_eq!(public.client(stranger, &headers), stranger);
        let v6: IpAddr = "2001:db8:1:2:3:4:5:6".parse().unwrap();
        assert_eq!(
            public.client(v6, &HeaderMap::new()),
            "2001:db8:1:2::".parse::<IpAddr>().unwrap()
        );
        assert!(parse_trusted("nope").is_err());
    }

    #[test]
    fn cache_expires() {
        let public = public("");
        let now = Instant::now();
        public.keep("/a".into(), Bytes::from_static(b"1"), now);
        assert!(public.cached("/a", now + Duration::from_secs(1)).is_some());
        assert!(public.cached("/a", now + CACHE_FOR).is_none());
    }
}
