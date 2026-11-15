//! The provider trait every machine provider crate implements
//! (`crates/providers/*`), and the types it speaks.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Months, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::money::Money;

/// A provider's id for a machine, as a string whatever the provider uses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for NodeId {
    fn from(s: &str) -> NodeId {
        NodeId(s.to_owned())
    }
}

impl From<String> for NodeId {
    fn from(s: String) -> NodeId {
        NodeId(s)
    }
}

/// Burst nodes are hourly cloud machines added and returned automatically;
/// base nodes are monthly dedicated machines ordered and cancelled only with
/// staff approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Burst,
    Base,
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NodeKind::Burst => "burst",
            NodeKind::Base => "base",
        })
    }
}

/// The unit a provider charges in: a started hour, or a started month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BillingStep {
    Hour,
    Month,
}

/// Average hours in a month, for turning monthly prices into an hourly rate.
pub const HOURS_PER_MONTH: f64 = 730.0;

/// What a machine costs: `amount` per billing step, optionally capped per
/// month (Hetzner Cloud charges hourly up to its monthly price).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    pub amount: Money,
    pub step: BillingStep,
    pub monthly_cap: Option<Money>,
}

impl Cost {
    pub fn hourly(per_hour: Money, monthly_cap: Option<Money>) -> Cost {
        Cost {
            amount: per_hour,
            step: BillingStep::Hour,
            monthly_cap,
        }
    }

    pub fn monthly(per_month: Money) -> Cost {
        Cost {
            amount: per_month,
            step: BillingStep::Month,
            monthly_cap: None,
        }
    }

    /// The rate per hour, averaged for monthly billing.
    pub fn per_hour(&self) -> Money {
        match self.step {
            BillingStep::Hour => self.amount,
            BillingStep::Month => self.amount.scale(1.0 / HOURS_PER_MONTH),
        }
    }

    /// When the next billing step starts for a machine created at
    /// `created_at`: from then on another hour (or month) is charged.
    pub fn next_billing_step(
        &self,
        created_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> DateTime<Utc> {
        if now < created_at {
            return created_at;
        }
        match self.step {
            BillingStep::Hour => {
                let hours = (now - created_at).num_seconds() / 3600 + 1;
                created_at + TimeDelta::hours(hours)
            }
            BillingStep::Month => {
                let mut months = 1;
                loop {
                    let next = created_at
                        .checked_add_months(Months::new(months))
                        .unwrap_or(DateTime::<Utc>::MAX_UTC);
                    if next > now {
                        return next;
                    }
                    months += 1;
                }
            }
        }
    }
}

/// What to create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSpec {
    /// Unique per provider project (Hetzner requires it); the caller picks it.
    pub name: String,
    /// The provider's location, e.g. `fsn1`.
    pub region: String,
    /// The provider's machine type, e.g. `cpx41`.
    pub machine_type: String,
    /// OS image name or id.
    pub image: String,
    /// Cloud-init user data that installs Docker and Wings and joins the
    /// private network.
    pub user_data: String,
    /// Extra labels; providers add their own Hallow marker on top.
    pub labels: BTreeMap<String, String>,
}

/// A machine as the provider reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    Provisioning,
    Running,
    Stopped,
    Deleting,
    Deleted,
    Unknown,
}

/// A machine Hallow owns at a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    /// The provider's name, e.g. `hetzner-cloud`.
    pub provider: String,
    pub name: String,
    pub region: String,
    pub machine_type: String,
    pub kind: NodeKind,
    pub state: ProviderState,
    pub cost: Cost,
    pub created_at: DateTime<Utc>,
    pub labels: BTreeMap<String, String>,
}

impl Node {
    pub fn billing_step(&self) -> BillingStep {
        self.cost.step
    }

    pub fn next_billing_step(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        self.cost.next_billing_step(self.created_at, now)
    }
}

/// One machine type's price in one region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    pub machine_type: String,
    pub region: String,
    pub cost: Cost,
}

/// A provider's price list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Prices {
    /// ISO 4217, e.g. `EUR`.
    pub currency: String,
    pub entries: Vec<Price>,
}

impl Prices {
    pub fn find(&self, machine_type: &str, region: &str) -> Option<&Price> {
        self.entries
            .iter()
            .find(|p| p.machine_type == machine_type && p.region == region)
    }
}

/// Why a provider call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("rate limited by the provider{}", retry_after.map(|d| format!(", retry in {}s", d.as_secs())).unwrap_or_default())]
    RateLimited { retry_after: Option<Duration> },
    #[error("the provider rejected the API key")]
    Unauthorized,
    #[error("the API key may not do this: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("node {0} is not one of Hallow's machines")]
    NotManaged(NodeId),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("provider quota or limit reached: {0}")]
    QuotaExceeded(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("the provider didn't answer in time")]
    Timeout,
    #[error("could not reach the provider: {0}")]
    Transport(String),
    #[error("unexpected response from the provider: {0}")]
    Decode(String),
    #[error("provider error {status} {code}: {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
}

impl ProviderError {
    /// Whether trying the same call again later can succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::Unavailable(_)
                | ProviderError::Timeout
                | ProviderError::Transport(_)
        )
    }
}

/// One machine provider. Implementations live in `crates/providers/*`.
///
/// Every method that talks to the provider is async and returns a `Send`
/// future, so the fleet manager can run them on a multi-threaded runtime.
pub trait Provider: Send + Sync {
    /// Short stable name, e.g. `hetzner-cloud`.
    fn name(&self) -> &str;

    /// What kind of capacity this provider sells.
    fn kind(&self) -> NodeKind;

    /// Orders a machine. It comes back `Provisioning`; cloud-init does the rest.
    fn create(&self, spec: &NodeSpec) -> impl Future<Output = Result<Node, ProviderError>> + Send;

    /// One of Hallow's machines.
    fn status(&self, id: &NodeId) -> impl Future<Output = Result<Node, ProviderError>> + Send;

    /// Returns a machine to the provider. Cancelling a machine that is
    /// already gone succeeds.
    fn cancel(&self, id: &NodeId) -> impl Future<Output = Result<(), ProviderError>> + Send;

    /// Every machine Hallow owns at this provider, and none it doesn't.
    fn list(&self) -> impl Future<Output = Result<Vec<Node>, ProviderError>> + Send;

    /// Current prices per machine type and region.
    fn prices(&self) -> impl Future<Output = Result<Prices, ProviderError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 15, h, m, 0).unwrap()
    }

    #[test]
    fn next_hourly_step() {
        let cost = Cost::hourly(Money::from_cents(3), None);
        assert_eq!(cost.next_billing_step(at(10, 0), at(10, 0)), at(11, 0));
        assert_eq!(cost.next_billing_step(at(10, 0), at(10, 59)), at(11, 0));
        assert_eq!(cost.next_billing_step(at(10, 0), at(11, 0)), at(12, 0));
        assert_eq!(cost.next_billing_step(at(10, 20), at(13, 5)), at(13, 20));
    }

    #[test]
    fn next_monthly_step() {
        let cost = Cost::monthly(Money::from_units(40));
        let created = Utc.with_ymd_and_hms(2026, 10, 3, 0, 0, 0).unwrap();
        assert_eq!(
            cost.next_billing_step(created, at(12, 0)),
            Utc.with_ymd_and_hms(2026, 12, 3, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn monthly_rate_per_hour() {
        assert_eq!(
            Cost::monthly(Money::from_units(73)).per_hour(),
            Money::from_cents(10)
        );
    }
}
