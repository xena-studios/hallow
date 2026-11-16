//! Hallow's fleet manager core (see `docs/FLEET.md`):
//!
//! - [`provider`]: the trait every machine provider implements (`create`,
//!   `status`, `cancel`, `list`, `prices`) and the types it speaks;
//! - [`planner`]: a pure function from a fleet snapshot and a policy to
//!   buy, drain, cancel and hold decisions;
//! - [`dry_run`]: a provider wrapper that logs what would happen and never
//!   calls the real provider;
//! - [`settings`]: the planner's [`Policy`] from the fleet settings the panel
//!   edits, and which offers they allow.
//! - [`bootstrap`]: the cloud-init user data that sets a new machine up as a
//!   node and joins it to the network.
//!
//! The control plane will run the loop: build a snapshot, call
//! [`planner::plan`], carry out the decisions through a provider, and record
//! each one in the audit log.

pub mod bootstrap;
pub mod dry_run;
pub mod money;
pub mod planner;
pub mod provider;
pub mod settings;

pub use bootstrap::{
    BootstrapError, NodeBootstrap, WingsBuild, WireGuard, WireGuardPeer, render_cloud_init,
};
pub use dry_run::{DryRun, DryRunAction};
pub use money::Money;
pub use planner::{
    Decision, Memory, NodeLoad, NodePhase, Offer, Plan, Policy, QueueWait, Reason, Snapshot, Spend,
    plan,
};
pub use provider::{
    BillingStep, Cost, Node, NodeId, NodeKind, NodeSpec, Price, Prices, Provider, ProviderError,
    ProviderState,
};
pub use settings::{InvalidSettings, allowed_offers, floor_nodes};
