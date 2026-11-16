//! The panel's "remote" API, as Wings sees it, for nodes enrolled with the
//! control plane as their remote (docs/INSTANCES.md).
//!
//! Wings calls `<remote>/api/remote/...` with `Authorization: Bearer
//! <token_id>.<token>`. For the control plane's own instances it answers
//! itself, from the instance records in Redis:
//!
//! | Call | Answer |
//! |---|---|
//! | `GET /api/remote/servers/{uuid}` | the instance's configuration ([`wings::raw_server`]) |
//! | `GET /api/remote/servers?page=&per_page=` | the panel's page, plus this node's instances on the last page |
//! | `GET\|POST /api/remote/servers/{uuid}/install` | nothing to install / acknowledged |
//!
//! Everything else (the panel's own servers, SFTP logins, activity,
//! backups, `servers/reset`) goes to the panel unchanged, with Wings'
//! credentials, so the panel keeps working for nodes as before.

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{AppState, orchestrator::wings};

type Shared = State<Arc<AppState>>;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/remote/servers", get(list_servers))
        .route("/api/remote/servers/{uuid}", get(server))
        .route("/api/remote/servers/{uuid}/install", any(install))
        .route("/api/remote/{*rest}", any(pass_through))
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "errors": [message] }))).into_response()
}

/// An early answer.
type Early = Box<Response>;

fn early(status: StatusCode, message: &str) -> Early {
    Box::new(error(status, message))
}

/// Sends the request on to the panel as it came; `None` body for GETs.
async fn forward(
    state: &AppState,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Option<String>, Bytes), Early> {
    let Some(panel) = &state.config.panel else {
        return Err(early(
            StatusCode::SERVICE_UNAVAILABLE,
            "no panel configured",
        ));
    };
    let path = uri.path_and_query().map_or(uri.path(), |p| p.as_str());
    let url = format!("{}{path}", panel.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut request = client
        .request(method, url)
        .timeout(std::time::Duration::from_secs(30))
        .body(body);
    for name in [header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value);
        }
    }
    let response = request
        .send()
        .await
        .map_err(|e| early(StatusCode::BAD_GATEWAY, &format!("panel: {e}")))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .bytes()
        .await
        .map_err(|e| early(StatusCode::BAD_GATEWAY, &format!("panel: {e}")))?;
    Ok((status, content_type, bytes))
}

fn respond(status: StatusCode, content_type: Option<String>, body: Bytes) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    if let Some(content_type) = content_type
        && let Ok(value) = content_type.parse()
    {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response
}

async fn pass_through(
    State(state): Shared,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match forward(&state, method, &uri, &headers, body).await {
        Ok((status, content_type, body)) => respond(status, content_type, body),
        Err(response) => *response,
    }
}

fn bearer(headers: &HeaderMap) -> &str {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
}

/// The configuration of one of our instances on the calling node:
/// `Ok(None)` when the UUID isn't ours.
async fn our_server(
    state: &AppState,
    headers: &HeaderMap,
    uuid: Uuid,
) -> Result<Option<Value>, Early> {
    let Some(manager) = &state.instances else {
        return Ok(None);
    };
    let Some(wings_orchestrator) = manager.wings() else {
        return Ok(None);
    };
    let instance = state
        .store
        .instance_by_uuid(uuid)
        .await
        .map_err(|_| early(StatusCode::SERVICE_UNAVAILABLE, "live state unavailable"))?;
    let Some(instance) = instance else {
        return Ok(None);
    };
    let Some(node) = wings_orchestrator
        .node_by_credentials(bearer(headers))
        .await
    else {
        return Err(early(StatusCode::UNAUTHORIZED, "unknown node credentials"));
    };
    if node.name != instance.node {
        return Err(early(StatusCode::NOT_FOUND, "not this node's server"));
    }
    let spec = state
        .store
        .instance_spec(&instance.name)
        .await
        .map_err(|_| early(StatusCode::SERVICE_UNAVAILABLE, "live state unavailable"))?
        .ok_or_else(|| early(StatusCode::NOT_FOUND, "instance has no spec"))?;
    Ok(Some(wings::raw_server(
        &spec,
        node.private_ip,
        wings_orchestrator.owner(),
    )))
}

async fn server(
    State(state): Shared,
    Path(uuid): Path<Uuid>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    match our_server(&state, &headers, uuid).await {
        Ok(Some(raw)) => axum::Json(raw).into_response(),
        Ok(None) => pass_through(State(state), method, uri, headers, Bytes::new()).await,
        Err(response) => *response,
    }
}

async fn install(
    State(state): Shared,
    Path(uuid): Path<Uuid>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match our_server(&state, &headers, uuid).await {
        // Created with skip_scripts; an empty script counts as installed.
        Ok(Some(raw)) if method == Method::GET => axum::Json(json!({
            "container_image": raw["settings"]["container"]["image"],
            "entrypoint": "sh",
            "script": "",
            "environment": {},
        }))
        .into_response(),
        Ok(Some(_)) => axum::Json(json!({})).into_response(),
        Ok(None) => pass_through(State(state), method, uri, headers, body).await,
        Err(response) => *response,
    }
}

async fn list_servers(State(state): Shared, uri: Uri, headers: HeaderMap) -> Response {
    let (status, content_type, body) =
        match forward(&state, Method::GET, &uri, &headers, Bytes::new()).await {
            Ok(answer) => answer,
            Err(response) => return *response,
        };
    if !status.is_success() {
        return respond(status, content_type, body);
    }
    let Ok(mut page) = serde_json::from_slice::<Value>(&body) else {
        return respond(status, content_type, body);
    };
    let ours = match ours_on_node(&state, &headers).await {
        Ok(ours) => ours,
        Err(response) => return *response,
    };
    merge_page(&mut page, ours);
    axum::Json(page).into_response()
}

/// Our instances on the calling node, as Wings configurations.
async fn ours_on_node(state: &AppState, headers: &HeaderMap) -> Result<Vec<Value>, Early> {
    let Some(manager) = &state.instances else {
        return Ok(Vec::new());
    };
    let Some(wings_orchestrator) = manager.wings() else {
        return Ok(Vec::new());
    };
    let Some(node) = wings_orchestrator
        .node_by_credentials(bearer(headers))
        .await
    else {
        // The panel accepted it, so it's a node we don't know (yet).
        return Ok(Vec::new());
    };
    let unavailable = |_| early(StatusCode::SERVICE_UNAVAILABLE, "live state unavailable");
    let mut ours = Vec::new();
    for instance in state.store.instances().await.map_err(unavailable)? {
        if instance.node != node.name {
            continue;
        }
        if let Some(spec) = state
            .store
            .instance_spec(&instance.name)
            .await
            .map_err(unavailable)?
        {
            ours.push(wings::raw_server(
                &spec,
                node.private_ip,
                wings_orchestrator.owner(),
            ));
        }
    }
    Ok(ours)
}

/// Appends `ours` to the panel's page if it is the last one.
pub fn merge_page(page: &mut Value, ours: Vec<Value>) {
    let current = page["meta"]["current_page"].as_u64().unwrap_or(1);
    let last = page["meta"]["last_page"].as_u64().unwrap_or(1);
    if current < last || ours.is_empty() {
        return;
    }
    let added = ours.len() as u64;
    if let Some(data) = page["data"].as_array_mut() {
        data.extend(ours);
        if let Some(total) = page["meta"]["total"].as_u64() {
            page["meta"]["total"] = json!(total + added);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_go_on_the_last_page() {
        let mut first = json!({
            "data": [{"settings": {"uuid": "a"}}],
            "meta": {"current_page": 1, "last_page": 2, "total": 2},
        });
        merge_page(&mut first, vec![json!({"settings": {"uuid": "ours"}})]);
        assert_eq!(first["data"].as_array().unwrap().len(), 1);
        let mut last = json!({
            "data": [{"settings": {"uuid": "b"}}],
            "meta": {"current_page": 2, "last_page": 2, "total": 2},
        });
        merge_page(&mut last, vec![json!({"settings": {"uuid": "ours"}})]);
        assert_eq!(last["data"].as_array().unwrap().len(), 2);
        assert_eq!(last["meta"]["total"], json!(3));
        // A panel with no servers of its own still lists ours.
        let mut empty = json!({
            "data": [],
            "meta": {"current_page": 1, "last_page": 1, "total": 0},
        });
        merge_page(&mut empty, vec![json!({"settings": {"uuid": "ours"}})]);
        assert_eq!(empty["meta"]["total"], json!(1));
    }
}
