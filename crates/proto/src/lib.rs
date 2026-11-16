//! Types for the Hallow control plane's HTTP/JSON API ([Decisions] 12).
//!
//! The control plane serves these; Vine plugins, the Hallow server plugin and
//! the panel call them. Plugins run in WASM and reach the API through
//! `wasi:http`, so this crate only depends on `serde`, `uuid` and `chrono-tz`
//! (to check fleet settings' time zones) and builds for `wasm32-wasip2` as
//! well as the host.
//!
//! Every request carries `Authorization: Bearer <token>` (a node's join
//! carries its one-time join token instead). Errors come back as
//! an [`ApiError`] body with a 4xx or 5xx status. Times are Unix milliseconds.
//!
//! [Decisions]: https://github.com/xena-studios/hallow/blob/main/docs/DECISIONS.md

pub mod command;
pub mod fleet;
pub mod friend;
pub mod instance;
pub mod node;
pub mod party;
pub mod permission;
pub mod presence;
pub mod proxy;
pub mod punishment;
pub mod queue;
pub mod routes;
pub mod server;
pub mod signal;
pub mod stats;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Version of this API, also the first path segment (`/v1/...`).
pub const API_VERSION: u32 = 1;

/// The error body for every 4xx and 5xx response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    /// For logs and staff, not for players.
    pub message: String,
    /// Field name to what is wrong with it, for a `bad_request` about a form
    /// such as the fleet settings. Left out when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Missing or wrong bearer token.
    Unauthorized,
    /// The body or a parameter doesn't parse or is out of range.
    BadRequest,
    /// The proxy, server or player isn't registered (or expired).
    NotFound,
    /// The request conflicts with current state, such as a duplicate name.
    Conflict,
    /// The control plane can't serve this right now; retry with backoff.
    Unavailable,
}

/// A long-poll response: either something changed since the caller's
/// revision, or nothing did before the timeout.
///
/// Callers send the last `revision` they saw as `?since=`, and `0` the first
/// time. On [`Poll::Unchanged`] they poll again with the same revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Poll<T> {
    Changed { revision: u64, data: T },
    Unchanged { revision: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn poll_is_tagged_by_status() {
        let changed: Poll<Vec<u8>> = Poll::Changed {
            revision: 7,
            data: vec![1],
        };
        assert_eq!(
            serde_json::to_value(&changed).unwrap(),
            json!({"status": "changed", "revision": 7, "data": [1]})
        );
        let unchanged: Poll<Vec<u8>> =
            serde_json::from_value(json!({"status": "unchanged", "revision": 7})).unwrap();
        assert_eq!(unchanged, Poll::Unchanged { revision: 7 });
    }

    #[test]
    fn errors_use_snake_case_codes() {
        let err = ApiError {
            code: ErrorCode::NotFound,
            message: "no such server".into(),
            fields: BTreeMap::new(),
        };
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({"code": "not_found", "message": "no such server"})
        );
        let old: ApiError =
            serde_json::from_value(json!({"code": "bad_request", "message": "no"})).unwrap();
        assert!(old.fields.is_empty());
        let form = ApiError {
            code: ErrorCode::BadRequest,
            message: "invalid settings".into(),
            fields: [(
                "max_nodes".to_owned(),
                "must be at least floor_nodes".to_owned(),
            )]
            .into(),
        };
        assert_eq!(
            serde_json::to_value(&form).unwrap(),
            json!({
                "code": "bad_request",
                "message": "invalid settings",
                "fields": {"max_nodes": "must be at least floor_nodes"}
            })
        );
    }
}
