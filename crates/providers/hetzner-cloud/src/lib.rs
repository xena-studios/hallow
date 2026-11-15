//! Hetzner Cloud provider for Hallow's fleet manager: hourly burst nodes
//! through the [Hetzner Cloud API](https://docs.hetzner.cloud/).
//!
//! Every server it creates carries the label `hallow=node`; `list` only asks
//! for servers with that label, and `status` and `cancel` refuse servers
//! without it, so other machines in the same Hetzner project are never
//! touched.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hallow_fleet::{
    Cost, Money, Node, NodeId, NodeKind, NodeSpec, Price, Prices, Provider, ProviderError,
    ProviderState,
};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The label every Hallow server carries.
pub const MANAGED_LABEL: (&str, &str) = ("hallow", "node");
/// Records the node kind on the server, for the panel's Fleet page.
pub const KIND_LABEL: &str = "hallow-kind";

pub const DEFAULT_BASE_URL: &str = "https://api.hetzner.cloud/v1";

/// The provider's name in [`Node::provider`].
pub const NAME: &str = "hetzner-cloud";

/// A Hetzner Cloud API token. The caller reads it from the secrets store;
/// it is never in config. `Debug` doesn't print it.
#[derive(Clone)]
pub struct ApiToken(String);

impl ApiToken {
    pub fn new(token: impl Into<String>) -> ApiToken {
        ApiToken(token.into())
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiToken(<redacted>)")
    }
}

/// Which Hetzner price to budget with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PriceBasis {
    /// Before VAT.
    #[default]
    Net,
    /// With VAT, what the card is charged without a VAT number.
    Gross,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Override for tests or a proxy. Includes `/v1`.
    pub base_url: String,
    /// Whole request, including reading the body.
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub price_basis: PriceBasis,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            price_basis: PriceBasis::Net,
        }
    }
}

/// The Hetzner Cloud provider. Cheap to clone.
#[derive(Debug, Clone)]
pub struct HetznerCloud {
    http: reqwest::Client,
    base_url: String,
    token: ApiToken,
    price_basis: PriceBasis,
}

impl HetznerCloud {
    pub fn new(token: ApiToken, config: Config) -> Result<HetznerCloud, ProviderError> {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .connect_timeout(config.connect_timeout)
            .user_agent(concat!("hallow-fleet/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        Ok(HetznerCloud {
            http,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            token,
            price_basis: config.price_basis,
        })
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(&self.token.0)
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, ProviderError> {
        let response = request.send().await.map_err(transport)?;
        let response = check(response).await?;
        response
            .json::<T>()
            .await
            .map_err(|e| ProviderError::Decode(e.to_string()))
    }

    async fn get_server(&self, id: &NodeId) -> Result<ApiServer, ProviderError> {
        let numeric: u64 = id.0.parse().map_err(|_| {
            ProviderError::InvalidRequest(format!("{id} is not a Hetzner server id"))
        })?;
        let envelope: ServerEnvelope = self
            .send(self.request(Method::GET, &format!("/servers/{numeric}")))
            .await?;
        if !envelope.server.is_managed() {
            return Err(ProviderError::NotManaged(id.clone()));
        }
        Ok(envelope.server)
    }

    fn to_node(&self, server: ApiServer) -> Result<Node, ProviderError> {
        let region = server
            .location
            .as_ref()
            .or(server.datacenter.as_ref().map(|d| &d.location))
            .map(|l| l.name.clone())
            .ok_or_else(|| {
                ProviderError::Decode(format!("server {} has no location", server.id))
            })?;
        let price = server
            .server_type
            .prices
            .iter()
            .find(|p| p.location == region)
            .ok_or_else(|| {
                ProviderError::Decode(format!(
                    "no price for {} in {region} on server {}",
                    server.server_type.name, server.id
                ))
            })?;
        Ok(Node {
            id: NodeId(server.id.to_string()),
            provider: NAME.to_owned(),
            name: server.name,
            region,
            machine_type: server.server_type.name,
            kind: NodeKind::Burst,
            state: provider_state(&server.status),
            cost: price.cost(self.price_basis)?,
            created_at: server.created,
            labels: server.labels,
        })
    }
}

impl Provider for HetznerCloud {
    fn name(&self) -> &str {
        NAME
    }

    fn kind(&self) -> NodeKind {
        NodeKind::Burst
    }

    async fn create(&self, spec: &NodeSpec) -> Result<Node, ProviderError> {
        let mut labels = spec.labels.clone();
        labels.insert(MANAGED_LABEL.0.to_owned(), MANAGED_LABEL.1.to_owned());
        labels.insert(KIND_LABEL.to_owned(), NodeKind::Burst.to_string());
        let body = CreateServer {
            name: &spec.name,
            server_type: &spec.machine_type,
            image: &spec.image,
            location: &spec.region,
            user_data: (!spec.user_data.is_empty()).then_some(spec.user_data.as_str()),
            labels: &labels,
            start_after_create: true,
        };
        let envelope: ServerEnvelope = self
            .send(self.request(Method::POST, "/servers").json(&body))
            .await?;
        let node = self.to_node(envelope.server)?;
        tracing::info!(provider = NAME, node = %node.id, name = %node.name, region = %node.region,
            machine_type = %node.machine_type, "created a node");
        Ok(node)
    }

    async fn status(&self, id: &NodeId) -> Result<Node, ProviderError> {
        let server = self.get_server(id).await?;
        self.to_node(server)
    }

    async fn cancel(&self, id: &NodeId) -> Result<(), ProviderError> {
        // Look first: only delete servers that carry Hallow's label.
        match self.get_server(id).await {
            Ok(_) => {}
            Err(ProviderError::NotFound(_)) => {
                tracing::warn!(provider = NAME, node = %id, "cancel: the node is already gone");
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let _: ActionEnvelope = self
            .send(self.request(Method::DELETE, &format!("/servers/{id}")))
            .await?;
        tracing::info!(provider = NAME, node = %id, "cancelled a node");
        Ok(())
    }

    async fn list(&self) -> Result<Vec<Node>, ProviderError> {
        let selector = format!("{}={}", MANAGED_LABEL.0, MANAGED_LABEL.1);
        let mut nodes = Vec::new();
        let mut page = 1u32;
        loop {
            let response: ServerList = self
                .send(self.request(Method::GET, "/servers").query(&[
                    ("label_selector", selector.as_str()),
                    ("page", &page.to_string()),
                    ("per_page", "50"),
                ]))
                .await?;
            for server in response.servers {
                // The selector already filters; this guards against a proxy
                // or API change that ignores it.
                if server.is_managed() {
                    nodes.push(self.to_node(server)?);
                }
            }
            match response.meta.and_then(|m| m.pagination.next_page) {
                Some(next) if next > page => page = next,
                _ => break,
            }
        }
        Ok(nodes)
    }

    async fn prices(&self) -> Result<Prices, ProviderError> {
        let response: PricingEnvelope = self.send(self.request(Method::GET, "/pricing")).await?;
        let mut entries = Vec::new();
        for server_type in response.pricing.server_types {
            for price in &server_type.prices {
                entries.push(Price {
                    machine_type: server_type.name.clone(),
                    region: price.location.clone(),
                    cost: price.cost(self.price_basis)?,
                });
            }
        }
        Ok(Prices {
            currency: response.pricing.currency,
            entries,
        })
    }
}

fn provider_state(status: &str) -> ProviderState {
    match status {
        "initializing" | "starting" => ProviderState::Provisioning,
        "running" => ProviderState::Running,
        "stopping" | "off" => ProviderState::Stopped,
        "deleting" => ProviderState::Deleting,
        _ => ProviderState::Unknown,
    }
}

fn transport(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Transport(e.to_string())
    }
}

/// Maps a Hetzner error response to a [`ProviderError`].
async fn check(response: Response) -> Result<Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retry_after = retry_after(&response);
    let body = response.text().await.unwrap_or_default();
    let (code, message) = match serde_json::from_str::<ErrorEnvelope>(&body) {
        Ok(e) => (e.error.code, e.error.message),
        Err(_) => (String::new(), body),
    };
    Err(match (status, code.as_str()) {
        (StatusCode::TOO_MANY_REQUESTS, _) | (_, "rate_limit_exceeded") => {
            ProviderError::RateLimited { retry_after }
        }
        (StatusCode::UNAUTHORIZED, _) | (_, "unauthorized") => ProviderError::Unauthorized,
        (StatusCode::FORBIDDEN, _) | (_, "forbidden") => ProviderError::Forbidden(message),
        (StatusCode::NOT_FOUND, _) | (_, "not_found") => ProviderError::NotFound(message),
        (_, "resource_limit_exceeded") => ProviderError::QuotaExceeded(message),
        (StatusCode::CONFLICT, _) | (_, "conflict" | "uniqueness_error" | "locked") => {
            ProviderError::Conflict(message)
        }
        (_, "invalid_input" | "json_error")
        | (StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY, _) => {
            ProviderError::InvalidRequest(message)
        }
        (_, "unavailable" | "maintenance" | "timeout") => ProviderError::Unavailable(message),
        (s, _) if s.is_server_error() => ProviderError::Unavailable(message),
        (s, _) => ProviderError::Api {
            status: s.as_u16(),
            code,
            message,
        },
    })
}

/// `Retry-After` in seconds, or Hetzner's `RateLimit-Reset` (a Unix time).
fn retry_after(response: &Response) -> Option<Duration> {
    let header = |name: &str| {
        response
            .headers()
            .get(name)?
            .to_str()
            .ok()?
            .trim()
            .parse::<i64>()
            .ok()
    };
    if let Some(secs) = header("retry-after") {
        return Some(Duration::from_secs(secs.max(0) as u64));
    }
    let reset = header("ratelimit-reset")?;
    let wait = reset - Utc::now().timestamp();
    Some(Duration::from_secs(wait.max(0) as u64))
}

// Wire types: only the fields Hallow reads.

#[derive(Serialize)]
struct CreateServer<'a> {
    name: &'a str,
    server_type: &'a str,
    image: &'a str,
    location: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_data: Option<&'a str>,
    labels: &'a BTreeMap<String, String>,
    start_after_create: bool,
}

#[derive(Deserialize)]
struct ServerEnvelope {
    server: ApiServer,
}

#[derive(Deserialize)]
struct ServerList {
    servers: Vec<ApiServer>,
    meta: Option<Meta>,
}

#[derive(Deserialize)]
struct Meta {
    pagination: Pagination,
}

#[derive(Deserialize)]
struct Pagination {
    next_page: Option<u32>,
}

#[derive(Deserialize)]
struct ApiServer {
    id: u64,
    name: String,
    status: String,
    created: DateTime<Utc>,
    server_type: ApiServerType,
    #[serde(default)]
    location: Option<ApiLocation>,
    #[serde(default)]
    datacenter: Option<ApiDatacenter>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

impl ApiServer {
    fn is_managed(&self) -> bool {
        self.labels.get(MANAGED_LABEL.0).map(String::as_str) == Some(MANAGED_LABEL.1)
    }
}

#[derive(Deserialize)]
struct ApiServerType {
    name: String,
    #[serde(default)]
    prices: Vec<ApiPrice>,
}

#[derive(Deserialize)]
struct ApiDatacenter {
    location: ApiLocation,
}

#[derive(Deserialize)]
struct ApiLocation {
    name: String,
}

#[derive(Deserialize)]
struct ApiPrice {
    location: String,
    price_hourly: ApiAmount,
    price_monthly: ApiAmount,
}

impl ApiPrice {
    fn cost(&self, basis: PriceBasis) -> Result<Cost, ProviderError> {
        Ok(Cost::hourly(
            self.price_hourly.pick(basis)?,
            Some(self.price_monthly.pick(basis)?),
        ))
    }
}

#[derive(Deserialize)]
struct ApiAmount {
    net: String,
    gross: String,
}

impl ApiAmount {
    fn pick(&self, basis: PriceBasis) -> Result<Money, ProviderError> {
        let s = match basis {
            PriceBasis::Net => &self.net,
            PriceBasis::Gross => &self.gross,
        };
        Money::parse(s).ok_or_else(|| ProviderError::Decode(format!("bad price {s:?}")))
    }
}

#[derive(Deserialize)]
struct ActionEnvelope {
    #[allow(dead_code)]
    action: serde_json::Value,
}

#[derive(Deserialize)]
struct PricingEnvelope {
    pricing: ApiPricing,
}

#[derive(Deserialize)]
struct ApiPricing {
    currency: String,
    server_types: Vec<ApiPricingServerType>,
}

#[derive(Deserialize)]
struct ApiPricingServerType {
    name: String,
    prices: Vec<ApiPrice>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ApiError,
}

#[derive(Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_redacted() {
        let token = ApiToken::new("secret-token");
        assert_eq!(format!("{token:?}"), "ApiToken(<redacted>)");
        let provider = HetznerCloud::new(token, Config::default()).unwrap();
        assert!(!format!("{provider:?}").contains("secret-token"));
    }

    #[test]
    fn maps_server_status() {
        assert_eq!(provider_state("initializing"), ProviderState::Provisioning);
        assert_eq!(provider_state("running"), ProviderState::Running);
        assert_eq!(provider_state("off"), ProviderState::Stopped);
        assert_eq!(provider_state("deleting"), ProviderState::Deleting);
        assert_eq!(provider_state("migrating"), ProviderState::Unknown);
    }
}
