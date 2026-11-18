//! The fleet manager over HTTP, for the panel's Fleet page:
//! `GET /v1/fleet`, `GET /v1/fleet/events`, `PUT /v1/fleet/dry-run`, and
//! `POST /v1/fleet/nodes/{name}/drain|reinstate|approve-cancel`.

use std::sync::Arc;

use axum::{
    extract::rejection::QueryRejection,
    extract::{Path, Query, State},
};
use chrono::Utc;
use chrono_tz::Tz;
use hallow_db::{NewFleetEvent, PgPool};
use hallow_proto::{
    ErrorCode,
    fleet::{FleetSettingsRecord, UpdateFleetSettings},
    fleet_state::{
        FleetEvent, FleetEventKind, FleetNode, FleetNodeAction, FleetProxies, FleetState, SetDryRun,
    },
    routes,
};
use serde::Deserialize;

use super::{NodeUsage, last_state, node_info};
use crate::{AppState, error::Failure};

type Shared = State<Arc<AppState>>;
type Reply<T> = Result<T, Failure>;

fn need_db(state: &AppState) -> Reply<&PgPool> {
    state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "the fleet manager needs a database (HALLOW_DATABASE_URL)",
        )
    })
}

fn check_by(by: &str) -> Reply<String> {
    let by = by.trim();
    if (1..=64).contains(&by.chars().count()) {
        Ok(by.to_owned())
    } else {
        Err(Failure::bad_request("by must be 1 to 64 characters"))
    }
}

fn ms(at: chrono::DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

/// `GET /v1/fleet`: the last tick's status, with every node's phase as it is
/// now.
pub async fn state(State(state): Shared) -> Reply<axum::Json<FleetState>> {
    let db = need_db(&state)?;
    let settings = crate::fleet::current(db).await?.settings;
    let last = if state.config.fleet.is_some() {
        last_state(&state.store).await?
    } else {
        None
    };
    let mut out = last.unwrap_or_else(|| FleetState {
        enabled: false,
        dry_run: settings.dry_run,
        paused: settings.paused,
        currency: settings.currency.clone(),
        last_tick_ms: None,
        free_slots: 0,
        headroom_target: 0,
        spend_today: "0.0000".into(),
        spend_month: "0.0000".into(),
        forecast: "0.0000".into(),
        daily_cap: settings.daily_cap.clone(),
        monthly_budget: settings.monthly_budget.clone(),
        capped: None,
        holds: Vec::new(),
        queues: Vec::new(),
        proxies: FleetProxies::default(),
        nodes: Vec::new(),
    });
    out.enabled = state.config.fleet.is_some();
    out.dry_run = settings.dry_run;
    out.paused = settings.paused;
    if out.enabled {
        let tz: Tz = settings.timezone.parse().unwrap_or(chrono_tz::UTC);
        let now = Utc::now();
        let (month_start, _) = hallow_fleet::billing::periods(now, tz);
        let usage: std::collections::BTreeMap<String, NodeUsage> = out
            .nodes
            .iter()
            .map(|n| {
                (
                    n.name.clone(),
                    NodeUsage {
                        instances: n.instances,
                        used_slots: n.used_slots,
                        players: n.players,
                    },
                )
            })
            .collect();
        out.nodes = hallow_db::fleet_nodes(db, month_start)
            .await?
            .iter()
            .filter(|r| r.phase != "cancelled")
            .map(|r| node_info(r, usage.get(&r.name), now, tz))
            .collect();
    }
    Ok(axum::Json(out))
}

#[derive(Deserialize)]
pub struct EventParams {
    limit: Option<u32>,
    before: Option<i64>,
    kind: Option<String>,
}

/// `GET /v1/fleet/events?limit=&before=&kind=`, newest first.
pub async fn events(
    State(state): Shared,
    query: Result<Query<EventParams>, QueryRejection>,
) -> Reply<axum::Json<Vec<FleetEvent>>> {
    let Query(params) = query?;
    let db = need_db(&state)?;
    let limit = params
        .limit
        .unwrap_or(routes::FLEET_EVENTS_DEFAULT)
        .clamp(1, routes::FLEET_EVENTS_MAX);
    if let Some(kind) = &params.kind
        && FleetEventKind::parse(kind).is_none()
    {
        return Err(Failure::bad_request(
            "kind must be decision, provider, node, proxy, staff or alert",
        ));
    }
    let rows = hallow_db::fleet_events(db, i64::from(limit), params.before, params.kind.as_deref())
        .await?;
    Ok(axum::Json(
        rows.into_iter()
            .map(|e| FleetEvent {
                id: e.id,
                at_ms: ms(e.at),
                kind: FleetEventKind::parse(&e.kind).unwrap_or(FleetEventKind::Alert),
                action: e.action,
                node: e.node,
                dry_run: e.dry_run,
                ok: e.ok,
                message: e.message,
                by: e.by,
                detail: e.detail.map(|d| d.0.to_string()),
            })
            .collect(),
    ))
}

async fn staff_event(
    db: &PgPool,
    action: &str,
    node: Option<&str>,
    dry_run: bool,
    by: &str,
    message: String,
) -> Reply<()> {
    hallow_db::add_fleet_events(
        db,
        &[NewFleetEvent {
            at: Utc::now(),
            kind: FleetEventKind::Staff.as_str(),
            action: action.to_owned(),
            node: node.map(str::to_owned),
            dry_run,
            ok: true,
            message,
            by: Some(by.to_owned()),
            detail: None,
        }],
    )
    .await?;
    Ok(())
}

/// `PUT /v1/fleet/dry-run`: one setting, saved with its history like any
/// other change.
pub async fn set_dry_run(
    State(state): Shared,
    crate::api::Body(req): crate::api::Body<SetDryRun>,
) -> Reply<axum::Json<FleetSettingsRecord>> {
    let db = need_db(&state)?;
    let by = check_by(&req.updated_by)?;
    let mut settings = crate::fleet::current(db).await?.settings;
    let was = settings.dry_run;
    settings.dry_run = req.dry_run;
    let saved = crate::fleet::save(
        db,
        &UpdateFleetSettings {
            settings,
            updated_by: by.clone(),
        },
    )
    .await?;
    if was != req.dry_run {
        let message = if req.dry_run {
            format!("{by} turned dry run on")
        } else {
            format!("{by} turned dry run off: providers will be called")
        };
        tracing::warn!(%by, dry_run = req.dry_run, "fleet dry run changed");
        staff_event(db, "dry_run", None, req.dry_run, &by, message).await?;
    }
    Ok(axum::Json(saved))
}

async fn node_action(
    state: &AppState,
    name: &str,
    action: &str,
    by: &str,
) -> Reply<axum::Json<FleetNode>> {
    let db = need_db(state)?;
    let by = check_by(by)?;
    let Some(row) = hallow_db::fleet_node(db, name).await? else {
        return Err(Failure::not_found("no such fleet node"));
    };
    let now = Utc::now();
    let conflict = |message: &str| Failure::new(ErrorCode::Conflict, message);
    let updated = match action {
        "drain" => {
            if row.floor {
                return Err(conflict("floor nodes are never drained"));
            }
            hallow_db::set_fleet_node_phase(db, name, &["ready", "setting_up"], "draining", now)
                .await?
                .ok_or_else(|| conflict("only a ready or setting-up node can be drained"))?
        }
        "reinstate" => hallow_db::set_fleet_node_phase(db, name, &["draining"], "ready", now)
            .await?
            .ok_or_else(|| conflict("only a draining node can be reinstated"))?,
        _ => hallow_db::approve_fleet_cancel(db, name, &by)
            .await?
            .ok_or_else(|| conflict("the node isn't waiting for approval"))?,
    };
    let settings = crate::fleet::current(db).await?.settings;
    let message = match action {
        "drain" => format!("{by} drained {name}"),
        "reinstate" => format!("{by} reinstated {name}"),
        _ => format!("{by} approved cancelling {name}"),
    };
    tracing::info!(node = %name, %by, action, "fleet node staff action");
    staff_event(db, action, Some(name), settings.dry_run, &by, message).await?;
    let tz: Tz = settings.timezone.parse().unwrap_or(chrono_tz::UTC);
    let usage = last_state(&state.store)
        .await?
        .and_then(|s| s.nodes.into_iter().find(|n| n.name == name))
        .map(|n| NodeUsage {
            instances: n.instances,
            used_slots: n.used_slots,
            players: n.players,
        });
    Ok(axum::Json(node_info(&updated, usage.as_ref(), now, tz)))
}

pub async fn drain(
    State(state): Shared,
    Path(name): Path<String>,
    crate::api::Body(req): crate::api::Body<FleetNodeAction>,
) -> Reply<axum::Json<FleetNode>> {
    node_action(&state, &name, "drain", &req.by).await
}

pub async fn reinstate(
    State(state): Shared,
    Path(name): Path<String>,
    crate::api::Body(req): crate::api::Body<FleetNodeAction>,
) -> Reply<axum::Json<FleetNode>> {
    node_action(&state, &name, "reinstate", &req.by).await
}

pub async fn approve_cancel(
    State(state): Shared,
    Path(name): Path<String>,
    crate::api::Body(req): crate::api::Body<FleetNodeAction>,
) -> Reply<axum::Json<FleetNode>> {
    node_action(&state, &name, "approve_cancel", &req.by).await
}
