//! Instances as Wings servers on the nodes that joined (docs/INSTANCES.md).
//!
//! Wings has no way to create a server from a config in the request: on
//! `POST /api/servers {uuid}` it fetches the server's configuration from its
//! `remote` (normally the panel) at `GET /api/remote/servers/{uuid}`, and it
//! asks again on every start and lists every server from there at boot. So
//! the control plane *is* each node's remote: nodes are enrolled with the
//! control plane's URL as their remote (`HALLOW_NODE_WINGS_REMOTE`), the
//! control plane answers those calls for its instances from Redis
//! ([`crate::remote`]) and passes every other call through to the panel.
//! The panel never holds a record for an instance.
//!
//! The control plane calls each node's Wings API with the node's token,
//! which it reads from the panel (`GET /api/admin/nodes/{node}/token`,
//! admin permission `nodes.read-token`) and caches.
//!
//! Run end to end against Wings 1.2.10 by `tools/wings-e2e` (docs/INSTANCES.md).

use std::{
    collections::{BTreeMap, HashMap},
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use hallow_db::{Node, PgPool};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    Created, Handle, InstanceSpec, Listed, NodeSlot, Orchestrator, OrchestratorError,
    OrchestratorFuture, Runtime,
};
use crate::panel::{NodeToken, Panel};

/// The egg every instance claims. Wings only uses it as an id.
pub const EGG: Uuid = Uuid::from_u128(0x6861_6c6c_6f77_0000_0000_0000_0000_0001);
/// Where Wings mounts the server's volume; the base image runs from here
/// when `HALLOW_SERVER_DIR` says so, because Wings makes the rest of the
/// container's filesystem read-only.
pub const SERVER_DIR: &str = "/home/container";
/// Disk quota per instance, MiB.
const DISK_MB: u64 = 4096;

#[derive(Clone, Debug)]
pub struct WingsConfig {
    /// Wings' API port on every node.
    pub wings_port: u16,
    /// Game ports instances bind on every node, inclusive.
    pub game_ports: (u16, u16),
    /// Marks this network's servers (`hallow.owner` label).
    pub owner: String,
}

/// See the module docs.
pub struct WingsOrchestrator {
    db: PgPool,
    panel: Arc<dyn Panel>,
    config: WingsConfig,
    http: reqwest::Client,
    /// Node panel UUID to its Wings token.
    tokens: Mutex<HashMap<Uuid, NodeToken>>,
}

#[derive(Deserialize)]
struct WingsServer {
    #[serde(default)]
    state: String,
    configuration: WingsServerConfiguration,
}

#[derive(Deserialize)]
struct WingsServerConfiguration {
    uuid: Uuid,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

impl WingsOrchestrator {
    pub fn new(
        db: PgPool,
        panel: Arc<dyn Panel>,
        config: WingsConfig,
    ) -> Result<Self, OrchestratorError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("hallow-control-plane/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| OrchestratorError::Unavailable(e.to_string()))?;
        Ok(Self {
            db,
            panel,
            config,
            http,
            tokens: Mutex::new(HashMap::new()),
        })
    }

    /// The `hallow.owner` label this network's servers carry.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.config.owner
    }

    async fn node(&self, name: &str) -> Result<Node, OrchestratorError> {
        hallow_db::list_nodes(&self.db)
            .await
            .map_err(|e| OrchestratorError::Unavailable(format!("database: {e}")))?
            .into_iter()
            .find(|n| n.name == name)
            .ok_or_else(|| OrchestratorError::Refused(format!("no node {name}")))
    }

    async fn token(&self, node: &Node, refresh: bool) -> Result<NodeToken, OrchestratorError> {
        if !refresh && let Some(token) = self.lock_tokens().get(&node.panel_uuid) {
            return Ok(token.clone());
        }
        let token = self
            .panel
            .node_token(node.panel_uuid)
            .await
            .map_err(|e| OrchestratorError::Unavailable(format!("node token: {e}")))?;
        self.lock_tokens().insert(node.panel_uuid, token.clone());
        Ok(token)
    }

    fn lock_tokens(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, NodeToken>> {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The node a Wings remote call comes from, by its
    /// `Authorization: Bearer <token_id>.<token>`.
    pub async fn node_by_credentials(&self, bearer: &str) -> Option<Node> {
        let (token_id, token) = bearer.split_once('.')?;
        let nodes = hallow_db::list_nodes(&self.db).await.ok()?;
        for refresh in [false, true] {
            for node in &nodes {
                let Ok(known) = self.token(node, refresh).await else {
                    continue;
                };
                if known.token_id == token_id
                    && crate::auth::constant_time_eq(known.token.as_bytes(), token.as_bytes())
                {
                    return Some(node.clone());
                }
            }
        }
        None
    }

    /// Calls a node's Wings API. A `401` refreshes the cached token once.
    async fn call(
        &self,
        node: &Node,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, String), OrchestratorError> {
        let host = match node.private_ip {
            IpAddr::V6(ip) => format!("[{ip}]"),
            IpAddr::V4(ip) => ip.to_string(),
        };
        let url = format!("http://{host}:{}{path}", self.config.wings_port);
        for refresh in [false, true] {
            let token = self.token(node, refresh).await?;
            let mut request = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&token.token)
                .header(reqwest::header::ACCEPT, "application/json");
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request
                .send()
                .await
                .map_err(|e| OrchestratorError::Unavailable(format!("wings {}: {e}", node.name)))?;
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if status == StatusCode::UNAUTHORIZED && !refresh {
                continue;
            }
            return Ok((status, text));
        }
        unreachable!("the loop returns on its second pass")
    }

    fn handle_uuid(instance: &Handle<'_>) -> Uuid {
        instance
            .backend_id
            .and_then(|id| id.parse().ok())
            .unwrap_or(instance.uuid)
    }

    async fn power(&self, instance: Handle<'_>, action: &str) -> Result<(), OrchestratorError> {
        let node = self.node(instance.node).await?;
        let uuid = Self::handle_uuid(&instance);
        let (status, text) = self
            .call(
                &node,
                Method::POST,
                &format!("/api/servers/{uuid}/power"),
                Some(json!({ "action": action })),
            )
            .await?;
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::NOT_FOUND if action != "start" => Ok(()),
            s => Err(refused(action, s, &text)),
        }
    }
}

fn refused(what: &str, status: StatusCode, text: &str) -> OrchestratorError {
    let text = format!("wings {what}: {status}: {text}");
    if status.is_server_error() {
        OrchestratorError::Unavailable(text)
    } else {
        OrchestratorError::Refused(text)
    }
}

/// The server configuration Wings fetches from its remote
/// (`GET /api/remote/servers/{uuid}`), Calagopus Wings' `RawServer`:
/// `{settings, process_configuration}`.
#[must_use]
pub fn raw_server(spec: &InstanceSpec, node_ip: IpAddr, owner: &str) -> Value {
    let port = spec.port.unwrap_or(25565);
    let ip = node_ip.to_string();
    let mut environment: BTreeMap<String, String> = spec.env.clone();
    environment.insert("HALLOW_SERVER_ADDRESS".into(), address(node_ip, port));
    environment.insert("HALLOW_SERVER_PORT".into(), port.to_string());
    environment.insert("HALLOW_SERVER_DIR".into(), SERVER_DIR.into());
    json!({
        "settings": {
            "uuid": spec.uuid,
            "start_on_completion": null,
            "meta": {"name": spec.name, "description": format!("Hallow {} instance", spec.kind)},
            "suspended": false,
            "invocation": "hallow-entrypoint",
            "skip_egg_scripts": true,
            // None: the image's own ENTRYPOINT runs.
            "entrypoint": null,
            "environment": environment,
            "labels": {
                "hallow.instance": spec.name,
                "hallow.kind": spec.kind,
                "hallow.owner": owner,
            },
            "allocations": {
                "force_outgoing_ip": false,
                "default": {"ip": ip, "port": port},
                "mappings": {ip.as_str(): [port]},
            },
            "build": {
                "memory_limit": spec.memory_mb,
                "swap": 0,
                "io_weight": null,
                "cpu_limit": 0,
                "disk_space": DISK_MB,
                "threads": null,
                "oom_disabled": false,
            },
            "mounts": [],
            "egg": {"id": EGG},
            "container": {"image": spec.image, "timezone": null},
            "auto_start_behavior": "never",
        },
        "process_configuration": {
            "startup": {"done": null, "strip_ansi": false},
            // Pumpkin saves and exits on SIGTERM; the server plugin
            // unregisters on the way down.
            "stop": {"type": "signal", "value": "SIGTERM"},
            "configs": [],
        },
    })
}

/// `ip:port`, bracketing IPv6.
#[must_use]
pub fn address(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(ip) => format!("{ip}:{port}"),
        IpAddr::V6(ip) => format!("[{ip}]:{port}"),
    }
}

impl Orchestrator for WingsOrchestrator {
    fn name(&self) -> &'static str {
        "wings"
    }

    fn nodes(&self) -> OrchestratorFuture<'_, Vec<NodeSlot>> {
        Box::pin(async move {
            let nodes = hallow_db::list_nodes(&self.db)
                .await
                .map_err(|e| OrchestratorError::Unavailable(format!("database: {e}")))?;
            // The instance lifecycle leaves out nodes the fleet manager is
            // draining or still setting up, and sizes the rest.
            Ok(nodes
                .into_iter()
                .map(|n| NodeSlot {
                    name: n.name,
                    ports: Some(self.config.game_ports),
                    slots: None,
                })
                .collect())
        })
    }

    fn create<'a>(
        &'a self,
        node: &'a str,
        spec: &'a InstanceSpec,
        start: bool,
    ) -> OrchestratorFuture<'a, Created> {
        Box::pin(async move {
            let node = self.node(node).await?;
            let port = spec
                .port
                .ok_or_else(|| OrchestratorError::Refused("wings instances need a port".into()))?;
            // Wings fetches the configuration from us (crate::remote) before
            // it answers; the lifecycle stored the spec first.
            let (status, text) = self
                .call(
                    &node,
                    Method::POST,
                    "/api/servers",
                    Some(json!({
                        "uuid": spec.uuid,
                        "start_on_completion": start,
                        "skip_scripts": true,
                    })),
                )
                .await?;
            if !status.is_success() && status != StatusCode::CONFLICT {
                return Err(refused("create", status, &text));
            }
            Ok(Created {
                backend_id: spec.uuid.to_string(),
                address: address(node.private_ip, port),
            })
        })
    }

    fn start<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()> {
        Box::pin(self.power(instance, "start"))
    }

    fn stop<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()> {
        Box::pin(self.power(instance, "stop"))
    }

    fn delete<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()> {
        Box::pin(async move {
            let node = self.node(instance.node).await?;
            let uuid = Self::handle_uuid(&instance);
            let (status, text) = self
                .call(&node, Method::DELETE, &format!("/api/servers/{uuid}"), None)
                .await?;
            match status {
                s if s.is_success() || s == StatusCode::NOT_FOUND => Ok(()),
                s => Err(refused("delete", s, &text)),
            }
        })
    }

    fn status<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, Runtime> {
        Box::pin(async move {
            let node = self.node(instance.node).await?;
            let uuid = Self::handle_uuid(&instance);
            let (status, text) = self
                .call(&node, Method::GET, &format!("/api/servers/{uuid}"), None)
                .await?;
            match status {
                StatusCode::NOT_FOUND => Ok(Runtime::Missing),
                s if s.is_success() => {
                    let server: WingsServer = serde_json::from_str(&text)
                        .map_err(|e| OrchestratorError::Unavailable(format!("wings: {e}")))?;
                    Ok(match server.state.as_str() {
                        "running" | "starting" => Runtime::Running,
                        _ => Runtime::Stopped,
                    })
                }
                s => Err(refused("status", s, &text)),
            }
        })
    }

    fn list(&self) -> OrchestratorFuture<'_, Vec<Listed>> {
        Box::pin(async move {
            let nodes = hallow_db::list_nodes(&self.db)
                .await
                .map_err(|e| OrchestratorError::Unavailable(format!("database: {e}")))?;
            let mut found = Vec::new();
            for node in nodes {
                let (status, text) = match self.call(&node, Method::GET, "/api/servers", None).await
                {
                    Ok(answer) => answer,
                    Err(err) => {
                        tracing::warn!(node = %node.name, %err, "can't list its servers");
                        continue;
                    }
                };
                if !status.is_success() {
                    continue;
                }
                let servers: Vec<WingsServer> = serde_json::from_str(&text).unwrap_or_default();
                for server in servers {
                    let labels = &server.configuration.labels;
                    if labels.get("hallow.owner") != Some(&self.config.owner) {
                        continue;
                    }
                    if let Some(name) = labels.get("hallow.instance") {
                        found.push(Listed {
                            node: node.name.clone(),
                            name: name.clone(),
                            backend_id: server.configuration.uuid.to_string(),
                        });
                    }
                }
            }
            Ok(found)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_server_binds_the_game_port_on_the_private_ip() {
        let spec = InstanceSpec {
            name: "test-game-1a2b3c".into(),
            uuid: Uuid::from_u128(7),
            kind: "test-game".into(),
            image: "ghcr.io/xena-studios/test-game:1.0.0".into(),
            env: [(
                "HALLOW_SERVER_NAME".to_owned(),
                "test-game-1a2b3c".to_owned(),
            )]
            .into(),
            port: Some(30007),
            memory_mb: 1024,
        };
        let raw = raw_server(&spec, "10.42.1.7".parse().unwrap(), "hallow");
        let settings = &raw["settings"];
        assert_eq!(settings["uuid"], json!(Uuid::from_u128(7)));
        assert_eq!(settings["skip_egg_scripts"], json!(true));
        assert_eq!(settings["entrypoint"], Value::Null);
        assert_eq!(
            settings["allocations"]["default"],
            json!({"ip": "10.42.1.7", "port": 30007})
        );
        assert_eq!(
            settings["allocations"]["mappings"]["10.42.1.7"],
            json!([30007])
        );
        assert_eq!(settings["container"]["image"], json!(spec.image));
        assert_eq!(settings["build"]["memory_limit"], json!(1024));
        let env = &settings["environment"];
        assert_eq!(env["HALLOW_SERVER_ADDRESS"], json!("10.42.1.7:30007"));
        assert_eq!(env["HALLOW_SERVER_PORT"], json!("30007"));
        assert_eq!(env["HALLOW_SERVER_DIR"], json!(SERVER_DIR));
        assert_eq!(env["HALLOW_SERVER_NAME"], json!("test-game-1a2b3c"));
        assert_eq!(settings["labels"]["hallow.owner"], json!("hallow"));
        assert_eq!(
            raw["process_configuration"]["stop"]["value"],
            json!("SIGTERM")
        );
    }

    #[test]
    fn ipv6_addresses_are_bracketed() {
        assert_eq!(
            address("fd00::7".parse().unwrap(), 30000),
            "[fd00::7]:30000"
        );
    }
}
