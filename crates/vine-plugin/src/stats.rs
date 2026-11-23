//! `/stats` and `/leaderboard`, from any server: players' totals and the
//! top of each game's boards, read from the control plane.
//!
//! | Command | |
//! |---|---|
//! | `/stats [player] [game]` | Your or another player's stats, every game or one |
//! | `/leaderboard [game] [stat] [all\|month\|week]` (`/lb`, `/top`) | The top 10; without a game, the games and their stats |
//!
//! Games are server kinds (`test-game`, `skywars`); they match without case
//! and without dashes, so `/lb testgame` works. The stat defaults to `wins`
//! (or the game's first stat), the window to all time.

use std::collections::BTreeMap;

use hallow_proto::{
    public::BoardKind,
    routes,
    stats::{Leaderboard, PlayerStats, Window},
};

use crate::vine::plugin::command::reply;
use crate::{CommandSender, Context, State, http};

pub const STATS: u32 = 40;
pub const LEADERBOARD: u32 = 41;

const TOP: u32 = 10;
const RULE: &str = "§8§m                    §r";

pub fn register(ctx: &Context) {
    ctx.register_command(STATS, "stats", "Show a player's stats", &[], None);
    ctx.register_command(
        LEADERBOARD,
        "leaderboard",
        "Show a game's leaderboard",
        &["lb".into(), "top".into()],
        None,
    );
}

/// `test-game` → `Test Game`.
#[must_use]
pub fn kind_title(kind: &str) -> String {
    words(kind, &['-', '_', '.'])
}

/// `play_time_s` → `Play time`, `kills` → `Kills`.
#[must_use]
pub fn stat_title(stat: &str) -> String {
    match stat {
        "play_time_s" => "Play time".into(),
        "games" => "Games played".into(),
        other => {
            let mut text = other.replace(['_', '.'], " ");
            if let Some(first) = text.get(..1) {
                let upper = first.to_ascii_uppercase();
                text.replace_range(..1, &upper);
            }
            text
        }
    }
}

fn words(text: &str, separators: &[char]) -> String {
    text.split(separators)
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map_or_else(String::new, |c| {
                c.to_uppercase().chain(chars).collect::<String>()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A value as players read it: play time in hours and minutes.
#[must_use]
pub fn value_text(stat: &str, value: i64) -> String {
    if stat == "play_time_s" {
        let minutes = value / 60;
        return match (minutes / 60, minutes % 60) {
            (0, 0) => format!("{value}s"),
            (0, m) => format!("{m}m"),
            (h, m) => format!("{h}h {m}m"),
        };
    }
    value.to_string()
}

/// A rank prefix's `&` codes as `§` codes.
#[must_use]
pub fn legacy(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len());
    let mut chars = prefix.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&'
            && chars.peek().is_some_and(
                |code| matches!(code.to_ascii_lowercase(), '0'..='9' | 'a'..='f' | 'k'..='o' | 'r'),
            )
        {
            out.push('§');
        } else {
            out.push(c);
        }
    }
    out
}

/// How the game list orders a game's stats for a summary: these first.
const HEADLINE: [&str; 5] = ["wins", "games", "kills", "deaths", "play_time_s"];

fn ordered(stats: &BTreeMap<String, i64>) -> Vec<(&str, i64)> {
    let mut out: Vec<(&str, i64)> = HEADLINE
        .iter()
        .filter_map(|h| stats.get(*h).map(|v| (*h, *v)))
        .collect();
    out.extend(
        stats
            .iter()
            .filter(|(k, _)| !HEADLINE.contains(&k.as_str()))
            .map(|(k, v)| (k.as_str(), *v)),
    );
    out
}

/// The `/stats` lines for `stats`, every kind or `only` one.
#[must_use]
pub fn stats_lines(stats: &PlayerStats, only: Option<&str>) -> Vec<String> {
    let mut lines = vec![format!("{RULE} §6§lStats §r§f{} {RULE}", stats.username)];
    let kinds: Vec<_> = stats
        .kinds
        .iter()
        .filter(|(kind, _)| only.is_none_or(|o| o == kind.as_str()))
        .collect();
    if kinds.is_empty() {
        lines.push(match only {
            Some(kind) => format!("§7No stats in {} yet.", kind_title(kind)),
            None => "§7No stats yet.".into(),
        });
    }
    for (kind, values) in kinds {
        let shown = ordered(values);
        if only.is_some() {
            lines.push(format!("§e{}", kind_title(kind)));
            for (stat, value) in shown {
                lines.push(format!(
                    " §7{}: §a{}",
                    stat_title(stat),
                    value_text(stat, value)
                ));
            }
        } else {
            let summary = shown
                .iter()
                .take(4)
                .map(|(stat, value)| {
                    format!("§7{} §a{}", stat_title(stat), value_text(stat, *value))
                })
                .collect::<Vec<_>>()
                .join(" §8· ");
            lines.push(format!("§e{} §8» {summary}", kind_title(kind)));
        }
    }
    lines
}

fn window_title(window: Window) -> &'static str {
    match window {
        Window::All => "All time",
        Window::Week => "This week",
        Window::Month => "This month",
    }
}

/// The `/leaderboard` lines for `board`.
#[must_use]
pub fn board_lines(board: &Leaderboard) -> Vec<String> {
    let mut lines = vec![format!(
        "{RULE} §6§l{} §r§7{} §8· §7{} {RULE}",
        kind_title(&board.kind),
        stat_title(&board.stat),
        window_title(board.window)
    )];
    if board.entries.is_empty() {
        lines.push("§7Nobody yet. Play a game!".into());
    }
    for entry in &board.entries {
        let place = match entry.position {
            1 => "§6",
            2 => "§f",
            3 => "§c",
            _ => "§7",
        };
        let name = if entry.prefix.is_empty() {
            format!("§7{}", entry.username)
        } else {
            format!("{}{}", legacy(&entry.prefix), entry.username)
        };
        lines.push(format!(
            "{place}#{} {name} §8» §a{}",
            entry.position,
            value_text(&board.stat, entry.value)
        ));
    }
    lines
}

/// The game list for `/leaderboard` without a game.
#[must_use]
pub fn kinds_lines(kinds: &[BoardKind]) -> Vec<String> {
    let mut lines = vec![format!("{RULE} §6§lLeaderboards {RULE}")];
    if kinds.is_empty() {
        lines.push("§7No games have stats yet.".into());
    }
    for kind in kinds {
        lines.push(format!(
            "§e{} §8(§7{}§8) §7{}",
            kind_title(&kind.kind),
            kind.kind,
            kind.stats.join(", ")
        ));
    }
    lines.push("§7Use §f/lb <game> [stat] [all|month|week]".into());
    lines
}

/// The kind `typed` names among `kinds`: exact, then without case and
/// dashes.
#[must_use]
pub fn match_kind<'a>(typed: &str, kinds: &'a [BoardKind]) -> Option<&'a BoardKind> {
    let squash = |s: &str| {
        s.chars()
            .filter(|c| !matches!(c, '-' | '_' | '.' | ' '))
            .collect::<String>()
            .to_ascii_lowercase()
    };
    kinds
        .iter()
        .find(|k| k.kind == typed)
        .or_else(|| kinds.iter().find(|k| squash(&k.kind) == squash(typed)))
}

impl State {
    /// Runs `/stats` or `/leaderboard`; `None` for any other command.
    pub fn stats_command(
        &self,
        id: u32,
        sender: &CommandSender,
        args: &[String],
    ) -> Option<Result<i32, String>> {
        match id {
            STATS => Some(self.show_stats(sender, args)),
            LEADERBOARD => Some(self.show_board(sender, args)),
            _ => None,
        }
    }

    fn kinds(&self) -> Result<Vec<BoardKind>, String> {
        self.client
            .get::<Vec<BoardKind>>(routes::LEADERBOARDS)
            .map_err(|err| unavailable(&err))
    }

    fn show_stats(&self, sender: &CommandSender, args: &[String]) -> Result<i32, String> {
        let (player, game) = match args {
            [] => (sender.name.clone(), None),
            [player] => (player.clone(), None),
            [player, game] => (player.clone(), Some(game.as_str())),
            _ => return Err("§cUsage: /stats [player] [game]".into()),
        };
        let stats = match self.client.get::<PlayerStats>(&routes::stats_of(&player)) {
            Ok(stats) => stats,
            Err(http::Error::Status(404, _)) => {
                return Err(format!("§cNobody called {player} has played here."));
            }
            Err(err) => return Err(unavailable(&err)),
        };
        let kind = match game {
            None => None,
            Some(typed) => {
                let known: Vec<BoardKind> = stats
                    .kinds
                    .keys()
                    .map(|k| BoardKind {
                        kind: k.clone(),
                        stats: Vec::new(),
                        players: 0,
                    })
                    .collect();
                Some(match_kind(typed, &known).map_or_else(|| typed.to_owned(), |k| k.kind.clone()))
            }
        };
        for line in stats_lines(&stats, kind.as_deref()) {
            reply(sender, &line);
        }
        Ok(1)
    }

    fn show_board(&self, sender: &CommandSender, args: &[String]) -> Result<i32, String> {
        let usage = || "§cUsage: /lb [game] [stat] [all|month|week]".to_owned();
        if args.len() > 3 {
            return Err(usage());
        }
        let kinds = self.kinds()?;
        let Some(typed) = args.first() else {
            for line in kinds_lines(&kinds) {
                reply(sender, &line);
            }
            return Ok(1);
        };
        let Some(kind) = match_kind(typed, &kinds) else {
            return Err(format!(
                "§cNo game called {typed} has stats. §7/lb lists them."
            ));
        };
        let mut stat = None;
        let mut window = Window::All;
        for arg in &args[1..] {
            if let Some(w) = Window::parse(arg) {
                window = w;
            } else if stat.is_none() {
                stat = Some(arg.to_ascii_lowercase());
            } else {
                return Err(usage());
            }
        }
        let stat = stat
            .or_else(|| {
                ["wins", "kills", "games"]
                    .iter()
                    .find(|s| kind.stats.iter().any(|k| k == *s))
                    .map(|s| (*s).to_owned())
            })
            .or_else(|| kind.stats.first().cloned())
            .unwrap_or_else(|| "wins".into());
        if !kind.stats.contains(&stat) {
            return Err(format!(
                "§c{} has no stat {stat}. §7It has: {}",
                kind_title(&kind.kind),
                kind.stats.join(", ")
            ));
        }
        let path = routes::leaderboard_query(&kind.kind, &stat, window, TOP);
        let board = self
            .client
            .get::<Leaderboard>(&path)
            .map_err(|err| unavailable(&err))?;
        for line in board_lines(&board) {
            reply(sender, &line);
        }
        Ok(1)
    }

    /// Tab completion: game names for `/lb`.
    pub fn stats_suggest(&self, id: u32, args: &[String]) -> Option<Vec<String>> {
        if id != LEADERBOARD {
            return None;
        }
        let kinds = self.kinds().unwrap_or_default();
        let prefix = args
            .last()
            .map(|a| a.to_ascii_lowercase())
            .unwrap_or_default();
        Some(match args.len() {
            0 | 1 => kinds
                .into_iter()
                .map(|k| k.kind)
                .filter(|k| k.starts_with(&prefix))
                .collect(),
            2 => match_kind(&args[0], &kinds)
                .map(|k| k.stats.clone())
                .unwrap_or_default()
                .into_iter()
                .chain(Window::ALL.iter().map(|w| w.as_str().to_owned()))
                .filter(|s| s.starts_with(&prefix))
                .collect(),
            3 => Window::ALL
                .iter()
                .map(|w| w.as_str().to_owned())
                .filter(|s| s.starts_with(&prefix))
                .collect(),
            _ => Vec::new(),
        })
    }
}

fn unavailable(err: &http::Error) -> String {
    crate::log(crate::Level::Warn, &format!("stats lookup failed: {err}"));
    if matches!(err, http::Error::Status(503, _)) {
        "§cStats are unavailable right now.".into()
    } else {
        crate::UNAVAILABLE.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::stats::LeaderboardEntry;
    use uuid::Uuid;

    #[test]
    fn titles_and_values() {
        assert_eq!(kind_title("test-game"), "Test Game");
        assert_eq!(stat_title("play_time_s"), "Play time");
        assert_eq!(stat_title("beds_broken"), "Beds broken");
        assert_eq!(value_text("play_time_s", 3_725), "1h 2m");
        assert_eq!(value_text("play_time_s", 125), "2m");
        assert_eq!(value_text("play_time_s", 40), "40s");
        assert_eq!(value_text("kills", 7), "7");
        assert_eq!(legacy("&c[ADMIN] "), "§c[ADMIN] ");
    }

    #[test]
    fn kinds_match_loosely() {
        let kinds = vec![BoardKind {
            kind: "test-game".into(),
            stats: vec!["wins".into()],
            players: 1,
        }];
        assert!(match_kind("test-game", &kinds).is_some());
        assert!(match_kind("TestGame", &kinds).is_some());
        assert!(match_kind("skywars", &kinds).is_none());
    }

    #[test]
    fn lines() {
        let stats = PlayerStats {
            uuid: Uuid::nil(),
            username: "Steve".into(),
            kinds: BTreeMap::from([(
                "test-game".into(),
                BTreeMap::from([
                    ("joins".into(), 4),
                    ("wins".into(), 2),
                    ("play_time_s".into(), 600),
                ]),
            )]),
        };
        let all = stats_lines(&stats, None);
        assert_eq!(all.len(), 2);
        assert!(
            all[1].starts_with("§eTest Game §8» §7Wins §a2"),
            "{}",
            all[1]
        );
        assert!(all[1].contains("Play time §a10m"));
        let one = stats_lines(&stats, Some("test-game"));
        assert_eq!(one.len(), 5);
        assert!(stats_lines(&stats, Some("skywars"))[1].contains("No stats in Skywars"));

        let board = Leaderboard {
            kind: "test-game".into(),
            stat: "wins".into(),
            window: Window::Week,
            since_ms: Some(0),
            entries: vec![LeaderboardEntry {
                position: 1,
                uuid: Uuid::nil(),
                username: "Steve".into(),
                value: 3,
                rank: Some("admin".into()),
                prefix: "&c[ADMIN] ".into(),
            }],
        };
        let lines = board_lines(&board);
        assert!(lines[0].contains("This week"));
        assert_eq!(lines[1], "§6#1 §c[ADMIN] Steve §8» §a3");
    }
}
