//! Server kind versions over HTTP (docs/SERVERS.md#versions,
//! docs/INSTANCES.md#versions): recording versions, the `live` and `canary`
//! channels, promote and roll back, and the audit log. Also which version
//! a new instance starts on ([`choose`]).
//!
//! | Route | |
//! |---|---|
//! | `GET /v1/kinds/{name}/versions` | versions, channels and instance counts |
//! | `POST /v1/kinds/{name}/versions` | records a version (CI) |
//! | `POST /v1/kinds/{name}/canary` | a share of new instances on a version |
//! | `POST /v1/kinds/{name}/canary/stop` | no more canary instances |
//! | `POST /v1/kinds/{name}/promote` | makes a version (default: the canary) live |
//! | `POST /v1/kinds/{name}/rollback` | makes the previous version live again |
//! | `GET /v1/version-audit` | every action, newest first |

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use axum::Json;
use axum::{
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
    http::StatusCode,
};
use chrono::Utc;
use hallow_db::{ChannelChange, KindChannels, KindState, NewVersionAudit, PgPool};
use hallow_proto::{
    ErrorCode,
    fleet::FieldErrors,
    instance::{InstanceInfo, KindSpec},
    routes,
    version::{
        Actor, AddVersion, Canary, KindVersion, KindVersions, Promote, Rollback, SetCanary,
        VersionAction, VersionAuditEntry, is_dev_version, is_version_name,
    },
};
use semver::{Version, VersionReq};
use serde::Deserialize;

use crate::{AppState, error::Failure, kinds::is_kind_name};

type Shared = State<Arc<AppState>>;
type Reply<T> = Result<T, Failure>;

/// The server base image version released with this control plane
/// (`templates/server/VERSION`), the network's unless `HALLOW_BASE_VERSION`
/// says otherwise.
pub const SHIPPED_BASE_VERSION: &str = include_str!("../../../templates/server/VERSION");

/// How versions may go live on this network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionsConfig {
    /// The base image version the network runs: a version goes canary or
    /// live only if its `base` range takes it (`HALLOW_BASE_VERSION`).
    pub base_version: Version,
    /// Whether dev builds (`sha-…`, `dev`) may go canary or live
    /// (`HALLOW_DEV_VERSIONS`). Off for production.
    pub dev_versions: bool,
}

impl Default for VersionsConfig {
    fn default() -> Self {
        Self {
            base_version: Version::parse(SHIPPED_BASE_VERSION.trim())
                .expect("templates/server/VERSION is a version"),
            dev_versions: false,
        }
    }
}

/// A kind's channels as the instance planner uses them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindRollout {
    pub live: Option<String>,
    pub canary: Option<CanaryImage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanaryImage {
    pub version: String,
    pub image: String,
    pub percent: u8,
}

/// Every kind's rollout, by kind.
pub async fn rollouts(
    db: &PgPool,
) -> Result<BTreeMap<String, KindRollout>, hallow_db::sqlx::Error> {
    Ok(hallow_db::list_rollouts(db)
        .await?
        .into_iter()
        .map(|r| {
            let canary = match (r.canary, r.canary_image, r.canary_percent) {
                (Some(version), Some(image), Some(percent)) => Some(CanaryImage {
                    version,
                    image,
                    percent: u8::try_from(percent).unwrap_or(100),
                }),
                _ => None,
            };
            (
                r.kind,
                KindRollout {
                    live: r.live,
                    canary,
                },
            )
        })
        .collect())
}

/// Whether the next instance goes on the canary: the canary keeps at most
/// `percent` of the kind's instances, counting the new one. At 10 % the
/// 10th instance is the first canary; at 50 % the 2nd.
#[must_use]
pub fn pick_canary(percent: u8, instances: u32, on_canary: u32) -> bool {
    u64::from(on_canary + 1) * 100 <= u64::from(percent) * u64::from(instances + 1)
}

/// The image and version a new instance of a kind starts on. `running`
/// are the versions of the kind's creating, starting and ready instances.
#[must_use]
pub fn choose(
    spec: &KindSpec,
    rollout: Option<&KindRollout>,
    running: &[Option<String>],
) -> (String, Option<String>) {
    let live = (spec.image.clone(), rollout.and_then(|r| r.live.clone()));
    let Some(canary) = rollout.and_then(|r| r.canary.as_ref()) else {
        return live;
    };
    let on_canary = running
        .iter()
        .filter(|v| v.as_deref() == Some(canary.version.as_str()))
        .count() as u32;
    if pick_canary(canary.percent, running.len() as u32, on_canary) {
        (canary.image.clone(), Some(canary.version.clone()))
    } else {
        live
    }
}

/// Instances on neither the live nor the canary image: they finish what
/// they are doing and aren't kept warm (docs/INSTANCES.md#versions).
#[must_use]
pub fn outdated(
    kinds: &BTreeMap<String, KindSpec>,
    rollouts: &BTreeMap<String, KindRollout>,
    instances: &[InstanceInfo],
) -> BTreeSet<String> {
    instances
        .iter()
        .filter(|i| {
            let Some(spec) = kinds.get(&i.kind) else {
                return false;
            };
            let canary = rollouts
                .get(&i.kind)
                .and_then(|r| r.canary.as_ref())
                .map(|c| c.image.as_str());
            i.image != spec.image && Some(i.image.as_str()) != canary
        })
        .map(|i| i.name.clone())
        .collect()
}

fn db(state: &AppState) -> Reply<&PgPool> {
    state
        .db
        .as_ref()
        .ok_or_else(|| Failure::new(ErrorCode::Unavailable, "versions need the database"))
}

fn check_name(name: &str) -> Reply<()> {
    if is_kind_name(name) {
        Ok(())
    } else {
        Err(Failure::bad_request("not a kind name"))
    }
}

fn check_actor(actor: &Actor, fields: &mut FieldErrors) {
    if actor.by.trim().is_empty() || actor.by.chars().count() > 64 {
        fields.insert("by".into(), "must be 1 to 64 characters".into());
    }
    if let Some(user) = &actor.panel_user
        && (user.is_empty() || user.chars().count() > 64)
    {
        fields.insert("panel_user".into(), "must be 1 to 64 characters".into());
    }
}

fn fields_ok(fields: FieldErrors) -> Reply<()> {
    if fields.is_empty() {
        Ok(())
    } else {
        Err(Failure::invalid_fields("invalid request", fields))
    }
}

fn conflict(message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::Conflict, message)
}

/// What is wrong with a version to record, by field.
#[must_use]
pub fn check_add(add: &AddVersion) -> FieldErrors {
    let mut fields = FieldErrors::new();
    if !is_version_name(&add.version) {
        fields.insert(
            "version".into(),
            "must be up to 64 letters, digits, '.', '_', '+' and '-'".into(),
        );
    }
    if add.image.trim().is_empty()
        || add.image.len() > 255
        || add.image.contains(char::is_whitespace)
    {
        fields.insert("image".into(), "must be an image reference".into());
    }
    let base = VersionReq::parse(&add.base);
    if base.is_err() || add.base.len() > 128 {
        fields.insert("base".into(), "must be a semver range, like ^0.2".into());
    }
    if let Some(built_on) = &add.base_version {
        match Version::parse(built_on) {
            Err(_) => {
                fields.insert(
                    "base_version".into(),
                    "must be a version, like 0.2.0".into(),
                );
            }
            Ok(built_on) => {
                if let Ok(base) = &base
                    && !base.matches(&built_on)
                {
                    fields.insert(
                        "base_version".into(),
                        format!("{built_on} is outside base {}", add.base),
                    );
                }
            }
        }
    }
    if let Some(commit) = &add.commit
        && (commit.is_empty()
            || commit.len() > 64
            || !commit.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        fields.insert("commit".into(), "must be a git commit hash".into());
    }
    check_actor(&add.actor, &mut fields);
    fields
}

/// Whether `version` may go canary or live on this network.
fn may_run(
    config: &VersionsConfig,
    state: &KindState,
    version: &str,
) -> Result<hallow_db::KindVersion, Failure> {
    let Some(found) = state.version(version) else {
        return Err(Failure::not_found(format!(
            "{} has no version {version}",
            state.kind.name
        )));
    };
    if is_dev_version(version) && !config.dev_versions {
        return Err(conflict(format!(
            "{version} is a dev build; this network doesn't run dev versions"
        )));
    }
    if !compatible(config, &found.base) {
        return Err(conflict(format!(
            "{} {version} needs base {}, and this network runs base {}",
            state.kind.name, found.base, config.base_version
        )));
    }
    Ok(found.clone())
}

fn compatible(config: &VersionsConfig, base: &str) -> bool {
    VersionReq::parse(base).is_ok_and(|req| req.matches(&config.base_version))
}

fn audit(state: &KindState, action: VersionAction, actor: &Actor) -> NewVersionAudit {
    NewVersionAudit {
        kind: state.kind.name.clone(),
        action: action.as_str(),
        version: None,
        from_version: None,
        image: None,
        percent: None,
        by: actor.by.trim().to_owned(),
        panel_user: actor.panel_user.clone(),
    }
}

fn no_canary(channels: &mut KindChannels) {
    channels.canary = None;
    channels.canary_percent = None;
    channels.canary_since = None;
}

/// The answer to every version route.
async fn answer(state: &AppState, kind: KindState) -> Reply<Json<KindVersions>> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    if state.instances.is_some() {
        for instance in state.store.instances().await? {
            if instance.kind == kind.kind.name
                && instance.state.is_live()
                && let Some(version) = instance.version
            {
                *counts.entry(version).or_default() += 1;
            }
        }
    }
    let config = &state.config.versions;
    let channels = kind.channels;
    let canary = match (
        channels.canary,
        channels.canary_percent,
        channels.canary_since,
    ) {
        (Some(version), Some(percent), Some(since)) => Some(Canary {
            version,
            percent: u8::try_from(percent).unwrap_or(100),
            since_ms: ms(since),
        }),
        _ => None,
    };
    Ok(Json(KindVersions {
        kind: kind.kind.name,
        live: channels.live,
        live_image: kind.kind.spec.0.image,
        previous: channels.previous,
        canary,
        base_version: config.base_version.to_string(),
        dev_versions: config.dev_versions,
        versions: kind
            .versions
            .into_iter()
            .map(|v| KindVersion {
                compatible: compatible(config, &v.base),
                dev: is_dev_version(&v.version),
                instances: counts.get(&v.version).copied().unwrap_or(0),
                version: v.version,
                image: v.image,
                base: v.base,
                base_version: v.base_version,
                commit: v.git_commit,
                added_ms: ms(v.added_at),
                added_by: v.added_by,
            })
            .collect(),
    }))
}

fn ms(at: chrono::DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

async fn change(
    state: &AppState,
    name: &str,
    decide: impl FnOnce(&KindState) -> Result<ChannelChange, Failure>,
) -> Reply<Json<KindVersions>> {
    match hallow_db::change_channels(db(state)?, name, decide).await? {
        None => Err(Failure::not_found("no such kind")),
        Some(Err(failure)) => Err(failure),
        Some(Ok(kind)) => {
            // The change takes effect from the next plan.
            if let Some(manager) = &state.instances {
                manager.kinds_changed();
            }
            answer(state, kind).await
        }
    }
}

pub async fn get(State(state): Shared, Path(name): Path<String>) -> Reply<Json<KindVersions>> {
    check_name(&name)?;
    let kind = hallow_db::kind_state(db(&state)?, &name)
        .await?
        .ok_or_else(|| Failure::not_found("no such kind"))?;
    answer(&state, kind).await
}

/// Records a version. The same version with the same image again is a
/// no-op (`200`); with another image it is a `409`.
pub async fn add(
    State(state): Shared,
    Path(name): Path<String>,
    body: Result<Json<AddVersion>, JsonRejection>,
) -> Reply<(StatusCode, Json<KindVersions>)> {
    check_name(&name)?;
    let Json(add) = body?;
    fields_ok(check_add(&add))?;
    let pool = db(&state)?;
    let new = hallow_db::NewVersion {
        kind: &name,
        version: &add.version,
        image: &add.image,
        base: &add.base,
        base_version: add.base_version.as_deref(),
        git_commit: add.commit.as_deref(),
        by: add.actor.by.trim(),
        panel_user: add.actor.panel_user.as_deref(),
    };
    let status = match hallow_db::add_version(pool, &new).await? {
        hallow_db::AddedVersion::NoKind => return Err(Failure::not_found("no such kind")),
        hallow_db::AddedVersion::Exists(existing) if existing.image != add.image => {
            return Err(conflict(format!(
                "{name} {} is already recorded with image {}",
                add.version, existing.image
            )));
        }
        hallow_db::AddedVersion::Exists(_) => StatusCode::OK,
        hallow_db::AddedVersion::Added(_) => {
            tracing::info!(kind = %name, version = %add.version, image = %add.image, by = %add.actor.by, "version added");
            StatusCode::CREATED
        }
    };
    let kind = hallow_db::kind_state(pool, &name)
        .await?
        .ok_or_else(|| Failure::not_found("no such kind"))?;
    Ok((status, answer(&state, kind).await?))
}

pub async fn canary(
    State(state): Shared,
    Path(name): Path<String>,
    body: Result<Json<SetCanary>, JsonRejection>,
) -> Reply<Json<KindVersions>> {
    check_name(&name)?;
    let Json(body) = body?;
    let mut fields = FieldErrors::new();
    check_actor(&body.actor, &mut fields);
    if !(1..=100).contains(&body.percent) {
        fields.insert("percent".into(), "must be 1 to 100".into());
    }
    fields_ok(fields)?;
    let config = state.config.versions.clone();
    let reply = change(&state, &name, |kind| {
        let version = may_run(&config, kind, &body.version)?;
        if kind.channels.live.as_deref() == Some(version.version.as_str())
            || kind.kind.spec.0.image == version.image
        {
            return Err(conflict(format!("{} is already live", version.version)));
        }
        let mut channels = kind.channels.clone();
        let same = channels.canary.as_deref() == Some(version.version.as_str());
        channels.canary = Some(version.version.clone());
        channels.canary_percent = Some(i16::from(body.percent));
        if !same || channels.canary_since.is_none() {
            channels.canary_since = Some(Utc::now());
        }
        let mut entry = audit(kind, VersionAction::Canary, &body.actor);
        entry.version = Some(version.version);
        entry.from_version = kind.channels.canary.clone();
        entry.percent = Some(i16::from(body.percent));
        Ok(ChannelChange {
            channels,
            image: None,
            audit: entry,
        })
    })
    .await?;
    tracing::info!(kind = %name, version = %body.version, percent = body.percent, by = %body.actor.by, "canary set");
    Ok(reply)
}

pub async fn stop_canary(
    State(state): Shared,
    Path(name): Path<String>,
    body: Result<Json<Actor>, JsonRejection>,
) -> Reply<Json<KindVersions>> {
    check_name(&name)?;
    let Json(actor) = body?;
    let mut fields = FieldErrors::new();
    check_actor(&actor, &mut fields);
    fields_ok(fields)?;
    let reply = change(&state, &name, |kind| {
        let Some(old) = kind.channels.canary.clone() else {
            return Err(conflict(format!("{} has no canary", kind.kind.name)));
        };
        let mut channels = kind.channels.clone();
        no_canary(&mut channels);
        let mut entry = audit(kind, VersionAction::StopCanary, &actor);
        entry.version = Some(old);
        Ok(ChannelChange {
            channels,
            image: None,
            audit: entry,
        })
    })
    .await?;
    tracing::info!(kind = %name, by = %actor.by, "canary stopped");
    Ok(reply)
}

pub async fn promote(
    State(state): Shared,
    Path(name): Path<String>,
    body: Result<Json<Promote>, JsonRejection>,
) -> Reply<Json<KindVersions>> {
    check_name(&name)?;
    let Json(body) = body?;
    let mut fields = FieldErrors::new();
    check_actor(&body.actor, &mut fields);
    fields_ok(fields)?;
    let config = state.config.versions.clone();
    let mut promoted = String::new();
    let reply = change(&state, &name, |kind| {
        let Some(wanted) = body
            .version
            .clone()
            .or_else(|| kind.channels.canary.clone())
        else {
            return Err(conflict(format!(
                "{} has no canary; name the version to promote",
                kind.kind.name
            )));
        };
        let version = may_run(&config, kind, &wanted)?;
        if kind.channels.live.as_deref() == Some(wanted.as_str()) {
            return Err(conflict(format!("{wanted} is already live")));
        }
        let mut channels = kind.channels.clone();
        if channels.live.is_some() {
            channels.previous.clone_from(&channels.live);
        }
        channels.live = Some(wanted.clone());
        if channels.canary.as_deref() == Some(wanted.as_str()) {
            no_canary(&mut channels);
        }
        let mut entry = audit(kind, VersionAction::Promote, &body.actor);
        entry.version = Some(wanted.clone());
        entry.from_version = kind.channels.live.clone();
        entry.image = Some(version.image.clone());
        promoted = wanted;
        Ok(ChannelChange {
            channels,
            image: Some(version.image),
            audit: entry,
        })
    })
    .await?;
    tracing::info!(kind = %name, version = %promoted, by = %body.actor.by, "version promoted");
    Ok(reply)
}

pub async fn rollback(
    State(state): Shared,
    Path(name): Path<String>,
    body: Result<Json<Rollback>, JsonRejection>,
) -> Reply<Json<KindVersions>> {
    check_name(&name)?;
    let Json(body) = body?;
    let mut fields = FieldErrors::new();
    check_actor(&body.actor, &mut fields);
    fields_ok(fields)?;
    let config = state.config.versions.clone();
    let mut target = String::new();
    let reply = change(&state, &name, |kind| {
        let Some(wanted) = body
            .version
            .clone()
            .or_else(|| kind.channels.previous.clone())
        else {
            return Err(conflict(format!(
                "{} has no previous version; name the version to roll back to",
                kind.kind.name
            )));
        };
        let version = may_run(&config, kind, &wanted)?;
        if kind.channels.live.as_deref() == Some(wanted.as_str()) {
            return Err(conflict(format!("{wanted} is already live")));
        }
        let mut channels = kind.channels.clone();
        channels.live = Some(wanted.clone());
        // Rolling back again needs a version named.
        channels.previous = None;
        no_canary(&mut channels);
        let mut entry = audit(kind, VersionAction::Rollback, &body.actor);
        entry.version = Some(wanted.clone());
        entry.from_version = kind.channels.live.clone();
        entry.image = Some(version.image.clone());
        target = wanted;
        Ok(ChannelChange {
            channels,
            image: Some(version.image),
            audit: entry,
        })
    })
    .await?;
    tracing::info!(kind = %name, version = %target, by = %body.actor.by, "rolled back");
    Ok(reply)
}

#[derive(Deserialize)]
pub struct AuditParams {
    kind: Option<String>,
    limit: Option<u32>,
}

pub async fn audit_log(
    State(state): Shared,
    query: Result<Query<AuditParams>, QueryRejection>,
) -> Reply<Json<Vec<VersionAuditEntry>>> {
    let Query(params) = query?;
    if let Some(kind) = &params.kind {
        check_name(kind)?;
    }
    let limit = params
        .limit
        .unwrap_or(routes::VERSION_AUDIT_DEFAULT)
        .clamp(1, routes::VERSION_AUDIT_MAX);
    let rows =
        hallow_db::version_audit(db(&state)?, params.kind.as_deref(), i64::from(limit)).await?;
    Ok(Json(
        rows.into_iter()
            .filter_map(|row| {
                Some(VersionAuditEntry {
                    id: row.id,
                    kind: row.kind,
                    action: VersionAction::parse(&row.action)?,
                    version: row.version,
                    from_version: row.from_version,
                    image: row.image,
                    percent: row.percent.and_then(|p| u8::try_from(p).ok()),
                    by: row.by_name,
                    panel_user: row.panel_user,
                    at_ms: ms(row.at),
                })
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::instance::{InstanceState, KindPlayers, KindRole, KindScaling};
    use uuid::Uuid;

    fn spec(image: &str) -> KindSpec {
        KindSpec {
            image: image.into(),
            role: KindRole::Game,
            lobby: None,
            players: KindPlayers { min: 1, max: 8 },
            scaling: KindScaling::default(),
            env: BTreeMap::new(),
            memory_mb: None,
            updated_by: None,
        }
    }

    fn instance(name: &str, image: &str, version: &str) -> InstanceInfo {
        InstanceInfo {
            name: name.into(),
            kind: "game".into(),
            uuid: Uuid::nil(),
            image: image.into(),
            version: Some(version.into()),
            node: "n".into(),
            state: InstanceState::Ready,
            state_since_ms: 0,
            created_ms: 0,
            address: None,
            port: None,
            backend_id: None,
            players: 0,
            had_players: false,
            empty_since_ms: None,
            reason: None,
        }
    }

    #[test]
    fn the_canary_keeps_its_share() {
        // 10 %: the 10th instance is the first canary, the 20th the second.
        let mut on_canary = 0;
        let mut picks = Vec::new();
        for n in 0..20 {
            let pick = pick_canary(10, n, on_canary);
            if pick {
                on_canary += 1;
                picks.push(n + 1);
            }
        }
        assert_eq!(picks, [10, 20]);
        // 50 %: every second one.
        assert!(!pick_canary(50, 0, 0));
        assert!(pick_canary(50, 1, 0));
        assert!(!pick_canary(50, 2, 1));
        // 100 %: all of them.
        assert!(pick_canary(100, 0, 0));
        assert!(pick_canary(100, 5, 5));
    }

    #[test]
    fn new_instances_start_on_live_or_canary() {
        let spec = spec("game:1.0.0");
        assert_eq!(choose(&spec, None, &[]), ("game:1.0.0".into(), None));
        let mut rollout = KindRollout {
            live: Some("1.0.0".into()),
            canary: Some(CanaryImage {
                version: "1.1.0".into(),
                image: "game:1.1.0".into(),
                percent: 50,
            }),
        };
        let a = Some("1.0.0".to_owned());
        let b = Some("1.1.0".to_owned());
        assert_eq!(
            choose(&spec, Some(&rollout), std::slice::from_ref(&a)),
            ("game:1.1.0".into(), Some("1.1.0".into()))
        );
        assert_eq!(
            choose(&spec, Some(&rollout), &[a.clone(), b]),
            ("game:1.0.0".into(), Some("1.0.0".into()))
        );
        rollout.canary = None;
        assert_eq!(
            choose(&spec, Some(&rollout), &[a]),
            ("game:1.0.0".into(), Some("1.0.0".into()))
        );
    }

    #[test]
    fn instances_off_live_and_canary_are_outdated() {
        let kinds = BTreeMap::from([("game".to_owned(), spec("game:1.1.0"))]);
        let rollouts = BTreeMap::from([(
            "game".to_owned(),
            KindRollout {
                live: Some("1.1.0".into()),
                canary: Some(CanaryImage {
                    version: "1.2.0".into(),
                    image: "game:1.2.0".into(),
                    percent: 10,
                }),
            },
        )]);
        let instances = [
            instance("old", "game:1.0.0", "1.0.0"),
            instance("live", "game:1.1.0", "1.1.0"),
            instance("canary", "game:1.2.0", "1.2.0"),
        ];
        assert_eq!(
            outdated(&kinds, &rollouts, &instances),
            BTreeSet::from(["old".to_owned()])
        );
    }

    #[test]
    fn versions_are_checked_by_field() {
        let mut add = AddVersion {
            version: "1.4.0".into(),
            image: "ghcr.io/xena-studios/skywars:1.4.0".into(),
            base: "^0.2".into(),
            base_version: Some("0.2.0".into()),
            commit: Some("3f2a9c1".into()),
            actor: Actor::new("ci"),
        };
        assert!(check_add(&add).is_empty());
        add.version = "a b".into();
        add.base = "not a range".into();
        add.commit = Some("xyz".into());
        add.actor.by = String::new();
        let keys: Vec<String> = check_add(&add).into_keys().collect();
        assert_eq!(keys, ["base", "by", "commit", "version"]);
        add.base = "^0.3".into();
        assert!(check_add(&add).contains_key("base_version"));
    }

    #[test]
    fn the_shipped_base_version_parses() {
        let config = VersionsConfig::default();
        assert!(compatible(&config, &format!("={}", config.base_version)));
        assert!(!compatible(&config, ">=999"));
    }
}
