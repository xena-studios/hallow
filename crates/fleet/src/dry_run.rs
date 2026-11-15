//! Dry run: a provider wrapper that logs what would happen and never calls
//! the provider it wraps. The default until each provider is trusted.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::provider::{
    Node, NodeId, NodeKind, NodeSpec, Prices, Provider, ProviderError, ProviderState,
};

/// One call dry run answered instead of the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DryRunAction {
    Create { node: NodeId, spec: NodeSpec },
    Cancel { node: NodeId },
}

#[derive(Default)]
struct State {
    next_id: u64,
    nodes: BTreeMap<NodeId, Node>,
    log: Vec<DryRunAction>,
}

/// Wraps a real provider without ever calling it. Machines it "creates" live
/// in memory, priced from the price list it was given, so the rest of the
/// fleet manager runs exactly as it would for real. Turning dry run off is
/// [`DryRun::into_inner`].
pub struct DryRun<P> {
    inner: P,
    prices: Prices,
    clock: fn() -> DateTime<Utc>,
    state: Mutex<State>,
}

impl<P: Provider> DryRun<P> {
    /// `prices` stands in for the provider's price list, which dry run doesn't
    /// fetch: a cached copy, or the prices from the provider's config.
    pub fn new(inner: P, prices: Prices) -> Self {
        DryRun {
            inner,
            prices,
            clock: Utc::now,
            state: Mutex::default(),
        }
    }

    /// Uses `clock` for created-at times, so simulations can run on their own time.
    pub fn with_clock(mut self, clock: fn() -> DateTime<Utc>) -> Self {
        self.clock = clock;
        self
    }

    /// Every create and cancel so far, in order.
    pub fn log(&self) -> Vec<DryRunAction> {
        self.state().log.clone()
    }

    pub fn inner(&self) -> &P {
        &self.inner
    }

    /// Turns dry run off: the real provider.
    pub fn into_inner(self) -> P {
        self.inner
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<P: Provider> Provider for DryRun<P> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn kind(&self) -> NodeKind {
        self.inner.kind()
    }

    async fn create(&self, spec: &NodeSpec) -> Result<Node, ProviderError> {
        let price = self
            .prices
            .find(&spec.machine_type, &spec.region)
            .ok_or_else(|| {
                ProviderError::InvalidRequest(format!(
                    "no price for {} in {}",
                    spec.machine_type, spec.region
                ))
            })?;
        let mut state = self.state();
        state.next_id += 1;
        let node = Node {
            id: NodeId(format!("dry-run-{}", state.next_id)),
            provider: self.inner.name().to_owned(),
            name: spec.name.clone(),
            region: spec.region.clone(),
            machine_type: spec.machine_type.clone(),
            kind: self.inner.kind(),
            state: ProviderState::Provisioning,
            cost: price.cost,
            created_at: (self.clock)(),
            labels: spec.labels.clone(),
        };
        tracing::info!(
            provider = self.inner.name(),
            node = %node.id,
            name = %spec.name,
            region = %spec.region,
            machine_type = %spec.machine_type,
            per_hour = %price.cost.per_hour(),
            "dry run: would create a node"
        );
        state.nodes.insert(node.id.clone(), node.clone());
        state.log.push(DryRunAction::Create {
            node: node.id.clone(),
            spec: spec.clone(),
        });
        Ok(node)
    }

    async fn status(&self, id: &NodeId) -> Result<Node, ProviderError> {
        self.state()
            .nodes
            .get(id)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))
    }

    async fn cancel(&self, id: &NodeId) -> Result<(), ProviderError> {
        let mut state = self.state();
        let Some(node) = state.nodes.remove(id) else {
            return Ok(());
        };
        tracing::info!(
            provider = self.inner.name(),
            node = %id,
            region = %node.region,
            machine_type = %node.machine_type,
            "dry run: would cancel a node"
        );
        state.log.push(DryRunAction::Cancel { node: id.clone() });
        Ok(())
    }

    async fn list(&self) -> Result<Vec<Node>, ProviderError> {
        Ok(self.state().nodes.values().cloned().collect())
    }

    async fn prices(&self) -> Result<Prices, ProviderError> {
        Ok(self.prices.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::Money;
    use crate::provider::{Cost, Price};

    /// A provider that fails the test if dry run ever calls it.
    struct Forbidden;

    impl Provider for Forbidden {
        fn name(&self) -> &str {
            "forbidden"
        }
        fn kind(&self) -> NodeKind {
            NodeKind::Burst
        }
        async fn create(&self, _: &NodeSpec) -> Result<Node, ProviderError> {
            panic!("dry run called create on the real provider")
        }
        async fn status(&self, _: &NodeId) -> Result<Node, ProviderError> {
            panic!("dry run called status on the real provider")
        }
        async fn cancel(&self, _: &NodeId) -> Result<(), ProviderError> {
            panic!("dry run called cancel on the real provider")
        }
        async fn list(&self) -> Result<Vec<Node>, ProviderError> {
            panic!("dry run called list on the real provider")
        }
        async fn prices(&self) -> Result<Prices, ProviderError> {
            panic!("dry run called prices on the real provider")
        }
    }

    fn prices() -> Prices {
        Prices {
            currency: "EUR".into(),
            entries: vec![Price {
                machine_type: "cpx41".into(),
                region: "fsn1".into(),
                cost: Cost::hourly(Money::from_micros(30_000), Some(Money::from_units(20))),
            }],
        }
    }

    fn spec(machine_type: &str) -> NodeSpec {
        NodeSpec {
            name: "hallow-burst-1".into(),
            region: "fsn1".into(),
            machine_type: machine_type.into(),
            image: "debian-13".into(),
            user_data: "#cloud-config\n".into(),
            labels: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn never_calls_the_real_provider() {
        let dry = DryRun::new(Forbidden, prices());
        let node = dry.create(&spec("cpx41")).await.unwrap();
        assert_eq!(node.provider, "forbidden");
        assert_eq!(node.kind, NodeKind::Burst);
        assert_eq!(node.state, ProviderState::Provisioning);
        assert_eq!(node.cost.per_hour(), Money::from_micros(30_000));
        assert_eq!(dry.status(&node.id).await.unwrap(), node);
        assert_eq!(dry.list().await.unwrap(), vec![node.clone()]);
        assert_eq!(dry.prices().await.unwrap(), prices());
        dry.cancel(&node.id).await.unwrap();
        dry.cancel(&node.id).await.unwrap(); // already gone: still fine
        assert!(dry.list().await.unwrap().is_empty());
        assert!(matches!(
            dry.status(&node.id).await,
            Err(ProviderError::NotFound(_))
        ));
        assert_eq!(
            dry.log(),
            vec![
                DryRunAction::Create {
                    node: node.id.clone(),
                    spec: spec("cpx41")
                },
                DryRunAction::Cancel { node: node.id },
            ]
        );
    }

    #[tokio::test]
    async fn unknown_machine_type_fails_like_the_provider_would() {
        let dry = DryRun::new(Forbidden, prices());
        let err = dry.create(&spec("cx99")).await.unwrap_err();
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
        assert!(dry.log().is_empty());
    }
}
