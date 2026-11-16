//! The game menu's layout and text: pure, so it's tested off the server.

use hallow_proto::lobby::GameMode;

/// A chest with three rows.
pub const SLOTS: u32 = 27;
/// The hotbar slot the menu item sits in: the middle one.
pub const ITEM_SLOT: u8 = 4;
/// Shown on the hotbar item and as the chest's title.
pub const TITLE: &str = "Game Menu";

/// Icons, picked by the mode's name so a mode keeps its icon.
const ICONS: [&str; 8] = [
    "minecraft:diamond_sword",
    "minecraft:bow",
    "minecraft:red_bed",
    "minecraft:golden_apple",
    "minecraft:ender_pearl",
    "minecraft:grass_block",
    "minecraft:iron_axe",
    "minecraft:fishing_rod",
];

/// One entry in the chest.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub slot: u32,
    /// What `/lobby` takes: the mode's name.
    pub mode: String,
    pub icon: &'static str,
    /// `§` coloured.
    pub name: String,
    pub lore: Vec<String>,
}

/// Where each mode goes: up to seven centred in the middle row, more filling
/// the chest from the top left.
#[must_use]
pub fn layout(modes: &[GameMode]) -> Vec<Entry> {
    let count = modes.len().min(SLOTS as usize);
    let first = if count <= 7 {
        9 + (9 - count as u32) / 2
    } else {
        0
    };
    modes
        .iter()
        .take(count)
        .zip(first..)
        .map(|(mode, slot)| Entry {
            slot,
            mode: mode.name.clone(),
            icon: icon(&mode.name),
            name: format!("§a§l{}", title_case(&mode.name)),
            lore: lore(mode),
        })
        .collect()
}

fn icon(name: &str) -> &'static str {
    let sum: usize = name.bytes().map(usize::from).sum();
    ICONS[sum % ICONS.len()]
}

fn lore(mode: &GameMode) -> Vec<String> {
    let lobbies = match mode.lobbies {
        0 => "§cStarting up...".to_owned(),
        1 => "§71 lobby".to_owned(),
        n => format!("§7{n} lobbies"),
    };
    vec![
        format!("§f{} §7playing", mode.players),
        lobbies,
        String::new(),
        "§eClick to play!".to_owned(),
    ]
}

/// `test-game` → `Test Game`.
#[must_use]
pub fn title_case(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The tab list's header and footer.
#[must_use]
pub fn tab_list(online: Option<u32>, server: &str) -> (String, String) {
    let header = "\n§6§lXENA STUDIOS\n§7Hallow network\n".to_owned();
    let footer = match online {
        Some(online) => format!("\n§a{online} §7online · §f{server}\n§7/lobby <mode> · /hub\n"),
        None => format!("\n§f{server}\n§7/lobby <mode> · /hub\n"),
    };
    (header, footer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(name: &str, players: u32, lobbies: u32) -> GameMode {
        GameMode {
            name: name.into(),
            lobby: format!("{name}-lobby"),
            players,
            lobbies,
        }
    }

    #[test]
    fn modes_centre_in_the_middle_row() {
        let one = layout(&[mode("test-game", 3, 1)]);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].slot, 13);
        assert_eq!(one[0].name, "§a§lTest Game");
        assert_eq!(one[0].lore[0], "§f3 §7playing");
        assert_eq!(one[0].lore[1], "§71 lobby");
        let three: Vec<u32> = layout(&[mode("a", 0, 0), mode("b", 0, 2), mode("c", 0, 0)])
            .iter()
            .map(|e| e.slot)
            .collect();
        assert_eq!(three, [12, 13, 14]);
        let modes: Vec<GameMode> = (0..30).map(|i| mode(&format!("m{i}"), 0, 0)).collect();
        let many = layout(&modes);
        assert_eq!(many.len(), 27);
        assert_eq!((many[0].slot, many[26].slot), (0, 26));
    }

    #[test]
    fn icons_stay_with_their_mode() {
        assert_eq!(icon("skywars"), icon("skywars"));
        assert!(ICONS.contains(&icon("bedwars")));
    }

    #[test]
    fn names_and_tab_list() {
        assert_eq!(title_case("sky_wars-duels"), "Sky Wars Duels");
        let (header, footer) = tab_list(Some(12), "lobby");
        assert!(header.contains("XENA STUDIOS"));
        assert!(footer.contains("§a12 §7online · §flobby"));
        assert!(!tab_list(None, "lobby").1.contains("online"));
    }
}
