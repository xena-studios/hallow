//! The match lifecycle as a pure state machine, stepped once a second.
//!
//! | Phase | Server state | Moves on when |
//! |---|---|---|
//! | [`Waiting`](Phase::Waiting) | `ready` | `min_players` are on: the countdown starts |
//! | [`Countdown`](Phase::Countdown) | `ready` | It reaches 0: the match starts. It goes back to waiting if players drop below `min_players`, and shortens to `full_countdown_secs` once `max_players` are on |
//! | [`InGame`](Phase::InGame) | `in_game` | One team or player is left, the time limit passes, or the game ends it |
//! | [`Ending`](Phase::Ending) | `in_game` | `ending_secs` have passed, for the result to sink in |
//! | [`Over`](Phase::Over) | `draining` | Never: Hallow's server plugin sends everyone to the lobby `hallow.toml` names, and the control plane ends the instance once it's empty |
//!
//! The matchmaker only sends players to `ready` servers, so nobody new
//! arrives once a match has started.

use crate::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Countdown,
    InGame,
    Ending,
    Over,
}

/// What a second's step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// The countdown started (or shortened) at this many seconds.
    Counting(u64),
    /// This many seconds are left.
    Tick(u64),
    /// Too few players: back to waiting.
    Cancelled,
    /// The match starts now.
    Start,
    /// The time limit passed.
    TimeUp,
    /// The ending is over: drain.
    Drain,
}

#[derive(Clone, Debug)]
pub struct Lifecycle {
    pub phase: Phase,
    /// Seconds left in the countdown or the ending.
    pub left: u64,
    /// Seconds since the match started.
    pub played: u64,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            phase: Phase::Waiting,
            left: 0,
            played: 0,
        }
    }
}

impl Lifecycle {
    /// One second passes with `players` waiting (before the match) or on
    /// the server.
    pub fn second(&mut self, players: usize, settings: &Settings) -> Step {
        let full = players >= settings.max_players;
        match self.phase {
            Phase::Waiting if players >= settings.min_players.max(1) => {
                self.phase = Phase::Countdown;
                self.left = if full {
                    settings.full_countdown_secs
                } else {
                    settings.countdown_secs
                };
                self.started_or_counting()
            }
            Phase::Waiting | Phase::Over => Step::Nothing,
            Phase::Countdown if players < settings.min_players.max(1) => {
                self.phase = Phase::Waiting;
                Step::Cancelled
            }
            Phase::Countdown if full && self.left > settings.full_countdown_secs => {
                self.left = settings.full_countdown_secs;
                self.started_or_counting()
            }
            Phase::Countdown => {
                self.left = self.left.saturating_sub(1);
                if self.left == 0 {
                    self.start()
                } else {
                    Step::Tick(self.left)
                }
            }
            Phase::InGame => {
                self.played += 1;
                if settings
                    .length_secs
                    .is_some_and(|length| self.played >= length)
                {
                    Step::TimeUp
                } else {
                    Step::Nothing
                }
            }
            Phase::Ending => {
                self.left = self.left.saturating_sub(1);
                if self.left == 0 {
                    self.phase = Phase::Over;
                    Step::Drain
                } else {
                    Step::Nothing
                }
            }
        }
    }

    fn started_or_counting(&mut self) -> Step {
        if self.left == 0 {
            self.start()
        } else {
            Step::Counting(self.left)
        }
    }

    fn start(&mut self) -> Step {
        self.phase = Phase::InGame;
        self.played = 0;
        Step::Start
    }

    /// The match ends now; the ending runs `ending_secs` (at least one).
    pub fn end(&mut self, settings: &Settings) {
        if matches!(self.phase, Phase::Ending | Phase::Over) {
            return;
        }
        self.phase = Phase::Ending;
        self.left = settings.ending_secs.max(1);
    }

    /// Seconds left in the match, with a time limit.
    #[must_use]
    pub fn time_left(&self, settings: &Settings) -> Option<u64> {
        settings
            .length_secs
            .map(|length| length.saturating_sub(self.played))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            min_players: 2,
            max_players: 4,
            countdown_secs: 5,
            full_countdown_secs: 2,
            length_secs: Some(3),
            ending_secs: 2,
            ..Settings::new("Test")
        }
    }

    #[test]
    fn waits_counts_down_plays_and_drains() {
        let settings = settings();
        let mut life = Lifecycle::default();
        assert_eq!(life.second(1, &settings), Step::Nothing);
        assert_eq!(life.second(2, &settings), Step::Counting(5));
        for left in (1..5).rev() {
            assert_eq!(life.second(2, &settings), Step::Tick(left));
        }
        assert_eq!(life.second(2, &settings), Step::Start);
        assert_eq!(life.phase, Phase::InGame);
        assert_eq!(life.second(2, &settings), Step::Nothing);
        assert_eq!(life.time_left(&settings), Some(2));
        assert_eq!(life.second(2, &settings), Step::Nothing);
        assert_eq!(life.second(2, &settings), Step::TimeUp);
        life.end(&settings);
        assert_eq!(life.phase, Phase::Ending);
        assert_eq!(life.second(0, &settings), Step::Nothing);
        assert_eq!(life.second(0, &settings), Step::Drain);
        assert_eq!(life.phase, Phase::Over);
        assert_eq!(life.second(0, &settings), Step::Nothing);
    }

    #[test]
    fn cancels_and_shortens_the_countdown() {
        let settings = settings();
        let mut life = Lifecycle::default();
        assert_eq!(life.second(2, &settings), Step::Counting(5));
        assert_eq!(life.second(1, &settings), Step::Cancelled);
        assert_eq!(life.phase, Phase::Waiting);
        assert_eq!(life.second(3, &settings), Step::Counting(5));
        assert_eq!(life.second(4, &settings), Step::Counting(2));
        assert_eq!(life.second(4, &settings), Step::Tick(1));
        assert_eq!(life.second(4, &settings), Step::Start);
    }

    #[test]
    fn starts_at_once_without_a_countdown() {
        let settings = Settings {
            countdown_secs: 0,
            ..settings()
        };
        let mut life = Lifecycle::default();
        assert_eq!(life.second(2, &settings), Step::Start);
        // Ending twice doesn't restart the ending.
        life.end(&settings);
        life.left = 1;
        life.end(&settings);
        assert_eq!(life.left, 1);
    }
}
