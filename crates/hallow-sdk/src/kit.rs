//! Kits: what a player starts the match with.
//!
//! A game returns a kit for each player from
//! [`Minigame::kit`](crate::Minigame::kit), often by team. The SDK clears the
//! player's inventory and gives it when the match starts.
//!
//! ```rust,ignore
//! use hallow_sdk::Kit;
//!
//! let kit = Kit::new()
//!     .item(0, "minecraft:stone_sword", 1)
//!     .item(1, "minecraft:red_wool", 32)
//!     .named(8, "minecraft:compass", "§eTracker")
//!     .chestplate("minecraft:leather_chestplate");
//! ```
//!
//! Item keys are registry names (`minecraft:stone_sword`). Slots 0 to 8 are
//! the hotbar, 9 to 35 the rest of the inventory.

use pumpkin_plugin_api::{ItemStack, Player, text::TextComponent};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Entry {
    key: String,
    count: u8,
    name: Option<String>,
}

impl Entry {
    fn stack(&self) -> ItemStack {
        let stack = ItemStack::new(&self.key, self.count);
        if let Some(name) = &self.name {
            stack.set_custom_name(Some(TextComponent::from_legacy_string(name)));
        }
        stack
    }
}

/// Items for the inventory slots and armour.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kit {
    items: Vec<(u8, Entry)>,
    helmet: Option<Entry>,
    chestplate: Option<Entry>,
    leggings: Option<Entry>,
    boots: Option<Entry>,
    off_hand: Option<Entry>,
}

fn entry(key: &str, count: u8) -> Entry {
    Entry {
        key: key.to_owned(),
        count,
        name: None,
    }
}

impl Kit {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `count` of `key` in inventory slot `slot`.
    #[must_use]
    pub fn item(mut self, slot: u8, key: &str, count: u8) -> Self {
        self.items.push((slot, entry(key, count)));
        self
    }

    /// One `key` named `name` (legacy `§` codes allowed) in `slot`.
    #[must_use]
    pub fn named(mut self, slot: u8, key: &str, name: &str) -> Self {
        self.items.push((
            slot,
            Entry {
                name: Some(name.to_owned()),
                ..entry(key, 1)
            },
        ));
        self
    }

    #[must_use]
    pub fn helmet(mut self, key: &str) -> Self {
        self.helmet = Some(entry(key, 1));
        self
    }

    #[must_use]
    pub fn chestplate(mut self, key: &str) -> Self {
        self.chestplate = Some(entry(key, 1));
        self
    }

    #[must_use]
    pub fn leggings(mut self, key: &str) -> Self {
        self.leggings = Some(entry(key, 1));
        self
    }

    #[must_use]
    pub fn boots(mut self, key: &str) -> Self {
        self.boots = Some(entry(key, 1));
        self
    }

    #[must_use]
    pub fn off_hand(mut self, key: &str, count: u8) -> Self {
        self.off_hand = Some(entry(key, count));
        self
    }

    /// How many slots the kit fills, armour included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
            + [
                &self.helmet,
                &self.chestplate,
                &self.leggings,
                &self.boots,
                &self.off_hand,
            ]
            .iter()
            .filter(|slot| slot.is_some())
            .count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clears `player`'s inventory and armour and gives them the kit.
    pub fn give(&self, player: &Player) {
        let inventory = player.get_inventory();
        inventory.clear_all();
        for (slot, item) in &self.items {
            player.set_inventory_item(*slot, Some(item.stack()));
        }
        let stack = |entry: &Option<Entry>| entry.as_ref().map(Entry::stack);
        if self.helmet.is_some() {
            inventory.set_helmet(stack(&self.helmet));
        }
        if self.chestplate.is_some() {
            inventory.set_chestplate(stack(&self.chestplate));
        }
        if self.leggings.is_some() {
            inventory.set_leggings(stack(&self.leggings));
        }
        if self.boots.is_some() {
            inventory.set_boots(stack(&self.boots));
        }
        if self.off_hand.is_some() {
            inventory.set_off_hand(stack(&self.off_hand));
        }
        inventory.set_selected_slot(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds() {
        let kit = Kit::new()
            .item(0, "minecraft:stone_sword", 1)
            .named(8, "minecraft:compass", "§eTracker")
            .boots("minecraft:leather_boots");
        assert_eq!(kit.len(), 3);
        assert!(Kit::new().is_empty());
    }
}
