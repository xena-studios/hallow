//! The HTTP routes. Paths come from `hallow_proto::routes`, so the control
//! plane and its callers can't drift apart.

use std::{future::Future, sync::Arc};

use axum::{
    Extension, Router,
    extract::{FromRequest, FromRequestParts, Path, Query, State},
    http::{StatusCode, header, request::Parts},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hallow_proto::{
    ErrorCode, Poll,
    command::{NetworkCommand, ProxyCommand},
    fleet::{FleetSettingsChange, FleetSettingsRecord},
    friend::{FriendAction, FriendReply},
    node::{IssueJoinToken, JoinReply, JoinRequest, JoinToken, NodeInfo},
    party::{PartyAction, PartyInfo, PartyReply},
    presence::{
        LoginDecision, LoginRequest, NetworkCount, PlayerLeft, PlayerLocation, PlayerSwitched,
    },
    proxy::{ProxyHeartbeat, ProxyInfo, ProxyRegistered, RegisterProxy, SetDraining},
    punishment::{
        IssuePunishment, PunishmentPage, PunishmentRecord, PunishmentReply, PunishmentSearch,
        PunishmentTemplate, RevokePunishment, SaveTemplate,
    },
    queue::{JoinQueue, QueueStatus},
    rank::{GrantRank, PlayerRanks, RankInfo, RevokeRank, SaveRank},
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerInfo, ServerPlayer, ServerPlayersChanged},
    signal::{SignalBatch, SignalRecord},
    stats::{PlayerStats, StatsBatch},
};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::{
    AppState, accounts,
    auth::Caller,
    error::Failure,
    events, fleet, friends, kinds, lobbies, nodes, punish, ranks, signals, stats,
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
        .route(
            &routes::server_players("{name}"),
            post(server_players_changed).get(server_players),
        )
        .route(routes::PRESENCE_LOGIN, post(login))
        .route(routes::PRESENCE_SWITCH, post(switch))
        .route(routes::PRESENCE_LEAVE, post(leave))
        .route(routes::PRESENCE_COUNT, get(count))
        .route(routes::PLAYERS, get(player_by_name))
        .route("/v1/players/{uuid}", get(player))
        .route(routes::COMMANDS, post(send_command))
        .route(routes::PARTIES, post(party_action))
        .route(routes::FRIENDS, post(friend_action))
        .route(routes::SIGNALS, post(add_signals).get(signal_history))
        .route(routes::STATS, post(add_stats))
        .route("/v1/stats/{player}", get(player_stats))
        .route(
            routes::PUNISHMENTS,
            post(issue_punishment).get(punishment_history),
        )
        .route(routes::PUNISHMENTS_REVOKE, post(revoke_punishment))
        .route(routes::PUNISHMENTS_SEARCH, get(search_punishments))
        .route(
            routes::PUNISHMENT_TEMPLATES,
            get(list_templates).post(create_template),
        )
        .route(
            &routes::punishment_template_path("{id}"),
            axum::routing::put(update_template).delete(delete_template),
        )
        .route(routes::RANKS, get(list_ranks))
        .route(
            &routes::rank("{name}"),
            axum::routing::put(save_rank).delete(delete_rank),
        )
        .route(routes::RANK_GRANTS, get(player_ranks).post(grant_rank))
        .route(routes::RANK_GRANTS_REVOKE, post(revoke_rank))
        .route(routes::QUEUES, post(join_queue))
        .route("/v1/queues/{uuid}", axum::routing::delete(leave_queue))
        .route(routes::LOBBIES, get(lobbies::overview))
        .route(routes::LOBBIES_SEND, post(lobbies::send))
        .route("/v1/parties/{uuid}", get(party_of))
        .route(
            routes::FLEET_SETTINGS,
            get(fleet_settings).put(update_fleet_settings),
        )
        .route(routes::FLEET_SETTINGS_HISTORY, get(fleet_settings_history))
        .route(routes::NODES, get(list_nodes))
        .route(routes::NODE_JOIN_TOKENS, post(issue_join_token))
        .route(routes::KINDS, get(kinds::list))
        .route(
            &routes::kind("{name}"),
            get(kinds::get).put(kinds::put).delete(kinds::delete),
        )
        .route(routes::INSTANCES, get(kinds::instances))
        .route(
            &routes::instance("{name}"),
            axum::routing::delete(kinds::end_instance),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::authorize,
        ));
    Router::new()
        .route("/healthz", get(health))
        .route("/metrics", get(metrics))
        // Authorized by the node's one-time join token, not the shared token.
        .route(routes::NODE_JOIN, post(node_join))
        // Wings' calls to its remote, authorized by node credentials.
        .merge(crate::remote::router())
        .merge(api)
        .fallback(|| async { Failure::not_found("no such endpoint") })
        .with_state(state)
}

/// The bearer token in `Authorization`, or `""`.
pub(crate) fn bearer(headers: &axum::http::HeaderMap) -> &str {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
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
    Extension(caller): Extension<Caller>,
    Body(req): Body<RegisterServer>,
) -> Reply<StatusCode> {
    check_name("server name", &req.name)?;
    check_name("server kind", &req.kind)?;
    caller.check_server(&req.name, Some(&req.kind))?;
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

async fn server_players_changed(
    State(state): Shared,
    Path(name): Path<String>,
    Body(changed): Body<ServerPlayersChanged>,
) -> Reply<StatusCode> {
    if state.store.server_players_changed(&name, &changed).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Failure::not_found(
            "server isn't registered; register again",
        ))
    }
}

async fn server_players(
    State(state): Shared,
    Path(name): Path<String>,
) -> Reply<Body<Vec<ServerPlayer>>> {
    state
        .store
        .server_players(&name)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("server isn't registered"))
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

// Anticheat signals

async fn add_signals(
    State(state): Shared,
    Extension(caller): Extension<Caller>,
    Body(batch): Body<SignalBatch>,
) -> Reply<StatusCode> {
    check_name("signal source", &batch.source)?;
    caller.check_server(&batch.source, None)?;
    if batch.samples.len() > signals::MAX_BATCH {
        return Err(Failure::bad_request(format!(
            "at most {} samples per batch",
            signals::MAX_BATCH
        )));
    }
    let db = database(&state)?;
    signals::store(db, &batch).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn signal_history(
    State(state): Shared,
    query: Result<Query<NameParams>, axum::extract::rejection::QueryRejection>,
) -> Reply<Body<Vec<SignalRecord>>> {
    let Query(params) = query?;
    let db = database(&state)?;
    signals::history(db, &state.store, &params.name)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("nobody by that name has joined"))
}

// Stats

async fn add_stats(
    State(state): Shared,
    Extension(caller): Extension<Caller>,
    Body(batch): Body<StatsBatch>,
) -> Reply<StatusCode> {
    check_name("stats source", &batch.source)?;
    check_name("server kind", &batch.kind)?;
    caller.check_server(&batch.source, Some(&batch.kind))?;
    stats::check(&batch).map_err(Failure::bad_request)?;
    let db = database(&state)?;
    stats::store(db, &batch).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn player_stats(
    State(state): Shared,
    Path(player): Path<String>,
) -> Reply<Body<PlayerStats>> {
    let db = database(&state)?;
    stats::of(db, &state.store, &player)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("nobody by that name or UUID has joined"))
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
            "this needs a database (HALLOW_DATABASE_URL)",
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

async fn search_punishments(
    State(state): Shared,
    query: Result<Query<PunishmentSearch>, axum::extract::rejection::QueryRejection>,
) -> Reply<Body<PunishmentPage>> {
    let Query(search) = query?;
    let db = database(&state)?;
    punish::search(db, &state.store, &search)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("nobody by that name has joined"))
}

async fn list_templates(State(state): Shared) -> Reply<Body<Vec<PunishmentTemplate>>> {
    let db = database(&state)?;
    Ok(Body(punish::templates(db).await?))
}

async fn create_template(
    State(state): Shared,
    Body(save): Body<SaveTemplate>,
) -> Reply<Body<PunishmentTemplate>> {
    let db = database(&state)?;
    punish::save_template(db, None, &save)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("no such template"))
}

fn template_id(id: &str) -> Reply<i64> {
    id.parse()
        .map_err(|_| Failure::bad_request("not a template id"))
}

async fn update_template(
    State(state): Shared,
    Path(id): Path<String>,
    Body(save): Body<SaveTemplate>,
) -> Reply<Body<PunishmentTemplate>> {
    let id = template_id(&id)?;
    let db = database(&state)?;
    punish::save_template(db, Some(id), &save)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("no such template"))
}

async fn delete_template(State(state): Shared, Path(id): Path<String>) -> Reply<StatusCode> {
    let id = template_id(&id)?;
    let db = database(&state)?;
    if hallow_db::delete_template(db, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Failure::not_found("no such template"))
    }
}

// Ranks

fn ranks_database(state: &AppState) -> Reply<&hallow_db::PgPool> {
    state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "ranks need a database (HALLOW_DATABASE_URL)",
        )
    })
}

async fn list_ranks(State(state): Shared) -> Reply<Body<Vec<RankInfo>>> {
    Ok(Body(ranks::list(ranks_database(&state)?).await?))
}

async fn save_rank(
    State(state): Shared,
    Path(name): Path<String>,
    Body(save): Body<SaveRank>,
) -> Reply<Body<RankInfo>> {
    let db = ranks_database(&state)?;
    Ok(Body(ranks::save(db, &name, save).await?))
}

async fn delete_rank(State(state): Shared, Path(name): Path<String>) -> Reply<StatusCode> {
    ranks::delete(ranks_database(&state)?, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn player_ranks(
    State(state): Shared,
    query: Result<Query<NameParams>, axum::extract::rejection::QueryRejection>,
) -> Reply<Body<PlayerRanks>> {
    let Query(params) = query?;
    let db = ranks_database(&state)?;
    ranks::of_player(db, &state.store, &params.name)
        .await?
        .map(Body)
        .ok_or_else(|| Failure::not_found("nobody by that name has joined"))
}

async fn grant_rank(State(state): Shared, Body(req): Body<GrantRank>) -> Reply<Body<PlayerRanks>> {
    let db = ranks_database(&state)?;
    Ok(Body(ranks::grant(db, &state.store, req).await?))
}

async fn revoke_rank(
    State(state): Shared,
    Body(req): Body<RevokeRank>,
) -> Reply<Body<PlayerRanks>> {
    let db = ranks_database(&state)?;
    Ok(Body(ranks::revoke(db, &state.store, req).await?))
}

// Fleet settings

fn fleet_database(state: &AppState) -> Reply<&hallow_db::PgPool> {
    state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "fleet settings need a database (HALLOW_DATABASE_URL)",
        )
    })
}

async fn fleet_settings(State(state): Shared) -> Reply<Body<FleetSettingsRecord>> {
    let db = fleet_database(&state)?;
    Ok(Body(fleet::current(db).await?))
}

/// `400` with a message per invalid field in `fields`.
async fn update_fleet_settings(
    State(state): Shared,
    Body(body): Body<serde_json::Value>,
) -> Reply<Body<FleetSettingsRecord>> {
    let db = fleet_database(&state)?;
    let update = fleet::parse_update(body)?;
    Ok(Body(fleet::save(db, &update).await?))
}

#[derive(Deserialize)]
struct LimitParams {
    limit: Option<u32>,
}

async fn fleet_settings_history(
    State(state): Shared,
    query: Result<Query<LimitParams>, axum::extract::rejection::QueryRejection>,
) -> Reply<Body<Vec<FleetSettingsChange>>> {
    let Query(params) = query?;
    let limit = params
        .limit
        .unwrap_or(routes::FLEET_SETTINGS_HISTORY_DEFAULT)
        .clamp(1, routes::FLEET_SETTINGS_HISTORY_MAX);
    let db = fleet_database(&state)?;
    Ok(Body(fleet::history(db, limit).await?))
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
    let startable = state
        .instances
        .as_ref()
        .is_some_and(|m| m.is_game_kind(&req.kind));
    match state.store.join_queue(req, startable).await? {
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

// Nodes

async fn issue_join_token(
    State(state): Shared,
    Body(req): Body<IssueJoinToken>,
) -> Reply<Body<JoinToken>> {
    Ok(Body(nodes::issue(&state, req).await?))
}

async fn node_join(
    State(state): Shared,
    headers: axum::http::HeaderMap,
    Body(req): Body<JoinRequest>,
) -> Reply<Body<JoinReply>> {
    Ok(Body(nodes::join(&state, bearer(&headers), req).await?))
}

async fn list_nodes(State(state): Shared) -> Reply<Body<Vec<NodeInfo>>> {
    Ok(Body(nodes::list(&state).await?))
}
