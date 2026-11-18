//! The control plane's public API, called server-side, with a short cache.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use bytes::Bytes;
use hallow_proto::public::{self, BoardKind, NetworkSummary, PlayerMatch, PlayerProfile};
use hallow_proto::stats::{Leaderboard, Window};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

/// How long an answer is reused.
const TTL: Duration = Duration::from_secs(15);
/// Entries kept before expired ones are swept.
const SWEEP_AT: usize = 2048;

/// Why the API gave no answer.
#[derive(Debug)]
pub enum ApiError {
    NotFound,
    /// `429`, with `Retry-After` seconds if sent.
    RateLimited(Option<u64>),
    /// Down, slow, or an answer that doesn't parse.
    Unavailable(String),
}

pub struct Api {
    http: reqwest::Client,
    base: String,
    cache: Mutex<HashMap<String, (Instant, Option<Bytes>)>>,
}

impl Api {
    /// # Panics
    /// If the HTTP client can't be built (no TLS roots).
    #[must_use]
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .user_agent(concat!("hallow-web/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client");
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn network(&self, from: &str) -> Result<NetworkSummary, ApiError> {
        self.get(public::NETWORK, from).await
    }

    pub async fn search(&self, query: &str, from: &str) -> Result<Vec<PlayerMatch>, ApiError> {
        let mut path = format!("{}?q=", public::SEARCH);
        path.push_str(&crate::fmt::segment(query));
        self.get(&path, from).await
    }

    pub async fn profile(&self, player: &str, from: &str) -> Result<PlayerProfile, ApiError> {
        self.get(&public::profile(player), from).await
    }

    pub async fn boards(&self, from: &str) -> Result<Vec<BoardKind>, ApiError> {
        self.get(public::LEADERBOARDS, from).await
    }

    pub async fn leaderboard(
        &self,
        kind: &str,
        stat: &str,
        window: Window,
        limit: u32,
        from: &str,
    ) -> Result<Leaderboard, ApiError> {
        self.get(&public::leaderboard_query(kind, stat, window, limit), from)
            .await
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, from: &str) -> Result<T, ApiError> {
        let body = match self.cached(path) {
            Some(body) => body,
            None => {
                let body = self.fetch(path, from).await?;
                self.store(path, body.clone());
                body
            }
        };
        let body = body.ok_or(ApiError::NotFound)?;
        serde_json::from_slice(&body).map_err(|e| ApiError::Unavailable(format!("{path}: {e}")))
    }

    /// The body, or `None` for a 404 (cached like an answer).
    async fn fetch(&self, path: &str, from: &str) -> Result<Option<Bytes>, ApiError> {
        let url = format!("{}{path}", self.base);
        let response = self
            .http
            .get(&url)
            .header("x-forwarded-for", from)
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("{path}: {e}")))?;
        match response.status() {
            StatusCode::OK => response
                .bytes()
                .await
                .map(Some)
                .map_err(|e| ApiError::Unavailable(format!("{path}: {e}"))),
            StatusCode::NOT_FOUND => Ok(None),
            StatusCode::TOO_MANY_REQUESTS => Err(ApiError::RateLimited(
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse().ok()),
            )),
            status => {
                let text = response.text().await.unwrap_or_default();
                Err(ApiError::Unavailable(format!("{path}: {status} {text}")))
            }
        }
    }

    fn cached(&self, path: &str) -> Option<Option<Bytes>> {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .get(path)
            .filter(|(at, _)| at.elapsed() < TTL)
            .map(|(_, body)| body.clone())
    }

    fn store(&self, path: &str, body: Option<Bytes>) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.len() >= SWEEP_AT {
            cache.retain(|_, (at, _)| at.elapsed() < TTL);
            if cache.len() >= SWEEP_AT {
                cache.clear();
            }
        }
        cache.insert(path.to_owned(), (Instant::now(), body));
    }
}

/// The `X-Forwarded-For` to send on: any the browser's request carried,
/// then the address it came from.
#[must_use]
pub fn forwarded_for(headers: &HeaderMap, peer: IpAddr) -> String {
    let incoming: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect();
    let peer = peer.to_string();
    if incoming.is_empty() {
        peer
    } else {
        format!("{}, {peer}", incoming.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_the_chain() {
        let peer: IpAddr = "10.0.0.2".parse().unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(forwarded_for(&headers, peer), "10.0.0.2");
        headers.append("x-forwarded-for", "203.0.113.7".parse().unwrap());
        headers.append("x-forwarded-for", " 198.51.100.1 , ".parse().unwrap());
        assert_eq!(
            forwarded_for(&headers, peer),
            "203.0.113.7, 198.51.100.1, 10.0.0.2"
        );
    }

    #[test]
    fn cache_keeps_answers_and_misses() {
        let api = Api::new("http://127.0.0.1:1/");
        assert_eq!(api.base, "http://127.0.0.1:1");
        assert!(api.cached("/a").is_none());
        api.store("/a", Some(Bytes::from_static(b"{}")));
        api.store("/b", None);
        assert_eq!(api.cached("/a"), Some(Some(Bytes::from_static(b"{}"))));
        assert_eq!(api.cached("/b"), Some(None));
    }
}
