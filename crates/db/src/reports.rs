//! Player reports: `open`, `claimed` by a staff member, then `resolved` or
//! `dismissed`. A closed report never changes again.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{decode_error, punishments::Actor};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReportStatus {
    Open,
    Claimed,
    Resolved,
    Dismissed,
}

impl ReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Claimed => "claimed",
            Self::Resolved => "resolved",
            Self::Dismissed => "dismissed",
        }
    }

    fn parse(value: &str) -> Result<Self, sqlx::Error> {
        match value {
            "open" => Ok(Self::Open),
            "claimed" => Ok(Self::Claimed),
            "resolved" => Ok(Self::Resolved),
            "dismissed" => Ok(Self::Dismissed),
            other => Err(decode_error("report status", other)),
        }
    }

    /// Resolved or dismissed.
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Resolved | Self::Dismissed)
    }
}

/// A report to record.
#[derive(Clone, Debug)]
pub struct NewReport<'a> {
    /// Both must be accounts that have joined, and not the same one.
    pub reporter: Uuid,
    pub target: Uuid,
    /// 1 to 256 characters.
    pub reason: &'a str,
    /// The server the reporter was on.
    pub server: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub id: i64,
    pub reporter: Uuid,
    pub target: Uuid,
    pub reason: String,
    pub server: Option<String>,
    pub status: ReportStatus,
    pub created_at: DateTime<Utc>,
    /// Who claimed it last, and when; kept once it's closed.
    pub claimed: Option<(DateTime<Utc>, Actor)>,
    /// Who resolved or dismissed it, and when.
    pub closed: Option<(DateTime<Utc>, Actor)>,
    pub note: Option<String>,
    pub punishment_id: Option<i64>,
}

/// A report with the names the panel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportWithNames {
    pub report: Report,
    pub reporter_name: String,
    pub target_name: String,
    /// The username of an account claimer, if it is one.
    pub claimed_by_username: Option<String>,
    /// The username of an account closer, if it is one.
    pub closed_by_username: Option<String>,
    /// Every report the target has had, this one included.
    pub target_reports: i64,
}

#[derive(FromRow)]
struct ReportRow {
    id: i64,
    reporter_uuid: Uuid,
    target_uuid: Uuid,
    reason: String,
    server: Option<String>,
    status: String,
    created_at: DateTime<Utc>,
    claimed_at: Option<DateTime<Utc>>,
    claimed_by_kind: Option<String>,
    claimed_by_uuid: Option<Uuid>,
    claimed_by_name: Option<String>,
    claimed_by_panel_user: Option<String>,
    closed_at: Option<DateTime<Utc>>,
    closed_by_kind: Option<String>,
    closed_by_uuid: Option<Uuid>,
    closed_by_name: Option<String>,
    closed_by_panel_user: Option<String>,
    note: Option<String>,
    punishment_id: Option<i64>,
}

fn actor(
    at: Option<DateTime<Utc>>,
    kind: Option<String>,
    uuid: Option<Uuid>,
    name: Option<String>,
    panel_user: Option<String>,
) -> Result<Option<(DateTime<Utc>, Actor)>, sqlx::Error> {
    match (at, kind) {
        (Some(at), Some(kind)) => Ok(Some((at, Actor::parse(&kind, uuid, name, panel_user)?))),
        _ => Ok(None),
    }
}

impl TryFrom<ReportRow> for Report {
    type Error = sqlx::Error;

    fn try_from(row: ReportRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            reporter: row.reporter_uuid,
            target: row.target_uuid,
            reason: row.reason,
            server: row.server,
            status: ReportStatus::parse(&row.status)?,
            created_at: row.created_at,
            claimed: actor(
                row.claimed_at,
                row.claimed_by_kind,
                row.claimed_by_uuid,
                row.claimed_by_name,
                row.claimed_by_panel_user,
            )?,
            closed: actor(
                row.closed_at,
                row.closed_by_kind,
                row.closed_by_uuid,
                row.closed_by_name,
                row.closed_by_panel_user,
            )?,
            note: row.note,
            punishment_id: row.punishment_id,
        })
    }
}

#[derive(FromRow)]
struct NamedRow {
    #[sqlx(flatten)]
    report: ReportRow,
    reporter_name: String,
    target_name: String,
    claimed_by_username: Option<String>,
    closed_by_username: Option<String>,
    target_reports: i64,
}

impl TryFrom<NamedRow> for ReportWithNames {
    type Error = sqlx::Error;

    fn try_from(row: NamedRow) -> Result<Self, Self::Error> {
        Ok(Self {
            report: row.report.try_into()?,
            reporter_name: row.reporter_name,
            target_name: row.target_name,
            claimed_by_username: row.claimed_by_username,
            closed_by_username: row.closed_by_username,
            target_reports: row.target_reports,
        })
    }
}

macro_rules! columns {
    () => {
        "r.id, r.reporter_uuid, r.target_uuid, r.reason, r.server, r.status, r.created_at, \
         r.claimed_at, r.claimed_by_kind, r.claimed_by_uuid, r.claimed_by_name, \
         r.claimed_by_panel_user, r.closed_at, r.closed_by_kind, r.closed_by_uuid, \
         r.closed_by_name, r.closed_by_panel_user, r.note, r.punishment_id"
    };
}

/// The columns of a [`NamedRow`], from `hallow.reports r` and its joins.
macro_rules! named {
    () => {
        concat!(
            "SELECT ",
            columns!(),
            ", ra.username AS reporter_name, ta.username AS target_name,
               ca.username AS claimed_by_username, xa.username AS closed_by_username,
               (SELECT count(*) FROM hallow.reports t WHERE t.target_uuid = r.target_uuid)
                 AS target_reports",
            named!(@from)
        )
    };
    (@from) => {
        " FROM hallow.reports r
          JOIN hallow.accounts ra ON ra.uuid = r.reporter_uuid
          JOIN hallow.accounts ta ON ta.uuid = r.target_uuid
          LEFT JOIN hallow.accounts ca ON ca.uuid = r.claimed_by_uuid
          LEFT JOIN hallow.accounts xa ON xa.uuid = r.closed_by_uuid"
    };
}

/// Why a report wasn't recorded.
#[derive(Debug, thiserror::Error)]
pub enum AddReportError {
    /// The reporter already has an open or claimed report on the target.
    #[error("there's already an open report on that player")]
    AlreadyOpen,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Records an open report.
pub async fn add_report(pool: &PgPool, new: &NewReport<'_>) -> Result<Report, AddReportError> {
    let row: ReportRow = sqlx::query_as(concat!(
        "INSERT INTO hallow.reports AS r (reporter_uuid, target_uuid, reason, server)
         VALUES ($1, $2, $3, $4)
         RETURNING ",
        columns!()
    ))
    .bind(new.reporter)
    .bind(new.target)
    .bind(new.reason)
    .bind(new.server)
    .fetch_one(pool)
    .await
    .map_err(|err| match &err {
        sqlx::Error::Database(db) if db.constraint() == Some("reports_one_active_idx") => {
            AddReportError::AlreadyOpen
        }
        _ => AddReportError::Database(err),
    })?;
    Ok(row.try_into()?)
}

/// Whether `reporter` has an open or claimed report on `target`.
pub async fn has_active_report(
    pool: &PgPool,
    reporter: Uuid,
    target: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM hallow.reports
         WHERE reporter_uuid = $1 AND target_uuid = $2 AND status IN ('open', 'claimed'))",
    )
    .bind(reporter)
    .bind(target)
    .fetch_one(pool)
    .await
}

/// How many reports `reporter` made after `since`.
pub async fn reports_by_since(
    pool: &PgPool,
    reporter: Uuid,
    since: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM hallow.reports WHERE reporter_uuid = $1 AND created_at > $2",
    )
    .bind(reporter)
    .bind(since)
    .fetch_one(pool)
    .await
}

/// How many reports have one of `statuses`.
pub async fn count_reports(pool: &PgPool, statuses: &[ReportStatus]) -> Result<i64, sqlx::Error> {
    let statuses: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
    sqlx::query_scalar("SELECT count(*) FROM hallow.reports WHERE status = ANY($1)")
        .bind(&statuses)
        .fetch_one(pool)
        .await
}

pub async fn get_report(pool: &PgPool, id: i64) -> Result<Option<ReportWithNames>, sqlx::Error> {
    sqlx::query_as::<_, NamedRow>(concat!(named!(), " WHERE r.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(ReportWithNames::try_from)
        .transpose()
}

/// Filters for [`search_reports`].
#[derive(Clone, Debug, Default)]
pub struct ReportFilter {
    /// Any of these; empty matches every status.
    pub statuses: Vec<ReportStatus>,
    /// Only reports on this account.
    pub target: Option<Uuid>,
}

/// Reports matching `filter`, newest first, `limit` from `offset`, and how
/// many match in all.
pub async fn search_reports(
    pool: &PgPool,
    filter: &ReportFilter,
    limit: i64,
    offset: i64,
) -> Result<(u64, Vec<ReportWithNames>), sqlx::Error> {
    let statuses: Vec<&str> = filter.statuses.iter().map(|s| s.as_str()).collect();
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM hallow.reports r
         WHERE (cardinality($1::text[]) = 0 OR r.status = ANY($1))
           AND ($2::uuid IS NULL OR r.target_uuid = $2)",
    )
    .bind(&statuses)
    .bind(filter.target)
    .fetch_one(pool)
    .await?;
    let rows: Vec<NamedRow> = sqlx::query_as(concat!(
        named!(),
        " WHERE (cardinality($1::text[]) = 0 OR r.status = ANY($1))
            AND ($2::uuid IS NULL OR r.target_uuid = $2)
          ORDER BY r.created_at DESC, r.id DESC
          LIMIT $3 OFFSET $4"
    ))
    .bind(&statuses)
    .bind(filter.target)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let reports = rows
        .into_iter()
        .map(ReportWithNames::try_from)
        .collect::<Result<_, _>>()?;
    Ok((u64::try_from(total).unwrap_or(0), reports))
}

/// What a change to a report came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportChange {
    Done(Box<Report>),
    NotFound,
    /// It's resolved or dismissed already.
    Closed,
}

async fn changed(
    pool: &PgPool,
    id: i64,
    row: Option<ReportRow>,
) -> Result<ReportChange, sqlx::Error> {
    if let Some(row) = row {
        return Ok(ReportChange::Done(Box::new(row.try_into()?)));
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM hallow.reports WHERE id = $1)")
            .bind(id)
            .fetch_one(pool)
            .await?;
    Ok(if exists {
        ReportChange::Closed
    } else {
        ReportChange::NotFound
    })
}

/// Claims an open report for `by`, or hands a claimed one over to them.
pub async fn claim_report(pool: &PgPool, id: i64, by: &Actor) -> Result<ReportChange, sqlx::Error> {
    let row: Option<ReportRow> = sqlx::query_as(concat!(
        "UPDATE hallow.reports AS r
         SET status = 'claimed', claimed_at = greatest(now(), created_at), claimed_by_kind = $2,
             claimed_by_uuid = $3, claimed_by_name = $4, claimed_by_panel_user = $5
         WHERE id = $1 AND status IN ('open', 'claimed')
         RETURNING ",
        columns!()
    ))
    .bind(id)
    .bind(by.kind())
    .bind(by.uuid())
    .bind(by.name())
    .bind(by.panel_user())
    .fetch_optional(pool)
    .await?;
    changed(pool, id, row).await
}

/// Puts a claimed report back in the open queue (an open one stays open).
pub async fn unclaim_report(pool: &PgPool, id: i64) -> Result<ReportChange, sqlx::Error> {
    let row: Option<ReportRow> = sqlx::query_as(concat!(
        "UPDATE hallow.reports AS r
         SET status = 'open', claimed_at = NULL, claimed_by_kind = NULL, claimed_by_uuid = NULL,
             claimed_by_name = NULL, claimed_by_panel_user = NULL
         WHERE id = $1 AND status IN ('open', 'claimed')
         RETURNING ",
        columns!()
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    changed(pool, id, row).await
}

/// Resolves or dismisses an open or claimed report. `status` must be
/// [`ReportStatus::Resolved`] or [`ReportStatus::Dismissed`]; only a
/// resolution carries a punishment.
pub async fn close_report(
    pool: &PgPool,
    id: i64,
    status: ReportStatus,
    by: &Actor,
    note: Option<&str>,
    punishment_id: Option<i64>,
) -> Result<ReportChange, sqlx::Error> {
    debug_assert!(status.is_closed());
    let row: Option<ReportRow> = sqlx::query_as(concat!(
        "UPDATE hallow.reports AS r
         SET status = $2, closed_at = greatest(now(), created_at), closed_by_kind = $3,
             closed_by_uuid = $4, closed_by_name = $5, closed_by_panel_user = $6, note = $7,
             punishment_id = $8
         WHERE id = $1 AND status IN ('open', 'claimed')
         RETURNING ",
        columns!()
    ))
    .bind(id)
    .bind(status.as_str())
    .bind(by.kind())
    .bind(by.uuid())
    .bind(by.name())
    .bind(by.panel_user())
    .bind(note)
    .bind(punishment_id)
    .fetch_optional(pool)
    .await?;
    changed(pool, id, row).await
}
