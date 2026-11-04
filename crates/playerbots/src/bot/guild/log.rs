//! What a bot knows about its guild, fed by the guild packets ([`aa_proto::guild`]).
//!
//! The server tells a character about its guild with the member list at enter-world (in chunks of
//! 20), `SCUnitExpeditionChanged` when it joins or leaves, a status packet for every member that
//! joins, logs in or out, and role and owner changes. [`GuildLog::apply`] keeps the picture: the
//! id, the name, the members with their roles, the invitation waiting for an answer, and how the
//! bot last left a guild. It is plain data and a pure function of the packets, so it is tested
//! without a connection.

use aa_proto::guild::{role, GuildEvent, MemberBrief};
use smallvec::SmallVec;

/// Most members one log keeps (the member list of a guild of this size is more than a bot needs).
pub const MAX_MEMBERS: usize = 100;
/// Newcomers kept for a greeting.
const MAX_NEWCOMERS: usize = 4;

/// A member of the guild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Character id.
    pub char_id: u32,
    /// Character name.
    pub name: Box<str>,
    /// Level when the server last said.
    pub level: u8,
    /// Role, see [`role`].
    pub role: u8,
    /// Online when the server last said.
    pub online: bool,
    /// Bot clock (ms) when the bot first saw the member in this guild.
    pub since_ms: u64,
}

/// A guild invitation that has not been answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// Guild id (what the reply echoes).
    pub guild: u32,
    /// Character id of the inviter.
    pub inviter: u32,
    /// Name of the inviter.
    pub inviter_name: Box<str>,
    /// Name of the guild.
    pub guild_name: Box<str>,
    /// Bot clock (ms) when it arrived.
    pub at_ms: u64,
}

/// How the bot last lost its guild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftHow {
    /// The bot left.
    Left,
    /// A member expelled the bot.
    Expelled,
    /// The guild was disbanded.
    Dismissed,
}

/// The last guild the bot lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Left {
    /// The guild.
    pub guild: u32,
    /// How.
    pub how: LeftHow,
    /// Bot clock (ms).
    pub at_ms: u64,
}

/// A character who joined the guild after the bot had its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Newcomer {
    /// Character id.
    pub char_id: u32,
    /// Bot clock (ms) when the bot saw the join.
    pub at_ms: u64,
}

/// What a bot knows about its guild. See the module docs.
#[derive(Debug, Default, Clone)]
pub struct GuildLog {
    /// Guild id (0: the bot is in none).
    pub id: u32,
    /// Guild name (empty until a packet names it).
    pub name: Box<str>,
    /// Character id of the owner (0 unknown).
    pub owner: u32,
    /// The members, the bot included once the list has come.
    pub members: Vec<Member>,
    /// An invitation waiting for an answer.
    pub invite: Option<Invite>,
    /// The last guild the bot lost.
    pub left: Option<Left>,
    /// Bot clock (ms) of the join or founding, or of the first list of a guild the bot logged in
    /// to (0 when the bot is in none).
    pub joined_ms: u64,
    /// Bot clock (ms) at which the bot founded the guild (0: it did not, or not in this
    /// connection).
    pub founded_ms: u64,
    /// The whole member list has arrived.
    pub loaded: bool,
    /// Characters that joined after the list had come, oldest first, until someone greets them.
    pub newcomers: SmallVec<[Newcomer; MAX_NEWCOMERS]>,
    /// Changes so far; the web snapshot is rebuilt when it moves.
    pub version: u32,
    pub(super) list_total: u32,
    pub(super) list_received: u32,
}

impl GuildLog {
    /// Whether the bot is in a guild.
    pub fn in_guild(&self) -> bool {
        self.id != 0
    }

    /// The member with character id `char_id`.
    pub fn member(&self, char_id: u32) -> Option<&Member> {
        self.members.iter().find(|m| m.char_id == char_id)
    }

    /// The role of character `char_id` (`None` when it is no member, or the list has not come).
    pub fn role_of(&self, char_id: u32) -> Option<u8> {
        self.member(char_id).map(|m| m.role)
    }

    /// Whether `char_id` owns the guild.
    pub fn is_owner(&self, char_id: u32) -> bool {
        char_id != 0 && (self.owner == char_id || self.role_of(char_id) == Some(role::OWNER))
    }

    /// Whether the default policy lets character `char_id` invite.
    pub fn can_invite(&self, char_id: u32) -> bool {
        self.role_of(char_id).is_some_and(role::can_invite)
    }

    /// Members online.
    pub fn online(&self) -> usize {
        self.members.iter().filter(|m| m.online).count()
    }

    /// The members with at least the officer role.
    pub fn officers(&self) -> impl Iterator<Item = &Member> {
        self.members
            .iter()
            .filter(|m| m.role >= role::OFFICER && m.role != role::OWNER)
    }

    /// Applies one packet for the bot whose character id is `me`. Returns whether the picture
    /// changed.
    pub fn apply(&mut self, ev: &GuildEvent, me: u32, now_ms: u64) -> bool {
        match ev {
            GuildEvent::Factions(list) => {
                // (the guild list names the guild a bot logged in to)
                let Some(f) = list.iter().find(|f| f.id == self.id && self.id != 0) else {
                    return false;
                };
                let changed = *f.name != *self.name || self.owner != f.owner_id;
                self.name = f.name.clone();
                self.owner = f.owner_id;
                self.bump(changed)
            }
            GuildEvent::Created { faction, .. } => {
                self.enter(faction.id, now_ms);
                self.founded_ms = now_ms.max(1);
                self.name = faction.name.clone();
                self.owner = faction.owner_id;
                self.invite = None;
                if self.member(me).is_none() && me != 0 {
                    self.members.push(Member {
                        char_id: me,
                        name: faction.owner_name.clone(),
                        level: 0,
                        role: role::OWNER,
                        online: true,
                        since_ms: now_ms,
                    });
                }
                self.bump(true)
            }
            GuildEvent::MemberList {
                total,
                guild,
                members,
            } => {
                if self.id != 0 && *guild != self.id {
                    return false;
                }
                if self.id == 0 {
                    self.enter(*guild, now_ms);
                }
                // a chunk after a complete list starts a new one
                if self.list_received == 0 || self.list_received >= self.list_total {
                    self.members.clear();
                    self.list_received = 0;
                }
                self.list_total = *total;
                self.list_received = self.list_received.saturating_add(members.len() as u32);
                for m in members.iter() {
                    self.upsert(m, now_ms, false);
                }
                self.loaded = self.list_received >= self.list_total;
                self.bump(true)
            }
            GuildEvent::MemberStatus { member, .. } => {
                if self.id != 0 && member.guild != self.id {
                    return false;
                }
                if self.id == 0 {
                    // (our own status before the list)
                    if member.char_id != me {
                        return false;
                    }
                    self.enter(member.guild, now_ms);
                }
                let greet = self.loaded && member.char_id != me;
                self.upsert(member, now_ms, greet);
                self.bump(true)
            }
            GuildEvent::Invitation {
                inviter,
                inviter_name,
                guild,
                guild_name,
            } => {
                self.invite = Some(Invite {
                    guild: *guild,
                    inviter: *inviter,
                    inviter_name: inviter_name.clone(),
                    guild_name: guild_name.clone(),
                    at_ms: now_ms.max(1),
                });
                self.bump(true)
            }
            GuildEvent::UnitChanged {
                char_id,
                from,
                to,
                expel,
                ..
            } => {
                if *char_id == me && me != 0 {
                    if *to != 0 {
                        if self.id != *to {
                            self.enter(*to, now_ms);
                        }
                        if let Some(inv) = self.invite.take().filter(|i| i.guild == *to) {
                            self.name = inv.guild_name;
                        }
                        self.left = None;
                        return self.bump(true);
                    }
                    if self.id != 0 {
                        let how = if *expel {
                            LeftHow::Expelled
                        } else {
                            LeftHow::Left
                        };
                        self.lose(how, now_ms);
                        return self.bump(true);
                    }
                    return false;
                }
                if *to == 0 && self.id != 0 && *from == self.id {
                    let before = self.members.len();
                    self.members.retain(|m| m.char_id != *char_id);
                    let removed = self.members.len() != before;
                    self.newcomers.retain(|n| n.char_id != *char_id);
                    return self.bump(removed);
                }
                false
            }
            GuildEvent::RoleChanged { char_id, role, .. } => {
                let Some(m) = self.members.iter_mut().find(|m| m.char_id == *char_id) else {
                    return false;
                };
                let changed = m.role != *role;
                m.role = *role;
                if *role == role::OWNER {
                    self.owner = *char_id;
                }
                self.bump(changed)
            }
            GuildEvent::OwnerChanged { from, to, .. } => {
                self.owner = *to;
                for m in &mut self.members {
                    if m.char_id == *to {
                        m.role = role::OWNER;
                    } else if m.char_id == *from {
                        m.role = role::APPRENTICE;
                    }
                }
                self.bump(true)
            }
            GuildEvent::Dismissed { guild, .. } => {
                if self.id == 0 || *guild != self.id {
                    return false;
                }
                self.lose(LeftHow::Dismissed, now_ms);
                self.bump(true)
            }
        }
    }

    /// The bot's own guild id at world entry (`SCCharacterState`): the id, before the list names
    /// anything. A different id than the log's starts a fresh picture.
    pub fn on_own_guild(&mut self, guild: u32, now_ms: u64) -> bool {
        if guild == self.id {
            return false;
        }
        if guild == 0 {
            // (the server says the bot has no guild: whatever the log held is stale)
            self.id = 0;
            self.reset_guild();
        } else {
            self.enter(guild, now_ms);
        }
        self.bump(true)
    }

    fn bump(&mut self, changed: bool) -> bool {
        if changed {
            self.version = self.version.wrapping_add(1);
        }
        changed
    }

    /// Starts the picture of guild `id`.
    fn enter(&mut self, id: u32, now_ms: u64) {
        if self.id != id {
            self.reset_guild();
        }
        self.id = id;
        self.joined_ms = now_ms.max(1);
        self.left = None;
    }

    fn reset_guild(&mut self) {
        self.name = "".into();
        self.owner = 0;
        self.members.clear();
        self.newcomers.clear();
        self.joined_ms = 0;
        self.founded_ms = 0;
        self.loaded = false;
        self.list_total = 0;
        self.list_received = 0;
    }

    fn lose(&mut self, how: LeftHow, now_ms: u64) {
        self.left = Some(Left {
            guild: self.id,
            how,
            at_ms: now_ms.max(1),
        });
        self.id = 0;
        self.reset_guild();
    }

    fn upsert(&mut self, m: &MemberBrief, now_ms: u64, greet: bool) {
        if let Some(x) = self.members.iter_mut().find(|x| x.char_id == m.char_id) {
            x.name = m.name.clone();
            x.level = m.level;
            x.role = m.role;
            x.online = m.online;
        } else if self.members.len() < MAX_MEMBERS {
            self.members.push(Member {
                char_id: m.char_id,
                name: m.name.clone(),
                level: m.level,
                role: m.role,
                online: m.online,
                since_ms: now_ms,
            });
            if greet {
                if self.newcomers.len() >= MAX_NEWCOMERS {
                    self.newcomers.remove(0);
                }
                self.newcomers.push(Newcomer {
                    char_id: m.char_id,
                    at_ms: now_ms,
                });
            }
        }
        if m.role == role::OWNER {
            self.owner = m.char_id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_proto::guild::FactionBrief;

    const ME: u32 = 10;

    fn brief(char_id: u32, name: &str, role: u8, online: bool) -> MemberBrief {
        MemberBrief {
            guild: 5,
            char_id,
            online,
            name: name.into(),
            level: 20,
            role,
        }
    }

    fn list(total: u32, members: &[MemberBrief]) -> GuildEvent {
        GuildEvent::MemberList {
            total,
            guild: 5,
            members: members.to_vec().into_boxed_slice(),
        }
    }

    fn status(m: MemberBrief) -> GuildEvent {
        GuildEvent::MemberStatus {
            member: Box::new(m),
            flag: 0,
        }
    }

    fn unit(char_id: u32, from: u32, to: u32, expel: bool) -> GuildEvent {
        GuildEvent::UnitChanged {
            obj: 0,
            char_id,
            kicker: "".into(),
            name: "x".into(),
            from,
            to,
            expel,
        }
    }

    fn joined_log() -> GuildLog {
        let mut g = GuildLog::default();
        g.on_own_guild(5, 100);
        g.apply(
            &list(
                3,
                &[
                    brief(1, "Boss", role::OWNER, true),
                    brief(ME, "Me", role::MEMBER, true),
                    brief(3, "Cy", role::OFFICER, false),
                ],
            ),
            ME,
            200,
        );
        g
    }

    #[test]
    fn the_list_at_world_entry_names_the_members_and_the_owner() {
        let g = joined_log();
        assert!(g.in_guild() && g.loaded);
        assert_eq!((g.id, g.owner, g.members.len()), (5, 1, 3));
        assert_eq!(g.role_of(ME), Some(role::MEMBER));
        assert!(g.is_owner(1) && !g.is_owner(ME));
        assert!(g.can_invite(1) && g.can_invite(3) && !g.can_invite(ME));
        assert_eq!(g.online(), 2);
        assert_eq!(g.officers().map(|m| m.char_id).collect::<Vec<_>>(), [3]);
        assert!(g.newcomers.is_empty(), "the first list greets nobody");
    }

    #[test]
    fn the_guild_list_names_the_guild_the_bot_logged_in_to() {
        let mut g = GuildLog::default();
        let other = FactionBrief {
            id: 9,
            mother_id: 1,
            name: "Other".into(),
            owner_id: 2,
            owner_name: "O".into(),
            created: 0,
        };
        let mine = FactionBrief {
            id: 5,
            name: "Silent Wardens".into(),
            owner_id: 1,
            ..other.clone()
        };
        let ev = GuildEvent::Factions(vec![other.clone(), mine.clone()].into_boxed_slice());
        // not in a guild yet: nothing to name
        assert!(!g.apply(&ev, ME, 1));
        g.on_own_guild(5, 2);
        assert!(g.apply(&ev, ME, 3));
        assert_eq!((&*g.name, g.owner), ("Silent Wardens", 1));
        // the same list again changes nothing
        assert!(!g.apply(&ev, ME, 4));
    }

    #[test]
    fn a_list_in_chunks_is_one_list_and_the_next_one_replaces_it() {
        let mut g = GuildLog::default();
        let ms: Vec<MemberBrief> = (1..=30)
            .map(|i| brief(i, &format!("M{i}"), role::MEMBER, true))
            .collect();
        g.apply(&list(30, &ms[..20]), ME, 1);
        assert!(!g.loaded);
        assert_eq!(g.members.len(), 20);
        g.apply(&list(30, &ms[20..]), ME, 2);
        assert!(g.loaded);
        assert_eq!(g.members.len(), 30);
        // a fresh list (after a join) replaces, it does not add
        g.apply(&list(2, &ms[..2]), ME, 3);
        assert_eq!(g.members.len(), 2);
        assert!(g.loaded);
    }

    #[test]
    fn a_member_who_joins_after_the_list_is_a_newcomer() {
        let mut g = joined_log();
        assert!(g.apply(&status(brief(7, "New", role::APPRENTICE, true)), ME, 500));
        assert_eq!(g.members.len(), 4);
        assert_eq!(g.newcomers.len(), 1);
        assert_eq!(g.newcomers[0].char_id, 7);
        // the same member logging in again is no newcomer
        g.apply(&status(brief(7, "New", role::APPRENTICE, false)), ME, 600);
        assert_eq!(g.newcomers.len(), 1);
        assert!(!g.member(7).unwrap().online);
        // somebody else's guild is ignored
        let mut m = brief(8, "Other", role::MEMBER, true);
        m.guild = 6;
        assert!(!g.apply(&status(m), ME, 700));
        assert_eq!(g.members.len(), 4);
    }

    #[test]
    fn leaving_and_being_expelled_clear_the_guild_and_say_how() {
        let mut g = joined_log();
        assert!(g.apply(&unit(ME, 5, 0, false), ME, 900));
        assert!(!g.in_guild() && g.members.is_empty() && !g.loaded);
        let left = g.left.unwrap();
        assert_eq!((left.guild, left.how, left.at_ms), (5, LeftHow::Left, 900));
        // the same packet twice (to the guild and to those who see it) is harmless
        assert!(!g.apply(&unit(ME, 5, 0, false), ME, 901));
        let mut g = joined_log();
        g.apply(&unit(ME, 5, 0, true), ME, 950);
        assert_eq!(g.left.unwrap().how, LeftHow::Expelled);
    }

    #[test]
    fn another_member_leaving_is_removed_once() {
        let mut g = joined_log();
        assert!(g.apply(&unit(3, 5, 0, false), ME, 300));
        assert!(g.member(3).is_none());
        assert!(!g.apply(&unit(3, 5, 0, false), ME, 301));
        // a character of another guild that a bot saw leave is none of the log's business
        assert!(!g.apply(&unit(99, 6, 0, false), ME, 302));
        assert_eq!(g.members.len(), 2);
    }

    #[test]
    fn joining_a_guild_keeps_the_name_of_the_invitation() {
        let mut g = GuildLog::default();
        g.apply(
            &GuildEvent::Invitation {
                inviter: 1,
                inviter_name: "Boss".into(),
                guild: 5,
                guild_name: "Silent Wardens".into(),
            },
            ME,
            1000,
        );
        let inv = g.invite.clone().unwrap();
        assert_eq!((inv.guild, inv.inviter, inv.at_ms), (5, 1, 1000));
        assert!(!g.in_guild());
        g.apply(&unit(ME, 0, 5, false), ME, 1500);
        assert!(g.in_guild() && g.invite.is_none());
        assert_eq!((&*g.name, g.joined_ms), ("Silent Wardens", 1500));
        // the list that follows fills the members
        g.apply(&list(1, &[brief(1, "Boss", role::OWNER, true)]), ME, 1600);
        assert_eq!(g.owner, 1);
    }

    #[test]
    fn founding_makes_the_bot_the_owner() {
        let mut g = GuildLog::default();
        let faction = FactionBrief {
            id: 7,
            mother_id: 101,
            name: "Iron Pilgrims".into(),
            owner_id: ME,
            owner_name: "Me".into(),
            created: 0,
        };
        g.apply(
            &GuildEvent::Created {
                faction: Box::new(faction),
                owner_obj: 1,
            },
            ME,
            2000,
        );
        assert!(g.in_guild() && g.is_owner(ME));
        assert_eq!((g.id, &*g.name, g.members.len()), (7, "Iron Pilgrims", 1));
        assert_eq!(g.founded_ms, 2000);
        assert_eq!(g.role_of(ME), Some(role::OWNER));
    }

    #[test]
    fn roles_and_the_owner_follow_the_packets() {
        let mut g = joined_log();
        g.apply(
            &GuildEvent::RoleChanged {
                char_id: ME,
                role: role::OFFICER,
                name: "Me".into(),
            },
            ME,
            1,
        );
        assert!(g.can_invite(ME));
        assert_eq!(g.officers().count(), 2);
        // the owner hands the guild to the officer Cy: the old owner is an apprentice then
        g.apply(
            &GuildEvent::OwnerChanged {
                from: 1,
                to: 3,
                name: "Cy".into(),
            },
            ME,
            2,
        );
        assert!(g.is_owner(3) && !g.is_owner(1));
        assert_eq!(g.role_of(3), Some(role::OWNER));
        assert_eq!(g.role_of(1), Some(role::APPRENTICE));
        // a role of somebody unknown changes nothing
        assert!(!g.apply(
            &GuildEvent::RoleChanged {
                char_id: 99,
                role: 2,
                name: "x".into()
            },
            ME,
            3
        ));
    }

    #[test]
    fn a_dismissed_guild_is_gone() {
        let mut g = joined_log();
        assert!(!g.apply(
            &GuildEvent::Dismissed {
                guild: 6,
                success: true
            },
            ME,
            1
        ));
        assert!(g.in_guild());
        assert!(g.apply(
            &GuildEvent::Dismissed {
                guild: 5,
                success: true
            },
            ME,
            2
        ));
        assert!(!g.in_guild());
        assert_eq!(g.left.unwrap().how, LeftHow::Dismissed);
    }

    #[test]
    fn the_own_guild_at_world_entry_overrides_a_stale_picture() {
        let mut g = joined_log();
        let v = g.version;
        assert!(!g.on_own_guild(5, 1000), "the same guild changes nothing");
        assert_eq!(g.version, v);
        assert!(g.on_own_guild(0, 1001));
        assert!(!g.in_guild() && g.members.is_empty());
        assert!(g.on_own_guild(8, 1002));
        assert_eq!((g.id, g.joined_ms), (8, 1002));
    }

    #[test]
    fn the_member_list_is_bounded() {
        let mut g = GuildLog::default();
        let ms: Vec<MemberBrief> = (1..=20)
            .map(|i| brief(i, "M", role::MEMBER, true))
            .collect();
        for round in 0..8u32 {
            let ms: Vec<MemberBrief> = ms
                .iter()
                .cloned()
                .map(|mut m| {
                    m.char_id += round * 20;
                    m
                })
                .collect();
            g.apply(&list(200, &ms), ME, 1);
        }
        assert_eq!(g.members.len(), MAX_MEMBERS);
    }
}
