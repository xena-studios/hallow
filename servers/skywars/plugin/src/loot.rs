//! What the chests hold. Each chest rolls a handful of items from its
//! table (island or middle), weighted, into random slots. A team's island
//! chests between them always hold blocks, a weapon and a piece of armour,
//! so nobody starts with nothing to bridge or fight with. Pure: the
//! plugin turns [`Item`]s into stacks.

use hallow_islands::map::Loot;

/// Enchantments loot carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enchant {
    Sharpness,
    Protection,
    Power,
    Knockback,
    Efficiency,
}

/// One stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    pub key: &'static str,
    pub count: u8,
    pub enchant: Option<(Enchant, u32)>,
}

/// What an entry is, for the guarantees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Blocks,
    Weapon,
    Armour,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: &'static str,
    pub min: u8,
    pub max: u8,
    pub weight: u32,
    pub kind: Kind,
    pub enchant: Option<(Enchant, u32)>,
}

const fn e(key: &'static str, min: u8, max: u8, weight: u32, kind: Kind) -> Entry {
    Entry {
        key,
        min,
        max,
        weight,
        kind,
        enchant: None,
    }
}

const fn ench(key: &'static str, weight: u32, kind: Kind, enchant: (Enchant, u32)) -> Entry {
    Entry {
        key,
        min: 1,
        max: 1,
        weight,
        kind,
        enchant: Some(enchant),
    }
}

use Kind::{Armour, Blocks, Other, Weapon};

/// Team islands' chests: the basics.
pub const ISLAND: &[Entry] = &[
    e("minecraft:oak_planks", 12, 24, 12, Blocks),
    e("minecraft:stone", 12, 20, 10, Blocks),
    e("minecraft:andesite", 10, 16, 6, Blocks),
    e("minecraft:stone_sword", 1, 1, 7, Weapon),
    e("minecraft:iron_sword", 1, 1, 2, Weapon),
    e("minecraft:stone_axe", 1, 1, 3, Weapon),
    e("minecraft:bow", 1, 1, 2, Weapon),
    e("minecraft:arrow", 4, 10, 4, Other),
    e("minecraft:leather_helmet", 1, 1, 4, Armour),
    e("minecraft:chainmail_chestplate", 1, 1, 4, Armour),
    e("minecraft:chainmail_leggings", 1, 1, 3, Armour),
    e("minecraft:iron_boots", 1, 1, 3, Armour),
    e("minecraft:iron_helmet", 1, 1, 2, Armour),
    e("minecraft:cooked_beef", 2, 5, 6, Other),
    e("minecraft:baked_potato", 3, 6, 4, Other),
    e("minecraft:golden_apple", 1, 1, 2, Other),
    e("minecraft:snowball", 4, 8, 4, Other),
    e("minecraft:egg", 4, 8, 3, Other),
    e("minecraft:stone_pickaxe", 1, 1, 4, Other),
    e("minecraft:water_bucket", 1, 1, 2, Other),
    e("minecraft:lava_bucket", 1, 1, 1, Other),
    e("minecraft:ender_pearl", 1, 1, 1, Other),
];

/// The middle's chests: better gear, worth the trip.
pub const MIDDLE: &[Entry] = &[
    e("minecraft:oak_planks", 16, 32, 5, Blocks),
    e("minecraft:iron_sword", 1, 1, 5, Weapon),
    e("minecraft:diamond_sword", 1, 1, 2, Weapon),
    ench("minecraft:iron_sword", 2, Weapon, (Enchant::Sharpness, 1)),
    ench("minecraft:bow", 2, Weapon, (Enchant::Power, 1)),
    e("minecraft:arrow", 8, 16, 4, Other),
    e("minecraft:iron_chestplate", 1, 1, 4, Armour),
    e("minecraft:iron_leggings", 1, 1, 4, Armour),
    e("minecraft:diamond_helmet", 1, 1, 2, Armour),
    e("minecraft:diamond_chestplate", 1, 1, 1, Armour),
    ench("minecraft:iron_boots", 2, Armour, (Enchant::Protection, 2)),
    ench(
        "minecraft:diamond_boots",
        1,
        Armour,
        (Enchant::Protection, 1),
    ),
    e("minecraft:golden_apple", 1, 2, 4, Other),
    e("minecraft:ender_pearl", 1, 2, 3, Other),
    e("minecraft:snowball", 8, 16, 2, Other),
    ench("minecraft:stick", 1, Weapon, (Enchant::Knockback, 1)),
    ench("minecraft:iron_pickaxe", 2, Other, (Enchant::Efficiency, 2)),
    e("minecraft:cooked_porkchop", 3, 6, 3, Other),
    e("minecraft:lava_bucket", 1, 1, 2, Other),
];

/// Items per chest.
const fn rolls(loot: Loot) -> (u32, u32) {
    match loot {
        Loot::Island => (4, 6),
        Loot::Middle => (5, 7),
    }
}

/// Chest slots: a single chest has 27.
pub const CHEST_SLOTS: u32 = 27;

/// A small, seedable random source (xorshift).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub const fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// `lo..=hi`.
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next() % u64::from(hi - lo + 1)) as u32
    }
}

fn pick<'a>(table: &'a [Entry], rng: &mut Rng, kind: Option<Kind>) -> &'a Entry {
    let total: u32 = table
        .iter()
        .filter(|e| kind.is_none_or(|k| e.kind == k))
        .map(|e| e.weight)
        .sum();
    let mut roll = rng.range(0, total.saturating_sub(1));
    for entry in table.iter().filter(|e| kind.is_none_or(|k| e.kind == k)) {
        if roll < entry.weight {
            return entry;
        }
        roll -= entry.weight;
    }
    &table[0]
}

fn item(entry: &Entry, rng: &mut Rng) -> Item {
    Item {
        key: entry.key,
        count: rng.range(u32::from(entry.min), u32::from(entry.max)) as u8,
        enchant: entry.enchant,
    }
}

/// Fills one chest: `(slot, item)` pairs, `must` first (the guarantees).
#[must_use]
pub fn fill(loot: Loot, must: &[Kind], rng: &mut Rng) -> Vec<(u32, Item)> {
    let table = match loot {
        Loot::Island => ISLAND,
        Loot::Middle => MIDDLE,
    };
    let (lo, hi) = rolls(loot);
    let count = rng.range(lo, hi).max(must.len() as u32);
    let mut slots: Vec<u32> = (0..CHEST_SLOTS).collect();
    let mut out = Vec::new();
    for i in 0..count {
        let entry = match must.get(i as usize) {
            Some(kind) => pick(table, rng, Some(*kind)),
            None => pick(table, rng, None),
        };
        let at = rng.range(0, slots.len() as u32 - 1) as usize;
        out.push((slots.swap_remove(at), item(entry, rng)));
    }
    out
}

/// Spreads the island guarantees over a team's `chests` chests: blocks in
/// the first, a weapon in the second, armour in the third (all in one if
/// there's only one).
#[must_use]
pub fn island_guarantees(chests: usize, index: usize) -> Vec<Kind> {
    const ALL: [Kind; 3] = [Blocks, Weapon, Armour];
    if chests <= 1 {
        return ALL.to_vec();
    }
    ALL.iter()
        .enumerate()
        .filter(|(i, _)| i % chests == index)
        .map(|(_, k)| *k)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_of(key: &str) -> Kind {
        ISLAND
            .iter()
            .chain(MIDDLE)
            .find(|e| e.key == key)
            .map_or(Other, |e| e.kind)
    }

    #[test]
    fn islands_always_get_the_basics() {
        for seed in 0..200 {
            let mut rng = Rng::new(seed);
            let mut kinds = Vec::new();
            for i in 0..3 {
                for (_, item) in fill(Loot::Island, &island_guarantees(3, i), &mut rng) {
                    kinds.push(kind_of(item.key));
                }
            }
            for kind in [Blocks, Weapon, Armour] {
                assert!(kinds.contains(&kind), "seed {seed}: no {kind:?}");
            }
        }
    }

    #[test]
    fn chests_use_distinct_slots_and_sane_counts() {
        let mut rng = Rng::new(42);
        for loot in [Loot::Island, Loot::Middle] {
            let items = fill(loot, &[], &mut rng);
            let mut slots: Vec<u32> = items.iter().map(|(s, _)| *s).collect();
            slots.sort_unstable();
            slots.dedup();
            assert_eq!(slots.len(), items.len());
            assert!(items.iter().all(|(s, i)| *s < CHEST_SLOTS && i.count >= 1));
        }
    }

    #[test]
    fn one_chest_gets_every_guarantee() {
        assert_eq!(island_guarantees(1, 0).len(), 3);
        assert_eq!(island_guarantees(2, 0), [Blocks, Armour]);
        assert_eq!(island_guarantees(2, 1), [Weapon]);
        assert!(island_guarantees(4, 3).is_empty());
    }
}
