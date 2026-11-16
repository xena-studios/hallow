//! The Hallow Panel's admin API, as far as joining nodes needs it.
//!
//! [`Panel`] is the seam; [`HttpPanel`] calls a Calagopus panel with an
//! admin API key (`Authorization: Bearer c7sp_…`). The key needs the admin
//! permissions `nodes.create`, `nodes.allocations`, `nodes.reset-token`
//! (for enrollment codes) and `nodes.delete` (to undo a failed join), plus
//! `nodes.read-token` when instances run on Wings and `nodes.update` with a
//! node public URL ([`PanelConfig::node_public_url`]).
//!
//! | Call | Panel route |
//! |---|---|
//! | [`Panel::create_node`] | `POST /api/admin/nodes` |
//! | [`Panel::add_allocations`] | `POST /api/admin/nodes/{node}/allocations` |
//! | [`Panel::create_enrollment`] | `POST /api/admin/nodes/{node}/enrollment` |
//! | [`Panel::delete_node`] | `DELETE /api/admin/nodes/{node}` |
//! | [`Panel::node_token`] | `GET /api/admin/nodes/{node}/token` |
//! | [`Panel::set_node_public_url`] | `PATCH /api/admin/nodes/{node}` (`nodes.update`) |

use std::{collections::BTreeMap, fmt, future::Future, net::IpAddr, pin::Pin, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How the control plane reaches the panel, and how it sets nodes up there.
#[derive(Clone)]
pub struct PanelConfig {
    /// Where the control plane calls the panel, e.g. `http://panel:8000`.
    pub url: String,
    /// The URL Wings uses for the panel, e.g. `https://panel.example.net`.
    pub public_url: String,
    /// An admin API key. `Debug` doesn't print it.
    pub api_key: String,
    /// Panel location UUID per region.
    pub locations: BTreeMap<String, Uuid>,
    /// The location for regions not in `locations`.
    pub default_location: Option<Uuid>,
    /// Wings' API port on every node.
    pub wings_port: u16,
    /// Wings' SFTP port on every node.
    pub sftp_port: u16,
    /// The game-port allocations made on every node, inclusive. Must match
    /// the range `install.sh` opens (`--game-ports`).
    pub game_ports: (u16, u16),
    /// The remote URL nodes' Wings are enrolled with, when it isn't the
    /// panel: the control plane, which answers for its instances and passes
    /// the rest to the panel (docs/INSTANCES.md). `None` uses `public_url`.
    pub wings_remote: Option<String>,
    /// The URL browsers use for each node's Wings (console, file downloads,
    /// the node list's online check), with `{uuid}` (the panel's node UUID)
    /// and `{name}` filled in. Wings listens on the node's private address,
    /// which staff browsers can't reach, so this is normally the panel's own
    /// Wings proxy: `https://panel.example.net/wings-proxy/{uuid}` (the panel
    /// needs `APP_ENABLE_WINGS_PROXY=true`). `None` leaves the node's public
    /// URL unset, so browsers use its private one.
    pub node_public_url: Option<String>,
}

impl PanelConfig {
    #[must_use]
    pub fn new(url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let url = url.into();
        Self {
            public_url: url.clone(),
            url,
            api_key: api_key.into(),
            locations: BTreeMap::new(),
            default_location: None,
            wings_port: 8080,
            sftp_port: 2022,
            game_ports: (30000, 30999),
            wings_remote: None,
            node_public_url: None,
        }
    }

    /// The panel location for a region.
    #[must_use]
    pub fn location(&self, region: &str) -> Option<Uuid> {
        self.locations
            .get(region)
            .copied()
            .or(self.default_location)
    }
}

impl fmt::Debug for PanelConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PanelConfig")
            .field("url", &self.url)
            .field("public_url", &self.public_url)
            .field("api_key", &"<redacted>")
            .field("locations", &self.locations)
            .field("default_location", &self.default_location)
            .field("wings_port", &self.wings_port)
            .field("sftp_port", &self.sftp_port)
            .field("game_ports", &self.game_ports)
            .field("wings_remote", &self.wings_remote)
            .field("node_public_url", &self.node_public_url)
            .finish()
    }
}

/// A node to create, in the panel's `CreateNodeOptions` shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NewPanelNode {
    pub location_uuid: Uuid,
    pub backup_configuration_uuid: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub deployment_enabled: bool,
    pub maintenance_enabled: bool,
    pub public_url: Option<String>,
    /// Where the panel reaches Wings, e.g. `http://10.42.1.7:8080`.
    pub url: String,
    pub sftp_host: Option<String>,
    pub sftp_port: u16,
    /// MiB.
    pub memory: i64,
    /// MiB.
    pub disk: i64,
}

/// A one-time Wings enrollment code.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Enrollment {
    pub code: String,
    pub expires: DateTime<Utc>,
}

/// A node's Wings credentials: Wings checks `token` on its API and sends
/// `<token_id>.<token>` to its remote.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct NodeToken {
    pub token_id: String,
    pub token: String,
}

impl fmt::Debug for NodeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeToken")
            .field("token_id", &self.token_id)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PanelError {
    /// A node with that name exists.
    #[error("the panel refused: {0}")]
    Conflict(String),
    /// The panel answered 4xx: a bad request or a key without permission.
    #[error("the panel refused ({status}): {message}")]
    Rejected { status: u16, message: String },
    /// Couldn't reach the panel, or it answered 5xx or nonsense.
    #[error("the panel is unavailable: {0}")]
    Unavailable(String),
}

pub type PanelFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PanelError>> + Send + 'a>>;

/// What the control plane needs from the panel to join a node.
pub trait Panel: Send + Sync {
    /// Creates a node; returns its UUID.
    fn create_node<'a>(&'a self, node: &'a NewPanelNode) -> PanelFuture<'a, Uuid>;
    /// Adds allocations for `ports` on `ip`; returns how many were created.
    fn add_allocations<'a>(
        &'a self,
        node: Uuid,
        ip: IpAddr,
        ports: &'a [u16],
    ) -> PanelFuture<'a, u64>;
    /// Mints a one-time enrollment code Wings redeems for its credentials.
    /// `remote` overrides the panel URL Wings is told to use.
    fn create_enrollment<'a>(
        &'a self,
        node: Uuid,
        remote: Option<&'a str>,
    ) -> PanelFuture<'a, Enrollment>;
    /// Deletes a node (used to undo a join that failed half way).
    fn delete_node(&self, node: Uuid) -> PanelFuture<'_, ()>;
    /// A node's Wings credentials, to call its Wings API.
    fn node_token(&self, node: Uuid) -> PanelFuture<'_, NodeToken>;
    /// Sets the URL browsers use for a node's Wings.
    fn set_node_public_url<'a>(&'a self, node: Uuid, url: &'a str) -> PanelFuture<'a, ()>;
}

/// [`PanelConfig::node_public_url`] for one node.
#[must_use]
pub fn node_public_url(template: &str, node: Uuid, name: &str) -> String {
    template
        .replace("{uuid}", &node.to_string())
        .replace("{name}", name)
}

/// The panel over HTTP.
pub struct HttpPanel {
    http: reqwest::Client,
    url: String,
    api_key: String,
}

impl HttpPanel {
    pub fn new(config: &PanelConfig) -> Result<Self, PanelError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("hallow-control-plane/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| PanelError::Unavailable(e.to_string()))?;
        Ok(Self {
            http,
            url: config.url.trim_end_matches('/').to_owned(),
            api_key: config.api_key.clone(),
        })
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, PanelError> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.api_key)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| PanelError::Unavailable(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| PanelError::Unavailable(e.to_string()))?;
        if status.is_success() {
            return serde_json::from_str(&text)
                .map_err(|e| PanelError::Unavailable(format!("unexpected answer: {e}")));
        }
        // Calagopus errors are `{"errors": ["..."]}`.
        let message = serde_json::from_str::<PanelErrors>(&text)
            .map(|e| e.errors.join("; "))
            .unwrap_or(text);
        Err(match status.as_u16() {
            409 => PanelError::Conflict(message),
            s @ 400..=499 => PanelError::Rejected { status: s, message },
            s => PanelError::Unavailable(format!("{s}: {message}")),
        })
    }
}

#[derive(Deserialize)]
struct PanelErrors {
    errors: Vec<String>,
}

#[derive(Deserialize)]
struct CreatedNode {
    node: CreatedNodeInner,
}

#[derive(Deserialize)]
struct CreatedNodeInner {
    uuid: Uuid,
}

#[derive(Deserialize)]
struct CreatedAllocations {
    created: u64,
}

#[derive(Deserialize)]
struct Empty {}

impl Panel for HttpPanel {
    fn create_node<'a>(&'a self, node: &'a NewPanelNode) -> PanelFuture<'a, Uuid> {
        Box::pin(async move {
            let body = serde_json::to_value(node).expect("plain struct");
            let created: CreatedNode = self
                .call(reqwest::Method::POST, "/api/admin/nodes", Some(body))
                .await?;
            Ok(created.node.uuid)
        })
    }

    fn add_allocations<'a>(
        &'a self,
        node: Uuid,
        ip: IpAddr,
        ports: &'a [u16],
    ) -> PanelFuture<'a, u64> {
        Box::pin(async move {
            let body = serde_json::json!({ "ip": ip, "ip_alias": null, "ports": ports });
            let created: CreatedAllocations = self
                .call(
                    reqwest::Method::POST,
                    &format!("/api/admin/nodes/{node}/allocations"),
                    Some(body),
                )
                .await?;
            Ok(created.created)
        })
    }

    fn create_enrollment<'a>(
        &'a self,
        node: Uuid,
        remote: Option<&'a str>,
    ) -> PanelFuture<'a, Enrollment> {
        Box::pin(async move {
            self.call(
                reqwest::Method::POST,
                &format!("/api/admin/nodes/{node}/enrollment"),
                Some(serde_json::json!({ "remote": remote })),
            )
            .await
        })
    }

    fn delete_node(&self, node: Uuid) -> PanelFuture<'_, ()> {
        Box::pin(async move {
            let _: Empty = self
                .call(
                    reqwest::Method::DELETE,
                    &format!("/api/admin/nodes/{node}"),
                    None,
                )
                .await?;
            Ok(())
        })
    }

    fn node_token(&self, node: Uuid) -> PanelFuture<'_, NodeToken> {
        Box::pin(async move {
            self.call(
                reqwest::Method::GET,
                &format!("/api/admin/nodes/{node}/token"),
                None,
            )
            .await
        })
    }

    fn set_node_public_url<'a>(&'a self, node: Uuid, url: &'a str) -> PanelFuture<'a, ()> {
        Box::pin(async move {
            let _: Empty = self
                .call(
                    reqwest::Method::PATCH,
                    &format!("/api/admin/nodes/{node}"),
                    Some(serde_json::json!({ "public_url": url })),
                )
                .await?;
            Ok(())
        })
    }
}
