//! Instance records in Redis (docs/INSTANCES.md).
//!
//! | Key (after the prefix) | Type | Holds |
//! |---|---|---|
//! | `instances` | hash | instance name → [`InstanceInfo`] JSON |
//! | `instance-specs` | hash | instance name → [`InstanceSpec`] JSON: what it was created from, environment included, for Wings to fetch |
//! | `instance-tokens` | hash | SHA-256 (hex) of an instance's scoped token → [`ScopedToken`] JSON (name, kind) |
//! | `instance-token-of` | hash | instance name → that SHA-256, to revoke the token with the record |
//!
//! An instance token works only while its record exists: removing the
//! record removes the token in the same script, and a lookup also checks
//! the record is still there.
//!
//! Records change state with a compare-and-set on the state (Lua), so a
//! task finishing late can't overwrite a decision made meanwhile.

use std::collections::BTreeMap;

use hallow_proto::instance::{InstanceInfo, InstanceState};
use redis::{AsyncCommands, Script};
use uuid::Uuid;

use super::{Keys, Result, Store, now_ms};
use crate::{auth::ScopedToken, orchestrator::InstanceSpec};

impl Keys {
    fn instances(&self) -> String {
        format!("{}:instances", self.prefix)
    }
    fn instance_specs(&self) -> String {
        format!("{}:instance-specs", self.prefix)
    }
    fn instance_tokens(&self) -> String {
        format!("{}:instance-tokens", self.prefix)
    }
    fn instance_token_of(&self) -> String {
        format!("{}:instance-token-of", self.prefix)
    }
}

/// Replaces field `ARGV[1]` of hash `KEYS[1]` with `ARGV[3]` if its JSON
/// `state` is one of the comma-separated `ARGV[2]`. With `ARGV[3] == ""` it
/// deletes the field instead, the same field of `KEYS[2]` (the spec) and of
/// `KEYS[4]` (the token hash), and that hash's field of `KEYS[3]` (the
/// token). Returns 1 if changed.
const SET_IF_STATE: &str = r"
local v = redis.call('HGET', KEYS[1], ARGV[1])
if not v then return 0 end
local state = cjson.decode(v).state
local ok = false
for s in string.gmatch(ARGV[2], '[^,]+') do
  if s == state then ok = true end
end
if not ok then return 0 end
if ARGV[3] == '' then
  redis.call('HDEL', KEYS[1], ARGV[1])
  redis.call('HDEL', KEYS[2], ARGV[1])
  local h = redis.call('HGET', KEYS[4], ARGV[1])
  if h then redis.call('HDEL', KEYS[3], h) end
  redis.call('HDEL', KEYS[4], ARGV[1])
else
  redis.call('HSET', KEYS[1], ARGV[1], ARGV[3])
end
return 1
";

/// Adds instance `ARGV[1]` to `KEYS[1]` (record `ARGV[2]`) unless the name
/// is taken, with its spec `ARGV[3]` in `KEYS[2]` and its token: `ARGV[5]`
/// under hash `ARGV[4]` in `KEYS[3]`, and that hash under the name in
/// `KEYS[4]`. Returns 1 if added.
const ADD_INSTANCE: &str = r"
if redis.call('HSETNX', KEYS[1], ARGV[1], ARGV[2]) == 0 then return 0 end
redis.call('HSET', KEYS[2], ARGV[1], ARGV[3])
redis.call('HSET', KEYS[3], ARGV[4], ARGV[5])
redis.call('HSET', KEYS[4], ARGV[1], ARGV[4])
return 1
";

/// The token under hash `ARGV[1]` in `KEYS[1]`, if its instance is still in
/// `KEYS[2]`.
const INSTANCE_TOKEN: &str = r"
local v = redis.call('HGET', KEYS[1], ARGV[1])
if not v then return false end
if redis.call('HEXISTS', KEYS[2], cjson.decode(v).name) == 0 then return false end
return v
";

fn states(from: &[InstanceState]) -> String {
    from.iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

impl Store {
    fn set_if_state(&self) -> redis::ScriptInvocation<'static> {
        static SCRIPT: std::sync::OnceLock<Script> = std::sync::OnceLock::new();
        let mut invocation = SCRIPT
            .get_or_init(|| Script::new(SET_IF_STATE))
            .prepare_invoke();
        invocation
            .key(self.keys.instances())
            .key(self.keys.instance_specs())
            .key(self.keys.instance_tokens())
            .key(self.keys.instance_token_of());
        invocation
    }

    /// Every instance record, by name.
    pub async fn instances(&self) -> Result<Vec<InstanceInfo>> {
        let raw: Vec<String> = self.conn().hvals(self.keys.instances()).await?;
        let mut instances = raw
            .iter()
            .map(|raw| serde_json::from_str(raw))
            .collect::<std::result::Result<Vec<InstanceInfo>, _>>()?;
        instances.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(instances)
    }

    pub async fn instance(&self, name: &str) -> Result<Option<InstanceInfo>> {
        let raw: Option<String> = self.conn().hget(self.keys.instances(), name).await?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    pub async fn instance_by_uuid(&self, uuid: Uuid) -> Result<Option<InstanceInfo>> {
        Ok(self.instances().await?.into_iter().find(|i| i.uuid == uuid))
    }

    pub async fn instance_spec(&self, name: &str) -> Result<Option<InstanceSpec>> {
        let raw: Option<String> = self.conn().hget(self.keys.instance_specs(), name).await?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    /// Records a new instance, what it is made from, and the SHA-256 of its
    /// scoped token (`auth`). Returns false if the name is taken.
    pub async fn add_instance(
        &self,
        info: &InstanceInfo,
        spec: &InstanceSpec,
        token_hash: &str,
    ) -> Result<bool> {
        let token = ScopedToken {
            name: info.name.clone(),
            kind: info.kind.clone(),
            created_ms: info.created_ms,
        };
        let added: i64 = Script::new(ADD_INSTANCE)
            .key(self.keys.instances())
            .key(self.keys.instance_specs())
            .key(self.keys.instance_tokens())
            .key(self.keys.instance_token_of())
            .arg(&info.name)
            .arg(serde_json::to_string(info)?)
            .arg(serde_json::to_string(spec)?)
            .arg(token_hash)
            .arg(serde_json::to_string(&token)?)
            .invoke_async(&mut self.conn())
            .await?;
        Ok(added == 1)
    }

    /// The instance a scoped token's SHA-256 belongs to, while its record
    /// exists.
    pub async fn instance_token(&self, token_hash: &str) -> Result<Option<ScopedToken>> {
        let raw: Option<String> = Script::new(INSTANCE_TOKEN)
            .key(self.keys.instance_tokens())
            .key(self.keys.instances())
            .arg(token_hash)
            .invoke_async(&mut self.conn())
            .await?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    /// Applies `change` to an instance if it is in one of the states `from`.
    /// A change of state restarts its clock. Returns the new record, or
    /// `None` if the instance is gone or in another state.
    pub async fn update_instance(
        &self,
        name: &str,
        from: &[InstanceState],
        change: impl FnOnce(&mut InstanceInfo),
    ) -> Result<Option<InstanceInfo>> {
        let Some(mut info) = self.instance(name).await? else {
            return Ok(None);
        };
        if !from.contains(&info.state) {
            return Ok(None);
        }
        let before = info.state;
        change(&mut info);
        if info.state != before {
            info.state_since_ms = now_ms();
        }
        let changed: i64 = self
            .set_if_state()
            .arg(name)
            .arg(states(&[before]))
            .arg(serde_json::to_string(&info)?)
            .invoke_async(&mut self.conn())
            .await?;
        Ok((changed == 1).then_some(info))
    }

    /// Deletes an instance's record if it is in one of the states `from`.
    pub async fn remove_instance(&self, name: &str, from: &[InstanceState]) -> Result<bool> {
        let removed: i64 = self
            .set_if_state()
            .arg(name)
            .arg(states(from))
            .arg("")
            .invoke_async(&mut self.conn())
            .await?;
        Ok(removed == 1)
    }

    /// Names of instances the matchmaker must not send players to.
    pub(super) async fn closed_instances(&self) -> Result<Vec<String>> {
        Ok(self
            .instances()
            .await?
            .into_iter()
            .filter(|i| !i.state.is_live())
            .map(|i| i.name)
            .collect())
    }

    /// Each queued kind's entries' sizes, counting members still online,
    /// oldest first.
    pub async fn queued_entry_sizes(&self) -> Result<BTreeMap<String, Vec<u32>>> {
        let kinds = self.queue_entries().await?;
        let mut sizes = BTreeMap::new();
        for (kind, entries) in kinds {
            let mut list = Vec::with_capacity(entries.len());
            for members in entries {
                let mut online = 0u32;
                for member in &members {
                    if self.player(member).await?.is_some() {
                        online += 1;
                    }
                }
                list.push(online);
            }
            sizes.insert(kind, list);
        }
        Ok(sizes)
    }
}
