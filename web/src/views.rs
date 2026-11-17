//! Pages, as pure functions: API data in, HTML out.

use std::collections::BTreeMap;

use hallow_proto::public::{BoardKind, NetworkSummary, PlayerMatch, PlayerProfile, RecentGame};
use hallow_proto::stats::{Leaderboard, Window};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use uuid::Uuid;

use crate::fmt::{
    duration, kind_label, legacy, number, relative, segment, stat_label, stat_rank, stat_value,
    strip_legacy,
};

/// What every page knows about the site.
#[derive(Clone, Copy)]
pub struct Site<'a> {
    pub name: &'a str,
    /// The stylesheet's URL, versioned by its content.
    pub css: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Nav {
    Home,
    Boards,
    None,
}

/// How many rows a full leaderboard shows, and the home page's previews.
pub const BOARD_ROWS: u32 = 100;
pub const PREVIEW_ROWS: u32 = 5;

const FAVICON: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Cpath d='M16 9c-3-2-9-1-11 4s0 13 6 13c2 0 3-1 5-1s3 1 5 1c6 0 8-8 6-13s-8-6-11-4z' fill='%23ff8a1f'/%3E%3Cpath d='M16 9c0-3 1-5 3-6' stroke='%235bbf6a' stroke-width='2.5' fill='none' stroke-linecap='round'/%3E%3C/svg%3E";

fn layout(site: Site, title: Option<&str>, nav: Nav, body: Markup) -> Markup {
    let full_title = match title {
        Some(title) => format!("{title} · {}", site.name),
        None => format!("{} · Stats and leaderboards", site.name),
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="dark light";
                meta name="theme-color" content="#0d0b1a" media="(prefers-color-scheme: dark)";
                meta name="theme-color" content="#fbf3e8" media="(prefers-color-scheme: light)";
                meta name="description" content={ "Player profiles, stats and leaderboards for " (site.name) "." };
                title { (full_title) }
                link rel="icon" href=(FAVICON);
                link rel="stylesheet" href=(site.css);
            }
            body {
                a.skip href="#main" { "Skip to content" }
                header.site-header {
                    div.wrap.header-row {
                        a.wordmark href="/" aria-label={ (site.name) " home" } {
                            (logo())
                            span { (site.name) }
                        }
                        nav aria-label="Main" {
                            a href="/" aria-current=[(nav == Nav::Home).then_some("page")] { "Home" }
                            a href="/leaderboards" aria-current=[(nav == Nav::Boards).then_some("page")] { "Leaderboards" }
                        }
                        (search_form("header-search", ""))
                    }
                }
                main #main.wrap { (body) }
                footer.site-footer {
                    div.wrap {
                        p { (site.name) " · stats update as games finish." }
                    }
                }
            }
        }
    }
}

fn logo() -> Markup {
    PreEscaped(
        r#"<svg class="logo" viewBox="0 0 32 32" aria-hidden="true" focusable="false"><path class="logo-body" d="M16 9c-3-2-9-1-11 4s0 13 6 13c2 0 3-1 5-1s3 1 5 1c6 0 8-8 6-13s-8-6-11-4z"/><path class="logo-rib" d="M16 10c-2 3-2 12 0 15M11 11c-3 3-3 11 0 14M21 11c3 3 3 11 0 14"/><path class="logo-stem" d="M16 9c0-3 1-5 3-6"/></svg>"#
            .to_owned(),
    )
}

fn search_form(class: &str, value: &str) -> Markup {
    html! {
        form class={ "search " (class) } action="/search" method="get" role="search" {
            label.visually-hidden for={ (class) "-q" } { "Find a player" }
            input id={ (class) "-q" } type="search" name="q" value=(value)
                placeholder="Find a player" autocomplete="off" autocapitalize="off"
                spellcheck="false" maxlength="64" required;
            button type="submit" { "Search" }
        }
    }
}

fn player_href(username: &str) -> String {
    format!("/player/{}", segment(username))
}

fn board_href(kind: &str, stat: &str, window: Window) -> String {
    let base = format!("/leaderboards/{}/{}", segment(kind), segment(stat));
    match window {
        Window::All => base,
        other => format!("{base}?window={}", other.as_str()),
    }
}

fn window_label(window: Window) -> &'static str {
    match window {
        Window::All => "All time",
        Window::Month => "This month",
        Window::Week => "This week",
    }
}

/// The stat a kind is ranked by on the home page: wins, then kills, then
/// games, then whatever comes first.
#[must_use]
pub fn headline_stat(stats: &[String]) -> Option<&str> {
    ["wins", "kills", "games"]
        .into_iter()
        .find(|s| stats.iter().any(|t| t == s))
        .or_else(|| stats.first().map(String::as_str))
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn is_java(edition: &str) -> bool {
    !edition.eq_ignore_ascii_case("bedrock")
}

/// A head from mc-heads.net over a letter, which shows if the image fails
/// (or for Bedrock players, who have no Java skin to look up).
fn avatar(uuid: Uuid, username: &str, java: bool, size: u32, class: &str) -> Markup {
    let letter = username
        .chars()
        .find(char::is_ascii_alphanumeric)
        .map_or('?', |c| c.to_ascii_uppercase());
    html! {
        span class={ "avatar " (class) } aria-hidden="true" {
            span.avatar-letter { (letter) }
            @if java {
                img src={ "https://mc-heads.net/avatar/" (uuid) "/" (size) } alt=""
                    width=(size) height=(size) loading="lazy" decoding="async"
                    referrerpolicy="no-referrer";
            }
        }
    }
}

fn display_name(prefix: &str, username: &str) -> Markup {
    html! {
        span.display-name {
            @if !prefix.is_empty() { span.prefix { (legacy(prefix)) } }
            span.username { (username) }
        }
    }
}

// ---------------------------------------------------------------- home

/// One kind on the home page, with its headline board if it loaded.
pub struct Preview {
    pub kind: BoardKind,
    pub board: Option<Leaderboard>,
}

#[must_use]
pub fn home(site: Site, summary: Option<&NetworkSummary>, previews: &[Preview]) -> Markup {
    let body = html! {
        section.hero {
            p.eyebrow { "Player stats" }
            h1.hero-title { (site.name) }
            p.hero-lede { "Profiles, game history and leaderboards for everyone on the network." }
            (search_form("hero-search", ""))
            @if let Some(summary) = summary {
                dl.tiles {
                    div.tile {
                        dt { "Online now" }
                        dd { span.live-dot aria-hidden="true" {} (number(i64::from(summary.online))) }
                    }
                    div.tile {
                        dt { "Players" }
                        dd { (number(i64::try_from(summary.players).unwrap_or(i64::MAX))) }
                    }
                    div.tile {
                        dt { "Games played" }
                        dd { (number(i64::try_from(summary.games).unwrap_or(i64::MAX))) }
                    }
                }
            }
        }
        @if !previews.is_empty() {
            section aria-labelledby="top-heading" {
                div.section-head {
                    h2 #top-heading { "Top players" }
                    a.more href="/leaderboards" { "All leaderboards →" }
                }
                div.grid.previews {
                    @for preview in previews { (preview_card(preview)) }
                }
            }
        }
    };
    layout(site, None, Nav::Home, body)
}

fn preview_card(preview: &Preview) -> Markup {
    let kind = &preview.kind;
    let stat = preview
        .board
        .as_ref()
        .map(|b| b.stat.as_str())
        .or_else(|| headline_stat(&kind.stats));
    html! {
        article.card.preview {
            header.card-head {
                h3 { (kind_label(&kind.kind)) }
                @if let Some(stat) = stat { span.chip { (stat_label(stat)) } }
            }
            @match &preview.board {
                Some(board) if !board.entries.is_empty() => {
                    ol.mini-board {
                        @for entry in &board.entries {
                            li class={ "pos-" (entry.position) } {
                                span.pos { (entry.position) }
                                a.who href=(player_href(&entry.username)) {
                                    (display_name(&entry.prefix, &entry.username))
                                }
                                span.value { (stat_value(&board.stat, entry.value)) }
                            }
                        }
                    }
                }
                Some(_) => p.muted { "Nobody on this board yet." }
                None => p.muted { "This board didn't load. Try again in a moment." }
            }
            @if let Some(stat) = stat {
                a.card-link href=(board_href(&kind.kind, stat, Window::All)) {
                    "Full board" span.visually-hidden { ": " (kind_label(&kind.kind)) " " (stat_label(stat)) } " →"
                }
            }
        }
    }
}

// ---------------------------------------------------------------- search

#[must_use]
pub fn search(site: Site, query: &str, matches: &[PlayerMatch]) -> Markup {
    let body = html! {
        section.page-head {
            h1 { "Search" }
            (search_form("page-search", query))
        }
        @if matches.is_empty() {
            div.empty {
                p { "No players found for " strong { "“" (query) "”" } "." }
                p.muted { "Names match from the start. Bedrock names begin with a dot." }
            }
        } @else {
            p.muted { (matches.len()) " " @if matches.len() == 1 { "player" } @else { "players" } " starting with " strong { "“" (query) "”" } }
            ul.results {
                @for m in matches {
                    li {
                        a href=(player_href(&m.username)) {
                            (avatar(m.uuid, &m.username, !m.username.starts_with('.'), 32, "avatar-sm"))
                            span.username { (m.username) }
                        }
                    }
                }
            }
        }
    };
    layout(site, Some("Search"), Nav::None, body)
}

// ---------------------------------------------------------------- player

#[must_use]
pub fn player(site: Site, now_ms: u64, profile: &PlayerProfile) -> Markup {
    let java = is_java(&profile.edition);
    let total = |stat: &str| -> i64 {
        profile
            .kinds
            .values()
            .filter_map(|stats| stats.get(stat))
            .sum()
    };
    let play_time = total("play_time_s");
    let games = total("games");
    let wins = total("wins");
    let rank_title = strip_legacy(&profile.prefix);
    let rank_title = rank_title.trim();
    let body = html! {
        section.profile-head {
            (avatar(profile.uuid, &profile.username, java, 96, "avatar-lg"))
            div.profile-id {
                h1.profile-name { (display_name(&profile.prefix, &profile.username)) }
                p.profile-meta {
                    @if profile.online {
                        span.status.online { span.dot aria-hidden="true" {} "Online now" }
                    } @else {
                        span.status { span.dot aria-hidden="true" {} "Offline" }
                    }
                    span.badge { @if java { "Java" } @else { "Bedrock" } }
                    @if let Some(rank) = &profile.rank {
                        @if rank_title.is_empty() { span.badge { (rank) } }
                    }
                }
                dl.seen {
                    div { dt { "First seen" } dd { (relative(now_ms, profile.first_seen_ms)) } }
                    div {
                        dt { "Last seen" }
                        dd { @if profile.online { "now" } @else { (relative(now_ms, profile.last_seen_ms)) } }
                    }
                }
            }
        }
        dl.tiles.profile-tiles {
            div.tile { dt { "Play time" } dd { (duration(play_time)) } }
            div.tile { dt { "Games" } dd { (number(games)) } }
            div.tile { dt { "Wins" } dd { (number(wins)) } }
        }
        @if profile.kinds.is_empty() {
            div.empty { p { (profile.username) " has no stats yet." } }
        } @else {
            section aria-labelledby="stats-heading" {
                h2 #stats-heading { "Stats" }
                div.grid.kinds {
                    @for (kind, stats) in kinds_by_games(&profile.kinds) { (kind_card(kind, stats)) }
                }
            }
        }
        section aria-labelledby="games-heading" {
            h2 #games-heading { "Recent games" }
            @if profile.recent_games.is_empty() {
                p.muted { "No games yet." }
            } @else {
                ol.games {
                    @for game in &profile.recent_games { (game_row(now_ms, game)) }
                }
            }
        }
    };
    layout(site, Some(&profile.username), Nav::None, body)
}

/// Kinds with the most games first; kinds without games (hubs) last.
fn kinds_by_games(
    kinds: &BTreeMap<String, BTreeMap<String, i64>>,
) -> Vec<(&String, &BTreeMap<String, i64>)> {
    let mut list: Vec<_> = kinds.iter().collect();
    list.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.get("games").copied().unwrap_or(0)));
    list
}

/// K/D and win rate, when their parts exist.
fn derived(stats: &BTreeMap<String, i64>) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let (Some(&wins), Some(&games)) = (stats.get("wins"), stats.get("games"))
        && games > 0
    {
        #[allow(clippy::cast_precision_loss)]
        let rate = wins as f64 * 100.0 / games as f64;
        out.push(("Win rate", format!("{rate:.0}%")));
    }
    if let (Some(&kills), Some(&deaths)) = (stats.get("kills"), stats.get("deaths")) {
        #[allow(clippy::cast_precision_loss)]
        let kd = kills as f64 / deaths.max(1) as f64;
        out.push(("K/D", format!("{kd:.2}")));
    }
    out
}

fn sorted_stats(stats: &BTreeMap<String, i64>) -> Vec<(&str, i64)> {
    let mut list: Vec<(&str, i64)> = stats.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    list.sort_by(|a, b| stat_rank(a.0).cmp(&stat_rank(b.0)));
    list
}

fn kind_card(kind: &str, stats: &BTreeMap<String, i64>) -> Markup {
    let derived = derived(stats);
    html! {
        article.card.kind-card {
            header.card-head {
                h3 { (kind_label(kind)) }
            }
            @if !derived.is_empty() {
                dl.derived {
                    @for (label, value) in &derived {
                        div { dt { (label) } dd { (value) } }
                    }
                }
            }
            dl.stats {
                @for (stat, value) in sorted_stats(stats) {
                    div {
                        dt {
                            a href=(board_href(kind, stat, Window::All)) title={ (stat_label(stat)) " leaderboard" } { (stat_label(stat)) }
                        }
                        dd { (stat_value(stat, value)) }
                    }
                }
            }
        }
    }
}

fn game_row(now_ms: u64, game: &RecentGame) -> Markup {
    let length = i64::try_from(game.ended_ms.saturating_sub(game.started_ms) / 1000).unwrap_or(0);
    let (class, result) = match (game.won, game.placement) {
        (true, _) => ("won", "Won".to_owned()),
        (false, Some(p)) => ("placed", ordinal(p)),
        (false, None) => ("played", "Played".to_owned()),
    };
    let stats = sorted_stats(&game.stats);
    html! {
        li class={ "game " (class) } {
            span.result { (result) }
            div.game-main {
                p.game-title {
                    span.game-kind { (kind_label(&game.kind)) }
                    @if let Some(map) = &game.map { span.game-map { " on " (map) } }
                }
                p.game-meta {
                    @if game.won { @if let Some(p) = game.placement { span { (ordinal(p)) " place" } } }
                    @if let Some(team) = &game.team { span { "Team " (team) } }
                    span { (duration(length)) }
                    span { (game.players) @if game.players == 1 { " player" } @else { " players" } }
                }
                @if !stats.is_empty() {
                    ul.game-stats {
                        @for (stat, value) in &stats {
                            li { (stat_label(stat)) " " span.v { (stat_value(stat, *value)) } }
                        }
                    }
                }
            }
            span.game-when { (relative(now_ms, game.ended_ms)) }
        }
    }
}

// ---------------------------------------------------------------- boards

#[must_use]
pub fn boards(site: Site, kinds: &[BoardKind]) -> Markup {
    let body = html! {
        section.page-head {
            h1 { "Leaderboards" }
            p.muted { "Every game and every stat, all time, this month and this week." }
        }
        @if kinds.is_empty() {
            div.empty { p { "No leaderboards yet. They appear once games report stats." } }
        } @else {
            div.grid.kinds {
                @for kind in kinds {
                    article.card {
                        header.card-head {
                            h2.h3 { (kind_label(&kind.kind)) }
                            span.chip { (number(i64::try_from(kind.players).unwrap_or(i64::MAX))) @if kind.players == 1 { " player" } @else { " players" } }
                        }
                        ul.stat-links {
                            @for stat in sorted_names(&kind.stats) {
                                li { a href=(board_href(&kind.kind, stat, Window::All)) { (stat_label(stat)) } }
                            }
                        }
                    }
                }
            }
        }
    };
    layout(site, Some("Leaderboards"), Nav::Boards, body)
}

fn sorted_names(stats: &[String]) -> Vec<&str> {
    let mut names: Vec<&str> = stats.iter().map(String::as_str).collect();
    names.sort_by_key(|s| stat_rank(s));
    names
}

/// A full leaderboard. `stats` are the kind's other stats, to switch to.
#[must_use]
pub fn leaderboard(site: Site, board: &Leaderboard, stats: &[String]) -> Markup {
    let title = format!("{} {}", kind_label(&board.kind), stat_label(&board.stat));
    let body = html! {
        section.page-head {
            p.eyebrow { a href="/leaderboards" { "Leaderboards" } " / " (kind_label(&board.kind)) }
            h1 { (stat_label(&board.stat)) }
            @if stats.len() > 1 {
                nav.chips aria-label="Stats" {
                    @for stat in sorted_names(stats) {
                        a.chip href=(board_href(&board.kind, stat, board.window))
                            aria-current=[(stat == board.stat).then_some("page")] { (stat_label(stat)) }
                    }
                }
            }
        }
        nav.tabs aria-label="Time window" {
            @for window in [Window::All, Window::Month, Window::Week] {
                a href=(board_href(&board.kind, &board.stat, window))
                    aria-current=[(window == board.window).then_some("page")] { (window_label(window)) }
            }
        }
        @if board.entries.is_empty() {
            div.empty {
                p { @match board.window {
                    Window::All => "Nobody on this board yet.",
                    Window::Month => "Nobody has scored this month yet.",
                    Window::Week => "Nobody has scored this week yet.",
                } }
            }
        } @else {
            div.table-wrap {
                table.board {
                    caption.visually-hidden { (title) ", " (window_label(board.window)) }
                    thead { tr { th scope="col" { "#" } th scope="col" { "Player" } th.num scope="col" { (stat_label(&board.stat)) } } }
                    tbody {
                        @for entry in &board.entries {
                            tr class={ @if entry.position <= 3 { "top " } "pos-" (entry.position) } {
                                td.pos { span { (entry.position) } }
                                td {
                                    a.who href=(player_href(&entry.username)) {
                                        (avatar(entry.uuid, &entry.username, !entry.username.starts_with('.'), 32, "avatar-sm"))
                                        (display_name(&entry.prefix, &entry.username))
                                    }
                                }
                                td.num { (stat_value(&board.stat, entry.value)) }
                            }
                        }
                    }
                }
            }
            @if board.window != Window::All {
                p.muted.note { "Weeks start Monday and months on the 1st, at 00:00 UTC." }
            }
        }
    };
    layout(site, Some(&title), Nav::Boards, body)
}

// ---------------------------------------------------------------- errors

#[must_use]
pub fn not_found(site: Site, message: &str) -> Markup {
    let body = html! {
        section.problem {
            p.problem-code { "404" }
            h1 { "Nothing here" }
            p { (message) }
            (search_form("page-search", ""))
            p { a href="/" { "Back to the home page" } }
        }
    };
    layout(site, Some("Not found"), Nav::None, body)
}

#[must_use]
pub fn unavailable(site: Site) -> Markup {
    let body = html! {
        section.problem {
            p.problem-code { "503" }
            h1 { "Stats are taking a nap" }
            p { "We can't reach the stats service right now. Try again in a minute." }
            p { a href="/" { "Back to the home page" } }
        }
    };
    layout(site, Some("Unavailable"), Nav::None, body)
}

#[must_use]
pub fn slow_down(site: Site, retry_after: Option<u64>) -> Markup {
    let body = html! {
        section.problem {
            p.problem-code { "429" }
            h1 { "Slow down" }
            p {
                "That's a lot of pages in a short time. "
                @match retry_after {
                    Some(secs) => { "Try again in " (duration(i64::try_from(secs).unwrap_or(60))) "." }
                    None => "Try again in a few seconds.",
                }
            }
        }
    };
    layout(site, Some("Slow down"), Nav::None, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE: Site = Site {
        name: "Hallow",
        css: "/static/style.css",
    };
    const NOW: u64 = 1_800_000_000_000;
    const DAY: u64 = 86_400_000;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn profile() -> PlayerProfile {
        let stats = |pairs: &[(&str, i64)]| -> BTreeMap<String, i64> {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
        };
        PlayerProfile {
            uuid: uuid(1),
            username: "Steve".into(),
            edition: "java".into(),
            first_seen_ms: NOW - 20 * DAY,
            last_seen_ms: NOW - 3 * DAY,
            online: false,
            rank: Some("admin".into()),
            prefix: "&c[ADMIN] ".into(),
            kinds: [
                (
                    "test-game".to_owned(),
                    stats(&[
                        ("wins", 3),
                        ("games", 4),
                        ("kills", 9),
                        ("deaths", 4),
                        ("play_time_s", 3600),
                        ("bed_breaks", 2),
                    ]),
                ),
                (
                    "hub".to_owned(),
                    stats(&[("play_time_s", 41_400), ("joins", 12)]),
                ),
            ]
            .into_iter()
            .collect(),
            recent_games: vec![
                RecentGame {
                    id: uuid(10),
                    kind: "test-game".into(),
                    map: Some("Pumpkin Patch".into()),
                    started_ms: NOW - 2 * DAY - 492_000,
                    ended_ms: NOW - 2 * DAY,
                    players: 8,
                    won: true,
                    placement: Some(1),
                    team: Some("Red".into()),
                    stats: stats(&[("kills", 4)]),
                },
                RecentGame {
                    id: uuid(11),
                    kind: "test-game".into(),
                    map: None,
                    started_ms: NOW - 3 * DAY - 60_000,
                    ended_ms: NOW - 3 * DAY,
                    players: 8,
                    won: false,
                    placement: Some(2),
                    team: None,
                    stats: BTreeMap::new(),
                },
            ],
        }
    }

    fn board(window: Window) -> Leaderboard {
        Leaderboard {
            kind: "test-game".into(),
            stat: "play_time_s".into(),
            window,
            since_ms: None,
            entries: (1..=4)
                .map(|n| hallow_proto::stats::LeaderboardEntry {
                    position: n,
                    uuid: uuid(u128::from(n)),
                    username: format!("Player{n}"),
                    value: 50_000 - i64::from(n) * 1000,
                    rank: (n == 1).then(|| "vip".to_owned()),
                    prefix: if n == 1 {
                        "&6[VIP] ".into()
                    } else {
                        String::new()
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn player_page() {
        let html = player(SITE, NOW, &profile()).into_string();
        assert!(html.contains("<title>Steve · Hallow</title>"));
        assert!(html.contains(r#"<span class="mc-c">[ADMIN] </span>"#));
        assert!(
            html.contains("https://mc-heads.net/avatar/00000000-0000-0000-0000-000000000001/96")
        );
        assert!(html.contains("Offline"));
        assert!(html.contains("20 days ago"));
        assert!(html.contains("3 days ago"));
        // 3600 + 41400 seconds over both kinds.
        assert!(html.contains("12h 30m"));
        assert!(html.contains("Test Game"));
        assert!(html.contains("Play time"));
        assert!(html.contains("Bed breaks"));
        assert!(html.contains("Win rate"));
        assert!(html.contains("75%"));
        assert!(html.contains("K/D"));
        assert!(html.contains("2.25"));
        // The hub has no wins, games, kills or deaths: no derived stats.
        assert_eq!(html.matches("Win rate").count(), 1);
        assert!(html.contains(r#"href="/leaderboards/test-game/wins""#));
        // Recent games.
        assert!(html.contains("Pumpkin Patch"));
        assert!(html.contains("Team Red"));
        assert!(html.contains("8m 12s"));
        assert!(html.contains("2 days ago"));
        assert!(html.contains("2nd"));
        assert!(html.contains("1st place"));
    }

    #[test]
    fn bedrock_and_online_players() {
        let mut p = profile();
        p.edition = "bedrock".into();
        p.username = ".Bedrock Guy".into();
        p.online = true;
        p.prefix = String::new();
        p.kinds.clear();
        p.recent_games.clear();
        let html = player(SITE, NOW, &p).into_string();
        assert!(!html.contains("mc-heads.net"));
        assert!(html.contains(r#"<span class="avatar-letter">B</span>"#));
        assert!(html.contains("Online now"));
        assert!(html.contains("Bedrock"));
        assert!(html.contains("has no stats yet"));
        assert!(html.contains("No games yet"));
        // No prefix, so the rank shows as a badge.
        assert!(html.contains(r#"<span class="badge">admin</span>"#));
    }

    #[test]
    fn player_text_is_escaped() {
        let mut p = profile();
        p.username = "<img src=x>".into();
        p.recent_games[0].map = Some("<script>".into());
        let html = player(SITE, NOW, &p).into_string();
        assert!(!html.contains("<img src=x>"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn leaderboard_page() {
        let stats = vec!["wins".to_owned(), "play_time_s".to_owned()];
        let html = leaderboard(SITE, &board(Window::Month), &stats).into_string();
        assert!(html.contains("<title>Test Game Play time · Hallow</title>"));
        assert!(html.contains(r#"class="top pos-1""#));
        assert!(html.contains(r#"class="top pos-3""#));
        assert!(html.contains(r#"<tr class="pos-4">"#));
        assert!(html.contains(r#"<span class="mc-6">[VIP] </span>"#));
        assert!(html.contains(r#"href="/player/Player2""#));
        assert!(html.contains("13h 20m"));
        // The month tab is current; the stat chips keep the window.
        assert!(html.contains(r#"href="/leaderboards/test-game/play_time_s?window=month" aria-current="page">This month"#));
        assert!(html.contains(r#"href="/leaderboards/test-game/wins?window=month""#));
        assert!(html.contains("00:00 UTC"));

        let mut empty = board(Window::Week);
        empty.entries.clear();
        let html = leaderboard(SITE, &empty, &stats).into_string();
        assert!(html.contains("Nobody has scored this week yet."));
    }

    #[test]
    fn home_page() {
        let summary = NetworkSummary {
            online: 12,
            players: 3400,
            games: 15_000,
        };
        let kind = BoardKind {
            kind: "test-game".into(),
            stats: vec!["games".into(), "kills".into(), "wins".into()],
            players: 40,
        };
        let mut top = board(Window::All);
        top.stat = "wins".into();
        let previews = [
            Preview {
                kind: kind.clone(),
                board: Some(top),
            },
            Preview {
                kind: BoardKind {
                    kind: "hub".into(),
                    ..kind
                },
                board: None,
            },
        ];
        let html = home(SITE, Some(&summary), &previews).into_string();
        assert!(html.contains("<title>Hallow · Stats and leaderboards</title>"));
        assert!(html.contains("3,400"));
        assert!(html.contains("15,000"));
        assert!(html.contains(r#"href="/leaderboards/test-game/wins""#));
        assert!(html.contains("49,000"));
        assert!(html.contains("didn't load"));
        assert!(html.contains(r#"action="/search""#));
    }

    #[test]
    fn headline_stats() {
        let names = |s: &[&str]| s.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            headline_stat(&names(&["games", "kills", "wins"])),
            Some("wins")
        );
        assert_eq!(headline_stat(&names(&["games", "kills"])), Some("kills"));
        assert_eq!(headline_stat(&names(&["joins", "games"])), Some("games"));
        assert_eq!(
            headline_stat(&names(&["joins", "play_time_s"])),
            Some("joins")
        );
        assert_eq!(headline_stat(&[]), None);
    }

    #[test]
    fn search_and_errors() {
        let matches = [PlayerMatch {
            uuid: uuid(1),
            username: "Steve".into(),
        }];
        let html = search(SITE, "Ste", &matches).into_string();
        assert!(html.contains(r#"href="/player/Steve""#));
        assert!(html.contains(r#"value="Ste""#));
        let html = search(SITE, "<b>", &[]).into_string();
        assert!(html.contains("No players found"));
        assert!(!html.contains("<b>"));
        assert!(
            not_found(SITE, "No such player.")
                .into_string()
                .contains("No such player.")
        );
        assert!(
            unavailable(SITE)
                .into_string()
                .contains("Unavailable · Hallow")
        );
        assert!(
            slow_down(SITE, Some(30))
                .into_string()
                .contains("Try again in 30s.")
        );
    }

    #[test]
    fn ordinals() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(22), "22nd");
        assert_eq!(ordinal(113), "113th");
    }
}
