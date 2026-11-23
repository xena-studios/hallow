//! What the fleet manager reads from and does to the rest of the network,
//! behind [`World`] so a simulation can stand in for it. [`LiveWorld`] is
//! the real one: live load from Redis, instances ended through the instance
//! lifecycle, proxies drained through their drain mode.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use hallow_proto::{
    instance::KindSpec,
    node::{IssueJoinToken, NodeRole},
};

use super::proxies::ProxyLoad;
use crate::AppState;

pub type WorldFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// One node's load, by the instances on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeUsage {
    /// Live instances (creating, starting, ready).
    pub instances: u32,
    /// Slots they take: each its kind's `players.max`.
    pub used_slots: u32,
    pub players: u32,
}

/// One game queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueLoad {
    pub kind: String,
    pub players: u32,
    /// How long its longest waiting entry has waited.
    pub wait: Duration,
}

/// The network's load at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Load {
    /// By node name; nodes without instances may be missing.
    pub nodes: BTreeMap<String, NodeUsage>,
    pub queues: Vec<QueueLoad>,
    pub proxies: Vec<ProxyLoad>,
}

/// What the fleet manager needs from the network.
pub trait World: Send + Sync {
    /// Live load: instances per node, queue waits, proxies.
    fn load(&self) -> WorldFuture<'_, Load>;
    /// Nodes that joined through the control plane (`hallow.nodes`), so a
    /// bought machine counts as set up.
    fn joined_nodes(&self) -> WorldFuture<'_, BTreeSet<String>>;
    /// Ends the instances on `node` with nobody on them, or every one with
    /// `occupied_too` (their players are sent to a lobby first). Returns how
    /// many it ended.
    fn clear_node<'a>(&'a self, node: &'a str, occupied_too: bool) -> WorldFuture<'a, u32>;
    /// Returns whether the proxy exists.
    fn set_proxy_draining<'a>(&'a self, id: &'a str, draining: bool) -> WorldFuture<'a, bool>;
    /// Tells every queued player `text`.
    fn notify_queued<'a>(&'a self, text: &'a str) -> WorldFuture<'a, ()>;
    /// A one-time join token for a node being bought (not in dry run).
    fn join_token<'a>(&'a self, name: &'a str, region: &'a str) -> WorldFuture<'a, String>;
}

/// The network itself.
pub struct LiveWorld {
    pub state: Arc<AppState>,
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

impl LiveWorld {
    async fn kinds(&self) -> Result<BTreeMap<String, KindSpec>, String> {
        let Some(db) = &self.state.db else {
            return Ok(BTreeMap::new());
        };
        Ok(hallow_db::list_kinds(db)
            .await
            .map_err(|e| format!("database: {e}"))?
            .into_iter()
            .map(|k| (k.name, k.spec.0))
            .collect())
    }
}

impl World for LiveWorld {
    fn load(&self) -> WorldFuture<'_, Load> {
        Box::pin(async move {
            let store = &self.state.store;
            let kinds = self.kinds().await?;
            let instances = store.instances().await.map_err(|e| e.to_string())?;
            let (_, servers) = store.read_servers().await.map_err(|e| e.to_string())?;
            let players: BTreeMap<&str, u32> = servers
                .iter()
                .map(|s| (s.name.as_str(), s.players))
                .collect();
            let mut nodes: BTreeMap<String, NodeUsage> = BTreeMap::new();
            for instance in instances.iter().filter(|i| i.state.is_live()) {
                let usage = nodes.entry(instance.node.clone()).or_default();
                usage.instances += 1;
                usage.used_slots += kinds.get(&instance.kind).map_or(0, |k| k.players.max);
                usage.players += players.get(instance.name.as_str()).copied().unwrap_or(0);
            }
            let now = now_ms();
            let queues = store
                .queue_waits()
                .await
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(kind, players, since)| QueueLoad {
                    kind,
                    players,
                    wait: Duration::from_millis(now.saturating_sub(since)),
                })
                .collect();
            let proxies = store
                .proxies()
                .await
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|p| ProxyLoad {
                    id: p.id,
                    players: p.players,
                    draining: p.draining,
                })
                .collect();
            Ok(Load {
                nodes,
                queues,
                proxies,
            })
        })
    }

    fn joined_nodes(&self) -> WorldFuture<'_, BTreeSet<String>> {
        Box::pin(async move {
            let Some(db) = &self.state.db else {
                return Ok(BTreeSet::new());
            };
            Ok(hallow_db::list_nodes(db)
                .await
                .map_err(|e| format!("database: {e}"))?
                .into_iter()
                .map(|n| n.name)
                .collect())
        })
    }

    fn clear_node<'a>(&'a self, node: &'a str, occupied_too: bool) -> WorldFuture<'a, u32> {
        Box::pin(async move {
            let Some(manager) = &self.state.instances else {
                return Ok(0);
            };
            let instances = manager
                .list(&self.state.store)
                .await
                .map_err(|e| e.to_string())?;
            let reason = if occupied_too {
                "its node is being cancelled"
            } else {
                "its node is draining"
            };
            let mut ended = 0;
            for instance in instances
                .iter()
                .filter(|i| i.node == node && i.state.is_live())
                .filter(|i| occupied_too || i.players == 0)
            {
                if manager
                    .end_for(&self.state, &instance.name, reason)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    ended += 1;
                }
            }
            Ok(ended)
        })
    }

    fn set_proxy_draining<'a>(&'a self, id: &'a str, draining: bool) -> WorldFuture<'a, bool> {
        Box::pin(async move {
            self.state
                .store
                .set_draining(id, draining)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn notify_queued<'a>(&'a self, text: &'a str) -> WorldFuture<'a, ()> {
        Box::pin(async move {
            self.state
                .store
                .notify_queued(text)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    fn join_token<'a>(&'a self, name: &'a str, region: &'a str) -> WorldFuture<'a, String> {
        Box::pin(async move {
            crate::nodes::issue(
                &self.state,
                IssueJoinToken {
                    role: NodeRole::Burst,
                    region: region.to_owned(),
                    name: Some(name.to_owned()),
                    labels: BTreeMap::new(),
                    ttl_seconds: None,
                },
            )
            .await
            .map(|t| t.token)
            .map_err(|e| format!("{e:?}"))
        })
    }
}
