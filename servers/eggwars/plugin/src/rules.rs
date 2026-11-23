//! EggWars' rules on top of the shared roster: eggs, when a death is
//! final, sudden death and the timings. Pure.

use hallow_islands::roster::{Death, Life, Roster};
use uuid::Uuid;

/// Seconds a dead player waits before respawning.
pub const RESPAWN_SECS: u64 = 5;
/// From the start: every egg cracks.
pub const EGGS_CRACK_SECS: u64 = 20 * 60;
/// From the start: the match is a draw.
pub const DRAW_SECS: u64 = 30 * 60;
/// From the end to sending everyone back to the lobby.
pub const OVER_SECS: u64 = 8;
/// The countdown, and the countdown once full.
pub const COUNTDOWN_SECS: u64 = 20;
pub const FULL_COUNTDOWN_SECS: u64 = 6;

/// The teams' eggs and the roster.
#[derive(Clone, Debug)]
pub struct Rules {
    pub roster: Roster,
    /// Per team: the egg still stands.
    pub eggs: Vec<bool>,
}

/// What breaking an egg did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EggBreak {
    /// Not an egg that can be broken by this player.
    No(&'static str),
    /// The team's egg is gone; these players were waiting to respawn and
    /// now are out.
    Broken { team: usize, out: Vec<Uuid> },
}

impl Rules {
    #[must_use]
    pub fn new(teams: usize, team_size: usize) -> Self {
        Self {
            roster: Roster::new(teams, team_size),
            eggs: vec![true; teams],
        }
    }

    /// When the match starts: teams nobody joined have no egg.
    pub fn start(&mut self) {
        for team in 0..self.eggs.len() {
            if self.roster.team(team).is_empty() {
                self.eggs[team] = false;
            }
        }
    }

    /// `by` breaks `team`'s egg.
    pub fn break_egg(&mut self, team: usize, by: Option<Uuid>) -> EggBreak {
        if !self.eggs.get(team).copied().unwrap_or(false) {
            return EggBreak::No("That egg is already gone.");
        }
        if let Some(by) = by {
            match self.roster.get(by) {
                Some(member) if member.team == team => {
                    return EggBreak::No("You can't break your own egg!");
                }
                Some(member) if member.life != Life::Alive => {
                    return EggBreak::No("You're not in the match.");
                }
                None => return EggBreak::No("You're not in the match."),
                Some(_) => {}
            }
        }
        self.eggs[team] = false;
        let out = self.roster.cancel_respawns(team);
        EggBreak::Broken { team, out }
    }

    /// A player died at second `now`; final when their egg is gone.
    pub fn death(&mut self, victim: Uuid, killer: Option<Uuid>, now: u64) -> Option<Death> {
        let team = self.roster.team_of(victim)?;
        let respawn = self.eggs[team].then_some(RESPAWN_SECS);
        self.roster.death(victim, killer, now, respawn)
    }

    /// Every egg still standing cracks.
    pub fn crack_all(&mut self) -> Vec<(usize, Vec<Uuid>)> {
        let standing: Vec<usize> = (0..self.eggs.len()).filter(|t| self.eggs[*t]).collect();
        standing
            .into_iter()
            .map(|t| {
                self.eggs[t] = false;
                (t, self.roster.cancel_respawns(t))
            })
            .collect()
    }

    /// The winner, once at most one team is left: `Some(Some(team))`, or
    /// `Some(None)` if nobody is.
    #[must_use]
    pub fn result(&self) -> Option<Option<usize>> {
        match self.roster.teams_in().as_slice() {
            [] => Some(None),
            [team] => Some(Some(*team)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn two_teams() -> Rules {
        let mut rules = Rules::new(4, 2);
        rules.roster.join(id(1), "a");
        rules.roster.join(id(2), "b");
        rules.start();
        rules
    }

    #[test]
    fn empty_teams_start_without_eggs() {
        let rules = two_teams();
        assert_eq!(rules.eggs, [true, true, false, false]);
    }

    #[test]
    fn players_respawn_until_their_egg_breaks() {
        let mut rules = two_teams();
        let death = rules.death(id(2), Some(id(1)), 10).unwrap();
        assert!(!death.final_death);
        assert_eq!(rules.result(), None, "b respawns");
        // a breaks b's egg while b waits to respawn: b is out.
        assert_eq!(
            rules.break_egg(1, Some(id(1))),
            EggBreak::Broken {
                team: 1,
                out: vec![id(2)]
            }
        );
        assert_eq!(rules.result(), Some(Some(0)));
    }

    #[test]
    fn eggs_are_guarded() {
        let mut rules = two_teams();
        assert!(matches!(rules.break_egg(0, Some(id(1))), EggBreak::No(_)));
        assert!(matches!(rules.break_egg(2, Some(id(1))), EggBreak::No(_)));
        assert!(matches!(
            rules.break_egg(1, Some(id(1))),
            EggBreak::Broken { .. }
        ));
        let death = rules.death(id(2), None, 50).unwrap();
        assert!(death.final_death);
        assert_eq!(rules.result(), Some(Some(0)));
    }

    #[test]
    fn sudden_death_cracks_every_egg() {
        let mut rules = two_teams();
        let cracked = rules.crack_all();
        assert_eq!(cracked.len(), 2);
        assert!(rules.death(id(1), None, 1).unwrap().final_death);
        assert!(rules.death(id(2), None, 1).unwrap().final_death);
        assert_eq!(rules.result(), Some(None));
    }
}

#[cfg(test)]
mod map_tests {
    use hallow_islands::{build::audit, map::Map};

    /// Every map in `maps/` parses, has four teams of two with an egg, a
    /// shop and generators each, and is playable as built.
    #[test]
    fn every_map_is_playable() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../maps");
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let map = Map::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            let layout = map.layout();
            assert_eq!(layout.teams.len(), 4, "{}", path.display());
            assert_eq!(layout.team_size, 2);
            for team in &layout.teams {
                assert!(
                    team.egg.is_some() && team.shop.is_some(),
                    "{}",
                    path.display()
                );
                assert!(!team.generators.is_empty());
            }
            let problems = audit(&layout);
            assert!(problems.is_empty(), "{}: {problems:#?}", path.display());
            count += 1;
        }
        assert!(count >= 3, "at least three maps");
    }
}
