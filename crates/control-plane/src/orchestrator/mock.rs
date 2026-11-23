//! An in-memory orchestrator for tests: it records every call and keeps a
//! map of the instances it "runs". Nothing registers by itself; a test plays
//! the server by registering under the instance's name.

use std::{collections::BTreeMap, sync::Mutex};

use super::{
    Created, Handle, InstanceSpec, Listed, NodeSlot, Orchestrator, OrchestratorError,
    OrchestratorFuture, Runtime,
};

/// One call the mock saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Create { node: String, name: String },
    Start(String),
    Stop(String),
    Delete(String),
}

#[derive(Clone, Debug)]
pub struct MockInstance {
    pub node: String,
    pub spec: InstanceSpec,
    pub running: bool,
}

#[derive(Default)]
struct Inner {
    instances: BTreeMap<String, MockInstance>,
    calls: Vec<Call>,
    fail_creates: bool,
}

/// See the module docs.
pub struct MockOrchestrator {
    nodes: Vec<NodeSlot>,
    inner: Mutex<Inner>,
}

impl MockOrchestrator {
    /// A mock with the given nodes (names, no port ranges).
    #[must_use]
    pub fn new(nodes: &[&str]) -> Self {
        Self {
            nodes: nodes
                .iter()
                .map(|name| NodeSlot {
                    name: (*name).to_owned(),
                    ports: None,
                    slots: None,
                })
                .collect(),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Instances it runs now, by name.
    #[must_use]
    pub fn instances(&self) -> BTreeMap<String, MockInstance> {
        self.lock().instances.clone()
    }

    /// Every call so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Makes every create fail (or succeed again).
    pub fn fail_creates(&self, fail: bool) {
        self.lock().fail_creates = fail;
    }

    /// Adds an instance the control plane doesn't know about.
    pub fn add_stray(&self, node: &str, spec: InstanceSpec) {
        self.lock().instances.insert(
            spec.name.clone(),
            MockInstance {
                node: node.to_owned(),
                spec,
                running: true,
            },
        );
    }
}

impl Orchestrator for MockOrchestrator {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn places_anywhere(&self) -> bool {
        true
    }

    fn nodes(&self) -> OrchestratorFuture<'_, Vec<NodeSlot>> {
        Box::pin(async move { Ok(self.nodes.clone()) })
    }

    fn create<'a>(
        &'a self,
        node: &'a str,
        spec: &'a InstanceSpec,
        start: bool,
    ) -> OrchestratorFuture<'a, Created> {
        Box::pin(async move {
            let mut inner = self.lock();
            inner.calls.push(Call::Create {
                node: node.to_owned(),
                name: spec.name.clone(),
            });
            if inner.fail_creates {
                return Err(OrchestratorError::Refused("creates are failing".into()));
            }
            inner.instances.insert(
                spec.name.clone(),
                MockInstance {
                    node: node.to_owned(),
                    spec: spec.clone(),
                    running: start,
                },
            );
            Ok(Created {
                backend_id: format!("mock-{}", spec.name),
                address: format!("{}:25565", spec.name),
            })
        })
    }

    fn start<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = self.lock();
            inner.calls.push(Call::Start(instance.name.to_owned()));
            match inner.instances.get_mut(instance.name) {
                Some(found) => {
                    found.running = true;
                    Ok(())
                }
                None => Err(OrchestratorError::Refused("no such instance".into())),
            }
        })
    }

    fn stop<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = self.lock();
            inner.calls.push(Call::Stop(instance.name.to_owned()));
            if let Some(found) = inner.instances.get_mut(instance.name) {
                found.running = false;
            }
            Ok(())
        })
    }

    fn delete<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = self.lock();
            inner.calls.push(Call::Delete(instance.name.to_owned()));
            inner.instances.remove(instance.name);
            Ok(())
        })
    }

    fn status<'a>(&'a self, instance: Handle<'a>) -> OrchestratorFuture<'a, Runtime> {
        Box::pin(async move {
            Ok(match self.lock().instances.get(instance.name) {
                Some(found) if found.running => Runtime::Running,
                Some(_) => Runtime::Stopped,
                None => Runtime::Missing,
            })
        })
    }

    fn list(&self) -> OrchestratorFuture<'_, Vec<Listed>> {
        Box::pin(async move {
            Ok(self
                .lock()
                .instances
                .values()
                .map(|found| Listed {
                    node: found.node.clone(),
                    name: found.spec.name.clone(),
                    backend_id: format!("mock-{}", found.spec.name),
                })
                .collect())
        })
    }
}
