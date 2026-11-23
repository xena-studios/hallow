//! What a banned player reads, as legacy `§` text. Pure, so it is tested on
//! the host.

use hallow_proto::punishment::{BanInfo, IssuerKind};

/// `1d 4h 3m 2s`, `59m 58s`, `12s`: every unit from the largest that isn't
/// zero, for the live countdown.
#[must_use]
pub fn countdown(seconds: u64) -> String {
    let (d, h, m, s) = (
        seconds / 86_400,
        (seconds / 3600) % 24,
        (seconds / 60) % 60,
        seconds % 60,
    );
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m {s}s"),
        (0, _, _) => format!("{h}h {m}m {s}s"),
        _ => format!("{d}d {h}h {m}m {s}s"),
    }
}

/// Unix milliseconds as `2026-11-16 18:04 UTC`.
#[must_use]
pub fn date(ms: u64) -> String {
    let secs = ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
    let (y, mo, d) = civil_from_days(days);
    let (h, mi) = ((secs / 3600) % 24, (secs / 60) % 60);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02} UTC")
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Who issued the ban, the way the panel shows it.
#[must_use]
pub fn issuer(ban: &BanInfo) -> String {
    match ban.issuer_kind {
        IssuerKind::Player => format!("{} §7(staff, in game)", ban.issuer),
        IssuerKind::Staff => format!("{} §7(panel staff)", ban.issuer),
        IssuerKind::Console => "the console".to_owned(),
        IssuerKind::System => "the network".to_owned(),
        IssuerKind::Anticheat => "the anticheat".to_owned(),
    }
}

/// Seconds left on a temporary ban at `now_ms`, `None` for a permanent one.
#[must_use]
pub fn left(ban: &BanInfo, now_ms: u64) -> Option<u64> {
    ban.expires_ms.map(|end| end.saturating_sub(now_ms) / 1000)
}

/// The chat block shown on arrival and for `/help` and `/appeal`, one line
/// each. The appeal URL is added by the caller as a clickable line.
#[must_use]
pub fn ban_lines(ban: &BanInfo, now_ms: u64) -> Vec<String> {
    let mut lines = vec![
        "§8§m                                        ".to_owned(),
        "§c§lYou are banned from this network.".to_owned(),
        format!("§7Reason: §f{}", ban.reason),
        format!("§7Issued by: §f{}", issuer(ban)),
        format!("§7Issued: §f{}", date(ban.created_ms)),
    ];
    lines.push(match (ban.expires_ms, left(ban, now_ms)) {
        (Some(end), Some(secs)) => {
            format!("§7Ends: §f{} §7(in §f{}§7)", date(end), countdown(secs))
        }
        _ => "§7Ends: §fnever §7(permanent)".to_owned(),
    });
    lines.push(format!("§7Punishment ID: §f#{}", ban.id));
    match &ban.appeal {
        Some(appeal) => lines.push(format!("§7Appeal: §f{appeal}")),
        None if ban.appeal_url.is_none() => {
            lines.push("§7To appeal, contact the staff with your punishment ID.".to_owned());
        }
        None => {}
    }
    lines
}

/// The action bar, every second.
#[must_use]
pub fn action_bar(ban: &BanInfo, now_ms: u64) -> String {
    match left(ban, now_ms) {
        Some(0) => ENDED.to_owned(),
        Some(secs) => format!("§cBan ends in §f{}", countdown(secs)),
        None => format!("§cPermanently banned §8| §7ID §f#{}", ban.id),
    }
}

pub const ENDED: &str = "§aYour ban has ended. Sending you to the hub…";
pub const LIFTED: &str = "§aYour ban was lifted. Sending you to the hub…";
pub const WAITING: &str = "§7Looking up your ban…";
pub const NO_CHAT: &str = "§cChat is off here. §7Type §f/appeal §7to see how to appeal.";
pub const NO_COMMANDS: &str =
    "§cYou can't use commands while banned. §7Only §f/help §7and §f/appeal §7work here.";

#[cfg(test)]
mod tests {
    use super::*;

    fn ban(expires_ms: Option<u64>) -> BanInfo {
        BanInfo {
            id: 42,
            reason: "Cheating".into(),
            issuer: "mod_alex".into(),
            issuer_kind: IssuerKind::Staff,
            created_ms: 1_763_316_240_000,
            expires_ms,
            appeal: None,
            appeal_url: None,
        }
    }

    #[test]
    fn countdowns() {
        assert_eq!(countdown(12), "12s");
        assert_eq!(countdown(3598), "59m 58s");
        assert_eq!(countdown(7198), "1h 59m 58s");
        assert_eq!(countdown(90_061), "1d 1h 1m 1s");
    }

    #[test]
    fn dates_are_utc() {
        assert_eq!(date(0), "1970-01-01 00:00 UTC");
        // 2025-11-16 18:04:00 UTC
        assert_eq!(date(1_763_316_240_000), "2025-11-16 18:04 UTC");
        assert_eq!(date(951_782_400_000), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn issuers_read_like_the_panel() {
        let mut b = ban(None);
        assert_eq!(issuer(&b), "mod_alex §7(panel staff)");
        b.issuer_kind = IssuerKind::Console;
        assert_eq!(issuer(&b), "the console");
    }

    #[test]
    fn the_block_says_when_it_ends() {
        let now = 1_763_316_240_000;
        let lines = ban_lines(&ban(Some(now + 7_200_000)), now);
        assert!(lines.iter().any(|l| l.contains("Cheating")));
        assert!(lines.iter().any(|l| l.contains("in §f2h 0m 0s")));
        assert!(lines.iter().any(|l| l.contains("#42")));
        assert!(lines.iter().any(|l| l.contains("contact the staff")));
        let lines = ban_lines(&ban(None), now);
        assert!(lines.iter().any(|l| l.contains("permanent")));
    }

    #[test]
    fn the_action_bar_counts_down() {
        let now = 1_000_000;
        assert_eq!(
            action_bar(&ban(Some(now + 61_500)), now),
            "§cBan ends in §f1m 1s"
        );
        assert_eq!(action_bar(&ban(Some(now - 1)), now), ENDED);
        assert!(action_bar(&ban(None), now).contains("#42"));
    }
}
