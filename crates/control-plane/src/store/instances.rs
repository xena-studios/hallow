//! Instance records in Redis (docs/INSTANCES.md).
//!
//! | Key (after the prefix) | Type | Holds |
//! |---|---|---|
//! | `instances` | hash | instance name → [`InstanceInfo`] JSON |
//! | `instance-specs` | hash | instance name → [`InstanceSpec`] JSON: what it was created from, environment included, for Wings to fetch |
//!
//! Records change state with a compare-and-set on the state (Lua), so a
//! task finishing late can't overwrite a decision made meanwhile.

use std::collections::BTreeMap;

use hallow_proto::instance::{InstanceInfo, InstanceState};
use redis::{AsyncCommands, Script};
use uuid::Uuid;

use super::{Keys, Result, Store, now_ms};
use crate::orchestrator::InstanceSpec;

impl Keys {
    fn instances(&self) -> String {
        format!("{}:instances", self.prefix)
    }
    fn instance_specs(&self) -> String {
        format!("{}:instance-specs", self.prefix)
    }
}

/// Replaces field `ARGV[1]` of hash `KEYS[1]` with `ARGV[3]` if its JSON
/// `state` is one of the comma-separated `ARGV[2]`. With `ARGV[3] == ""` it
/// deletes the field instead, and the same field of `KEYS[2]`. Returns 1 if
/// changed.
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
else
  redis.call('HSET', KEYS[1], ARGV[1], ARGV[3])
end
return 1
";

fn states(from: &[InstanceState]) -> String {
    from.iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

impl Store {
    fn set_if_state_script() -> Script {
        Script::new(SET_IF_STATE)
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

    /// Records a new instance and what it is made from. Returns false if
    /// the name is taken.
    pub async fn add_instance(&self, info: &InstanceInfo, spec: &InstanceSpec) -> Result<bool> {
        let added: bool = self
            .conn()
            .hset_nx(
                self.keys.instances(),
                &info.name,
                serde_json::to_string(info)?,
            )
            .await?;
        if added {
            let _: () = self
                .conn()
                .hset(
                    self.keys.instance_specs(),
                    &info.name,
                    serde_json::to_string(spec)?,
                )
                .await?;
        }
        Ok(added)
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
        let changed: i64 = Self::set_if_state_script()
            .key(self.keys.instances())
            .key(self.keys.instance_specs())
            .arg(name)
            .arg(states(&[before]))
            .arg(serde_json::to_string(&info)?)
            .invoke_async(&mut self.conn())
            .await?;
        Ok((changed == 1).then_some(info))
    }

    /// Deletes an instance's record if it is in one of the states `from`.
    pub async fn remove_instance(&self, name: &str, from: &[InstanceState]) -> Result<bool> {
        let removed: i64 = Self::set_if_state_script()
            .key(self.keys.instances())
            .key(self.keys.instance_specs())
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
