//! Where instances run. [`Orchestrator`] is the seam between the instance
//! lifecycle ([`crate::instances`]) and what actually starts containers:
//!
//! - [`wings::WingsOrchestrator`] drives Calagopus Wings on real nodes, with
//!   the control plane standing in for the panel's remote API for the
//!   servers it creates ([`crate::remote`], docs/INSTANCES.md);
//! - [`docker::DockerOrchestrator`] runs containers on the local Docker
//!   Engine, for the dev network;
//! - [`mock::MockOrchestrator`] keeps everything in memory, for tests.
//!
//! Calls are idempotent where they can be: stopping or deleting something
//! that's already gone succeeds.

pub mod docker;
pub mod mock;
pub mod wings;

use std::{collections::BTreeMap, fmt, future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A node an orchestrator can place instances on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeSlot {
    pub name: String,
    /// The game ports instances bind on the node, inclusive; `None` when
    /// instances don't bind host ports (Docker on one network).
    pub ports: Option<(u16, u16)>,
    /// Player slots its instances may take in all (each instance takes its
    /// kind's `players.max`); `None` for no limit. Set from the fleet
    /// manager's node sizes (`crate::fleet_manager`).
    pub slots: Option<u32>,
}

/// What to create.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceSpec {
    /// Unique, `<kind>-<6 hex>`; also the server's name.
    pub name: String,
    pub uuid: Uuid,
    pub kind: String,
    pub image: String,
    /// The instance's environment, apart from what the orchestrator adds
    /// itself (`HALLOW_SERVER_ADDRESS` and anything node specific).
    pub env: BTreeMap<String, String>,
    /// The game port on the node, if the node has a port range.
    pub port: Option<u16>,
    pub memory_mb: u32,
}

/// What [`Orchestrator::create`] made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    /// The orchestrator's id: a container id, a Wings server UUID.
    pub backend_id: String,
    /// Where proxies reach the server, `host:port`.
    pub address: String,
}

/// One existing instance, by the orchestrator's handle on it.
#[derive(Clone, Copy, Debug)]
pub struct Handle<'a> {
    pub node: &'a str,
    pub name: &'a str,
    pub uuid: Uuid,
    /// From [`Created::backend_id`]; `None` if create never answered, in
    /// which case orchestrators look the instance up by name or UUID.
    pub backend_id: Option<&'a str>,
}

/// Whether an instance's process runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    Running,
    Stopped,
    /// The orchestrator doesn't know it.
    Missing,
}

/// An instance the orchestrator has, found by [`Orchestrator::list`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub node: String,
    /// The instance name it was created with.
    pub name: String,
    pub backend_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    /// The orchestrator refused: no such image, a bad spec, no room.
    #[error("refused: {0}")]
    Refused(String),
    /// Couldn't reach it, or it answered nonsense.
    #[error("unavailable: {0}")]
    Unavailable(String),
}

pub type OrchestratorFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, OrchestratorError>> + Send + 'a>>;

/// Creates, starts, stops and deletes instances on nodes.
pub trait Orchestrator: Send + Sync {
    /// For logs and the API: `wings`, `docker`, `mock`.
    fn name(&self) -> &'static str;
    /// Whether instances can go on any node name, not only those
    /// [`Orchestrator::nodes`] lists: Docker runs everything on one engine
    /// whatever the node, and the mock keeps it in memory. The fleet
    /// manager's dry-run nodes take instances only with such an
    /// orchestrator, so the dev network can play a fleet out.
    fn places_anywhere(&self) -> bool {
        false
    }
    /// The nodes instances can go on now (ready, not draining).
    fn nodes(&self) -> OrchestratorFuture<'_, Vec<NodeSlot>>;
    /// Creates an instance on `node`, starting it if `start`.
    fn create<'a>(
        &'a self,
        node: &'a str,
        spec: &'a InstanceSpec,
        start: bool,
    ) -> OrchestratorFuture<'a, Created>;
    fn start<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()>;
    /// Stops gracefully (the server unregisters and kicks its players).
    fn stop<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()>;
    /// Deletes, killing it if it still runs. Deleting a missing instance
    /// succeeds.
    fn delete<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()>;
    fn status<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, Runtime>;
    /// Every instance this network's control plane created, on every node,
    /// to find ones it lost track of.
    fn list(&self) -> OrchestratorFuture<'_, Vec<Listed>>;
}

impl fmt::Debug for dyn Orchestrator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Orchestrator({})", self.name())
    }
}
