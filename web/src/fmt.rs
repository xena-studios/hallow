//! Turning API values into text: durations, relative times, numbers, stat
//! and kind names, and legacy Minecraft colour codes.

use maud::{Markup, html};

/// A length of time: `45s`, `8m 12s`, `12h 30m`. Hours don't roll into
/// days, so a total play time reads the way players compare it.
#[must_use]
pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    }
}

/// How long ago `then_ms` was at `now_ms`: `just now`, `5 minutes ago`,
/// `3 days ago`. A time in the future (clock skew) is `just now`.
#[must_use]
pub fn relative(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    let (n, unit) = match secs {
        0..60 => return "just now".to_owned(),
        60..3600 => (secs / 60, "minute"),
        3600..86_400 => (secs / 3600, "hour"),
        86_400..2_592_000 => (secs / 86_400, "day"),
        2_592_000..31_536_000 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

/// `1234567` as `1,234,567`.
#[must_use]
pub fn number(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Whether a stat counts seconds, shown as a duration.
#[must_use]
pub fn is_seconds(stat: &str) -> bool {
    stat.ends_with("_s")
}

/// A stat's value as shown: a duration for seconds, otherwise a number.
#[must_use]
pub fn stat_value(stat: &str, value: i64) -> String {
    if is_seconds(stat) {
        duration(value)
    } else {
        number(value)
    }
}

/// A stat's label: `play_time_s` → `Play time`, `bed_breaks` →
/// `Bed breaks`, `skywars.chests_opened` → `Skywars chests opened`.
#[must_use]
pub fn stat_label(stat: &str) -> String {
    match stat {
        "wins" => "Wins".into(),
        "kills" => "Kills".into(),
        "deaths" => "Deaths".into(),
        "games" => "Games".into(),
        "play_time_s" => "Play time".into(),
        "joins" => "Joins".into(),
        _ => {
            let base = stat.strip_suffix("_s").unwrap_or(stat);
            capitalize(&base.replace(['_', '.'], " "))
        }
    }
}

/// A server kind's name: `test-game` → `Test Game`.
#[must_use]
pub fn kind_label(kind: &str) -> String {
    kind.split(['-', '_', '.', ' '])
        .filter(|w| !w.is_empty())
        .map(capitalize)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The order stats are shown in: the ones players look for first, then the
/// rest by name, with the bookkeeping ones last.
#[must_use]
pub fn stat_rank(stat: &str) -> (u8, &str) {
    let rank = match stat {
        "wins" => 0,
        "games" => 1,
        "kills" => 2,
        "deaths" => 3,
        "play_time_s" => 8,
        "joins" => 9,
        _ => 5,
    };
    (rank, stat)
}

/// Text with legacy colour codes (`&c`, `§l`, …) as styled spans. Colours
/// `0-9a-f` reset the formats, as in game; `k-o` add one; `r` resets all.
/// Everything is escaped: this only ever adds classes.
#[must_use]
pub fn legacy(text: &str) -> Markup {
    let runs = legacy_runs(text);
    html! {
        @for run in &runs {
            @if run.classes.is_empty() {
                (run.text)
            } @else {
                span class=(run.classes) { (run.text) }
            }
        }
    }
}

/// [`legacy`] without the codes, for titles and alt text.
#[must_use]
pub fn strip_legacy(text: &str) -> String {
    legacy_runs(text).into_iter().map(|r| r.text).collect()
}

struct Run {
    classes: String,
    text: String,
}

fn legacy_runs(text: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut colour: Option<char> = None;
    let mut formats: Vec<char> = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '&' | '§')
            && let Some(&next) = chars.peek()
        {
            let code = next.to_ascii_lowercase();
            if code.is_ascii_hexdigit() || matches!(code, 'k'..='o' | 'r') {
                chars.next();
                if !current.is_empty() {
                    runs.push(Run {
                        classes: classes(colour, &formats),
                        text: std::mem::take(&mut current),
                    });
                }
                if code == 'r' {
                    colour = None;
                    formats.clear();
                } else if code.is_ascii_hexdigit() {
                    colour = Some(code);
                    formats.clear();
                } else if !formats.contains(&code) {
                    formats.push(code);
                }
                continue;
            }
        }
        current.push(c);
    }
    if !current.is_empty() {
        runs.push(Run {
            classes: classes(colour, &formats),
            text: current,
        });
    }
    runs
}

fn classes(colour: Option<char>, formats: &[char]) -> String {
    let mut out = String::new();
    for code in colour.iter().chain(formats) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("mc-");
        out.push(*code);
    }
    out
}

/// A path segment for one of this site's links (`/player/{name}`).
#[must_use]
pub fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(-5), "0s");
        assert_eq!(duration(59), "59s");
        assert_eq!(duration(492), "8m 12s");
        assert_eq!(duration(45_000), "12h 30m");
        assert_eq!(duration(400_000), "111h 6m");
    }

    #[test]
    fn relative_times() {
        let now = 1_000_000_000_000;
        assert_eq!(relative(now, now), "just now");
        assert_eq!(relative(now, now + 60_000), "just now");
        assert_eq!(relative(now, now - 59_000), "just now");
        assert_eq!(relative(now, now - 60_000), "1 minute ago");
        assert_eq!(relative(now, now - 5 * 60_000), "5 minutes ago");
        assert_eq!(relative(now, now - 3_600_000), "1 hour ago");
        assert_eq!(relative(now, now - 3 * 86_400_000), "3 days ago");
        assert_eq!(relative(now, now - 65 * 86_400_000), "2 months ago");
        assert_eq!(relative(now, now - 800 * 86_400_000), "2 years ago");
    }

    #[test]
    fn numbers() {
        assert_eq!(number(0), "0");
        assert_eq!(number(999), "999");
        assert_eq!(number(1000), "1,000");
        assert_eq!(number(1_234_567), "1,234,567");
        assert_eq!(number(-12_345), "-12,345");
    }

    #[test]
    fn labels() {
        assert_eq!(stat_label("play_time_s"), "Play time");
        assert_eq!(stat_label("wins"), "Wins");
        assert_eq!(stat_label("bed_breaks"), "Bed breaks");
        assert_eq!(stat_label("skywars.chests_opened"), "Skywars chests opened");
        assert_eq!(stat_label("longest_life_s"), "Longest life");
        assert_eq!(kind_label("test-game"), "Test Game");
        assert_eq!(kind_label("skywars"), "Skywars");
        assert_eq!(kind_label("bed_wars.duos"), "Bed Wars Duos");
        assert_eq!(stat_value("play_time_s", 3720), "1h 2m");
        assert_eq!(stat_value("kills", 1200), "1,200");
    }

    #[test]
    fn legacy_colours() {
        assert_eq!(
            legacy("&c[ADMIN] ").into_string(),
            r#"<span class="mc-c">[ADMIN] </span>"#
        );
        assert_eq!(
            legacy("§6§lVIP&r+").into_string(),
            r#"<span class="mc-6 mc-l">VIP</span>+"#
        );
        // A colour resets formats; upper-case codes work.
        assert_eq!(
            legacy("&l&Ab").into_string(),
            r#"<span class="mc-a">b</span>"#
        );
        // Not a code: kept as text.
        assert_eq!(
            legacy("Tom & Jerry &z").into_string(),
            "Tom &amp; Jerry &amp;z"
        );
        assert_eq!(legacy("trailing&").into_string(), "trailing&amp;");
        assert_eq!(legacy("").into_string(), "");
        assert_eq!(strip_legacy("&c[ADMIN]&r "), "[ADMIN] ");
    }

    #[test]
    fn legacy_escapes_html() {
        let html = legacy("&4<script>alert('x')</script>&r\"><b>").into_string();
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<b>"));
        assert_eq!(
            html,
            r#"<span class="mc-4">&lt;script&gt;alert('x')&lt;/script&gt;</span>&quot;&gt;&lt;b&gt;"#
        );
    }

    #[test]
    fn segments() {
        assert_eq!(segment("Steve"), "Steve");
        assert_eq!(segment(".Bedrock Guy"), ".Bedrock%20Guy");
        assert_eq!(segment("a/b?c"), "a%2Fb%3Fc");
    }
}
