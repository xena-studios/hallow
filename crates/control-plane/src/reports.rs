//! Player reports (`hallow_proto::report`): `/report` from a proxy, and the
//! panel's queue of them. Reports are kept in PostgreSQL; without a database
//! these routes answer `503` like punishments.
//!
//! A new report is told to every online player whose ranks grant
//! [`STAFF_NODE`], through their own proxy's command queue, so staff on any
//! proxy see it.
//!
//! A reporter may have one open (or claimed) report on a player at a time,
//! and make at most [`RATE_LIMIT`] reports per [`RATE_WINDOW_MINUTES`].

use std::sync::Arc;

use axum::extract::{Path, Query, State, rejection::QueryRejection};
use chrono::{DateTime, TimeDelta, Utc};
use hallow_db::{Actor, AddReportError, PgPool, ReportChange, ReportStatus, ReportWithNames};
use hallow_proto::{
    ErrorCode,
    command::ProxyCommand,
    punishment::IssuerKind,
    report::{
        ClaimReport, CloseReport, CreateReport, Report, ReportActor, ReportCreated, ReportPage,
        ReportPlayer, ReportSearch, ReportStatusFilter,
    },
    routes,
};

use crate::{AppState, accounts::resolve, error::Failure, store::Store};

type Shared = State<Arc<AppState>>;
type Reply<T> = Result<axum::Json<T>, Failure>;

/// Players whose ranks grant this are told of new reports.
pub const STAFF_NODE: &str = "hallow.staff.reports";
/// Reports one player may make per [`RATE_WINDOW_MINUTES`].
pub const RATE_LIMIT: i64 = 5;
pub const RATE_WINDOW_MINUTES: i64 = 10;
/// The longest resolution or dismissal note.
const NOTE_MAX: usize = 1024;

fn db(state: &AppState) -> Result<&PgPool, Failure> {
    state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "reports need a database (HALLOW_DATABASE_URL)",
        )
    })
}

fn millis(at: DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

fn db_status(status: hallow_db::ReportStatus) -> hallow_proto::report::ReportStatus {
    use hallow_proto::report::ReportStatus as Wire;
    match status {
        ReportStatus::Open => Wire::Open,
        ReportStatus::Claimed => Wire::Claimed,
        ReportStatus::Resolved => Wire::Resolved,
        ReportStatus::Dismissed => Wire::Dismissed,
    }
}

/// An actor as the panel shows it; `username` is an account actor's name.
fn wire_actor(actor: &Actor, username: Option<&str>) -> ReportActor {
    let (name, kind, uuid, panel_user) = match actor {
        Actor::Account(uuid) => (
            username.map_or_else(|| uuid.to_string(), str::to_owned),
            IssuerKind::Player,
            Some(*uuid),
            None,
        ),
        Actor::Staff { name, panel_user } => {
            (name.clone(), IssuerKind::Staff, None, panel_user.clone())
        }
        Actor::Console => ("console".into(), IssuerKind::Console, None, None),
        Actor::System => ("system".into(), IssuerKind::System, None, None),
        Actor::Anticheat => ("anticheat".into(), IssuerKind::Anticheat, None, None),
    };
    ReportActor {
        name,
        kind,
        uuid,
        panel_user,
    }
}

fn wire(found: ReportWithNames) -> Report {
    let r = found.report;
    Report {
        id: r.id,
        reporter: ReportPlayer {
            uuid: r.reporter,
            name: found.reporter_name,
        },
        target: ReportPlayer {
            uuid: r.target,
            name: found.target_name,
        },
        reason: r.reason,
        server: r.server,
        status: db_status(r.status),
        created_ms: millis(r.created_at),
        claimed_by: r
            .claimed
            .as_ref()
            .map(|(_, a)| wire_actor(a, found.claimed_by_username.as_deref())),
        claimed_ms: r.claimed.as_ref().map(|(at, _)| millis(*at)),
        closed_by: r
            .closed
            .as_ref()
            .map(|(_, a)| wire_actor(a, found.closed_by_username.as_deref())),
        closed_ms: r.closed.as_ref().map(|(at, _)| millis(*at)),
        note: r.note,
        punishment_id: r.punishment_id,
        target_reports: u64::try_from(found.target_reports).unwrap_or(0),
    }
}

fn report_id(id: &str) -> Result<i64, Failure> {
    id.parse()
        .map_err(|_| Failure::bad_request("not a report id"))
}

fn conflict(message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::Conflict, message)
}

/// `POST /v1/reports`.
pub async fn create(
    State(state): Shared,
    body: Result<axum::Json<CreateReport>, axum::extract::rejection::JsonRejection>,
) -> Reply<ReportCreated> {
    let axum::Json(req) = body?;
    let pool = db(&state)?;
    let reason = req.reason.trim().replace('§', "");
    if reason.is_empty() || reason.chars().count() > 256 {
        return Err(Failure::bad_request(
            "Give a reason of at most 256 characters.",
        ));
    }
    let Some(reporter) = hallow_db::get_account(pool, req.reporter).await? else {
        return Err(Failure::bad_request("the reporter has never joined"));
    };
    let Some((target, target_name)) = resolve(pool, &state.store, req.target.trim()).await? else {
        return Err(Failure::not_found(format!(
            "No player called {} has joined.",
            req.target.trim()
        )));
    };
    if target == reporter.uuid {
        return Err(Failure::bad_request("You can't report yourself."));
    }
    if hallow_db::has_active_report(pool, reporter.uuid, target).await? {
        return Err(conflict(format!(
            "You already reported {target_name}; staff will look at it soon."
        )));
    }
    let since = Utc::now() - TimeDelta::minutes(RATE_WINDOW_MINUTES);
    if hallow_db::reports_by_since(pool, reporter.uuid, since).await? >= RATE_LIMIT {
        return Err(Failure::new(
            ErrorCode::RateLimited,
            "You've sent a lot of reports. Try again in a few minutes.",
        ));
    }
    let location = state.store.player(&reporter.uuid).await?;
    let server = location.as_ref().and_then(|l| l.server.clone());
    let reporter_name = location.map_or(reporter.username, |l| l.username);
    let report = match hallow_db::add_report(
        pool,
        &hallow_db::NewReport {
            reporter: reporter.uuid,
            target,
            reason: &reason,
            server: server.as_deref(),
        },
    )
    .await
    {
        Ok(report) => report,
        // Two reports raced.
        Err(AddReportError::AlreadyOpen) => {
            return Err(conflict(format!(
                "You already reported {target_name}; staff will look at it soon."
            )));
        }
        Err(AddReportError::Database(err)) => return Err(err.into()),
    };
    tracing::info!(id = report.id, reporter = %reporter.uuid, %target, "report made");
    let place = server.as_deref().unwrap_or("no server");
    notify_staff(
        pool,
        &state.store,
        &format!(
            "§c[Report #{}] §f{reporter_name} §7→ §f{target_name} §7({place}): §f{reason}",
            report.id
        ),
    )
    .await;
    Ok(axum::Json(ReportCreated {
        id: report.id,
        message: format!("§aThanks, your report on {target_name} was sent to staff."),
    }))
}

/// Tells every online player holding [`STAFF_NODE`], on their proxy.
async fn notify_staff(pool: &PgPool, store: &Store, text: &str) {
    let staff = match store.online_players().await {
        Ok(online) => hallow_db::accounts_with_permission(pool, &online, STAFF_NODE).await,
        Err(err) => {
            tracing::warn!(%err, "can't list online staff for a report");
            return;
        }
    };
    let staff = match staff {
        Ok(staff) => staff,
        Err(err) => {
            tracing::warn!(%err, "can't find online staff for a report");
            return;
        }
    };
    for player in staff {
        if let Ok(Some(location)) = store.player(&player).await {
            let message = ProxyCommand::Message {
                player,
                text: text.to_owned(),
                from: None,
            };
            if let Err(err) = store.send_to_proxy(&location.proxy, &message).await {
                tracing::warn!(%err, "couldn't tell staff of a report");
            }
        }
    }
}

/// `GET /v1/reports`.
pub async fn list(
    State(state): Shared,
    query: Result<Query<ReportSearch>, QueryRejection>,
) -> Reply<ReportPage> {
    let Query(search) = query?;
    let pool = db(&state)?;
    let target = match search.target.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => match resolve(pool, &state.store, name).await? {
            Some((uuid, _)) => Some(uuid),
            None => return Err(Failure::not_found("nobody by that name has joined")),
        },
        _ => None,
    };
    let statuses = match search.status {
        None => Vec::new(),
        Some(ReportStatusFilter::Open) => vec![ReportStatus::Open],
        Some(ReportStatusFilter::Claimed) => vec![ReportStatus::Claimed],
        Some(ReportStatusFilter::Resolved) => vec![ReportStatus::Resolved],
        Some(ReportStatusFilter::Dismissed) => vec![ReportStatus::Dismissed],
        Some(ReportStatusFilter::Active) => vec![ReportStatus::Open, ReportStatus::Claimed],
    };
    let limit = search
        .limit
        .unwrap_or(routes::REPORTS_DEFAULT)
        .clamp(1, routes::REPORTS_MAX);
    let (total, found) = hallow_db::search_reports(
        pool,
        &hallow_db::ReportFilter { statuses, target },
        i64::from(limit),
        i64::from(search.offset.unwrap_or(0)),
    )
    .await?;
    Ok(axum::Json(ReportPage {
        reports: found.into_iter().map(wire).collect(),
        total,
    }))
}

async fn fetch(pool: &PgPool, id: i64) -> Reply<Report> {
    hallow_db::get_report(pool, id)
        .await?
        .map(|r| axum::Json(wire(r)))
        .ok_or_else(|| Failure::not_found("no such report"))
}

/// `GET /v1/reports/{id}`.
pub async fn get(State(state): Shared, Path(id): Path<String>) -> Reply<Report> {
    let id = report_id(&id)?;
    fetch(db(&state)?, id).await
}

/// The report after a change, or why it didn't happen.
async fn after(pool: &PgPool, id: i64, change: ReportChange) -> Reply<Report> {
    match change {
        ReportChange::Done(_) => fetch(pool, id).await,
        ReportChange::NotFound => Err(Failure::not_found("no such report")),
        ReportChange::Closed => Err(conflict("the report is already closed")),
    }
}

/// `POST /v1/reports/{id}/claim`: from open, or from another staff member.
pub async fn claim(
    State(state): Shared,
    Path(id): Path<String>,
    body: Result<axum::Json<ClaimReport>, axum::extract::rejection::JsonRejection>,
) -> Reply<Report> {
    let id = report_id(&id)?;
    let axum::Json(req) = body?;
    let pool = db(&state)?;
    let by = crate::punish::actor(req.issuer, req.staff.as_ref()).map_err(Failure::bad_request)?;
    let change = hallow_db::claim_report(pool, id, &by).await?;
    tracing::info!(id, ?by, "report claimed");
    after(pool, id, change).await
}

/// `POST /v1/reports/{id}/unclaim`: back to open. Takes no body.
pub async fn unclaim(State(state): Shared, Path(id): Path<String>) -> Reply<Report> {
    let id = report_id(&id)?;
    let pool = db(&state)?;
    let change = hallow_db::unclaim_report(pool, id).await?;
    after(pool, id, change).await
}

/// `POST /v1/reports/{id}/resolve`.
pub async fn resolve_report(
    State(state): Shared,
    Path(id): Path<String>,
    body: Result<axum::Json<CloseReport>, axum::extract::rejection::JsonRejection>,
) -> Reply<Report> {
    close(&state, &id, body, ReportStatus::Resolved).await
}

/// `POST /v1/reports/{id}/dismiss`.
pub async fn dismiss(
    State(state): Shared,
    Path(id): Path<String>,
    body: Result<axum::Json<CloseReport>, axum::extract::rejection::JsonRejection>,
) -> Reply<Report> {
    close(&state, &id, body, ReportStatus::Dismissed).await
}

async fn close(
    state: &AppState,
    id: &str,
    body: Result<axum::Json<CloseReport>, axum::extract::rejection::JsonRejection>,
    status: ReportStatus,
) -> Reply<Report> {
    let id = report_id(id)?;
    let axum::Json(req) = body?;
    let pool = db(state)?;
    let by = crate::punish::actor(req.issuer, req.staff.as_ref()).map_err(Failure::bad_request)?;
    let note = req.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > NOTE_MAX) {
        return Err(Failure::bad_request(format!(
            "the note must be at most {NOTE_MAX} characters"
        )));
    }
    let Some(current) = hallow_db::get_report(pool, id).await? else {
        return Err(Failure::not_found("no such report"));
    };
    if current.report.status.is_closed() {
        return Err(conflict("the report is already closed"));
    }
    if let Some(punishment) = req.punishment_id {
        if status != ReportStatus::Resolved {
            return Err(Failure::bad_request(
                "only a resolved report names a punishment",
            ));
        }
        match hallow_db::get_punishment(pool, punishment).await? {
            Some(p) if p.target == current.report.target => {}
            Some(_) => {
                return Err(Failure::bad_request("that punishment is of another player"));
            }
            None => return Err(Failure::bad_request("no such punishment")),
        }
    }
    let change = hallow_db::close_report(pool, id, status, &by, note, req.punishment_id).await?;
    tracing::info!(id, status = status.as_str(), ?by, "report closed");
    after(pool, id, change).await
}

/// Open and claimed reports, for `/metrics`; `None` without a database.
pub async fn active_count(state: &AppState) -> Option<i64> {
    let pool = state.db.as_ref()?;
    match hallow_db::count_reports(pool, &[ReportStatus::Open, ReportStatus::Claimed]).await {
        Ok(count) => Some(count),
        Err(err) => {
            tracing::warn!(%err, "can't count reports for metrics");
            None
        }
    }
}
