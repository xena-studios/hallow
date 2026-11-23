//! The shop: what the shop keeper sells and for how much. Prices are in
//! the three resources the generators make. Pure data and rules, so the
//! tuning is tested and easy to change.

/// What things cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Currency {
    Iron,
    Gold,
    Diamond,
}

impl Currency {
    #[must_use]
    pub const fn item(self) -> &'static str {
        match self {
            Self::Iron => "minecraft:iron_ingot",
            Self::Gold => "minecraft:gold_ingot",
            Self::Diamond => "minecraft:diamond",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Iron => "§fIron",
            Self::Gold => "§6Gold",
            Self::Diamond => "§bDiamond",
        }
    }
}

/// A price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price(pub u32, pub Currency);

impl Price {
    #[must_use]
    pub fn label(self) -> String {
        format!("{} {}", self.0, self.1.label())
    }
}

/// Enchantments the shop puts on things.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enchant {
    Sharpness,
    Knockback,
    Power,
    Protection,
}

/// Armour tiers. Everyone wears leather; a bought tier replaces the
/// chestplate and leggings, and is kept through respawns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Armour {
    #[default]
    Leather,
    Chain,
    Iron,
    Diamond,
}

impl Armour {
    /// The chestplate and leggings.
    #[must_use]
    pub const fn pieces(self) -> (&'static str, &'static str) {
        match self {
            Self::Leather => ("minecraft:leather_chestplate", "minecraft:leather_leggings"),
            Self::Chain => (
                "minecraft:chainmail_chestplate",
                "minecraft:chainmail_leggings",
            ),
            Self::Iron => ("minecraft:iron_chestplate", "minecraft:iron_leggings"),
            Self::Diamond => ("minecraft:diamond_chestplate", "minecraft:diamond_leggings"),
        }
    }
}

/// Pickaxe tiers, kept through respawns. 0 is none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pickaxe(pub u8);

impl Pickaxe {
    /// The item and its efficiency level.
    #[must_use]
    pub const fn item(self) -> Option<(&'static str, u32)> {
        match self.0 {
            0 => None,
            1 => Some(("minecraft:wooden_pickaxe", 1)),
            2 => Some(("minecraft:stone_pickaxe", 1)),
            3 => Some(("minecraft:iron_pickaxe", 2)),
            _ => Some(("minecraft:diamond_pickaxe", 3)),
        }
    }
}

/// What buying an offer gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Give {
    /// `count` of an item, with an optional enchantment and name.
    Item {
        key: &'static str,
        count: u8,
        enchant: Option<(Enchant, u32)>,
        name: Option<&'static str>,
    },
    /// The buyer's team wool.
    TeamWool(u8),
    /// An armour tier.
    Armour(Armour),
    /// A pickaxe tier.
    Pickaxe(Pickaxe),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer {
    /// Shown on the icon.
    pub name: &'static str,
    /// The icon; for team wool the buyer's own colour is shown.
    pub icon: &'static str,
    pub give: Give,
    pub price: Price,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Category {
    pub name: &'static str,
    pub icon: &'static str,
    pub blurb: &'static str,
    pub offers: &'static [Offer],
}

const fn item(name: &'static str, key: &'static str, count: u8, price: Price) -> Offer {
    Offer {
        name,
        icon: key,
        give: Give::Item {
            key,
            count,
            enchant: None,
            name: None,
        },
        price,
    }
}

const fn enchanted(
    name: &'static str,
    key: &'static str,
    enchant: (Enchant, u32),
    price: Price,
) -> Offer {
    Offer {
        name,
        icon: key,
        give: Give::Item {
            key,
            count: 1,
            enchant: Some(enchant),
            name: Some(name),
        },
        price,
    }
}

use Currency::{Diamond, Gold, Iron};

/// The shop, category by category.
pub const CATEGORIES: [Category; 6] = [
    Category {
        name: "Blocks",
        icon: "minecraft:white_wool",
        blurb: "Bridge out and wall in",
        offers: &[
            Offer {
                name: "Team Wool ×16",
                icon: "minecraft:white_wool",
                give: Give::TeamWool(16),
                price: Price(4, Iron),
            },
            item(
                "Terracotta ×12",
                "minecraft:terracotta",
                12,
                Price(10, Iron),
            ),
            item("Oak Planks ×12", "minecraft:oak_planks", 12, Price(3, Gold)),
            item("End Stone ×8", "minecraft:end_stone", 8, Price(20, Iron)),
            item("Glass ×6", "minecraft:glass", 6, Price(8, Iron)),
            item("Ladder ×8", "minecraft:ladder", 8, Price(4, Iron)),
            item("Obsidian ×2", "minecraft:obsidian", 2, Price(3, Diamond)),
        ],
    },
    Category {
        name: "Weapons",
        icon: "minecraft:iron_sword",
        blurb: "Hit harder",
        offers: &[
            item("Stone Sword", "minecraft:stone_sword", 1, Price(10, Iron)),
            item("Iron Sword", "minecraft:iron_sword", 1, Price(6, Gold)),
            enchanted(
                "Keen Iron Sword",
                "minecraft:iron_sword",
                (Enchant::Sharpness, 1),
                Price(3, Diamond),
            ),
            item(
                "Diamond Sword",
                "minecraft:diamond_sword",
                1,
                Price(5, Diamond),
            ),
            enchanted(
                "Shove Stick",
                "minecraft:stick",
                (Enchant::Knockback, 1),
                Price(5, Gold),
            ),
        ],
    },
    Category {
        name: "Armour",
        icon: "minecraft:iron_chestplate",
        blurb: "Kept when you respawn",
        offers: &[
            Offer {
                name: "Chainmail Set",
                icon: "minecraft:chainmail_chestplate",
                give: Give::Armour(Armour::Chain),
                price: Price(28, Iron),
            },
            Offer {
                name: "Iron Set",
                icon: "minecraft:iron_chestplate",
                give: Give::Armour(Armour::Iron),
                price: Price(12, Gold),
            },
            Offer {
                name: "Diamond Set",
                icon: "minecraft:diamond_chestplate",
                give: Give::Armour(Armour::Diamond),
                price: Price(6, Diamond),
            },
        ],
    },
    Category {
        name: "Tools",
        icon: "minecraft:iron_pickaxe",
        blurb: "Pickaxes are kept when you respawn",
        offers: &[
            Offer {
                name: "Wooden Pickaxe",
                icon: "minecraft:wooden_pickaxe",
                give: Give::Pickaxe(Pickaxe(1)),
                price: Price(8, Iron),
            },
            Offer {
                name: "Stone Pickaxe",
                icon: "minecraft:stone_pickaxe",
                give: Give::Pickaxe(Pickaxe(2)),
                price: Price(16, Iron),
            },
            Offer {
                name: "Iron Pickaxe",
                icon: "minecraft:iron_pickaxe",
                give: Give::Pickaxe(Pickaxe(3)),
                price: Price(4, Gold),
            },
            Offer {
                name: "Diamond Pickaxe",
                icon: "minecraft:diamond_pickaxe",
                give: Give::Pickaxe(Pickaxe(4)),
                price: Price(3, Diamond),
            },
            item("Shears", "minecraft:shears", 1, Price(16, Iron)),
            item("Stone Axe", "minecraft:stone_axe", 1, Price(12, Iron)),
        ],
    },
    Category {
        name: "Ranged",
        icon: "minecraft:bow",
        blurb: "Pick them off from afar",
        offers: &[
            item("Bow", "minecraft:bow", 1, Price(10, Gold)),
            enchanted(
                "Strong Bow",
                "minecraft:bow",
                (Enchant::Power, 1),
                Price(4, Diamond),
            ),
            item("Arrows ×8", "minecraft:arrow", 8, Price(2, Gold)),
            item("Snowballs ×8", "minecraft:snowball", 8, Price(12, Iron)),
        ],
    },
    Category {
        name: "Supplies",
        icon: "minecraft:golden_apple",
        blurb: "Food and tricks",
        offers: &[
            item(
                "Baked Potatoes ×6",
                "minecraft:baked_potato",
                6,
                Price(4, Iron),
            ),
            item("Steak ×4", "minecraft:cooked_beef", 4, Price(2, Gold)),
            item("Golden Apple", "minecraft:golden_apple", 1, Price(3, Gold)),
            item("Water Bucket", "minecraft:water_bucket", 1, Price(3, Gold)),
            item("Ender Pearl", "minecraft:ender_pearl", 1, Price(4, Diamond)),
            enchanted(
                "Guard Helmet",
                "minecraft:iron_helmet",
                (Enchant::Protection, 1),
                Price(2, Diamond),
            ),
        ],
    },
];

/// The chest slots a menu's entries go in: the middle of the second and
/// third rows of a four-row chest (7 per row).
#[must_use]
pub fn entry_slots(count: usize) -> Vec<u32> {
    (0..count)
        .map(|i| {
            let row = (i / 7) as u32;
            let col = (i % 7) as u32;
            9 + row * 9 + 1 + col
        })
        .collect()
}

/// The back button's slot in a category.
pub const BACK_SLOT: u32 = 31;

/// Whether buying `give` changes anything for a player who has `armour` and
/// `pickaxe`: tiers only go up.
#[must_use]
pub fn worth_buying(give: Give, armour: Armour, pickaxe: Pickaxe) -> Result<(), &'static str> {
    match give {
        Give::Armour(tier) if tier <= armour => Err("You already wear that or better."),
        Give::Pickaxe(tier) if tier <= pickaxe => Err("You already have that pickaxe or better."),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_category_fits_its_chest() {
        for category in CATEGORIES {
            assert!(!category.offers.is_empty());
            let slots = entry_slots(category.offers.len());
            assert!(slots.iter().all(|s| *s < 36 && *s != BACK_SLOT));
            for offer in category.offers {
                assert!(offer.price.0 > 0, "{} is free", offer.name);
            }
        }
        assert_eq!(entry_slots(8), [10, 11, 12, 13, 14, 15, 16, 19]);
    }

    #[test]
    fn tiers_only_go_up() {
        let iron = Give::Armour(Armour::Iron);
        assert!(worth_buying(iron, Armour::Chain, Pickaxe(0)).is_ok());
        assert!(worth_buying(iron, Armour::Iron, Pickaxe(0)).is_err());
        assert!(worth_buying(Give::Pickaxe(Pickaxe(2)), Armour::Leather, Pickaxe(3)).is_err());
        assert!(worth_buying(Give::TeamWool(16), Armour::Diamond, Pickaxe(4)).is_ok());
    }

    #[test]
    fn dearer_tiers_cost_rarer_resources() {
        let armour: Vec<Price> = CATEGORIES[2].offers.iter().map(|o| o.price).collect();
        assert!(armour.windows(2).all(|w| w[0].1 <= w[1].1));
    }
}
