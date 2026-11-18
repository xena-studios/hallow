//! Inventories: paying for things with items, and giving items so they
//! stack like a pickup would. Pumpkin's plugin API (v0.1) has slots but no
//! "add item" or "remove n of", so these do it slot by slot.

use pumpkin_plugin_api::{ItemStack, Player};

/// The main inventory: hotbar 0–8, then 9–35. Armour and the off hand
/// come after.
pub const MAIN_SLOTS: u32 = 36;

/// One occupied slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub slot: u32,
    /// Without the `minecraft:` prefix.
    pub key: String,
    pub count: u8,
}

/// An item key without its namespace: Pumpkin reads `minecraft:iron_ingot`
/// back as `iron_ingot`.
#[must_use]
pub fn bare(key: &str) -> &str {
    key.strip_prefix("minecraft:").unwrap_or(key)
}

/// How many of `key` the slots hold.
#[must_use]
pub fn count(slots: &[Slot], key: &str) -> u32 {
    let key = bare(key);
    slots
        .iter()
        .filter(|s| s.key == key)
        .map(|s| u32::from(s.count))
        .sum()
}

/// The slots to change, and their new counts, to take `amount` of `key`;
/// `None` if there aren't enough. Takes from the last slots first, so the
/// hotbar keeps its stacks longest.
#[must_use]
pub fn take(slots: &[Slot], key: &str, amount: u32) -> Option<Vec<(u32, u8)>> {
    if count(slots, key) < amount {
        return None;
    }
    let key = bare(key);
    let mut left = amount;
    let mut changes = Vec::new();
    for slot in slots.iter().rev().filter(|s| s.key == key) {
        if left == 0 {
            break;
        }
        let used = u32::from(slot.count).min(left);
        left -= used;
        changes.push((slot.slot, slot.count - used as u8));
    }
    Some(changes)
}

/// The player's main inventory.
#[must_use]
pub fn slots(player: &Player) -> Vec<Slot> {
    let inventory = player.get_inventory().as_inventory();
    (0..MAIN_SLOTS)
        .filter_map(|slot| {
            let item = item_in(&inventory, slot)?;
            Some(Slot {
                slot,
                key: bare(&item.get_registry_key()).to_owned(),
                count: item.get_count(),
            })
        })
        .collect()
}

/// Takes `amount` of `key` from the player; false (and nothing taken) if
/// they haven't enough.
pub fn pay(player: &Player, key: &str, amount: u32) -> bool {
    let Some(changes) = take(&slots(player), key, amount) else {
        return false;
    };
    let inventory = player.get_inventory().as_inventory();
    for (slot, left) in changes {
        if left == 0 {
            inventory.set_item(slot, None);
        } else if let Some(item) = item_in(&inventory, slot) {
            item.set_count(left);
            inventory.set_item(slot, Some(item));
        }
    }
    true
}

/// The item in a slot, if it holds one (an empty slot may read as air).
fn item_in(inventory: &pumpkin_plugin_api::Inventory, slot: u32) -> Option<ItemStack> {
    inventory
        .get_item(slot)
        .filter(|item| item.get_count() > 0 && bare(&item.get_registry_key()) != "air")
}

fn plain(item: &ItemStack) -> bool {
    item.get_custom_name().is_none() && item.get_enchantments().is_empty()
}

/// Gives `count` of a plain item, topping up stacks first. What doesn't
/// fit is dropped silently (counted in the return).
pub fn give(player: &Player, key: &str, count: u32) -> u32 {
    let inventory = player.get_inventory().as_inventory();
    let key_bare = bare(key);
    let mut left = count;
    for slot in 0..MAIN_SLOTS {
        if left == 0 {
            return 0;
        }
        if let Some(item) = item_in(&inventory, slot)
            && bare(&item.get_registry_key()) == key_bare
            && plain(&item)
        {
            let room = u32::from(item.get_max_count().saturating_sub(item.get_count()));
            let add = room.min(left);
            if add > 0 {
                item.set_count(item.get_count() + add as u8);
                inventory.set_item(slot, Some(item));
                left -= add;
            }
        }
    }
    for slot in 0..MAIN_SLOTS {
        if left == 0 {
            return 0;
        }
        if item_in(&inventory, slot).is_none() {
            let stack = ItemStack::new(&format!("minecraft:{key_bare}"), 1);
            let add = u32::from(stack.get_max_count()).min(left);
            stack.set_count(add as u8);
            inventory.set_item(slot, Some(stack));
            left -= add;
        }
    }
    left
}

/// Puts a finished stack (named, enchanted) in the first free slot, or the
/// hotbar's first slot if there's none. Returns whether it fit.
pub fn give_stack(player: &Player, stack: ItemStack) -> bool {
    let inventory = player.get_inventory().as_inventory();
    match (0..MAIN_SLOTS).find(|slot| item_in(&inventory, *slot).is_none()) {
        Some(slot) => {
            inventory.set_item(slot, Some(stack));
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(slot: u32, key: &str, count: u8) -> Slot {
        Slot {
            slot,
            key: key.into(),
            count,
        }
    }

    #[test]
    fn paying_takes_from_the_back() {
        let slots = [
            slot(0, "iron_ingot", 10),
            slot(3, "gold_ingot", 4),
            slot(20, "iron_ingot", 5),
        ];
        assert_eq!(count(&slots, "minecraft:iron_ingot"), 15);
        assert_eq!(take(&slots, "iron_ingot", 7), Some(vec![(20, 0), (0, 8)]));
        assert_eq!(take(&slots, "minecraft:gold_ingot", 4), Some(vec![(3, 0)]));
        assert_eq!(take(&slots, "iron_ingot", 16), None);
        assert_eq!(take(&slots, "diamond", 0), Some(vec![]));
    }
}
