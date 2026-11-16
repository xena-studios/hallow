//! Who is calling: the network's shared token, or a server's scoped token
//! (docs/INSTANCES.md#credentials).
//!
//! Every `/v1` route takes the shared bearer token (`HALLOW_TOKEN`), which
//! can do anything. A server can instead carry a **scoped token**, which can
//! only do what the server plugin does, for its own server name:
//!
//! | Route | Allowed when |
//! |---|---|
//! | `POST /v1/servers` | the body's `name` and `kind` are the token's |
//! | `POST /v1/servers/{name}/heartbeat` | `{name}` is the token's |
//! | `DELETE /v1/servers/{name}` | `{name}` is the token's |
//! | `POST`/`GET /v1/servers/{name}/players` | `{name}` is the token's |
//! | `POST /v1/stats` | the batch's `source` and `kind` are the token's |
//! | `POST /v1/signals` | the batch's `source` is the token's |
//!
//! Anything else answers `403`. The path checks happen here; the body checks
//! in the handlers, through [`Caller`].
//!
//! Scoped tokens come from two places:
//! - **Instances.** The control plane mints one per instance it creates
//!   ([`new_instance_token`]) and passes it as `HALLOW_CONTROL_PLANE_TOKEN`.
//!   Redis keeps its SHA-256 with the instance's name and kind (see
//!   `store::instances`); it works while the instance record exists and is
//!   gone the moment the instance is deleted.
//! - **Static servers** run by something else (the compose hub, say), from
//!   `HALLOW_SERVER_TOKENS` ([`StaticServerToken`]). They may also keep the
//!   shared token.

use std::sync::Arc;

use axum::{
    extract::{MatchedPath, Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};
use hallow_proto::{ErrorCode, routes};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{AppState, error::Failure};

/// Every instance token starts with this, so a leaked one is recognisable.
pub const INSTANCE_TOKEN_PREFIX: &str = "hit_";

/// The caller of a `/v1` request, put in the request's extensions by
/// [`authorize`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    /// The shared token: anything goes.
    Network,
    /// A scoped token: only its own server's routes.
    Server { name: String, kind: String },
}

impl Caller {
    /// Refuses a server acting for another server name (or kind).
    pub fn check_server(&self, name: &str, kind: Option<&str>) -> Result<(), Failure> {
        match self {
            Self::Network => Ok(()),
            Self::Server {
                name: own,
                kind: own_kind,
            } => {
                if own != name {
                    return Err(forbidden(&format!(
                        "this token belongs to server {own}, not {name}"
                    )));
                }
                if let Some(kind) = kind
                    && kind != own_kind
                {
                    return Err(forbidden(&format!(
                        "server {own} is of kind {own_kind}, not {kind}"
                    )));
                }
                Ok(())
            }
        }
    }
}

/// What Redis keeps for an instance token, under its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedToken {
    pub name: String,
    pub kind: String,
    pub created_ms: u64,
}

/// A scoped token from the configuration, for a server the control plane
/// doesn't start: `HALLOW_SERVER_TOKENS=name:kind:token,...`.
#[derive(Clone, PartialEq, Eq)]
pub struct StaticServerToken {
    pub name: String,
    pub kind: String,
    pub token: String,
}

impl std::fmt::Debug for StaticServerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticServerToken")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Parses `HALLOW_SERVER_TOKENS`: comma-separated `name:kind:token`, tokens
/// at least 16 characters.
pub fn parse_static_tokens(list: &str) -> Result<Vec<StaticServerToken>, String> {
    let mut tokens = Vec::new();
    for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let mut parts = entry.splitn(3, ':');
        let (Some(name), Some(kind), Some(token)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err("HALLOW_SERVER_TOKENS entries are name:kind:token".into());
        };
        if name.is_empty() || kind.is_empty() {
            return Err("HALLOW_SERVER_TOKENS entries are name:kind:token".into());
        }
        if token.len() < 16 {
            return Err(format!(
                "HALLOW_SERVER_TOKENS: the token for {name} must be at least 16 characters"
            ));
        }
        tokens.push(StaticServerToken {
            name: name.to_owned(),
            kind: kind.to_owned(),
            token: token.to_owned(),
        });
    }
    Ok(tokens)
}

/// A new instance token: the prefix and 32 random bytes in hex.
pub fn new_instance_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)?;
    let mut token = String::with_capacity(INSTANCE_TOKEN_PREFIX.len() + 64);
    token.push_str(INSTANCE_TOKEN_PREFIX);
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    Ok(token)
}

/// The SHA-256 of a token, in hex: what Redis stores.
#[must_use]
pub fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn forbidden(message: &str) -> Failure {
    Failure::forbidden(message)
}

fn unauthorized() -> Response {
    Failure::new(ErrorCode::Unauthorized, "missing or wrong bearer token").into_response()
}

/// Whether a server's scoped token may call `method` on the route `route`
/// (as the router matched it, `{name}` and all) for the server `{name}` in
/// the path. Body checks come later, in the handlers.
#[must_use]
pub fn server_may(method: &Method, route: &str, path_name: Option<&str>, own: &str) -> bool {
    let own_path = path_name == Some(own);
    if route == routes::SERVERS {
        return method == Method::POST;
    }
    if route == routes::server_heartbeat("{name}") {
        return method == Method::POST && own_path;
    }
    if route == routes::server("{name}") {
        return method == Method::DELETE && own_path;
    }
    if route == routes::server_players("{name}") {
        return (method == Method::POST || method == Method::GET) && own_path;
    }
    if route == routes::STATS || route == routes::SIGNALS {
        return method == Method::POST;
    }
    false
}

/// The `{name}` segment of `/v1/servers/{name}/...`.
fn server_name_in(path: &str) -> Option<&str> {
    path.strip_prefix(routes::SERVERS)?
        .strip_prefix('/')?
        .split('/')
        .next()
}

/// The scoped token `given` stands for, if any.
async fn scoped(state: &AppState, given: &str) -> Result<Option<(String, String)>, Failure> {
    for token in &state.config.server_tokens {
        if constant_time_eq(given.as_bytes(), token.token.as_bytes()) {
            return Ok(Some((token.name.clone(), token.kind.clone())));
        }
    }
    if !given.starts_with(INSTANCE_TOKEN_PREFIX) {
        return Ok(None);
    }
    Ok(state
        .store
        .instance_token(&token_hash(given))
        .await?
        .map(|t| (t.name, t.kind)))
}

/// Checks `Authorization: Bearer <token>`: the shared token in constant time,
/// then scoped tokens against the route. Puts the [`Caller`] in the request.
pub async fn authorize(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let given = crate::api::bearer(request.headers()).to_owned();
    if constant_time_eq(given.as_bytes(), state.config.token.as_bytes()) {
        request.extensions_mut().insert(Caller::Network);
        return next.run(request).await;
    }
    let (name, kind) = match scoped(&state, &given).await {
        Ok(Some(found)) => found,
        Ok(None) => return unauthorized(),
        Err(failure) => return failure.into_response(),
    };
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
        .unwrap_or_default();
    let path_name = server_name_in(request.uri().path());
    if !server_may(request.method(), &route, path_name, &name) {
        return forbidden(&format!(
            "server {name}'s token can't {} {}",
            request.method(),
            request.uri().path()
        ))
        .into_response();
    }
    request
        .extensions_mut()
        .insert(Caller::Server { name, kind });
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_only_reach_their_own_routes() {
        let own = "test-game-1a2b3c";
        let may =
            |method: Method, route: &str, path: Option<&str>| server_may(&method, route, path, own);
        assert!(may(Method::POST, routes::SERVERS, None));
        assert!(!may(Method::GET, routes::SERVERS, None));
        let beat = routes::server_heartbeat("{name}");
        assert!(may(Method::POST, &beat, Some(own)));
        assert!(!may(Method::POST, &beat, Some("lobby")));
        let server = routes::server("{name}");
        assert!(may(Method::DELETE, &server, Some(own)));
        assert!(!may(Method::DELETE, &server, Some("lobby")));
        let players = routes::server_players("{name}");
        assert!(may(Method::POST, &players, Some(own)));
        assert!(may(Method::GET, &players, Some(own)));
        assert!(!may(Method::GET, &players, Some("lobby")));
        assert!(may(Method::POST, routes::STATS, None));
        assert!(may(Method::POST, routes::SIGNALS, None));
        assert!(!may(Method::GET, routes::SIGNALS, None));
        for admin in [
            routes::KINDS,
            routes::INSTANCES,
            routes::PROXIES,
            routes::COMMANDS,
            routes::PRESENCE_LOGIN,
            routes::NODE_JOIN_TOKENS,
            routes::PUNISHMENTS,
            routes::RANK_GRANTS,
            routes::QUEUES,
        ] {
            assert!(!may(Method::POST, admin, None), "{admin}");
            assert!(!may(Method::GET, admin, None), "{admin}");
        }
    }

    #[test]
    fn server_names_come_from_the_path() {
        assert_eq!(server_name_in("/v1/servers/hub-1/heartbeat"), Some("hub-1"));
        assert_eq!(server_name_in("/v1/servers/hub-1"), Some("hub-1"));
        assert_eq!(server_name_in("/v1/servers"), None);
        assert_eq!(server_name_in("/v1/serversx/a"), None);
    }

    #[test]
    fn tokens_are_random_and_hashed() {
        let a = new_instance_token().unwrap();
        let b = new_instance_token().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(INSTANCE_TOKEN_PREFIX));
        assert_eq!(a.len(), INSTANCE_TOKEN_PREFIX.len() + 64);
        assert_eq!(token_hash(&a).len(), 64);
        assert_ne!(token_hash(&a), token_hash(&b));
        assert_eq!(
            token_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn static_tokens_parse() {
        let tokens =
            parse_static_tokens("lobby:hub:0123456789abcdef, x:y:z:0123456789abcdef").unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].name, "lobby");
        assert_eq!(tokens[0].kind, "hub");
        assert_eq!(tokens[1].token, "z:0123456789abcdef");
        assert!(parse_static_tokens("lobby:hub:short").is_err());
        assert!(parse_static_tokens("lobby:0123456789abcdef").is_err());
        assert!(parse_static_tokens("").unwrap().is_empty());
        assert!(!format!("{:?}", tokens[0]).contains("0123"));
    }
}
