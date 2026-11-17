//! The instance lifecycle (docs/INSTANCES.md): keeps warm instances of each
//! registered kind, starts more when the queue needs them, and deletes game
//! instances when their game is over, through an [`Orchestrator`].
//!
//! Every half second, under the matchmaker's Redis lock, [`Manager::plan`]
//! reads the kinds, instance records, server list and queues, runs the pure
//! planner ([`plan::plan`]), and makes its quick decisions in Redis: new
//! records in `creating`, `starting` → `ready`, bookkeeping, and `ending`
//! (from then on the matchmaker skips the instance). The matchmaker pass
//! follows under the same lock, so it never places anyone on an instance
//! that's ending, and a placement it makes is seen by the next plan. The
//! slow orchestrator calls (create, stop, delete) then run on their own
//! tasks, outside the lock, and move the records on when they finish.

pub mod plan;

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use hallow_db::PgPool;
use hallow_proto::instance::{InstanceInfo, InstanceState, KindRole, KindSpec};
use uuid::Uuid;

use crate::{
    AppState,
    orchestrator::{Handle, InstanceSpec, NodeSlot, Orchestrator, wings::WingsOrchestrator},
    store::{Store, StoreError},
};

pub use plan::Settings;

/// How long kinds and nodes are cached between reads.
const CACHE_FOR: Duration = Duration::from_secs(2);
/// How long an ending instance waits for the players it sent to a lobby to
/// leave before it is stopped.
const EVACUATE_WAIT: Duration = Duration::from_secs(5);
/// Memory per instance unless the kind says.
pub const DEFAULT_MEMORY_MB: u32 = 1024;

/// What every instance gets.
#[derive(Clone, Debug)]
pub struct InstancesConfig {
    pub settings: Settings,
    /// `host:port` instances reach the control plane at
    /// (`HALLOW_CONTROL_PLANE`). Each instance gets its own scoped token
    /// for it, never the shared one (`auth`).
    pub control_plane: String,
    /// Environment for every instance, such as `HALLOW_VINE_SECRET`.
    pub env: BTreeMap<String, String>,
}

/// One slow piece of work the plan left for a task.
#[derive(Debug)]
pub enum Work {
    Create { name: String },
    End { name: String },
}

struct Cached<T> {
    at: Option<Instant>,
    value: T,
}

impl<T: Default> Default for Cached<T> {
    fn default() -> Self {
        Self {
            at: None,
            value: T::default(),
        }
    }
}

impl<T> Cached<T> {
    fn fresh(&self) -> bool {
        self.at.is_some_and(|at| at.elapsed() < CACHE_FOR)
    }
}

/// Runs instances of registered kinds. See the module docs.
pub struct Manager {
    orchestrator: Arc<dyn Orchestrator>,
    /// The same orchestrator when it is Wings, for the remote API.
    wings: Option<Arc<WingsOrchestrator>>,
    db: PgPool,
    config: InstancesConfig,
    kinds: RwLock<Cached<BTreeMap<String, KindSpec>>>,
    nodes: RwLock<Cached<Vec<NodeSlot>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("database: {0}")]
    Database(#[from] hallow_db::sqlx::Error),
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

impl Manager {
    #[must_use]
    pub fn new(
        orchestrator: Arc<dyn Orchestrator>,
        wings: Option<Arc<WingsOrchestrator>>,
        db: PgPool,
        config: InstancesConfig,
    ) -> Self {
        Self {
            orchestrator,
            wings,
            db,
            config,
            kinds: RwLock::default(),
            nodes: RwLock::default(),
        }
    }

    #[must_use]
    pub fn orchestrator(&self) -> &dyn Orchestrator {
        &*self.orchestrator
    }

    #[must_use]
    pub fn wings(&self) -> Option<&WingsOrchestrator> {
        self.wings.as_deref()
    }

    /// Forgets the cached kinds, after one changed.
    pub fn kinds_changed(&self) {
        self.kinds
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .at = None;
    }

    /// Whether `kind` is a registered game. Reads the database when the
    /// cache is stale or a kind changed since, so a kind registered a moment
    /// ago can be queued for before the next plan; the cache if the
    /// database can't be read.
    pub async fn is_game_kind(&self, kind: &str) -> bool {
        self.kind(kind)
            .await
            .is_some_and(|k| k.role == KindRole::Game)
    }

    /// A registered kind's spec, read like [`Manager::is_game_kind`].
    pub async fn kind(&self, kind: &str) -> Option<KindSpec> {
        let kinds = match self.kinds().await {
            Ok(kinds) => kinds,
            Err(err) => {
                tracing::warn!(%err, "can't read the kinds; using the cached ones");
                self.kinds
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .value
                    .clone()
            }
        };
        kinds.get(kind).cloned()
    }

    async fn kinds(&self) -> Result<BTreeMap<String, KindSpec>, PlanError> {
        {
            let cached = self
                .kinds
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cached.fresh() {
                return Ok(cached.value.clone());
            }
        }
        let kinds: BTreeMap<String, KindSpec> = hallow_db::list_kinds(&self.db)
            .await?
            .into_iter()
            .map(|k| (k.name, k.spec.0))
            .collect();
        *self
            .kinds
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Cached {
            at: Some(Instant::now()),
            value: kinds.clone(),
        };
        Ok(kinds)
    }

    async fn nodes(&self) -> Vec<NodeSlot> {
        {
            let cached = self
                .nodes
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cached.fresh() {
                return cached.value.clone();
            }
        }
        let nodes = match self.orchestrator.nodes().await {
            Ok(nodes) => nodes,
            Err(err) => {
                tracing::warn!(%err, "can't list nodes; starting nothing");
                Vec::new()
            }
        };
        *self
            .nodes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Cached {
            at: Some(Instant::now()),
            value: nodes.clone(),
        };
        nodes
    }

    /// Plans and makes the quick decisions; see the module docs. The caller
    /// holds the matchmaker lock and runs the returned work with
    /// [`Manager::run`] once it has let go.
    pub async fn plan(&self, store: &Store) -> Result<Vec<Work>, PlanError> {
        let kinds = self.kinds().await?;
        let instances = store.instances().await?;
        let (_, servers) = store.read_servers().await?;
        let queued = store.queued_entry_sizes().await?;
        let players_online = store.online().await?;
        let nodes = self.nodes().await;
        let actions = plan::plan(&plan::World {
            now_ms: now_ms(),
            kinds: &kinds,
            instances: &instances,
            servers: &servers,
            queued: &queued,
            players_online,
            nodes: &nodes,
            settings: &self.config.settings,
        });
        let mut work = Vec::new();
        for action in actions {
            match action {
                plan::Action::Create { kind, node, port } => {
                    let Some(spec) = kinds.get(&kind) else {
                        continue;
                    };
                    if let Some(name) = self.add(store, &kind, spec, &node, port).await? {
                        work.push(Work::Create { name });
                    }
                }
                plan::Action::Ready { name } => {
                    if store
                        .update_instance(&name, &[InstanceState::Starting], |i| {
                            i.state = InstanceState::Ready;
                        })
                        .await?
                        .is_some()
                    {
                        tracing::info!(instance = %name, "instance ready");
                    }
                }
                plan::Action::Track {
                    name,
                    had_players,
                    empty_since_ms,
                } => {
                    store
                        .update_instance(&name, &[InstanceState::Ready], |i| {
                            i.had_players = had_players;
                            i.empty_since_ms = empty_since_ms;
                        })
                        .await?;
                }
                plan::Action::End { name, reason } => {
                    if self.mark_ending(store, &name, &reason).await? {
                        work.push(Work::End { name });
                    }
                }
            }
        }
        Ok(work)
    }

    /// Records a new instance of `kind`; returns its name.
    async fn add(
        &self,
        store: &Store,
        kind: &str,
        spec: &KindSpec,
        node: &str,
        port: Option<u16>,
    ) -> Result<Option<String>, PlanError> {
        for _ in 0..4 {
            let token = match crate::auth::new_instance_token() {
                Ok(token) => token,
                Err(err) => {
                    tracing::error!(%err, "no randomness for an instance token");
                    return Ok(None);
                }
            };
            let uuid = Uuid::new_v4();
            let name = format!("{kind}-{}", &uuid.simple().to_string()[..6]);
            let now = now_ms();
            let info = InstanceInfo {
                name: name.clone(),
                kind: kind.to_owned(),
                uuid,
                image: spec.image.clone(),
                node: node.to_owned(),
                state: InstanceState::Creating,
                state_since_ms: now,
                created_ms: now,
                address: None,
                port,
                backend_id: None,
                players: 0,
                had_players: false,
                empty_since_ms: None,
                reason: None,
            };
            let instance_spec = InstanceSpec {
                name: name.clone(),
                uuid,
                kind: kind.to_owned(),
                image: spec.image.clone(),
                env: self.env(&name, spec, &token),
                port,
                memory_mb: spec.memory_mb.unwrap_or(DEFAULT_MEMORY_MB),
            };
            let token_hash = crate::auth::token_hash(&token);
            if store
                .add_instance(&info, &instance_spec, &token_hash)
                .await?
            {
                tracing::info!(instance = %name, %kind, %node, "creating instance");
                return Ok(Some(name));
            }
        }
        Ok(None)
    }

    /// The instance's environment: the server plugin's settings, with the
    /// instance's own scoped token, then the network's and the kind's extras.
    fn env(&self, name: &str, spec: &KindSpec, token: &str) -> BTreeMap<String, String> {
        let mut env = self.config.env.clone();
        env.extend(spec.env.clone());
        env.insert("HALLOW_SERVER_NAME".into(), name.to_owned());
        env.insert(
            "HALLOW_CONTROL_PLANE".into(),
            self.config.control_plane.clone(),
        );
        env.insert("HALLOW_CONTROL_PLANE_TOKEN".into(), token.to_owned());
        env
    }

    async fn mark_ending(
        &self,
        store: &Store,
        name: &str,
        reason: &str,
    ) -> Result<bool, PlanError> {
        let any = [
            InstanceState::Creating,
            InstanceState::Starting,
            InstanceState::Ready,
            InstanceState::Ending,
            InstanceState::Deleting,
            InstanceState::Failed,
        ];
        let marked = store
            .update_instance(name, &any, |i| {
                i.state = InstanceState::Ending;
                i.reason = Some(reason.to_owned());
            })
            .await?
            .is_some();
        if marked {
            tracing::info!(instance = %name, %reason, "ending instance");
        }
        Ok(marked)
    }

    /// Ends an instance now (staff asked). Returns false if there is none.
    pub async fn end_now(
        self: &Arc<Self>,
        state: &Arc<AppState>,
        name: &str,
    ) -> Result<bool, PlanError> {
        if !self
            .mark_ending(&state.store, name, "ended by staff")
            .await?
        {
            return Ok(false);
        }
        self.run(
            state,
            vec![Work::End {
                name: name.to_owned(),
            }],
        );
        Ok(true)
    }

    /// Runs the plan's slow work on its own tasks.
    pub fn run(self: &Arc<Self>, state: &Arc<AppState>, work: Vec<Work>) {
        for item in work {
            let manager = self.clone();
            let state = state.clone();
            tokio::spawn(async move {
                let store = &state.store;
                let result = match &item {
                    Work::Create { name } => manager.create(store, name).await,
                    Work::End { name } => manager.end(store, name, &state.config.hub_kind).await,
                };
                if let Err(err) = result {
                    tracing::warn!(%err, ?item, "instance work failed");
                }
            });
        }
    }

    async fn create(&self, store: &Store, name: &str) -> Result<(), StoreError> {
        let (Some(info), Some(spec)) = (
            store.instance(name).await?,
            store.instance_spec(name).await?,
        ) else {
            return Ok(());
        };
        match self.orchestrator.create(&info.node, &spec, true).await {
            Ok(created) => {
                let moved = store
                    .update_instance(name, &[InstanceState::Creating], |i| {
                        i.state = InstanceState::Starting;
                        i.backend_id = Some(created.backend_id.clone());
                        i.address = Some(created.address.clone());
                    })
                    .await?;
                if moved.is_some() {
                    tracing::info!(instance = %name, address = %created.address, "instance started");
                } else {
                    // Ended while we were creating it.
                    let handle = Handle {
                        node: &info.node,
                        name,
                        uuid: info.uuid,
                        backend_id: Some(&created.backend_id),
                    };
                    if let Err(err) = self.orchestrator.delete(handle).await {
                        tracing::warn!(instance = %name, %err, "couldn't delete it");
                    }
                }
            }
            Err(err) => {
                tracing::warn!(instance = %name, %err, "couldn't create instance");
                store
                    .update_instance(name, &[InstanceState::Creating], |i| {
                        i.state = InstanceState::Failed;
                        i.reason = Some(err.to_string());
                    })
                    .await?;
            }
        }
        Ok(())
    }

    async fn end(&self, store: &Store, name: &str, hub: &str) -> Result<(), StoreError> {
        let Some(info) = store.instance(name).await? else {
            return Ok(());
        };
        if info.state != InstanceState::Ending {
            return Ok(());
        }
        // Anyone still on it goes to its kind's lobby first, so the stop
        // drops nobody.
        let lobby = self
            .kinds()
            .await
            .ok()
            .and_then(|kinds| kinds.get(&info.kind).and_then(|k| k.lobby.clone()));
        if let Err(err) =
            crate::lobbies::evacuate(store, name, lobby.as_deref(), hub, EVACUATE_WAIT).await
        {
            tracing::warn!(instance = %name, %err, "couldn't send its players to a lobby");
        }
        let handle = Handle {
            node: &info.node,
            name,
            uuid: info.uuid,
            backend_id: info.backend_id.as_deref(),
        };
        if let Err(err) = self.orchestrator.stop(handle).await {
            tracing::warn!(instance = %name, %err, "couldn't stop it; deleting anyway");
        }
        if store
            .update_instance(name, &[InstanceState::Ending], |i| {
                i.state = InstanceState::Deleting;
            })
            .await?
            .is_none()
        {
            return Ok(());
        }
        match self.orchestrator.delete(handle).await {
            Ok(()) => {
                store
                    .remove_instance(name, &[InstanceState::Deleting])
                    .await?;
                // Proxies drop it now rather than when its heartbeat lapses.
                store.remove_server(name).await?;
                tracing::info!(instance = %name, reason = ?info.reason, "instance deleted");
            }
            Err(err) => {
                tracing::warn!(instance = %name, %err, "couldn't delete instance");
                store
                    .update_instance(name, &[InstanceState::Deleting], |i| {
                        i.state = InstanceState::Failed;
                        i.reason = Some(err.to_string());
                    })
                    .await?;
            }
        }
        Ok(())
    }

    /// Deletes instances the orchestrator runs for this network that have
    /// no record (left by a control plane that lost its Redis, say).
    pub async fn sweep(&self, store: &Store) -> Result<Vec<String>, StoreError> {
        let listed = match self.orchestrator.list().await {
            Ok(listed) => listed,
            Err(err) => {
                tracing::warn!(%err, "can't list instances to sweep");
                return Ok(Vec::new());
            }
        };
        let known: std::collections::BTreeSet<String> = store
            .instances()
            .await?
            .into_iter()
            .map(|i| i.name)
            .collect();
        let mut swept = Vec::new();
        for stray in listed {
            if known.contains(&stray.name) {
                continue;
            }
            let handle = Handle {
                node: &stray.node,
                name: &stray.name,
                uuid: Uuid::nil(),
                backend_id: Some(&stray.backend_id),
            };
            match self.orchestrator.delete(handle).await {
                Ok(()) => {
                    tracing::info!(instance = %stray.name, node = %stray.node, "deleted a stray instance");
                    swept.push(stray.name);
                }
                Err(err) => tracing::warn!(instance = %stray.name, %err, "couldn't delete a stray"),
            }
        }
        Ok(swept)
    }

    /// Instance records with their servers' player counts, for the API.
    pub async fn list(&self, store: &Store) -> Result<Vec<InstanceInfo>, StoreError> {
        let mut instances = store.instances().await?;
        let (_, servers) = store.read_servers().await?;
        for instance in &mut instances {
            instance.players = servers
                .iter()
                .find(|s| s.name == instance.name)
                .map_or(0, |s| s.players);
        }
        Ok(instances)
    }
}

/// A kinds file entry: an image and the `hallow.toml` it was built from.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileKind {
    image: String,
    /// Path to the kind's `hallow.toml`, relative to the kinds file.
    manifest: String,
    #[serde(default)]
    memory_mb: Option<u32>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KindsFile {
    #[serde(default)]
    kind: Vec<FileKind>,
}

/// Reads a kinds file (docs/INSTANCES.md) into kind specs by name.
/// `${NAME}` and `${NAME:-default}` in images come from the environment.
pub fn read_kinds_file(path: &Path) -> Result<Vec<(String, KindSpec)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("can't read {}: {err}", path.display()))?;
    let file: KindsFile =
        toml::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut kinds = Vec::new();
    for entry in file.kind {
        let manifest_path = dir.join(&entry.manifest);
        let manifest = hallow_manifest::Manifest::load(&manifest_path)
            .map_err(|err| format!("{}: {err}", manifest_path.display()))?;
        let spec = KindSpec {
            image: expand_env(&entry.image, |name| std::env::var(name).ok()),
            role: match manifest.role {
                hallow_manifest::Role::Hub => KindRole::Hub,
                hallow_manifest::Role::Lobby => KindRole::Lobby,
                hallow_manifest::Role::Game => KindRole::Game,
            },
            lobby: manifest.lobby.clone(),
            players: hallow_proto::instance::KindPlayers {
                min: manifest.players.min,
                max: manifest.players.max,
            },
            scaling: hallow_proto::instance::KindScaling {
                warm: manifest.scaling.warm,
                max_instances: manifest.scaling.max_instances,
            },
            // The image applies hallow.toml's [env] itself.
            env: BTreeMap::new(),
            memory_mb: entry.memory_mb,
            updated_by: Some("kinds-file".into()),
            enabled: true,
        };
        kinds.push((manifest.name, spec));
    }
    Ok(kinds)
}

/// Replaces `${NAME}` and `${NAME:-default}`.
fn expand_env(text: &str, var: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &rest[start + 2..start + end];
        let (name, default) = inner.split_once(":-").map_or((inner, ""), |(n, d)| (n, d));
        out.push_str(
            &var(name)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_owned()),
        );
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_take_variables() {
        let var = |name: &str| (name == "TAG").then(|| "gatetest".to_owned());
        assert_eq!(
            expand_env("hallow-test-game:${TAG:-dev}", var),
            "hallow-test-game:gatetest"
        );
        assert_eq!(expand_env("hallow-hub:${NOPE:-dev}", var), "hallow-hub:dev");
        assert_eq!(expand_env("x${NOPE}y", var), "xy");
        assert_eq!(expand_env("plain:1.0", var), "plain:1.0");
        assert_eq!(expand_env("broken:${TAG", var), "broken:${TAG");
    }

    #[test]
    fn the_kinds_file_reads_hallow_toml() {
        let dir = std::env::temp_dir().join(format!("hallow-kinds-{}", Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("tg")).unwrap();
        std::fs::write(
            dir.join("tg/hallow.toml"),
            "name = \"test-game\"\nrole = \"game\"\nlobby = \"hub\"\nbase = \"^0.1\"\n\
             [players]\nmin = 2\nmax = 16\n[scaling]\nwarm = 1\nmax_instances = 20\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("kinds.toml"),
            "[[kind]]\nimage = \"hallow-test-game:dev\"\nmanifest = \"tg/hallow.toml\"\n",
        )
        .unwrap();
        let kinds = read_kinds_file(&dir.join("kinds.toml")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(kinds.len(), 1);
        let (name, spec) = &kinds[0];
        assert_eq!(name, "test-game");
        assert_eq!(spec.role, KindRole::Game);
        assert_eq!(spec.players.max, 16);
        assert_eq!(spec.scaling.warm, 1);
        assert_eq!(spec.scaling.max_instances, Some(20));
        assert_eq!(spec.lobby.as_deref(), Some("hub"));
    }
}
