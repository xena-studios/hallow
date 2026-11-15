//! Parties in Redis.
//!
//! | Key (after the prefix) | Type | Holds |
//! |---|---|---|
//! | `party:{id}` | hash | `leader` → UUID |
//! | `party:{id}:members` | sorted set | member UUIDs scored by when they joined |
//! | `player-party` | hash | player UUID → party id |
//! | `party-invite:{id}:{uuid}` | string with TTL | an open invite to that party |
//!
//! A party lasts while its members are online: leaving the network leaves the
//! party, a leaving leader hands over to the longest member, and the last one
//! out ends it. Two members acting at the same instant could race (say, two
//! accepts into the last free slot); that's acceptable for chat-speed actions.

use hallow_proto::{
    command::ProxyCommand,
    party::{INVITE_SECONDS, MAX_PARTY_SIZE, PartyAction, PartyInfo, PartyMember, PartyReply},
};
use redis::AsyncCommands;
use uuid::Uuid;

use super::{Keys, Result, Store, now_ms};

impl Keys {
    fn party(&self, id: &str) -> String {
        format!("{}:party:{id}", self.prefix)
    }
    fn party_members(&self, id: &str) -> String {
        format!("{}:party:{id}:members", self.prefix)
    }
    fn player_party(&self) -> String {
        format!("{}:player-party", self.prefix)
    }
    fn party_invite(&self, id: &str, uuid: &str) -> String {
        format!("{}:party-invite:{id}:{uuid}", self.prefix)
    }
}

fn ok(message: impl Into<String>) -> PartyReply {
    PartyReply {
        ok: true,
        message: message.into(),
    }
}

fn refuse(message: impl Into<String>) -> PartyReply {
    PartyReply {
        ok: false,
        message: format!("§c{}", message.into()),
    }
}

/// Players can't inject colour codes into what others see.
fn plain(text: &str) -> String {
    text.replace('§', "")
}

impl Store {
    pub async fn party_action(&self, action: PartyAction) -> Result<PartyReply> {
        let player = match &action {
            PartyAction::Invite { player, .. }
            | PartyAction::Accept { player, .. }
            | PartyAction::Leave { player }
            | PartyAction::Kick { player, .. }
            | PartyAction::Disband { player }
            | PartyAction::Warp { player }
            | PartyAction::Chat { player, .. }
            | PartyAction::List { player } => *player,
        };
        let Some(me) = self.player(&player).await? else {
            return Ok(refuse("You aren't online."));
        };
        let uuid = player.to_string();
        let party = self.party_id_of(&uuid).await?;
        match action {
            PartyAction::Invite { target, .. } => {
                self.invite(&uuid, &me.username, party, &target).await
            }
            PartyAction::Accept { leader, .. } => {
                if let Some(party) = party {
                    // A party of one (invites nobody took up) gives way.
                    if self.party_member_ids(&party).await?.len() > 1 {
                        return Ok(refuse("Leave your party first."));
                    }
                    self.disband(&party).await?;
                }
                self.accept(&uuid, &me.username, &leader).await
            }
            other => {
                let Some(party) = party else {
                    return Ok(refuse("You aren't in a party."));
                };
                let leader = self.party_leader(&party).await?;
                let is_leader = leader.as_deref() == Some(uuid.as_str());
                match other {
                    PartyAction::Leave { .. } => {
                        self.remove_member(
                            &party,
                            &uuid,
                            &format!("§e{} left the party.", me.username),
                        )
                        .await?;
                        Ok(ok("§eYou left the party."))
                    }
                    PartyAction::Chat { text, .. } => {
                        let members = self.party_member_ids(&party).await?;
                        let line = format!("§9Party §8> §f{}§7: {}", me.username, plain(&text));
                        self.notify(&members, &line).await?;
                        Ok(ok(""))
                    }
                    PartyAction::List { .. } => Ok(ok(self.describe(&party).await?)),
                    _ if !is_leader => Ok(refuse("Only the party leader can do that.")),
                    PartyAction::Kick { target, .. } => self.kick(&party, &uuid, &target).await,
                    PartyAction::Disband { .. } => {
                        self.disband(&party).await?;
                        Ok(ok("§eYou disbanded the party."))
                    }
                    PartyAction::Warp { .. } => {
                        self.warp(&party, &uuid, &me.username, me.server).await
                    }
                    PartyAction::Invite { .. } | PartyAction::Accept { .. } => unreachable!(),
                }
            }
        }
    }

    /// The party `uuid` is in, members in join order.
    pub async fn party_of(&self, uuid: &Uuid) -> Result<Option<PartyInfo>> {
        let Some(id) = self.party_id_of(&uuid.to_string()).await? else {
            return Ok(None);
        };
        let Some(leader) = self.party_leader(&id).await? else {
            return Ok(None);
        };
        let mut members = Vec::new();
        for member in self.party_member_ids(&id).await? {
            let Ok(member_uuid) = Uuid::parse_str(&member) else {
                continue;
            };
            let username = match self.player(&member_uuid).await? {
                Some(location) => location.username,
                None => continue,
            };
            members.push(PartyMember {
                uuid: member_uuid,
                username,
            });
        }
        let Ok(leader) = Uuid::parse_str(&leader) else {
            return Ok(None);
        };
        Ok(Some(PartyInfo {
            id,
            leader,
            members,
        }))
    }

    /// Takes a player who left the network out of their party.
    pub(super) async fn party_on_leave(&self, uuid: &str, username: &str) -> Result<()> {
        if let Some(party) = self.party_id_of(uuid).await? {
            self.remove_member(
                &party,
                uuid,
                &format!("§e{username} disconnected and left the party."),
            )
            .await?;
        }
        Ok(())
    }

    async fn invite(
        &self,
        uuid: &str,
        name: &str,
        party: Option<String>,
        target: &str,
    ) -> Result<PartyReply> {
        let Some(them) = self.player_by_name(target).await? else {
            return Ok(refuse(format!("{target} isn't online.")));
        };
        let their_uuid = them.uuid.to_string();
        if their_uuid == uuid {
            return Ok(refuse("You can't invite yourself."));
        }
        if self.party_id_of(&their_uuid).await?.is_some() {
            return Ok(refuse(format!("{} is already in a party.", them.username)));
        }
        let party = match party {
            Some(party) => {
                if self.party_leader(&party).await?.as_deref() != Some(uuid) {
                    return Ok(refuse("Only the party leader can invite."));
                }
                if self.party_member_ids(&party).await?.len() >= MAX_PARTY_SIZE {
                    return Ok(refuse("Your party is full."));
                }
                party
            }
            None => {
                let party = Uuid::new_v4().simple().to_string();
                let _: () = self
                    .conn()
                    .hset(self.keys.party(&party), "leader", uuid)
                    .await?;
                self.add_member(&party, uuid).await?;
                party
            }
        };
        let _: () = self
            .conn()
            .set_ex(
                self.keys.party_invite(&party, &their_uuid),
                1,
                INVITE_SECONDS,
            )
            .await?;
        self.notify(
            &[their_uuid],
            &format!(
                "§e{name} invited you to their party. §7Type §f/party accept {name} §7within {INVITE_SECONDS} s to join."
            ),
        )
        .await?;
        Ok(ok(format!(
            "§eInvited {}. They have {INVITE_SECONDS} s to accept.",
            them.username
        )))
    }

    async fn accept(&self, uuid: &str, name: &str, leader: &str) -> Result<PartyReply> {
        let Some(leader) = self.player_by_name(leader).await? else {
            return Ok(refuse(format!("{leader} isn't online.")));
        };
        let Some(party) = self.party_id_of(&leader.uuid.to_string()).await? else {
            return Ok(refuse(format!("{} isn't in a party.", leader.username)));
        };
        let invite = self.keys.party_invite(&party, uuid);
        let invited: bool = self.conn().exists(&invite).await?;
        if !invited {
            return Ok(refuse(format!(
                "You have no open invite from {}.",
                leader.username
            )));
        }
        if self.party_member_ids(&party).await?.len() >= MAX_PARTY_SIZE {
            return Ok(refuse("That party is full."));
        }
        let _: () = self.conn().del(&invite).await?;
        let members = self.party_member_ids(&party).await?;
        self.add_member(&party, uuid).await?;
        self.notify(&members, &format!("§e{name} joined the party."))
            .await?;
        Ok(ok(format!("§eYou joined {}'s party.", leader.username)))
    }

    async fn kick(&self, party: &str, uuid: &str, target: &str) -> Result<PartyReply> {
        let Some(them) = self.player_by_name(target).await? else {
            return Ok(refuse(format!("{target} isn't online.")));
        };
        let their_uuid = them.uuid.to_string();
        if their_uuid == uuid {
            return Ok(refuse("Use /party leave or /party disband."));
        }
        if self.party_id_of(&their_uuid).await?.as_deref() != Some(party) {
            return Ok(refuse(format!("{} isn't in your party.", them.username)));
        }
        self.notify(
            std::slice::from_ref(&their_uuid),
            "§cYou were removed from the party.",
        )
        .await?;
        self.remove_member(
            party,
            &their_uuid,
            &format!("§e{} was removed from the party.", them.username),
        )
        .await?;
        Ok(ok(format!("§eRemoved {}.", them.username)))
    }

    async fn warp(
        &self,
        party: &str,
        uuid: &str,
        name: &str,
        server: Option<String>,
    ) -> Result<PartyReply> {
        let Some(server) = server else {
            return Ok(refuse("You aren't on a server yet."));
        };
        let mut warped = 0;
        for member in self.party_member_ids(party).await? {
            if member == uuid {
                continue;
            }
            let Ok(member_uuid) = Uuid::parse_str(&member) else {
                continue;
            };
            let Some(location) = self.player(&member_uuid).await? else {
                continue;
            };
            if location.server.as_deref() == Some(server.as_str()) {
                continue;
            }
            self.push_command(
                &location.proxy,
                &ProxyCommand::Message {
                    player: member_uuid,
                    text: format!("§eWarping to {name}'s server..."),
                    from: None,
                },
            )
            .await?;
            self.push_command(
                &location.proxy,
                &ProxyCommand::Send {
                    player: member_uuid,
                    server: server.clone(),
                },
            )
            .await?;
            warped += 1;
        }
        Ok(ok(format!(
            "§eWarped {warped} party member(s) to {server}."
        )))
    }

    async fn disband(&self, party: &str) -> Result<()> {
        let members = self.party_member_ids(party).await?;
        self.notify(&members, "§cThe party was disbanded.").await?;
        for member in &members {
            let _: () = self.conn().hdel(self.keys.player_party(), member).await?;
        }
        let _: () = self
            .conn()
            .del(&[self.keys.party(party), self.keys.party_members(party)])
            .await?;
        Ok(())
    }

    async fn describe(&self, party: &str) -> Result<String> {
        let leader = self.party_leader(party).await?.unwrap_or_default();
        let mut names = Vec::new();
        let members = self.party_member_ids(party).await?;
        for member in &members {
            let name = match Uuid::parse_str(member) {
                Ok(id) => self.player(&id).await?.map(|p| p.username),
                Err(_) => None,
            }
            .unwrap_or_else(|| "?".into());
            names.push(if *member == leader {
                format!("{name} §7(leader)§f")
            } else {
                name
            });
        }
        Ok(format!(
            "§eParty ({}/{MAX_PARTY_SIZE}): §f{}",
            members.len(),
            names.join(", ")
        ))
    }

    async fn party_id_of(&self, uuid: &str) -> Result<Option<String>> {
        Ok(self.conn().hget(self.keys.player_party(), uuid).await?)
    }

    async fn party_leader(&self, party: &str) -> Result<Option<String>> {
        Ok(self.conn().hget(self.keys.party(party), "leader").await?)
    }

    async fn party_member_ids(&self, party: &str) -> Result<Vec<String>> {
        Ok(self
            .conn()
            .zrange(self.keys.party_members(party), 0, -1)
            .await?)
    }

    async fn add_member(&self, party: &str, uuid: &str) -> Result<()> {
        let _: () = redis::pipe()
            .atomic()
            .cmd("ZADD")
            .arg(self.keys.party_members(party))
            .arg("NX")
            .arg(now_ms())
            .arg(uuid)
            .ignore()
            .hset(self.keys.player_party(), uuid, party)
            .ignore()
            .query_async(&mut self.conn())
            .await?;
        Ok(())
    }

    /// Removes a member and tells the rest with `announce`. Hands the lead
    /// over if the leader left, and ends the party when it drops below two.
    async fn remove_member(&self, party: &str, uuid: &str, announce: &str) -> Result<()> {
        let _: () = redis::pipe()
            .atomic()
            .zrem(self.keys.party_members(party), uuid)
            .ignore()
            .hdel(self.keys.player_party(), uuid)
            .ignore()
            .query_async(&mut self.conn())
            .await?;
        let members = self.party_member_ids(party).await?;
        if members.len() < 2 {
            if !members.is_empty() {
                self.notify(&members, announce).await?;
            }
            return self.disband(party).await;
        }
        self.notify(&members, announce).await?;
        if self.party_leader(party).await?.as_deref() == Some(uuid) {
            let next = &members[0];
            let _: () = self
                .conn()
                .hset(self.keys.party(party), "leader", next)
                .await?;
            let name = match Uuid::parse_str(next) {
                Ok(id) => self.player(&id).await?.map(|p| p.username),
                Err(_) => None,
            }
            .unwrap_or_else(|| "Someone".into());
            self.notify(&members, &format!("§e{name} is now the party leader."))
                .await?;
        }
        Ok(())
    }

    /// Sends `text` to each of `members` who is online, on their proxy.
    pub(super) async fn notify(&self, members: &[String], text: &str) -> Result<()> {
        for member in members {
            let Ok(uuid) = Uuid::parse_str(member) else {
                continue;
            };
            if let Some(location) = self.player(&uuid).await? {
                self.push_command(
                    &location.proxy,
                    &ProxyCommand::Message {
                        player: uuid,
                        text: text.to_owned(),
                        from: None,
                    },
                )
                .await?;
            }
        }
        Ok(())
    }
}
