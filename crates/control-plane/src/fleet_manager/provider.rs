//! Providers behind one boxed type, so the fleet manager can hold the real
//! provider and dry run side by side and switch per call.

use std::{future::Future, pin::Pin, sync::Arc};

use hallow_fleet::{Node, NodeId, NodeKind, NodeSpec, Prices, Provider, ProviderError};

type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

/// [`Provider`] with boxed futures, so it can be a trait object.
pub trait DynProvider: Send + Sync {
    fn name(&self) -> &str;
    fn kind(&self) -> NodeKind;
    fn create<'a>(&'a self, spec: &'a NodeSpec) -> ProviderFuture<'a, Node>;
    fn status<'a>(&'a self, id: &'a NodeId) -> ProviderFuture<'a, Node>;
    fn cancel<'a>(&'a self, id: &'a NodeId) -> ProviderFuture<'a, ()>;
    fn list(&self) -> ProviderFuture<'_, Vec<Node>>;
    fn prices(&self) -> ProviderFuture<'_, Prices>;
}

impl<P: Provider> DynProvider for P {
    fn name(&self) -> &str {
        Provider::name(self)
    }
    fn kind(&self) -> NodeKind {
        Provider::kind(self)
    }
    fn create<'a>(&'a self, spec: &'a NodeSpec) -> ProviderFuture<'a, Node> {
        Box::pin(Provider::create(self, spec))
    }
    fn status<'a>(&'a self, id: &'a NodeId) -> ProviderFuture<'a, Node> {
        Box::pin(Provider::status(self, id))
    }
    fn cancel<'a>(&'a self, id: &'a NodeId) -> ProviderFuture<'a, ()> {
        Box::pin(Provider::cancel(self, id))
    }
    fn list(&self) -> ProviderFuture<'_, Vec<Node>> {
        Box::pin(Provider::list(self))
    }
    fn prices(&self) -> ProviderFuture<'_, Prices> {
        Box::pin(Provider::prices(self))
    }
}

/// A shared [`DynProvider`] that is a [`Provider`] again, so it can go in
/// [`hallow_fleet::DryRun`].
#[derive(Clone)]
pub struct Shared(pub Arc<dyn DynProvider>);

impl Provider for Shared {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn kind(&self) -> NodeKind {
        self.0.kind()
    }
    async fn create(&self, spec: &NodeSpec) -> Result<Node, ProviderError> {
        self.0.create(spec).await
    }
    async fn status(&self, id: &NodeId) -> Result<Node, ProviderError> {
        self.0.status(id).await
    }
    async fn cancel(&self, id: &NodeId) -> Result<(), ProviderError> {
        self.0.cancel(id).await
    }
    async fn list(&self) -> Result<Vec<Node>, ProviderError> {
        self.0.list().await
    }
    async fn prices(&self) -> Result<Prices, ProviderError> {
        self.0.prices().await
    }
}

/// The provider when no credentials are set: every call fails, so turning
/// dry run off without them buys nothing.
pub struct Unconfigured {
    pub name: String,
}

impl Unconfigured {
    fn refuse<T>(&self) -> Result<T, ProviderError> {
        Err(ProviderError::Unauthorized)
    }
}

impl Provider for Unconfigured {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Burst
    }
    async fn create(&self, _: &NodeSpec) -> Result<Node, ProviderError> {
        self.refuse()
    }
    async fn status(&self, _: &NodeId) -> Result<Node, ProviderError> {
        self.refuse()
    }
    async fn cancel(&self, _: &NodeId) -> Result<(), ProviderError> {
        self.refuse()
    }
    async fn list(&self) -> Result<Vec<Node>, ProviderError> {
        self.refuse()
    }
    async fn prices(&self) -> Result<Prices, ProviderError> {
        self.refuse()
    }
}
