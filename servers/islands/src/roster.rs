//! Who plays for which team, who is still in, who killed whom, and the
//! match's phases. Pure, so both games' rules are tested off the server.
//! This is the part of `hallow-sdk`'s "teams" and "match lifecycle" the
//! island games need.

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::play::balance;

/// How long a hit counts towards a kill: a player knocked into the void
/// within this many seconds of a hit was killed by the hitter.
pub const KILL_CREDIT_SECS: u64 = 10;

/// Where a player is in the match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Life {
    /// In the waiting area or cage, or playing.
    Alive,
    /// Dead, back at `at` (the match's second).
    Respawning { at: u64 },
    /// Out of the match: eliminated, or left. Watches as a spectator.
    Out,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub team: usize,
    pub life: Life,
    /// Still on the server.
    pub online: bool,
    /// The last player to hit this one, and when.
    pub last_hit: Option<(Uuid, u64)>,
    pub kills: u32,
    pub final_kills: u32,
    pub deaths: u32,
}

/// What a death meant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Death {
    pub victim: Uuid,
    pub killer: Option<Uuid>,
    /// The victim is out of the match.
    pub final_death: bool,
}

/// The teams and their players.
#[derive(Clone, Debug, Default)]
pub struct Roster {
    team_size: usize,
    /// Team → members, in join order.
    teams: Vec<Vec<Uuid>>,
    members: BTreeMap<Uuid, Member>,
}

impl Roster {
    #[must_use]
    pub fn new(teams: usize, team_size: usize) -> Self {
        Self {
            team_size,
            teams: vec![Vec::new(); teams],
            members: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.teams.len() * self.team_size
    }

    #[must_use]
    pub fn team_count(&self) -> usize {
        self.teams.len()
    }

    /// Puts a joining player on a team, the emptiest first. A player
    /// already on the roster (back after a drop) keeps theirs. `None` when
    /// full.
    pub fn join(&mut self, id: Uuid, name: &str) -> Option<usize> {
        if let Some(member) = self.members.get_mut(&id) {
            member.online = true;
            return Some(member.team);
        }
        let sizes: Vec<usize> = self.teams.iter().map(Vec::len).collect();
        let team = balance(&sizes, self.team_size)?;
        self.teams[team].push(id);
        self.members.insert(
            id,
            Member {
                name: name.to_owned(),
                team,
                life: Life::Alive,
                online: true,
                last_hit: None,
                kills: 0,
                final_kills: 0,
                deaths: 0,
            },
        );
        Some(team)
    }

    /// Before the match: frees the player's place.
    pub fn remove(&mut self, id: Uuid) {
        if let Some(member) = self.members.remove(&id) {
            self.teams[member.team].retain(|m| *m != id);
        }
    }

    /// During the match: the player left and is out.
    pub fn leave(&mut self, id: Uuid) {
        if let Some(member) = self.members.get_mut(&id) {
            member.online = false;
            member.life = Life::Out;
        }
    }

    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<&Member> {
        self.members.get(&id)
    }

    pub fn get_mut(&mut self, id: Uuid) -> Option<&mut Member> {
        self.members.get_mut(&id)
    }

    #[must_use]
    pub fn team_of(&self, id: Uuid) -> Option<usize> {
        self.members.get(&id).map(|m| m.team)
    }

    /// The team's members.
    #[must_use]
    pub fn team(&self, team: usize) -> &[Uuid] {
        self.teams.get(team).map_or(&[], Vec::as_slice)
    }

    pub fn members(&self) -> impl Iterator<Item = (&Uuid, &Member)> {
        self.members.iter()
    }

    #[must_use]
    pub fn teammates(&self, a: Uuid, b: Uuid) -> bool {
        matches!((self.team_of(a), self.team_of(b)), (Some(x), Some(y)) if x == y)
    }

    /// Players on teams that have any.
    #[must_use]
    pub fn player_count(&self) -> usize {
        self.members.len()
    }

    /// Teams with at least one player, before the match.
    #[must_use]
    pub fn teams_with_players(&self) -> usize {
        self.teams.iter().filter(|t| !t.is_empty()).count()
    }

    /// `attacker` hit `victim` at second `now`. Teammates don't count.
    pub fn hit(&mut self, victim: Uuid, attacker: Uuid, now: u64) {
        if victim == attacker || self.teammates(victim, attacker) {
            return;
        }
        if let Some(member) = self.members.get_mut(&victim) {
            member.last_hit = Some((attacker, now));
        }
    }

    /// Records a death at second `now`. `killer` is who Pumpkin says killed
    /// them, if anyone; otherwise the last hitter within
    /// [`KILL_CREDIT_SECS`] gets it. With `respawn`, the player comes back
    /// at `now + respawn` (EggWars with an egg); without, they're out.
    pub fn death(
        &mut self,
        victim: Uuid,
        killer: Option<Uuid>,
        now: u64,
        respawn: Option<u64>,
    ) -> Option<Death> {
        let member = self.members.get_mut(&victim)?;
        if member.life != Life::Alive {
            return None;
        }
        let credited = killer.or_else(|| {
            member
                .last_hit
                .filter(|(_, at)| now.saturating_sub(*at) <= KILL_CREDIT_SECS)
                .map(|(by, _)| by)
        });
        member.last_hit = None;
        member.deaths += 1;
        member.life = match respawn {
            Some(after) => Life::Respawning { at: now + after },
            None => Life::Out,
        };
        let final_death = respawn.is_none();
        let killer = credited.filter(|k| *k != victim && !self.teammates(*k, victim));
        if let Some(killer) = killer.and_then(|k| self.members.get_mut(&k)) {
            killer.kills += 1;
            if final_death {
                killer.final_kills += 1;
            }
        }
        Some(Death {
            victim,
            killer,
            final_death,
        })
    }

    /// Players due back at second `now`; they're alive again.
    pub fn respawns(&mut self, now: u64) -> Vec<Uuid> {
        let mut back = Vec::new();
        for (id, member) in &mut self.members {
            if let Life::Respawning { at } = member.life
                && at <= now
            {
                member.life = if member.online { Life::Alive } else { Life::Out };
                if member.online {
                    back.push(*id);
                }
            }
        }
        back
    }

    /// Respawning players are out after all (their egg broke meanwhile).
    pub fn cancel_respawns(&mut self, team: usize) -> Vec<Uuid> {
        let mut out = Vec::new();
        for id in self.team(team).to_vec() {
            if let Some(member) = self.members.get_mut(&id)
                && matches!(member.life, Life::Respawning { .. })
            {
                member.life = Life::Out;
                out.push(id);
            }
        }
        out
    }

    /// Whether the team still has a player in the match (alive or about
    /// to respawn).
    #[must_use]
    pub fn team_in(&self, team: usize) -> bool {
        self.team(team).iter().any(|id| {
            self.members
                .get(id)
                .is_some_and(|m| m.online && m.life != Life::Out)
        })
    }

    /// Teams still in the match.
    #[must_use]
    pub fn teams_in(&self) -> Vec<usize> {
        (0..self.teams.len()).filter(|t| self.team_in(*t)).collect()
    }

    /// Players still in the match.
    #[must_use]
    pub fn players_in(&self) -> usize {
        self.members
            .values()
            .filter(|m| m.online && m.life != Life::Out)
            .count()
    }
}

/// The match's phases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for players.
    Waiting,
    /// Enough players: starting at the second `starts`.
    Countdown { starts: u64 },
    /// Playing since the second `since`.
    Playing { since: u64 },
    /// Over; the server drains at the second `drains`.
    Over { drains: u64 },
}

/// When a match starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Start {
    /// Players needed to count down.
    pub min_players: usize,
    /// The countdown, in seconds.
    pub countdown: u64,
    /// The countdown once the match is full.
    pub full_countdown: u64,
}

impl Start {
    /// The phase after the player count changed (or a second passed), at
    /// second `now`, with `players` waiting of `capacity`, on `teams`
    /// teams. A countdown needs two teams; it stops if players leave, and
    /// shortens once the match is full.
    #[must_use]
    pub fn next(self, phase: Phase, now: u64, players: usize, capacity: usize, teams: usize) -> Phase {
        let enough = players >= self.min_players.max(2) && teams >= 2;
        match phase {
            Phase::Waiting if enough => Phase::Countdown {
                starts: now + if players >= capacity { self.full_countdown } else { self.countdown },
            },
            Phase::Countdown { .. } if !enough => Phase::Waiting,
            Phase::Countdown { starts } if players >= capacity => Phase::Countdown {
                starts: starts.min(now + self.full_countdown),
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn joins_fill_teams_evenly() {
        let mut roster = Roster::new(4, 2);
        assert_eq!(roster.join(id(1), "a"), Some(0));
        assert_eq!(roster.join(id(2), "b"), Some(1));
        assert_eq!(roster.join(id(1), "a"), Some(0), "rejoin keeps the team");
        roster.remove(id(2));
        assert_eq!(roster.join(id(3), "c"), Some(1));
        assert_eq!(roster.teams_with_players(), 2);
    }

    #[test]
    fn void_deaths_credit_the_last_hitter() {
        let mut roster = Roster::new(2, 1);
        roster.join(id(1), "a");
        roster.join(id(2), "b");
        roster.hit(id(2), id(1), 100);
        let death = roster.death(id(2), None, 105, None).unwrap();
        assert_eq!(death.killer, Some(id(1)));
        assert!(death.final_death);
        assert_eq!(roster.get(id(1)).unwrap().final_kills, 1);
        assert_eq!(roster.teams_in(), vec![0]);
        // A dead player can't die again.
        assert_eq!(roster.death(id(2), None, 106, None), None);
    }

    #[test]
    fn old_hits_and_teammates_get_no_credit() {
        let mut roster = Roster::new(2, 2);
        roster.join(id(1), "a");
        roster.join(id(2), "b");
        roster.join(id(3), "c"); // team 0 with a
        roster.hit(id(2), id(1), 100);
        assert_eq!(roster.death(id(2), None, 200, Some(5)).unwrap().killer, None);
        roster.hit(id(3), id(1), 10);
        assert_eq!(roster.get(id(3)).unwrap().last_hit, None);
    }

    #[test]
    fn respawns_come_back_unless_their_egg_broke() {
        let mut roster = Roster::new(2, 2);
        roster.join(id(1), "a");
        roster.join(id(2), "b");
        roster.join(id(3), "c");
        roster.death(id(1), Some(id(2)), 10, Some(5));
        roster.death(id(3), None, 11, Some(5));
        assert!(roster.team_in(0));
        assert_eq!(roster.respawns(15), vec![id(1)]);
        assert_eq!(roster.cancel_respawns(0), vec![id(3)]);
        assert_eq!(roster.get(id(3)).unwrap().life, Life::Out);
        assert!(roster.team_in(0), "a is back");
        roster.leave(id(1));
        assert!(!roster.team_in(0));
        assert_eq!(roster.teams_in(), vec![1]);
    }

    #[test]
    fn countdown_starts_shortens_and_stops() {
        let start = Start {
            min_players: 2,
            countdown: 30,
            full_countdown: 5,
        };
        let p = start.next(Phase::Waiting, 0, 1, 8, 1);
        assert_eq!(p, Phase::Waiting);
        let p = start.next(p, 0, 2, 8, 2);
        assert_eq!(p, Phase::Countdown { starts: 30 });
        let p = start.next(p, 10, 8, 8, 4);
        assert_eq!(p, Phase::Countdown { starts: 15 });
        assert_eq!(start.next(p, 11, 1, 8, 1), Phase::Waiting);
        // Two players on one team can't start.
        assert_eq!(start.next(Phase::Waiting, 0, 2, 8, 1), Phase::Waiting);
    }
}
