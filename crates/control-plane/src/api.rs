//! The HTTP routes. Paths come from `hallow_proto::routes`, so the control
//! plane and its callers can't drift apart.

use std::{future::Future, sync::Arc};

use axum::{
    Router,
    extract::{FromRequest, FromRequestParts, Path, Query, Request, State},
    http::{StatusCode, header, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hallow_proto::{
    ErrorCode, Poll,
    command::{NetworkCommand, ProxyCommand},
    friend::{FriendAction, FriendReply},
    party::{PartyAction, PartyInfo, PartyReply},
    presence::{
        LoginDecision, LoginRequest, NetworkCount, PlayerLeft, PlayerLocation, PlayerSwitched,
    },
    proxy::{ProxyHeartbeat, ProxyInfo, ProxyRegistered, RegisterProxy, SetDraining},
    punishment::{IssuePunishment, PunishmentRecord, PunishmentReply, RevokePunishment},
    queue::{JoinQueue, QueueStatus},
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerInfo},
};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::{
    AppState, accounts,
    error::Failure,
    events, friends, punish,
    store::{LoginOutcome, RegisterError},
};

type Shared = State<Arc<AppState>>;
type Reply<T> = Result<T, Failure>;

/// `Json<T>` that rejects with an [`ApiError`](hallow_proto::ApiError) body.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(Failure))]
struct Body<T>(T);

impl<T: serde::Serialize> IntoResponse for Body<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

#[derive(Deserialize)]
struct SinceParams {
    #[serde(default)]
    since: u64,
    /// Seconds to hold the poll, capped at the configured long poll.
    wait: Option<u64>,
}

impl SinceParams {
    fn hold(&self, config: &crate::Config) -> std::time::Duration {
        self.wait.map_or(config.long_poll, |wait| {
            config.long_poll.min(std::time::Duration::from_secs(wait))
        })
    }
}

/// `?since=`, `0` if absent.
struct Since(SinceParams);

impl<S: Send + Sync> FromRequestParts<S> for Since {
    type Rejection = Failure;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Failure> {
        let Query(params) = Query::<SinceParams>::from_request_parts(parts, state).await?;
        Ok(Self(params))
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route(routes::PROXIES, post(register_proxy).get(list_proxies))
        .route(&routes::proxy_drain("{id}"), post(drain_proxy))
        .route(&routes::proxy("{id}"), axum::routing::delete(remove_proxy))
        .route(&routes::proxy_heartbeat("{id}"), post(proxy_heartbeat))
        .route(&routes::proxy_commands("{id}"), get(poll_commands))
        .route(routes::SERVERS, post(register_server).get(poll_servers))
        .route(
            &routes::server("{name}"),
            axum::routing::delete(remove_server),
        )
        .route(&routes::server_heartbeat("{name}"), post(server_heartbeat))
        .route(routes::PRESENCE_LOGIN, post(login))
        .route(routes::PRESENCE_SWITCH, post(switch))
        .route(routes::PRESENCE_LEAVE, post(leave))
        .route(routes::PRESENCE_COUNT, get(count))
        .route(routes::PLAYERS, get(player_by_name))
        .route("/v1/players/{uuid}", get(player))
        .route(routes::COMMANDS, post(send_command))
        .route(routes::PARTIES, post(party_action))
        .route(routes::FRIENDS, post(friend_action))
        .route(
            routes::PUNISHMENTS,
            post(issue_punishment).get(punishment_history),
        )
        .route(routes::PUNISHMENTS_REVOKE, post(revoke_punishment))
        .route(routes::QUEUES, post(join_queue))
        .route("/v1/queues/{uuid}", axum::routing::delete(leave_queue))
        .route("/v1/parties/{uuid}", get(party_of))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize));
    Router::new()
        .route("/healthz", get(health))
        .route("/metrics", get(metrics))
        .merge(api)
        .fallback(|| async { Failure::not_found("no such endpoint") })
        .with_state(state)
}

/// Checks `Authorization: Bearer <token>` in constant time.
async fn authorize(State(state): Shared, request: Request, next: Next) -> Response {
    let given = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !constant_time_eq(given.as_bytes(), state.config.token.as_bytes()) {
        return Failure::new(ErrorCode::Unauthorized, "missing or wrong bearer token")
            .into_response();
    }
    next.run(request).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Names and ids go into URLs and Redis keys: 1 to 64 of `A-Z a-z 0-9 . _ -`.
fn check_name(what: &str, name: &str) -> Reply<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(Failure::bad_request(format!(
            "{what} must be 1 to 64 letters, digits, '.', '_' or '-'"
        )))
    }
}

async fn metrics(State(state): Shared) -> Reply<impl IntoResponse> {
    let body = crate::metrics::render(&state).await?;
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    ))
}

async fn health(State(state): Shared) -> Reply<StatusCode> {
    state.store.ping().await?;
    Ok(StatusCode::NO_CONTENT)
}

// Proxies

async fn register_proxy(
    State(state): Shared,
    Body(req): Body<RegisterProxy>,
) -> Reply<Body<ProxyRegistered>> {
    check_name("proxy id", &req.id)?;
    let id = req.id.clone();
    // A proxy registering again restarted: its players are gone.
    if let Some(db) = &state.db {
        hallow_db::close_proxy_sessions(db, &id).await?;
    }
    let registered = state.store.register_proxy(req).await?;
    tracing::info!(proxy = %id, "proxy registered");
    Ok(Body(registered))
}

async fn list_proxies(State(state): Shared) -> Reply<Body<Vec<ProxyInfo>>> {
    Ok(Body(state.store.proxies().await?))
}

async fn drain_proxy(
    State(state): Shared,
    Path(id): Path<String>,
    Body(req): Body<SetDraining>,
) -> Reply<StatusCode> {
    if state.store.set_draining(&id, req.draining).await? {
        tracing::info!(proxy = %id, draining = req.draining, "proxy drain set");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Failure::not_found("proxy isn't registered"))
    }
}

async fn proxy_heartbeat(
    State(state): Shared,
    Path(id): Path<String>,
    Body(beat): Body<ProxyHeartbeat>,
) -> Reply<StatusCode> {
    if state.store.proxy_heartbeat(&id, beat).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Failure::not_found("proxy isn't registered; register again"))
    }
}

async fn remove_proxy(State(state): Shared, Path(id): Path<String>) -> Reply<StatusCode> {
    if let Some(db) = &state.db {
        hallow_db::close_proxy_sessions(db, &id).await?;
    }
    if state.store.remove_proxy(&id).await? {
        tracing::info!(proxy = %id, "proxy unregistered");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Failure::not_found("proxy isn't registered"))
    }
}

async fn poll_commands(
    State(state): Shared,
    Path(id): Path<String>,
    Since(params): Since,
) -> Reply<Body<Poll<Vec<ProxyCommand>>>> {
    if state.store.proxy(&id).await?.is_none() {
        return Err(Failure::not_found("proxy isn't registered; register again"));
    }
    let topic = events::commands(&id);
    let poll = long_poll(
        &state,
        &topic,
        params.since,
        params.hold(&state.config),
        || state.store.read_commands(&id, params.since),
    )
    .await?;
    Ok(Body(poll))
}

// Servers

async fn register_server(
    State(state): Shared,
    Body(req): Body<RegisterServer>,
) -> Reply<StatusCode> {
    check_name("server name", &req.name)?;
    check_name("server kind", &req.kind)?;
    let name = req.name.clone();
    match state.store.register_server(req).await? {
        Ok(()) => {
            tracing::info!(server = %name, "server registered");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(RegisterError::NameTaken) => Err(Failure::new(
            ErrorCode::Conflict,
            "a live server already uses this name at another address",
        )),
    }
}

async fn server_heartbeat(
    State(state): Shared,
    Path(name): Path<String>,
    Body(beat): Body<ServerHeartbeat>,
) -> Reply<StatusCode> {
    if state.store.server_heartbeat(&name, beat).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Failure::not_found(
            "server isn't registered; register again",
        ))
    }
}

async fn remove_server(State(state): Shared, Path(name): Path<String>) -> Reply<StatusCode> {
    if state.store.remove_server(&name).await? {
        tracing::info!(server = %name, "server unregistered");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Failure::not_found("server isn't registered"))
    }
}

async fn poll_servers(
    State(state): Shared,
    Since(params): Since,
) -> Reply<Body<Poll<Vec<ServerInfo>>>> {
    let poll = long_poll(
        &state,
        events::SERVERS,
        params.since,
        params.hold(&state.config),
        || state.store.read_servers(),
    )
    .await?;
    Ok(Body(poll))
}

// Presence

/// With a database, a banned account is refused before it touches presence,
/// and an allowed login gets its permission nodes and an open session.
async fn login(State(state): Shared, Body(req): Body<LoginRequest>) -> Reply<Body<LoginDecision>> {
    let mut grant = accounts::Grant {
        permissions: Vec::new(),
        mute: None,
    };
    if let Some(db) = &state.db {
        match accounts::check_login(db, &req, &state.config.admins).await? {
            Ok(granted) => grant = granted,
            Err(reason) => return Ok(Body(LoginDecision::Deny { reason })),
        }
    }
    let decision = match state.store.login(req.clone(), &state.config).await? {
        LoginOutcome::Decided(decision) => decision,
        LoginOutcome::UnknownProxy => {
            return Err(Failure::not_found("proxy isn't registered; register again"));
        }
    };
    let LoginDecision::Allow { server, .. } = decision else {
        return Ok(Body(decision));
    };
    if let Some(db) = &state.db {
        // A session left open by a lost leave ends here.
        if let Some(old) = state.store.take_session(&req.uuid).await? {
            hallow_db::close_session(db, old).await?;
        }
        let session = accounts::open_session(db, &req).await?;
        state.store.set_session(&req.uuid, &session).await?;
        friends::announce(db, &state.store, req.uuid, &req.username, true).await;
    }
    Ok(Body(LoginDecision::Allow {
        server,
        permissions: grant.permissions,
        mute: grant.mute,
    }))
}

async fn switch(State(state): Shared, Body(moved): Body<PlayerSwitched>) -> Reply<StatusCode> {
    if let Some(db) = &state.db
        && let Some(session) = state.store.session(&moved.uuid).await?
    {
        hallow_db::set_session_server(db, session, &moved.server).await?;
    }
    if state.store.switch(moved).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Failure::not_found("player isn't online"))
    }
}

/// Always `204`: a leave for a player who already left, or who has since
/// logged in on another proxy, is simply ignored.
async fn leave(State(state): Shared, Body(left): Body<PlayerLeft>) -> Reply<StatusCode> {
    let uuid = left.uuid;
    let username = state.store.player(&uuid).await?.map(|p| p.username);
    if state.store.leave(left).await?
        && let Some(db) = &state.db
    {
        if let Some(session) = state.store.take_session(&uuid).await? {
            hallow_db::close_session(db, session).await?;
        }
        if let Some(username) = username {
            friends::announce(db, &state.store, uuid, &username, false).await;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn count(State(state): Shared) -> Reply<Body<NetworkCount>> {
    Ok(Body(NetworkCount {
        online: state.store.online().await?,
        max: state.config.max_players,
    }))
}

async fn player(State(state): Shared, Path(uuid): Path<String>) -> Reply<Body<PlayerLocation>> {
    let uuid = Uuid::parse_str(&uuid).map_err(|_| Failure::bad_request("not a UUID"))?;
    state
        .store
        .player(&uuid)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("player isn't online"))
}

#[derive(Deserialize)]
struct NameParams {
    name: String,
}

async fn player_by_name(
    State(state): Shared,
    query: Result<Query<NameParams>, axum::extract::rejection::QueryRejection>,
) -> Reply<Body<PlayerLocation>> {
    let Query(params) = query?;
    state
        .store
        .player_by_name(&params.name)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("player isn't online"))
}

// Friends

async fn friend_action(
    State(state): Shared,
    Body(action): Body<FriendAction>,
) -> Reply<Body<FriendReply>> {
    let db = state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "friends need a database (HALLOW_DATABASE_URL)",
        )
    })?;
    Ok(Body(friends::act(db, &state.store, action).await?))
}

// Punishments

fn database(state: &AppState) -> Reply<&hallow_db::PgPool> {
    state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "punishments need a database (HALLOW_DATABASE_URL)",
        )
    })
}

async fn issue_punishment(
    State(state): Shared,
    Body(req): Body<IssuePunishment>,
) -> Reply<Body<PunishmentReply>> {
    let db = database(&state)?;
    Ok(Body(punish::issue(db, &state.store, req).await?))
}

async fn revoke_punishment(
    State(state): Shared,
    Body(req): Body<RevokePunishment>,
) -> Reply<Body<PunishmentReply>> {
    let db = database(&state)?;
    Ok(Body(punish::revoke(db, &state.store, req).await?))
}

async fn punishment_history(
    State(state): Shared,
    query: Result<Query<NameParams>, axum::extract::rejection::QueryRejection>,
) -> Reply<Body<Vec<PunishmentRecord>>> {
    let Query(params) = query?;
    let db = database(&state)?;
    punish::history(db, &state.store, &params.name)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("nobody by that name has joined"))
}

// Parties

async fn party_action(
    State(state): Shared,
    Body(action): Body<PartyAction>,
) -> Reply<Body<PartyReply>> {
    Ok(Body(state.store.party_action(action).await?))
}

async fn party_of(State(state): Shared, Path(uuid): Path<String>) -> Reply<Body<PartyInfo>> {
    let uuid = Uuid::parse_str(&uuid).map_err(|_| Failure::bad_request("not a UUID"))?;
    state
        .store
        .party_of(&uuid)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("player isn't in a party"))
}

// Queues

/// A refusal (`409`) carries a message for the player.
async fn join_queue(State(state): Shared, Body(req): Body<JoinQueue>) -> Reply<Body<QueueStatus>> {
    check_name("game kind", &req.kind)?;
    match state.store.join_queue(req).await? {
        Ok(status) => Ok(Body(status)),
        Err(message) => Err(Failure::new(ErrorCode::Conflict, message)),
    }
}

async fn leave_queue(State(state): Shared, Path(uuid): Path<String>) -> Reply<StatusCode> {
    let uuid = Uuid::parse_str(&uuid).map_err(|_| Failure::bad_request("not a UUID"))?;
    match state.store.dequeue(&uuid).await? {
        Some(_) => Ok(StatusCode::NO_CONTENT),
        None => Err(Failure::not_found("player isn't queued")),
    }
}

// Commands

async fn send_command(
    State(state): Shared,
    Body(command): Body<NetworkCommand>,
) -> Reply<StatusCode> {
    if state.store.send_command(command).await? {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(Failure::not_found("player isn't online"))
    }
}

/// Answers as soon as the revision differs from `since`, or with
/// [`Poll::Unchanged`] after `hold`. Wakes on `topic` events
/// from any control plane instance.
async fn long_poll<T, F, Fut>(
    state: &AppState,
    topic: &str,
    since: u64,
    hold: std::time::Duration,
    read: F,
) -> Reply<Poll<T>>
where
    F: Fn() -> Fut,
    Fut: Future<Output = crate::store::Result<(u64, T)>>,
{
    // Subscribe before the first read, so a change between the two still wakes us.
    let mut events = state.events.subscribe();
    let deadline = tokio::time::Instant::now() + hold;
    loop {
        let (revision, data) = read().await?;
        if revision != since {
            return Ok(Poll::Changed { revision, data });
        }
        if hold.is_zero() {
            return Ok(Poll::Unchanged { revision });
        }
        let woke = tokio::time::timeout_at(deadline, async {
            loop {
                match events.recv().await {
                    Ok(event) if events::matches(&event, topic) => return,
                    Ok(_) => {}
                    // Missed some; look again to be safe.
                    Err(RecvError::Lagged(_)) => return,
                    // The listener is gone; wait out the timeout.
                    Err(RecvError::Closed) => std::future::pending::<()>().await,
                }
            }
        })
        .await;
        if woke.is_err() {
            // One last look, in case an event was lost.
            let (revision, data) = read().await?;
            return Ok(if revision == since {
                Poll::Unchanged { revision }
            } else {
                Poll::Changed { revision, data }
            });
        }
    }
}
