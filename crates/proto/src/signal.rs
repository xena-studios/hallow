//! Anticheat signals: per-player measurements from proxies (packet timing)
//! and servers (movement, combat), collected before any check acts on them
//! (docs/ANTICHEAT.md, phase 1). Each sample covers one window of play; the
//! control plane keeps them in PostgreSQL for a while for tuning and review.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How long a sample's window should be. Shorter windows cost more rows.
pub const WINDOW_SECONDS: u64 = 30;

/// `POST /v1/signals`: one batch from a proxy or server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalBatch {
    /// The proxy id or server name that measured them.
    pub source: String,
    pub samples: Vec<PlayerSignals>,
}

/// One player's measurements over one window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerSignals {
    pub player: Uuid,
    /// When the window ended, Unix milliseconds.
    pub at_ms: u64,
    pub window_ms: u32,
    /// The client's real protocol version; checks differ between versions.
    pub protocol: Option<i32>,
    /// Named measurements, such as `attacks`, `cps_max`, `rotation_snaps` or
    /// `packets_per_second_max`. Names are free-form so new signals need no
    /// schema change; each source documents its own.
    pub metrics: BTreeMap<String, f64>,
}

/// One stored sample in `GET /v1/signals?name=`, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalRecord {
    pub source: String,
    pub at_ms: u64,
    pub window_ms: u32,
    pub protocol: Option<i32>,
    pub metrics: BTreeMap<String, f64>,
}
