//! Instances as containers on the local Docker Engine, for the dev network.
//!
//! The control plane talks to the Engine API (v1.41) over its Unix socket
//! (`/var/run/docker.sock`, mounted into the control plane's container by a
//! dev-only compose file) or over TCP. Every instance is one container named
//! after the instance, on one Docker network (the compose network), where
//! proxies reach it by name on port 25565. There is one node, `docker`.
//!
//! Containers carry the labels `hallow.instance`, `hallow.kind` and
//! `hallow.owner`; the owner (by default the network name) is what
//! [`Orchestrator::list`] filters on, so two dev networks on one Docker host
//! never touch each other's containers.

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode, header};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Created, Handle, InstanceSpec, Listed, NodeSlot, Orchestrator, OrchestratorError,
    OrchestratorFuture, Runtime,
};

/// The one node name.
pub const NODE: &str = "docker";
/// Engine API version used in paths.
const API: &str = "/v1.41";
/// Every call's limit.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a stop waits before Docker kills the container.
const STOP_SECONDS: u32 = 10;
/// The port servers listen on inside their container.
const SERVER_PORT: u16 = 25565;

/// Where the Engine API is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Unix(PathBuf),
    /// `host:port`, plain HTTP.
    Tcp(String),
}

impl Endpoint {
    /// `unix:///path`, `/path`, or `tcp://host:port`.
    pub fn parse(text: &str) -> Result<Self, String> {
        if let Some(addr) = text.strip_prefix("tcp://") {
            return Ok(Self::Tcp(addr.trim_end_matches('/').to_owned()));
        }
        let path = text.strip_prefix("unix://").unwrap_or(text);
        if path.starts_with('/') {
            Ok(Self::Unix(PathBuf::from(path)))
        } else {
            Err(format!(
                "{text} isn't unix:///path or tcp://host:port for the Docker Engine"
            ))
        }
    }
}

#[derive(Clone, Debug)]
pub struct DockerConfig {
    pub endpoint: Endpoint,
    /// The network instances join, such as the compose project's
    /// `gatetest_default`.
    pub network: String,
    /// Marks containers as this network's; defaults to `network`.
    pub owner: String,
}

impl DockerConfig {
    #[must_use]
    pub fn new(endpoint: Endpoint, network: impl Into<String>) -> Self {
        let network = network.into();
        Self {
            endpoint,
            owner: network.clone(),
            network,
        }
    }
}

/// See the module docs.
pub struct DockerOrchestrator {
    config: DockerConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CreatedContainer {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerSummary {
    id: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Inspected {
    state: InspectedState,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectedState {
    running: bool,
}

#[derive(Deserialize)]
struct EngineMessage {
    message: String,
}

impl DockerOrchestrator {
    #[must_use]
    pub fn new(config: DockerConfig) -> Self {
        Self { config }
    }

    /// One request; returns the status and body. Each request opens its own
    /// connection, which is plenty for a few calls a second.
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, Bytes), OrchestratorError> {
        let request = async {
            let body = body.map_or_else(Bytes::new, |b| Bytes::from(b.to_string()));
            let request = Request::builder()
                .method(method)
                .uri(format!("{API}{path}"))
                .header(header::HOST, "docker")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Full::new(body))
                .map_err(|e| e.to_string())?;
            let response = match &self.config.endpoint {
                Endpoint::Unix(socket) => {
                    let stream = tokio::net::UnixStream::connect(socket)
                        .await
                        .map_err(|e| format!("{}: {e}", socket.display()))?;
                    send(TokioIo::new(stream), request).await?
                }
                Endpoint::Tcp(addr) => {
                    let stream = tokio::net::TcpStream::connect(addr)
                        .await
                        .map_err(|e| format!("{addr}: {e}"))?;
                    send(TokioIo::new(stream), request).await?
                }
            };
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|e| e.to_string())?
                .to_bytes();
            Ok::<_, String>((status, bytes))
        };
        match tokio::time::timeout(CALL_TIMEOUT, request).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(err)) => Err(OrchestratorError::Unavailable(format!("docker: {err}"))),
            Err(_) => Err(OrchestratorError::Unavailable(
                "docker didn't answer in time".into(),
            )),
        }
    }

    /// The container's id, or its name when create never answered (Docker
    /// takes either).
    fn id<'a>(instance: &Handle<'a>) -> &'a str {
        instance.backend_id.unwrap_or(instance.name)
    }

    fn create_body(&self, spec: &InstanceSpec, address: &str) -> Value {
        let mut env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        env.push(format!("HALLOW_SERVER_ADDRESS={address}"));
        json!({
            "Image": spec.image,
            "Env": env,
            "Tty": true,
            "Labels": {
                "hallow.instance": spec.name,
                "hallow.kind": spec.kind,
                "hallow.owner": self.config.owner,
            },
            "HostConfig": {
                "NetworkMode": self.config.network,
                "Memory": u64::from(spec.memory_mb) * 1024 * 1024,
                "RestartPolicy": {"Name": "no"},
            },
            "NetworkingConfig": {
                "EndpointsConfig": {
                    self.config.network.as_str(): {"Aliases": [spec.name]},
                },
            },
        })
    }
}

async fn send<T>(
    io: TokioIo<T>,
    request: Request<Full<Bytes>>,
) -> Result<hyper::Response<hyper::body::Incoming>, String>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::debug!(%err, "docker connection ended");
        }
    });
    sender
        .send_request(request)
        .await
        .map_err(|e| e.to_string())
}

/// The Engine's error message, or the raw body.
fn message(body: &Bytes) -> String {
    serde_json::from_slice::<EngineMessage>(body)
        .map(|m| m.message)
        .unwrap_or_else(|_| String::from_utf8_lossy(body).into_owned())
}

fn refused(what: &str, status: StatusCode, body: &Bytes) -> OrchestratorError {
    let text = format!("{what}: {status}: {}", message(body));
    if status.is_server_error() {
        OrchestratorError::Unavailable(text)
    } else {
        OrchestratorError::Refused(text)
    }
}

impl Orchestrator for DockerOrchestrator {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn places_anywhere(&self) -> bool {
        true
    }

    fn nodes(&self) -> OrchestratorFuture<'_, Vec<NodeSlot>> {
        Box::pin(async move {
            Ok(vec![NodeSlot {
                name: NODE.to_owned(),
                ports: None,
                slots: None,
            }])
        })
    }

    fn create<'a>(
        &'a self,
        _node: &'a str,
        spec: &'a InstanceSpec,
        start: bool,
    ) -> OrchestratorFuture<'a, Created> {
        Box::pin(async move {
            let address = format!("{}:{SERVER_PORT}", spec.name);
            let (status, body) = self
                .call(
                    Method::POST,
                    &format!("/containers/create?name={}", spec.name),
                    Some(self.create_body(spec, &address)),
                )
                .await?;
            if status != StatusCode::CREATED {
                return Err(refused("create", status, &body));
            }
            let created: CreatedContainer = serde_json::from_slice(&body)
                .map_err(|e| OrchestratorError::Unavailable(format!("create: {e}")))?;
            let created = Created {
                backend_id: created.id,
                address,
            };
            if start {
                self.start(Handle {
                    node: NODE,
                    name: &spec.name,
                    uuid: spec.uuid,
                    backend_id: Some(&created.backend_id),
                })
                .await?;
            }
            Ok(created)
        })
    }

    fn start<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()> {
        Box::pin(async move {
            let (status, body) = self
                .call(
                    Method::POST,
                    &format!("/containers/{}/start", Self::id(&instance)),
                    None,
                )
                .await?;
            match status {
                StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED => Ok(()),
                status => Err(refused("start", status, &body)),
            }
        })
    }

    fn stop<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()> {
        Box::pin(async move {
            let (status, body) = self
                .call(
                    Method::POST,
                    &format!("/containers/{}/stop?t={STOP_SECONDS}", Self::id(&instance)),
                    None,
                )
                .await?;
            match status {
                StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED | StatusCode::NOT_FOUND => Ok(()),
                status => Err(refused("stop", status, &body)),
            }
        })
    }

    fn delete<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()> {
        Box::pin(async move {
            let (status, body) = self
                .call(
                    Method::DELETE,
                    &format!("/containers/{}?force=true&v=true", Self::id(&instance)),
                    None,
                )
                .await?;
            match status {
                StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(()),
                // Already being removed.
                StatusCode::CONFLICT if message(&body).contains("in progress") => Ok(()),
                status => Err(refused("delete", status, &body)),
            }
        })
    }

    fn status<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, Runtime> {
        Box::pin(async move {
            let (status, body) = self
                .call(
                    Method::GET,
                    &format!("/containers/{}/json", Self::id(&instance)),
                    None,
                )
                .await?;
            match status {
                StatusCode::OK => {
                    let inspected: Inspected = serde_json::from_slice(&body)
                        .map_err(|e| OrchestratorError::Unavailable(format!("inspect: {e}")))?;
                    Ok(if inspected.state.running {
                        Runtime::Running
                    } else {
                        Runtime::Stopped
                    })
                }
                StatusCode::NOT_FOUND => Ok(Runtime::Missing),
                status => Err(refused("inspect", status, &body)),
            }
        })
    }

    fn list(&self) -> OrchestratorFuture<'_, Vec<Listed>> {
        Box::pin(async move {
            let filters = json!({ "label": [format!("hallow.owner={}", self.config.owner)] });
            let (status, body) = self
                .call(
                    Method::GET,
                    &format!(
                        "/containers/json?all=true&filters={}",
                        percent_encode(&filters.to_string())
                    ),
                    None,
                )
                .await?;
            if status != StatusCode::OK {
                return Err(refused("list", status, &body));
            }
            let containers: Vec<ContainerSummary> = serde_json::from_slice(&body)
                .map_err(|e| OrchestratorError::Unavailable(format!("list: {e}")))?;
            Ok(containers
                .into_iter()
                .filter_map(|c| {
                    Some(Listed {
                        node: NODE.to_owned(),
                        name: c.labels.get("hallow.instance")?.clone(),
                        backend_id: c.id,
                    })
                })
                .collect())
        })
    }
}

/// Percent-encodes everything but unreserved characters.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn endpoints_parse() {
        assert_eq!(
            Endpoint::parse("unix:///var/run/docker.sock").unwrap(),
            Endpoint::Unix("/var/run/docker.sock".into())
        );
        assert_eq!(
            Endpoint::parse("/var/run/docker.sock").unwrap(),
            Endpoint::Unix("/var/run/docker.sock".into())
        );
        assert_eq!(
            Endpoint::parse("tcp://127.0.0.1:2375/").unwrap(),
            Endpoint::Tcp("127.0.0.1:2375".into())
        );
        assert!(Endpoint::parse("docker.sock").is_err());
    }

    #[test]
    fn containers_join_the_network_with_labels() {
        let docker = DockerOrchestrator::new(DockerConfig::new(
            Endpoint::Unix("/var/run/docker.sock".into()),
            "gatetest_default",
        ));
        let spec = InstanceSpec {
            name: "test-game-1a2b3c".into(),
            uuid: Uuid::nil(),
            kind: "test-game".into(),
            image: "hallow-test-game:dev".into(),
            env: [(
                "HALLOW_SERVER_NAME".to_owned(),
                "test-game-1a2b3c".to_owned(),
            )]
            .into(),
            port: None,
            memory_mb: 512,
        };
        let body = docker.create_body(&spec, "test-game-1a2b3c:25565");
        assert_eq!(body["Image"], "hallow-test-game:dev");
        assert_eq!(body["HostConfig"]["NetworkMode"], "gatetest_default");
        assert_eq!(body["HostConfig"]["Memory"], 512 * 1024 * 1024);
        assert_eq!(body["Labels"]["hallow.owner"], "gatetest_default");
        assert_eq!(
            body["NetworkingConfig"]["EndpointsConfig"]["gatetest_default"]["Aliases"][0],
            "test-game-1a2b3c"
        );
        let env: Vec<&str> = body["Env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            env,
            [
                "HALLOW_SERVER_NAME=test-game-1a2b3c",
                "HALLOW_SERVER_ADDRESS=test-game-1a2b3c:25565"
            ]
        );
    }

    #[test]
    fn filters_are_encoded() {
        assert_eq!(
            percent_encode(r#"{"label":["a=b"]}"#),
            "%7B%22label%22%3A%5B%22a%3Db%22%5D%7D"
        );
    }
}
