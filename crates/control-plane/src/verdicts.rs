//! The anticheat verdict service (docs/ANTICHEAT.md): proxies and servers
//! report violations with evidence; this keeps them, scores them per player
//! and check with decay, and records a verdict when a score reaches the level
//! of an action. In observe-only mode (phase 2) verdicts are recorded and
//! never carried out: nothing here kicks, bans or moves a player.
//!
//! A score halves every [`Policy::half_life_secs`]. Each violation adds its
//! severity (the measured value over the check's limit), so one bad window
//! scores more than one borderline one. The action for a level:
//!
//! | Level | Action |
//! |---|---|
//! | 1 | `review` |
//! | 3 | `setback`, for movement checks (`speed`, `fly`, `timer`); `review` for the rest |
//! | 6 | `kick` |
//! | 12 | `ban` |
//!
//! A verdict is recorded each time a score's action steps up. When a score
//! decays below its action, the action steps back down quietly, so the next
//! rise is recorded again.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use hallow_db::PgPool;
use hallow_proto::violation::{
    CheckScore, PlayerViolations, VerdictAction, VerdictMode, VerdictRecord, ViolationBatch,
    ViolationRecord,
};

use crate::{
    AppState,
    accounts::{DbResult, resolve},
    store::Store,
};

/// The most violations one batch may carry.
pub const MAX_BATCH: usize = 5000;
/// How many violations and verdicts a lookup returns.
const HISTORY: i64 = 200;

/// How violations become verdicts.
#[derive(Clone, Debug)]
pub struct Policy {
    pub mode: VerdictMode,
    pub half_life_secs: f64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: VerdictMode::Observe,
            half_life_secs: 15.0 * 60.0,
        }
    }
}

/// Checks where moving the player back is the first step after review.
const MOVEMENT_CHECKS: [&str; 3] = ["speed", "fly", "timer"];

/// The action a score of `level` on `check` calls for.
#[must_use]
pub fn action_for(check: &str, level: f64) -> VerdictAction {
    if level >= 12.0 {
        VerdictAction::Ban
    } else if level >= 6.0 {
        VerdictAction::Kick
    } else if level >= 3.0 && MOVEMENT_CHECKS.contains(&check) {
        VerdictAction::Setback
    } else if level >= 1.0 {
        VerdictAction::Review
    } else {
        VerdictAction::None
    }
}

/// A score's level decayed from `updated` to `now`.
#[must_use]
pub fn decayed(level: f64, updated: DateTime<Utc>, now: DateTime<Utc>, half_life_secs: f64) -> f64 {
    let age = (now - updated).as_seconds_f64().max(0.0);
    level * 0.5_f64.powf(age / half_life_secs.max(1.0))
}

/// Whether a check name is 1 to 32 lowercase letters, digits or `_`.
#[must_use]
pub fn valid_check(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn time(ms: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(i64::try_from(ms).unwrap_or(0)).unwrap_or_else(Utc::now)
}

fn millis(at: DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

/// Cuts text to `max` characters.
fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Stores a batch, scores it and records any verdicts. Returns how many
/// violations were kept (those for players who never joined aren't).
pub async fn record(pool: &PgPool, policy: &Policy, batch: &ViolationBatch) -> DbResult<u64> {
    let mut kept = 0;
    for v in &batch.violations {
        let evidence = serde_json::to_value(&v.evidence).unwrap_or_default();
        let severity = if v.severity.is_finite() {
            v.severity.clamp(0.0, 100.0)
        } else {
            0.0
        };
        let summary = clip(&v.summary, 512);
        let at = time(v.at_ms);
        let stored = hallow_db::add_violation(
            pool,
            &hallow_db::NewViolation {
                account: v.player,
                source: &batch.source,
                check: &v.check,
                at,
                severity,
                protocol: v.protocol,
                summary: &summary,
                evidence: &evidence,
            },
        )
        .await?;
        if !stored {
            continue;
        }
        kept += 1;
        let Some((level, recorded)) = hallow_db::bump_score(
            pool,
            v.player,
            &v.check,
            severity,
            at,
            policy.half_life_secs,
        )
        .await?
        else {
            continue;
        };
        let action = action_for(&v.check, level);
        let recorded = VerdictAction::parse(&recorded).unwrap_or_default();
        if action == recorded {
            continue;
        }
        if action > recorded {
            let enforced = policy.mode == VerdictMode::Enforce;
            let reason = clip(&format!("{}: {}", batch.source, summary), 512);
            hallow_db::add_verdict(
                pool,
                v.player,
                &v.check,
                action.as_str(),
                level,
                enforced,
                &reason,
            )
            .await?;
            tracing::info!(
                player = %v.player,
                check = %v.check,
                action = action.as_str(),
                level,
                enforced,
                "anticheat verdict"
            );
        }
        hallow_db::set_score_action(pool, v.player, &v.check, action.as_str()).await?;
    }
    Ok(kept)
}

/// A player's scores, verdicts and recent violations by username; `None` if
/// nobody by that name has joined.
pub async fn standing(
    pool: &PgPool,
    store: &Store,
    policy: &Policy,
    name: &str,
) -> DbResult<Option<PlayerViolations>> {
    let Some((account, _)) = resolve(pool, store, name).await? else {
        return Ok(None);
    };
    let now = Utc::now();
    let scores = hallow_db::scores_of(pool, account)
        .await?
        .into_iter()
        .map(|s| {
            let level = decayed(s.level, s.updated_at, now, policy.half_life_secs);
            CheckScore {
                action: action_for(&s.check_name, level),
                check: s.check_name,
                level: (level * 1000.0).round() / 1000.0,
                updated_ms: millis(s.updated_at),
            }
        })
        .collect();
    let verdicts = hallow_db::recent_verdicts(pool, account, HISTORY)
        .await?
        .into_iter()
        .map(|v| VerdictRecord {
            action: VerdictAction::parse(&v.action).unwrap_or_default(),
            check: v.check_name,
            level: v.level,
            at_ms: millis(v.at),
            enforced: v.enforced,
            reason: v.reason,
        })
        .collect();
    let violations = hallow_db::recent_violations(pool, account, HISTORY)
        .await?
        .into_iter()
        .map(|v| ViolationRecord {
            source: v.source,
            check: v.check_name,
            at_ms: millis(v.at),
            severity: v.severity,
            protocol: v.protocol,
            summary: v.summary,
            evidence: serde_json::from_value(v.evidence.0).unwrap_or_default(),
        })
        .collect();
    Ok(Some(PlayerViolations {
        player: account,
        mode: policy.mode,
        scores,
        verdicts,
        violations,
    }))
}

/// Deletes violations past the signal retention period, hourly. Scores and
/// verdicts stay.
pub async fn retention(state: Arc<AppState>) {
    let Some(pool) = state.db.clone() else { return };
    let days = i64::from(state.config.signal_retention_days);
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    loop {
        tick.tick().await;
        let before = Utc::now() - TimeDelta::days(days);
        match hallow_db::delete_violations_before(&pool, before).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(deleted = n, "old anticheat violations deleted"),
            Err(err) => tracing::warn!(%err, "violation retention failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_to_actions() {
        assert_eq!(action_for("reach", 0.9), VerdictAction::None);
        assert_eq!(action_for("reach", 1.0), VerdictAction::Review);
        assert_eq!(action_for("reach", 4.0), VerdictAction::Review);
        assert_eq!(action_for("fly", 4.0), VerdictAction::Setback);
        assert_eq!(action_for("click_rate", 6.0), VerdictAction::Kick);
        assert_eq!(action_for("timer", 12.5), VerdictAction::Ban);
    }

    #[test]
    fn scores_halve_every_half_life() {
        let now = Utc::now();
        let level = decayed(8.0, now - TimeDelta::minutes(30), now, 900.0);
        assert!((level - 2.0).abs() < 1e-9, "{level}");
        // A score from the future doesn't grow.
        assert_eq!(decayed(8.0, now + TimeDelta::minutes(5), now, 900.0), 8.0);
    }

    #[test]
    fn check_names() {
        assert!(valid_check("click_rate"));
        assert!(!valid_check(""));
        assert!(!valid_check("Speed"));
        assert!(!valid_check("a-b"));
        assert!(!valid_check(&"x".repeat(33)));
    }
}
