//! The fleet manager's nodes and its audit log.
//!
//! A node row is kept after it is cancelled, so this month's spend can be
//! worked out from the table. Events are only ever added.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, types::Json};

/// One node the fleet manager knows.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct FleetNode {
    pub name: String,
    pub provider: String,
    pub provider_id: Option<String>,
    /// `base` or `burst`.
    pub kind: String,
    pub region: String,
    pub machine_type: String,
    pub slots: i32,
    pub cost_micros: i64,
    /// `hour` or `month`.
    pub billing_step: String,
    pub monthly_cap_micros: Option<i64>,
    /// `setting_up`, `ready`, `draining` or `cancelled`.
    pub phase: String,
    pub dry_run: bool,
    pub floor: bool,
    pub created_at: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,
    pub draining_since: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub awaiting_approval: bool,
    pub approved_by: Option<String>,
}

macro_rules! fleet_node_columns {
    () => {
        "name, provider, provider_id, kind, region, machine_type, slots, cost_micros, billing_step, \
         monthly_cap_micros, phase, dry_run, floor, created_at, ready_at, draining_since, \
         cancelled_at, awaiting_approval, approved_by"
    };
}

/// A node to record.
#[derive(Clone, Debug)]
pub struct NewFleetNode<'a> {
    pub name: &'a str,
    pub provider: &'a str,
    pub provider_id: Option<&'a str>,
    pub kind: &'a str,
    pub region: &'a str,
    pub machine_type: &'a str,
    pub slots: i32,
    pub cost_micros: i64,
    pub billing_step: &'a str,
    pub monthly_cap_micros: Option<i64>,
    pub phase: &'a str,
    pub dry_run: bool,
    pub floor: bool,
    pub created_at: DateTime<Utc>,
}

/// Records a bought node.
pub async fn add_fleet_node(
    pool: &PgPool,
    node: &NewFleetNode<'_>,
) -> Result<FleetNode, sqlx::Error> {
    sqlx::query_as(concat!(
        "INSERT INTO hallow.fleet_nodes (name, provider, provider_id, kind, region, machine_type,
             slots, cost_micros, billing_step, monthly_cap_micros, phase, dry_run, floor, created_at,
             ready_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
             CASE WHEN $11 = 'ready' THEN $14 END)
         RETURNING ",
        fleet_node_columns!()
    ))
    .bind(node.name)
    .bind(node.provider)
    .bind(node.provider_id)
    .bind(node.kind)
    .bind(node.region)
    .bind(node.machine_type)
    .bind(node.slots)
    .bind(node.cost_micros)
    .bind(node.billing_step)
    .bind(node.monthly_cap_micros)
    .bind(node.phase)
    .bind(node.dry_run)
    .bind(node.floor)
    .bind(node.created_at)
    .fetch_one(pool)
    .await
}

/// Records a floor node from the fleet file, or updates its size and price.
/// A floor node keeps its creation time, and is ready whatever it was.
pub async fn put_floor_node(
    pool: &PgPool,
    node: &NewFleetNode<'_>,
) -> Result<FleetNode, sqlx::Error> {
    sqlx::query_as(concat!(
        "INSERT INTO hallow.fleet_nodes (name, provider, provider_id, kind, region, machine_type,
             slots, cost_micros, billing_step, monthly_cap_micros, phase, dry_run, floor, created_at,
             ready_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'ready', false, true, $11, $11)
         ON CONFLICT (name) DO UPDATE SET
             provider = EXCLUDED.provider, kind = EXCLUDED.kind, region = EXCLUDED.region,
             machine_type = EXCLUDED.machine_type, slots = EXCLUDED.slots,
             cost_micros = EXCLUDED.cost_micros, billing_step = EXCLUDED.billing_step,
             monthly_cap_micros = EXCLUDED.monthly_cap_micros, floor = true, phase = 'ready',
             ready_at = COALESCE(hallow.fleet_nodes.ready_at, EXCLUDED.ready_at),
             draining_since = NULL, cancelled_at = NULL, awaiting_approval = false
         RETURNING ",
        fleet_node_columns!()
    ))
    .bind(node.name)
    .bind(node.provider)
    .bind(node.provider_id)
    .bind(node.kind)
    .bind(node.region)
    .bind(node.machine_type)
    .bind(node.slots)
    .bind(node.cost_micros)
    .bind(node.billing_step)
    .bind(node.monthly_cap_micros)
    .bind(node.created_at)
    .fetch_one(pool)
    .await
}

/// Every node not cancelled, and those cancelled since `since` (for spend),
/// floor first, then oldest first.
pub async fn fleet_nodes(
    pool: &PgPool,
    cancelled_since: DateTime<Utc>,
) -> Result<Vec<FleetNode>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        fleet_node_columns!(),
        " FROM hallow.fleet_nodes
         WHERE phase <> 'cancelled' OR cancelled_at >= $1
         ORDER BY floor DESC, created_at, name"
    ))
    .bind(cancelled_since)
    .fetch_all(pool)
    .await
}

pub async fn fleet_node(pool: &PgPool, name: &str) -> Result<Option<FleetNode>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        fleet_node_columns!(),
        " FROM hallow.fleet_nodes WHERE name = $1"
    ))
    .bind(name)
    .fetch_optional(pool)
    .await
}

/// Moves a node to `to` if it is in one of `from`, at `at`: `ready` sets
/// `ready_at` the first time and clears the drain, `draining` sets
/// `draining_since`, `cancelled` sets `cancelled_at`. `None` if the node
/// isn't in one of `from`. Floor nodes never leave `ready`.
pub async fn set_fleet_node_phase(
    pool: &PgPool,
    name: &str,
    from: &[&str],
    to: &str,
    at: DateTime<Utc>,
) -> Result<Option<FleetNode>, sqlx::Error> {
    let from: Vec<String> = from.iter().map(|s| (*s).to_owned()).collect();
    sqlx::query_as(concat!(
        "UPDATE hallow.fleet_nodes SET
             phase = $3,
             ready_at = CASE WHEN $3 = 'ready' THEN COALESCE(ready_at, $4) ELSE ready_at END,
             draining_since = CASE WHEN $3 = 'draining' THEN $4
                                   WHEN $3 = 'ready' THEN NULL ELSE draining_since END,
             cancelled_at = CASE WHEN $3 = 'cancelled' THEN $4 ELSE cancelled_at END,
             awaiting_approval = CASE WHEN $3 = 'draining' THEN awaiting_approval ELSE false END,
             approved_by = CASE WHEN $3 = 'ready' THEN NULL ELSE approved_by END
         WHERE name = $1 AND phase = ANY($2) AND (NOT floor OR $3 = 'ready')
         RETURNING ",
        fleet_node_columns!()
    ))
    .bind(name)
    .bind(&from)
    .bind(to)
    .bind(at)
    .fetch_optional(pool)
    .await
}

/// Marks a draining node as waiting for staff to approve its cancellation.
/// Returns whether it wasn't already.
pub async fn await_fleet_approval(pool: &PgPool, name: &str) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE hallow.fleet_nodes SET awaiting_approval = true
         WHERE name = $1 AND phase = 'draining' AND NOT awaiting_approval",
    )
    .bind(name)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Approves the cancellation of a node waiting for it.
pub async fn approve_fleet_cancel(
    pool: &PgPool,
    name: &str,
    by: &str,
) -> Result<Option<FleetNode>, sqlx::Error> {
    sqlx::query_as(concat!(
        "UPDATE hallow.fleet_nodes SET approved_by = $2
         WHERE name = $1 AND phase = 'draining' AND awaiting_approval
         RETURNING ",
        fleet_node_columns!()
    ))
    .bind(name)
    .bind(by)
    .fetch_optional(pool)
    .await
}

/// One audit log entry.
#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct FleetEvent {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub action: String,
    pub node: Option<String>,
    pub dry_run: bool,
    pub ok: bool,
    pub message: String,
    pub by: Option<String>,
    pub detail: Option<Json<serde_json::Value>>,
}

/// An entry to add.
#[derive(Clone, Debug, PartialEq)]
pub struct NewFleetEvent {
    pub at: DateTime<Utc>,
    /// `decision`, `provider`, `node`, `proxy`, `staff` or `alert`.
    pub kind: &'static str,
    pub action: String,
    pub node: Option<String>,
    pub dry_run: bool,
    pub ok: bool,
    pub message: String,
    pub by: Option<String>,
    pub detail: Option<serde_json::Value>,
}

/// Adds entries in one transaction, in order.
pub async fn add_fleet_events(pool: &PgPool, events: &[NewFleetEvent]) -> Result<(), sqlx::Error> {
    if events.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for event in events {
        sqlx::query(
            "INSERT INTO hallow.fleet_events (at, kind, action, node, dry_run, ok, message, by, detail)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(event.at)
        .bind(event.kind)
        .bind(&event.action)
        .bind(&event.node)
        .bind(event.dry_run)
        .bind(event.ok)
        .bind(&event.message)
        .bind(&event.by)
        .bind(event.detail.as_ref().map(Json))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// The latest entries, newest first: before entry `before` if given, of
/// `kind` if given.
pub async fn fleet_events(
    pool: &PgPool,
    limit: i64,
    before: Option<i64>,
    kind: Option<&str>,
) -> Result<Vec<FleetEvent>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, at, kind, action, node, dry_run, ok, message, by, detail
         FROM hallow.fleet_events
         WHERE ($2::bigint IS NULL OR id < $2) AND ($3::text IS NULL OR kind = $3)
         ORDER BY id DESC LIMIT $1",
    )
    .bind(limit)
    .bind(before)
    .bind(kind)
    .fetch_all(pool)
    .await
}
