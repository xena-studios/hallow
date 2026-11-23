//! The fleet manager's live state in Redis (`crate::fleet_manager`).
//!
//! | Key (after the prefix) | Type | Holds |
//! |---|---|---|
//! | `fleet-lock` | string with TTL | the control plane instance running the fleet manager |
//! | `fleet:memory` | string | what the planner carries between ticks (JSON) |
//! | `fleet:status` | string | the fleet as of the last tick (JSON) |
//! | `fleet:view` | string | each node's slots and whether it takes instances (JSON), for the instance lifecycle |

use hallow_proto::command::ProxyCommand;
use redis::{AsyncCommands, Script};
use uuid::Uuid;

use super::{Keys, Result, Store};

/// Takes `KEYS[1]` for `ARGV[1]` for `ARGV[2]` ms, or renews it if `ARGV[1]`
/// holds it already. Returns 1 if `ARGV[1]` holds it now.
const LOCK: &str = r"
local holder = redis.call('GET', KEYS[1])
if holder == false then
  redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
  return 1
end
if holder == ARGV[1] then
  redis.call('PEXPIRE', KEYS[1], ARGV[2])
  return 1
end
return 0
";

impl Keys {
    fn fleet_lock(&self) -> String {
        format!("{}:fleet-lock", self.prefix)
    }
    fn fleet(&self, name: &str) -> String {
        format!("{}:fleet:{name}", self.prefix)
    }
}

impl Store {
    /// Takes or renews the fleet manager's lock for `holder`. Only one
    /// control plane instance plans the fleet at a time.
    pub async fn lock_fleet(&self, holder: &str, ttl_ms: u64) -> Result<bool> {
        let held: i64 = Script::new(LOCK)
            .key(self.keys.fleet_lock())
            .arg(holder)
            .arg(ttl_ms.max(1))
            .invoke_async(&mut self.conn())
            .await?;
        Ok(held == 1)
    }

    /// One of the fleet manager's values: `memory`, `status` or `view`.
    pub async fn fleet_value(&self, name: &str) -> Result<Option<String>> {
        Ok(self.conn().get(self.keys.fleet(name)).await?)
    }

    pub async fn set_fleet_value(&self, name: &str, value: &str) -> Result<()> {
        let _: () = self.conn().set(self.keys.fleet(name), value).await?;
        Ok(())
    }

    /// Tells one player `text` through their proxy, if they're online.
    pub async fn send_to_proxy_of(&self, player: &Uuid, text: &str) -> Result<()> {
        if let Some(location) = self.player(player).await? {
            self.push_command(
                &location.proxy,
                &ProxyCommand::Message {
                    player: *player,
                    text: text.to_owned(),
                    from: None,
                },
            )
            .await?;
        }
        Ok(())
    }
}
