//! Fleet manager settings: kept in PostgreSQL, read and changed by the
//! panel, every change in an append-only history.

use chrono::{DateTime, Utc};
use hallow_db::PgPool;
use hallow_proto::fleet::{
    FieldErrors, FleetSettings, FleetSettingsChange, FleetSettingsRecord, UpdateFleetSettings,
};

use crate::{accounts::DbResult, error::Failure};

fn millis(at: DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

/// The settings in force: the saved ones, or the defaults.
pub async fn current(pool: &PgPool) -> DbResult<FleetSettingsRecord> {
    Ok(match hallow_db::fleet_settings(pool).await? {
        Some(stored) => FleetSettingsRecord {
            settings: stored.settings.0,
            updated_at: Some(millis(stored.updated_at)),
            updated_by: Some(stored.updated_by),
        },
        None => FleetSettingsRecord {
            settings: FleetSettings::default(),
            updated_at: None,
            updated_by: None,
        },
    })
}

/// Parses a `PUT` body. A value of the wrong type (a negative node count, a
/// string for a number) is reported against its field like any other
/// invalid setting, so the panel can show it in place.
pub fn parse_update(body: serde_json::Value) -> Result<UpdateFleetSettings, Failure> {
    let invalid = |fields| Failure::invalid_fields("invalid fleet settings", fields);
    let serde_json::Value::Object(mut body) = body else {
        return Err(Failure::bad_request("the body must be a JSON object"));
    };
    let mut fields = FieldErrors::new();
    let updated_by = match body.remove("updated_by") {
        Some(serde_json::Value::String(by)) => by,
        _ => {
            fields.insert("updated_by".into(), "is required".into());
            String::new()
        }
    };
    let settings = match body.remove("settings") {
        Some(serde_json::Value::Object(given)) => {
            let defaults = serde_json::to_value(FleetSettings::default()).unwrap_or_default();
            let mut merged = defaults.clone();
            // Try each field alone over the defaults to find the ones of the
            // wrong type; the rest are still validated.
            for (key, value) in given {
                let mut one = defaults.clone();
                one[&key] = value.clone();
                match serde_json::from_value::<FleetSettings>(one) {
                    Ok(_) => merged[&key] = value,
                    Err(err) => {
                        fields.insert(key, type_error(&err));
                    }
                }
            }
            serde_json::from_value::<FleetSettings>(merged).ok()
        }
        _ => {
            fields.insert("settings".into(), "is required".into());
            None
        }
    };
    let updated_by = updated_by.trim().to_owned();
    if !fields.contains_key("updated_by") && !(1..=64).contains(&updated_by.chars().count()) {
        fields.insert("updated_by".into(), "must be 1 to 64 characters".into());
    }
    if let Some(settings) = &settings
        && let Err(errors) = settings.validate()
    {
        for (field, message) in errors {
            fields.entry(field).or_insert(message);
        }
    }
    match settings {
        Some(settings) if fields.is_empty() => Ok(UpdateFleetSettings {
            settings,
            updated_by,
        }),
        _ => Err(invalid(fields)),
    }
}

/// serde's message without its line and column, which mean nothing here.
fn type_error(err: &serde_json::Error) -> String {
    let message = err.to_string();
    match message.rfind(" at line ") {
        Some(i) => message[..i].to_owned(),
        None => message,
    }
}

/// Saves validated settings and returns them as now in force.
pub async fn save(pool: &PgPool, update: &UpdateFleetSettings) -> DbResult<FleetSettingsRecord> {
    let stored = hallow_db::save_fleet_settings(pool, &update.settings, &update.updated_by).await?;
    tracing::info!(by = %stored.updated_by, "fleet settings changed");
    Ok(FleetSettingsRecord {
        settings: stored.settings.0,
        updated_at: Some(millis(stored.updated_at)),
        updated_by: Some(stored.updated_by),
    })
}

/// The latest changes, newest first.
pub async fn history(pool: &PgPool, limit: u32) -> DbResult<Vec<FleetSettingsChange>> {
    Ok(hallow_db::fleet_settings_history(pool, i64::from(limit))
        .await?
        .into_iter()
        .map(|change| FleetSettingsChange {
            id: change.id,
            before: change.before.map(|b| b.0),
            after: change.after.0,
            updated_at: millis(change.updated_at),
            updated_by: change.updated_by,
        })
        .collect())
}
