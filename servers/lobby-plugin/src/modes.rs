//! The lobby's settings from the environment, parsed off the server.

/// A game kind the lobby queues for, with its button label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode {
    /// What `/play` takes: the game kind.
    pub kind: String,
    pub label: String,
    /// One line about it, shown on hover.
    pub about: String,
}

/// Parses `LOBBY_MODES`: `kind=Label|About;kind=Label|About`. A missing
/// label is the kind; a missing about is empty.
#[must_use]
pub fn parse(spec: &str) -> Vec<Mode> {
    spec.split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (kind, rest) = part.split_once('=').unwrap_or((part, part));
            let (label, about) = rest.split_once('|').unwrap_or((rest, ""));
            Mode {
                kind: kind.trim().to_ascii_lowercase(),
                label: label.trim().to_owned(),
                about: about.trim().to_owned(),
            }
        })
        .filter(|mode| !mode.kind.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse() {
        let modes = parse("skywars=Solo|Every player for themselves; skywars-teams=Teams|Pairs ;;");
        assert_eq!(modes.len(), 2);
        assert_eq!(modes[0].kind, "skywars");
        assert_eq!(modes[0].label, "Solo");
        assert_eq!(modes[1].about, "Pairs");
        assert_eq!(parse("eggwars")[0].label, "eggwars");
        assert!(parse("").is_empty());
    }
}
