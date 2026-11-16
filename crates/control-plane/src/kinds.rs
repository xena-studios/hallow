//! The kinds registry and instances over HTTP (docs/INSTANCES.md):
//! `GET /v1/kinds`, `GET|PUT|DELETE /v1/kinds/{name}`, `GET /v1/instances`
//! and `DELETE /v1/instances/{name}`.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
};
use hallow_db::{PgPool, ServerKind};
use hallow_proto::{
    ErrorCode,
    fleet::FieldErrors,
    instance::{InstanceInfo, KindInfo, KindSpec},
};

use crate::{AppState, error::Failure};

type Shared = State<Arc<AppState>>;
type Reply<T> = Result<T, Failure>;

/// `hallow.toml`'s rule for names: up to 40 lowercase letters, digits and
/// single dashes, starting with a letter.
#[must_use]
pub fn is_kind_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 40
        && bytes[0].is_ascii_lowercase()
        && !name.ends_with('-')
        && !name.contains("--")
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn is_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && name.len() <= 128
}

/// What is wrong with a spec, by field.
#[must_use]
pub fn check_spec(spec: &KindSpec) -> FieldErrors {
    let mut fields = FieldErrors::new();
    if spec.image.trim().is_empty()
        || spec.image.len() > 255
        || spec.image.contains(char::is_whitespace)
    {
        fields.insert("image".into(), "must be an image reference".into());
    }
    if spec.players.max == 0 {
        fields.insert("players.max".into(), "must be at least 1".into());
    }
    if spec.players.min == 0 || spec.players.min > spec.players.max {
        fields.insert("players.min".into(), "must be 1 to players.max".into());
    }
    if spec.scaling.max_instances == Some(0) {
        fields.insert("scaling.max_instances".into(), "must be at least 1".into());
    }
    if let Some(max) = spec.scaling.max_instances
        && spec.scaling.warm > max
    {
        fields.insert(
            "scaling.warm".into(),
            "can't be more than max_instances".into(),
        );
    }
    if let Some(lobby) = &spec.lobby
        && !is_kind_name(lobby)
    {
        fields.insert("lobby".into(), "must be a kind name".into());
    }
    if let Some(memory) = spec.memory_mb
        && !(128..=65536).contains(&memory)
    {
        fields.insert("memory_mb".into(), "must be 128 to 65536".into());
    }
    for name in spec.env.keys() {
        if !is_env_name(name) {
            fields.insert(format!("env.{name}"), "isn't a variable name".into());
        } else if name.starts_with("HALLOW_") {
            fields.insert(format!("env.{name}"), "HALLOW_ names are reserved".into());
        }
    }
    if let Some(by) = &spec.updated_by
        && (by.is_empty() || by.chars().count() > 64)
    {
        fields.insert("updated_by".into(), "must be 1 to 64 characters".into());
    }
    fields
}

fn info(kind: ServerKind) -> KindInfo {
    KindInfo {
        name: kind.name,
        spec: kind.spec.0,
        updated_ms: u64::try_from(kind.updated_at.timestamp_millis()).unwrap_or(0),
    }
}

fn db(state: &AppState) -> Reply<&PgPool> {
    state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "the kinds registry needs the database",
        )
    })
}

fn check_name(name: &str) -> Reply<()> {
    if is_kind_name(name) {
        Ok(())
    } else {
        Err(Failure::bad_request(
            "a kind name is up to 40 lowercase letters, digits and single dashes, starting with a letter",
        ))
    }
}

pub async fn list(State(state): Shared) -> Reply<axum::Json<Vec<KindInfo>>> {
    let kinds = hallow_db::list_kinds(db(&state)?).await?;
    Ok(axum::Json(kinds.into_iter().map(info).collect()))
}

pub async fn get(State(state): Shared, Path(name): Path<String>) -> Reply<axum::Json<KindInfo>> {
    check_name(&name)?;
    hallow_db::list_kinds(db(&state)?)
        .await?
        .into_iter()
        .find(|k| k.name == name)
        .map(|k| axum::Json(info(k)))
        .ok_or_else(|| Failure::not_found("no such kind"))
}

pub async fn put(
    State(state): Shared,
    Path(name): Path<String>,
    body: Result<axum::Json<KindSpec>, axum::extract::rejection::JsonRejection>,
) -> Reply<axum::Json<KindInfo>> {
    check_name(&name)?;
    let axum::Json(spec) = body?;
    let fields = check_spec(&spec);
    if !fields.is_empty() {
        return Err(Failure::invalid_fields("invalid kind", fields));
    }
    let stored = hallow_db::put_kind(db(&state)?, &name, &spec).await?;
    if let Some(instances) = &state.instances {
        instances.kinds_changed();
    }
    tracing::info!(kind = %name, image = %spec.image, by = ?spec.updated_by, "kind registered");
    Ok(axum::Json(info(stored)))
}

pub async fn delete(State(state): Shared, Path(name): Path<String>) -> Reply<StatusCode> {
    check_name(&name)?;
    if !hallow_db::delete_kind(db(&state)?, &name).await? {
        return Err(Failure::not_found("no such kind"));
    }
    if let Some(instances) = &state.instances {
        instances.kinds_changed();
    }
    tracing::info!(kind = %name, "kind removed");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn instances(State(state): Shared) -> Reply<axum::Json<Vec<InstanceInfo>>> {
    match &state.instances {
        Some(manager) => Ok(axum::Json(manager.list(&state.store).await?)),
        None => Ok(axum::Json(Vec::new())),
    }
}

pub async fn end_instance(State(state): Shared, Path(name): Path<String>) -> Reply<StatusCode> {
    let Some(manager) = &state.instances else {
        return Err(Failure::not_found("no such instance"));
    };
    match manager.end_now(&state, &name).await {
        Ok(true) => Ok(StatusCode::ACCEPTED),
        Ok(false) => Err(Failure::not_found("no such instance")),
        Err(err) => {
            tracing::error!(%err, "can't end the instance");
            Err(Failure::new(
                ErrorCode::Unavailable,
                "live state is unavailable; retry",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::instance::{KindPlayers, KindRole, KindScaling};

    #[test]
    fn kind_names_follow_hallow_toml() {
        for good in ["hub", "test-game", "a1-b2", &"a".repeat(40)] {
            assert!(is_kind_name(good), "{good}");
        }
        for bad in ["", "1game", "Game", "a--b", "a-", "a_b", &"a".repeat(41)] {
            assert!(!is_kind_name(bad), "{bad}");
        }
    }

    #[test]
    fn specs_are_checked_by_field() {
        let mut spec = KindSpec {
            image: "hallow-test-game:dev".into(),
            role: KindRole::Game,
            lobby: Some("hub".into()),
            players: KindPlayers { min: 2, max: 16 },
            scaling: KindScaling {
                warm: 1,
                max_instances: Some(20),
            },
            env: [("MODE".to_owned(), "solo".to_owned())].into(),
            memory_mb: Some(1024),
            updated_by: Some("alice".into()),
        };
        assert!(check_spec(&spec).is_empty());
        spec.image = "has space".into();
        spec.players.min = 17;
        spec.scaling.warm = 21;
        spec.env.insert("HALLOW_SERVER_NAME".into(), "x".into());
        spec.env.insert("1BAD".into(), "x".into());
        spec.memory_mb = Some(64);
        let fields = check_spec(&spec);
        let keys: Vec<&str> = fields.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "env.1BAD",
                "env.HALLOW_SERVER_NAME",
                "image",
                "memory_mb",
                "players.min",
                "scaling.warm"
            ]
        );
    }
}
