//! The shop and generator menus, and the kit players spawn with.

use hallow_islands::{
    items,
    play::{self, COLOURS},
};
use pumpkin_plugin_api::{
    ItemStack, Player, Screen, Server, World, enchantments_wit::Enchantment, gui::Gui,
};
use uuid::Uuid;

use crate::gens::{self, tier_name};
use crate::shop::{self, BACK_SLOT, CATEGORIES, Currency, Enchant, Give};
use crate::{Game, Gear};

/// Which menu a player has open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Main,
    Category(usize),
    Upgrade(usize),
}

/// Category icons in the main menu.
const MAIN_SLOTS: [u32; 6] = [10, 11, 12, 14, 15, 16];
const WALLET_SLOT: u32 = 13;
const UPGRADE_SLOT: u32 = 13;

fn icon(key: &str, name: &str, lore: &[String]) -> ItemStack {
    let item = ItemStack::new(key, 1);
    item.set_custom_name(Some(play::text(name)));
    item.set_lore(lore.iter().map(|line| play::text(line)).collect());
    item
}

fn wallet(player: &Player) -> String {
    let slots = items::slots(player);
    format!(
        "§f{} Iron §6{} Gold §b{} Diamond",
        items::count(&slots, Currency::Iron.item()),
        items::count(&slots, Currency::Gold.item()),
        items::count(&slots, Currency::Diamond.item())
    )
}

fn team_wool(game: &Game, id: Uuid) -> &'static str {
    let team = game.rules.roster.team_of(id).unwrap_or(0);
    COLOURS[team % COLOURS.len()].wool
}

pub fn open_main(game: &mut Game, player: &Player, id: Uuid) {
    let gui = Gui::new(Screen::Generic9x3, play::text("§8Shop"));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    for (slot, category) in MAIN_SLOTS.iter().zip(CATEGORIES.iter()) {
        let key = if category.name == "Blocks" {
            team_wool(game, id)
        } else {
            category.icon
        };
        gui.set_item(
            *slot,
            icon(
                key,
                &format!("§a§l{}", category.name),
                &[
                    format!("§7{}", category.blurb),
                    String::new(),
                    "§eClick to browse".into(),
                ],
            ),
        );
    }
    gui.set_item(
        WALLET_SLOT,
        icon("minecraft:emerald", "§eYour resources", &[wallet(player)]),
    );
    game.menus.insert(id, Menu::Main);
    player.open_gui(gui);
}

fn open_category(game: &mut Game, player: &Player, id: Uuid, index: usize) {
    let category = CATEGORIES[index];
    let gui = Gui::new(
        Screen::Generic9x4,
        play::text(&format!("§8Shop › {}", category.name)),
    );
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let gear = game.gear.get(&id).copied().unwrap_or_default();
    for (slot, offer) in shop::entry_slots(category.offers.len())
        .into_iter()
        .zip(category.offers)
    {
        let key = match offer.give {
            Give::TeamWool(_) => team_wool(game, id),
            _ => offer.icon,
        };
        let mut lore = vec![format!("§7Cost: {}", offer.price.label())];
        if let Err(why) = shop::worth_buying(offer.give, gear.armour, gear.pickaxe) {
            lore.push(format!("§c{why}"));
        } else {
            lore.push(String::new());
            lore.push("§eClick to buy".into());
        }
        gui.set_item(slot, icon(key, &format!("§a{}", offer.name), &lore));
    }
    gui.set_item(
        BACK_SLOT,
        icon("minecraft:arrow", "§7« Back", &[wallet(player)]),
    );
    game.menus.insert(id, Menu::Category(index));
    player.open_gui(gui);
}

pub fn open_upgrade(game: &mut Game, player: &Player, id: Uuid, index: usize) {
    let Some((_, generator)) = game.generators.get(index) else {
        return;
    };
    let name = format!("{} Generator", tier_name(generator.tier));
    let gui = Gui::new(Screen::Generic9x3, play::text(&format!("§8{name}")));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let mut lore = vec![match generator.seconds_per() {
        Some(secs) => format!("§7Makes one every §f{secs:.1}§7 s"),
        None => "§7Off until upgraded".into(),
    }];
    match generator.next_cost() {
        Some(price) => {
            lore.push(String::new());
            lore.push(format!(
                "§eUpgrade to level {}: {}",
                generator.level + 1,
                price.label()
            ));
            lore.push("§eClick to upgrade".into());
        }
        None => lore.push("§aTop level".into()),
    }
    gui.set_item(
        UPGRADE_SLOT,
        icon(
            hallow_islands::build::pad(generator.tier),
            &format!("{name} §7(level {})", generator.level),
            &lore,
        ),
    );
    game.menus.insert(id, Menu::Upgrade(index));
    player.open_gui(gui);
}

pub fn click(
    game: &mut Game,
    _server: &Server,
    _world: &World,
    player: &Player,
    id: Uuid,
    slot: u32,
) {
    let Some(menu) = game.menus.get(&id).copied() else {
        return;
    };
    if !game.alive(id) {
        return;
    }
    match menu {
        Menu::Main => {
            if let Some(index) = MAIN_SLOTS.iter().position(|s| *s == slot) {
                open_category(game, player, id, index);
            }
        }
        Menu::Category(index) => {
            if slot == BACK_SLOT {
                open_main(game, player, id);
                return;
            }
            let category = CATEGORIES[index];
            let Some(at) = shop::entry_slots(category.offers.len())
                .iter()
                .position(|s| *s == slot)
            else {
                return;
            };
            if buy(game, player, id, category.offers[at]) {
                open_category(game, player, id, index);
            }
        }
        Menu::Upgrade(index) => {
            if slot == UPGRADE_SLOT {
                upgrade(game, player, id, index);
            }
        }
    }
}

fn say(player: &Player, line: &str) {
    player.show_actionbar(play::text(line));
}

fn buy(game: &mut Game, player: &Player, id: Uuid, offer: shop::Offer) -> bool {
    let mut gear = game.gear.get(&id).copied().unwrap_or_default();
    if let Err(why) = shop::worth_buying(offer.give, gear.armour, gear.pickaxe) {
        say(player, &format!("§c{why}"));
        return false;
    }
    let shop::Price(amount, currency) = offer.price;
    if !items::pay(player, currency.item(), amount) {
        let have = items::count(&items::slots(player), currency.item());
        say(
            player,
            &format!(
                "§cYou need {} more {}§c.",
                amount - have.min(amount),
                currency.label()
            ),
        );
        return false;
    }
    match offer.give {
        Give::Item {
            key,
            count,
            enchant,
            name,
        } => {
            if enchant.is_none() && name.is_none() {
                items::give(player, key, u32::from(count));
            } else {
                let stack = ItemStack::new(key, count);
                if let Some((enchant, level)) = enchant {
                    stack.add_enchantment(enchantment(enchant), level);
                }
                if let Some(name) = name {
                    stack.set_custom_name(Some(play::text(&format!("§a{name}"))));
                }
                if key.ends_with("_helmet") {
                    player.get_inventory().set_helmet(Some(stack));
                } else if !items::give_stack(player, stack) {
                    say(player, "§cYour inventory is full.");
                }
            }
        }
        Give::TeamWool(count) => {
            items::give(player, team_wool(game, id), u32::from(count));
        }
        Give::Armour(tier) => {
            gear.armour = tier;
            wear(player, tier);
        }
        Give::Pickaxe(tier) => {
            gear.pickaxe = tier;
            let inventory = player.get_inventory().as_inventory();
            for slot in 0..items::MAIN_SLOTS {
                if inventory
                    .get_item(slot)
                    .is_some_and(|item| item.get_registry_key().ends_with("_pickaxe"))
                {
                    inventory.set_item(slot, None);
                }
            }
            if let Some(stack) = pickaxe(tier) {
                items::give_stack(player, stack);
            }
        }
    }
    game.gear.insert(id, gear);
    play::stat(id, "items_bought", 1);
    say(player, &format!("§aBought {}!", offer.name));
    true
}

fn upgrade(game: &mut Game, player: &Player, id: Uuid, index: usize) {
    let Some((_, generator)) = game.generators.get(index) else {
        return;
    };
    let Some(shop::Price(amount, currency)) = generator.next_cost() else {
        say(player, "§aThat generator is at its top level.");
        return;
    };
    if !items::pay(player, currency.item(), amount) {
        say(
            player,
            &format!("§cYou need {amount} {}§c.", currency.label()),
        );
        return;
    }
    let generator = &mut game.generators[index].1;
    generator.upgrade();
    let line = format!(
        "§a{} generator upgraded to level {}!",
        gens::tier_name(generator.tier),
        generator.level
    );
    say(player, &line);
    play::stat(id, "generators_upgraded", 1);
    open_upgrade(game, player, id, index);
}

const fn enchantment(enchant: Enchant) -> Enchantment {
    match enchant {
        Enchant::Sharpness => Enchantment::Sharpness,
        Enchant::Knockback => Enchantment::Knockback,
        Enchant::Power => Enchantment::Power,
        Enchant::Protection => Enchantment::Protection,
    }
}

fn pickaxe(tier: shop::Pickaxe) -> Option<ItemStack> {
    let (key, efficiency) = tier.item()?;
    let stack = ItemStack::new(key, 1);
    stack.add_enchantment(Enchantment::Efficiency, efficiency);
    Some(stack)
}

fn wear(player: &Player, tier: shop::Armour) {
    let inventory = player.get_inventory();
    let (chest, legs) = tier.pieces();
    inventory.set_chestplate(Some(ItemStack::new(chest, 1)));
    inventory.set_leggings(Some(ItemStack::new(legs, 1)));
}

/// What a player spawns with: a wooden sword, leather cap and boots, their
/// armour tier and pickaxe.
pub fn kit(player: &Player, _team: usize, gear: Gear) {
    let inventory = player.get_inventory();
    inventory.clear_all();
    inventory.set_helmet(Some(ItemStack::new("minecraft:leather_helmet", 1)));
    inventory.set_boots(Some(ItemStack::new("minecraft:leather_boots", 1)));
    wear(player, gear.armour);
    let main = inventory.as_inventory();
    main.set_item(0, Some(ItemStack::new("minecraft:wooden_sword", 1)));
    if let Some(stack) = pickaxe(gear.pickaxe) {
        main.set_item(1, Some(stack));
    }
    inventory.set_selected_slot(0);
}
