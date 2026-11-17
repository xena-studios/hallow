//! The combat seam. Matches use Pumpkin's own (current, 1.9+) combat:
//! attack cooldown, sweeping, shields. Whether Hallow adds 1.8-style combat
//! (#28) is undecided; if it does, it plugs in here: [`Combat::setup`] runs
//! for every player when the match gives them their kit, and
//! [`Combat::damage`] sees every player-on-player hit before it lands, so a
//! 1.8 module could drop the cooldown (attack speed attribute) and rescale
//! damage and knockback without the games changing.

use pumpkin_plugin_api::Player;

/// Which rules a match fights by. Only [`Combat::Modern`] exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Combat {
    /// Pumpkin's combat, unchanged.
    #[default]
    Modern,
}

impl Combat {
    /// The rules named by `HALLOW_COMBAT` (`modern`, the default).
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var("HALLOW_COMBAT").as_deref().map(str::trim) {
            Ok("modern") | Err(_) => Self::Modern,
            Ok(other) => {
                tracing::warn!("islands: unknown HALLOW_COMBAT {other}; using modern combat");
                Self::Modern
            }
        }
    }

    /// Prepares a player for fighting, after their kit.
    pub const fn setup(self, _player: &Player) {
        match self {
            Self::Modern => {}
        }
    }

    /// A player hits another for `damage`; returns the damage to deal.
    #[must_use]
    pub const fn damage(self, damage: f32) -> f32 {
        match self {
            Self::Modern => damage,
        }
    }
}
