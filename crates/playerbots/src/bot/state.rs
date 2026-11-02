//! What one bot knows: itself, the units and doodads the server shows it, its bag, quests,
//! cooldowns and buffs.
//!
//! [`BotState::apply`] is the only writer: every parsed [`ServerEvent`] updates the state, and
//! the few events that need an answer (a server teleport, a death, a kick) leave a note in
//! [`BotState::reactions`] for the bot task to act on. The state never touches the network and
//! never reads a clock (the bot task sets [`BotState::now_ms`] first), so it is deterministic and
//! testable.
//!
//! Memory is bounded: units are capped at [`MAX_UNITS`], doodads at [`MAX_DOODADS`] (and the
//! bot task drops doodads that do nothing before they get here), the small lists are
//! [`SmallVec`]s with hard limits, strings are kept only where something reads them (character
//! names).

use aa_proto::sc::ServerEvent;
use aa_proto::sc_activity::{quest_status, ActivityEvent, QuestRow};
use aa_proto::sc_world::{unit_type, EquipSlot, ItemChange, UnitInfo, WorldEvent};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

/// Most units kept in the view.
pub const MAX_UNITS: usize = 1024;
/// Most doodads kept in the view.
pub const MAX_DOODADS: usize = 2048;
/// `SlotType` of the equipment container in item tasks.
pub const SLOT_EQUIPMENT: u8 = 1;
/// `SlotType` of the bag.
pub const SLOT_BAG: u8 = 2;

/// Unit flag: the unit is dead.
pub const U_DEAD: u8 = 1;
/// Unit flag: the corpse still has loot.
pub const U_LOOT: u8 = 2;
/// Unit flag: the unit belongs to a player (summon, pet).
pub const U_OWNED: u8 = 4;

/// What kind of unit the server shows. Houses, transfers, mates and shipyards are not tracked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    /// A player character (a bot or a real player).
    Character,
    /// An NPC: monster, quest giver, merchant.
    Npc,
    /// A vehicle (cart, boat).
    Slave,
}

impl UnitKind {
    /// The kind of a `BaseUnitType` wire value, `None` for the kinds bots ignore.
    pub fn from_wire(t: u8) -> Option<UnitKind> {
        match t {
            unit_type::CHARACTER => Some(UnitKind::Character),
            unit_type::NPC => Some(UnitKind::Npc),
            unit_type::SLAVE => Some(UnitKind::Slave),
            _ => None,
        }
    }
}

/// A unit in view.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    /// What it is.
    pub kind: UnitKind,
    /// Character id (characters) or template id (NPCs, vehicles).
    pub id: u32,
    /// Position.
    pub pos: [f32; 3],
    /// Hit points (real value); only moves with the server's updates.
    pub hp: i32,
    /// Faction id; 0 when the server did not send one (use the NPC template's).
    pub faction: u32,
    /// Level.
    pub level: u8,
    /// `U_DEAD | U_LOOT | U_OWNED`.
    pub flags: u8,
    /// Name, characters only.
    pub name: Option<Box<str>>,
}

impl Unit {
    /// Whether the unit is dead.
    pub fn dead(&self) -> bool {
        self.flags & U_DEAD != 0
    }

    /// Whether the corpse still has loot.
    pub fn has_loot(&self) -> bool {
        self.flags & U_LOOT != 0
    }

    /// Whether a player owns the unit.
    pub fn owned(&self) -> bool {
        self.flags & U_OWNED != 0
    }

    /// Horizontal distance to `p`.
    pub fn dist2d(&self, p: [f32; 3]) -> f32 {
        dist2d(self.pos, p)
    }
}

/// A doodad in view (only doodads that can be used, looted or crafted at).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Doodad {
    /// Doodad template.
    pub template: u32,
    /// Position.
    pub pos: [f32; 3],
    /// Current phase (function group).
    pub phase: u32,
}

/// An item in the bag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// Item instance id.
    pub id: u64,
    /// Item template.
    pub template: u32,
    /// Stack size.
    pub count: i32,
}

/// One of the bot's houses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HouseBrief {
    /// Transport id the house packets use.
    pub tl: u16,
    /// House id.
    pub id: u32,
    /// Design (template).
    pub template: u32,
    /// Position.
    pub pos: [f32; 3],
    /// Build steps left (0 = finished).
    pub build_left: i32,
}

/// The bot itself.
#[derive(Debug, Clone, Default)]
pub struct Me {
    /// Character id.
    pub char_id: u32,
    /// Object id in the world, 0 until the own unit state arrived.
    pub obj: u32,
    /// Character name.
    pub name: Box<str>,
    /// Race id.
    pub race: u8,
    /// Gender id.
    pub gender: u8,
    /// Level.
    pub level: u8,
    /// Experience.
    pub exp: i32,
    /// Hit points.
    pub hp: i32,
    /// Mana.
    pub mp: i32,
    /// Highest hit points seen (the wire has no maximum).
    pub hp_max: i32,
    /// Highest mana seen.
    pub mp_max: i32,
    /// Position (the motion controller owns it while in the world).
    pub pos: [f32; 3],
    /// Facing in radians (0 = +Y, counter-clockwise).
    pub yaw: f32,
    /// Copper.
    pub money: i64,
    /// Labor points.
    pub labor: i16,
    /// Faction id.
    pub faction: u32,
    /// The three active abilities (11 = none).
    pub abilities: [u8; 3],
    /// Zone key from the server.
    pub zone_id: u32,
    /// Dead and waiting to be resurrected.
    pub dead: bool,
    /// Bot clock (ms) at which a resurrection may be requested.
    pub respawn_at_ms: u64,
    /// The own unit state arrived: the character is in the world.
    pub spawned: bool,
}

impl Me {
    /// Hit points in percent of the highest value seen; 100 when unknown.
    pub fn hp_pct(&self) -> u8 {
        pct(self.hp, self.hp_max)
    }

    /// Mana in percent of the highest value seen; 100 when unknown.
    pub fn mp_pct(&self) -> u8 {
        pct(self.mp, self.mp_max)
    }
}

fn pct(v: i32, max: i32) -> u8 {
    if max <= 0 {
        100
    } else {
        (i64::from(v.max(0)) * 100 / i64::from(max)).clamp(0, 100) as u8
    }
}

/// The quests of the bot.
#[derive(Debug, Clone, Default)]
pub struct QuestLog {
    /// Quests in progress (status in-progress, ready or failed).
    pub active: Vec<QuestRow>,
    /// Completed quest ids, ascending.
    pub completed: Vec<u32>,
}

impl QuestLog {
    /// The active row of quest `id`.
    pub fn row(&self, id: u32) -> Option<&QuestRow> {
        self.active.iter().find(|q| q.template == id)
    }

    /// Whether quest `id` is completed.
    pub fn is_done(&self, id: u32) -> bool {
        self.completed.binary_search(&id).is_ok()
    }

    fn mark_done(&mut self, id: u32) {
        if let Err(at) = self.completed.binary_search(&id) {
            self.completed.insert(at, id);
        }
    }

    fn mark_undone(&mut self, id: u32) {
        if let Ok(at) = self.completed.binary_search(&id) {
            self.completed.remove(at);
        }
    }

    fn upsert(&mut self, row: QuestRow) {
        match quest_status_class(row.status) {
            StatusClass::Gone => {
                self.active.retain(|q| q.template != row.template);
                if matches!(
                    row.status,
                    quest_status::COMPLETED | quest_status::DAILY_COMPLETED
                ) {
                    self.mark_done(row.template);
                }
            }
            StatusClass::Active => {
                match self.active.iter().position(|q| q.template == row.template) {
                    Some(i) => self.active[i] = row,
                    None if self.active.len() < 64 => self.active.push(row),
                    None => {}
                }
            }
        }
    }
}

enum StatusClass {
    Active,
    Gone,
}

fn quest_status_class(status: u8) -> StatusClass {
    match status {
        quest_status::DROPPED | quest_status::COMPLETED | quest_status::DAILY_COMPLETED => {
            StatusClass::Gone
        }
        _ => StatusClass::Active,
    }
}

/// Something the server refused or reported, newest last in [`BotState::errors`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateError {
    /// `SCErrorMsg` with the `ErrorMessageType` id.
    Msg(i16),
    /// `SCQuestContextFailed`.
    Quest {
        /// Quest template.
        quest: u32,
        /// Reason byte.
        err: u8,
    },
    /// `SCLootItemFailed`.
    Loot(i16),
    /// A skill start the server refused (`SkillStarted.result != 0`).
    Skill {
        /// Skill id.
        skill: u32,
        /// `SkillResult` byte.
        result: u8,
    },
}

/// A chat line addressed to or heard by the bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatIn {
    /// Chat type (0 say, -3 whisper, -2 system, -1 notice, 1 shout, ...).
    pub kind: i16,
    /// Sender object id (0 for system text).
    pub sender_obj: u32,
    /// Sender name.
    pub name: Box<str>,
    /// Text, cut to 200 characters.
    pub text: Box<str>,
    /// Bot clock (ms) when it arrived.
    pub at_ms: u64,
}

/// Things [`BotState::apply`] noticed that the bot task must answer on the wire.
#[derive(Debug, Clone, Default)]
pub struct Reactions {
    /// The server moved the bot here (`SCTeleportUnit`): answer `CSTeleportEnded`.
    pub teleport: Option<[f32; 3]>,
    /// `SCLoadInstance`: answer `CSInstanceLoaded`.
    pub load_instance: bool,
    /// The bot was resurrected here.
    pub resurrected: Option<[f32; 3]>,
    /// The bot died.
    pub died: bool,
    /// `SCKicked` with its reason and message.
    pub kicked: Option<(u8, Box<str>)>,
    /// `SCLeaveWorldGranted`: the server lets the bot log out.
    pub leave_granted: bool,
}

/// Everything one bot knows. See the module docs.
#[derive(Debug, Default)]
pub struct BotState {
    /// The bot itself.
    pub me: Me,
    /// Units in view by object id.
    pub units: FxHashMap<u32, Unit>,
    /// Interesting doodads in view by object id.
    pub doodads: FxHashMap<u32, Doodad>,
    /// Bag content by slot.
    pub bag: FxHashMap<u16, Item>,
    /// Equipped items.
    pub equipment: Vec<EquipSlot>,
    /// Quests.
    pub quests: QuestLog,
    /// Learned skills `(skill, level)`.
    pub skills: Vec<(u32, u8)>,
    /// Skill cooldowns `(skill, ready at bot clock ms)`.
    pub cooldowns: SmallVec<[(u32, u64); 8]>,
    /// Own buffs `(instance index, buff id)`.
    pub buffs: SmallVec<[(u32, u32); 8]>,
    /// Own houses.
    pub houses: SmallVec<[HouseBrief; 1]>,
    /// Units that target the bot `(object id, bot clock ms)`.
    pub threats: SmallVec<[(u32, u64); 4]>,
    /// Units the bot killed `(object id, bot clock ms)`, newest last.
    pub kills: SmallVec<[(u32, u64); 4]>,
    /// Bot clock (ms) of the last hit point loss.
    pub last_damage_ms: u64,
    /// Recent errors, newest last (at most 8).
    pub errors: SmallVec<[(u64, StateError); 8]>,
    /// Chat the bot heard, newest last (at most 4).
    pub inbox: SmallVec<[ChatIn; 2]>,
    /// The bot's clock in milliseconds, set by the bot task before it applies events.
    pub now_ms: u64,
    /// Notes for the bot task, see [`Reactions`].
    pub reactions: Reactions,
    /// Events dropped because a collection was full.
    pub dropped: u32,
}

impl BotState {
    /// A fresh state.
    pub fn new() -> BotState {
        BotState::default()
    }

    /// Forgets everything about the world (a new connection starts from scratch) and frees the
    /// memory of the collections.
    pub fn reset(&mut self) {
        *self = BotState::default();
    }

    // ---- queries ------------------------------------------------------------------------

    /// The unit with object id `obj`.
    pub fn unit(&self, obj: u32) -> Option<&Unit> {
        self.units.get(&obj)
    }

    /// The bag count of item `template` (all stacks).
    pub fn item_count(&self, template: u32) -> i64 {
        self.bag
            .values()
            .filter(|i| i.template == template)
            .map(|i| i64::from(i.count))
            .sum()
    }

    /// The bag slot holding `template`.
    pub fn find_item(&self, template: u32) -> Option<(u16, &Item)> {
        self.bag
            .iter()
            .filter(|(_, i)| i.template == template)
            .min_by_key(|(slot, _)| **slot)
            .map(|(slot, item)| (*slot, item))
    }

    /// The bot's skill level, `None` when it has not learned `skill`.
    pub fn skill_level(&self, skill: u32) -> Option<u8> {
        self.skills.iter().find(|s| s.0 == skill).map(|s| s.1)
    }

    /// Whether `skill` is off cooldown at the bot clock `now_ms`.
    pub fn cooldown_ready(&self, skill: u32, now_ms: u64) -> bool {
        self.cooldowns
            .iter()
            .find(|c| c.0 == skill)
            .is_none_or(|c| now_ms >= c.1)
    }

    /// Marks `skill` ready at `ready_at_ms`.
    pub fn set_cooldown(&mut self, skill: u32, ready_at_ms: u64) {
        let now = self.now_ms;
        if let Some(c) = self.cooldowns.iter_mut().find(|c| c.0 == skill) {
            c.1 = ready_at_ms;
            return;
        }
        if self.cooldowns.len() >= 16 {
            self.cooldowns.retain(|c| c.1 > now);
        }
        if self.cooldowns.len() < 16 {
            self.cooldowns.push((skill, ready_at_ms));
        }
    }

    /// Whether a unit that is not a bot of ours is a character within `radius` metres of the bot.
    pub fn any_character_within(&self, radius: f32, mut is_ours: impl FnMut(u32) -> bool) -> bool {
        self.units.values().any(|u| {
            u.kind == UnitKind::Character && !is_ours(u.id) && u.dist2d(self.me.pos) <= radius
        })
    }

    /// Records a refused action.
    pub fn push_error(&mut self, e: StateError) {
        if self.errors.len() >= 8 {
            self.errors.remove(0);
        }
        self.errors.push((self.now_ms, e));
    }

    /// Whether the last `window_ms` contained a hit point loss.
    pub fn damaged_within(&self, window_ms: u64) -> bool {
        self.last_damage_ms != 0 && self.now_ms.saturating_sub(self.last_damage_ms) <= window_ms
    }

    // ---- events -------------------------------------------------------------------------

    /// Updates the state from one server event.
    pub fn apply(&mut self, ev: &ServerEvent) {
        match ev {
            ServerEvent::World(w) => self.apply_world(w),
            ServerEvent::Activity(a) => self.apply_activity(a),
        }
    }

    fn apply_world(&mut self, ev: &WorldEvent) {
        match ev {
            WorldEvent::CharacterState(own) => {
                let me = &mut self.me;
                me.char_id = own.id;
                me.name = own.name.clone();
                me.race = own.race;
                me.gender = own.gender;
                me.level = own.level;
                me.hp = own.hp;
                me.mp = own.mp;
                me.hp_max = me.hp_max.max(own.hp);
                me.mp_max = me.mp_max.max(own.mp);
                me.zone_id = own.zone_id;
                me.faction = own.faction_id;
                me.abilities = own.abilities;
                me.pos = [own.pos.0, own.pos.1, own.pos.2];
                me.yaw = own.yaw;
                me.exp = own.exp;
                me.labor = own.labor;
                me.money = own.money;
                self.equipment = own.equipment.to_vec();
            }
            WorldEvent::UnitState(info) => self.apply_unit_state(info),
            WorldEvent::UnitsRemoved { objs } => {
                for o in objs {
                    self.units.remove(o);
                    self.threats.retain(|t| t.0 != *o);
                }
            }
            WorldEvent::UnitMoved { obj, pos, .. } => {
                if let Some(u) = self.units.get_mut(obj) {
                    u.pos = [pos.0, pos.1, pos.2];
                }
            }
            WorldEvent::TeleportUnit { pos, .. } => {
                self.me.pos = [pos.0, pos.1, pos.2];
                self.reactions.teleport = Some(self.me.pos);
            }
            WorldEvent::LoadInstance { pos, yaw, .. } => {
                self.me.pos = [pos.0, pos.1, pos.2];
                self.me.yaw = *yaw;
                self.reactions.load_instance = true;
            }
            WorldEvent::UnitPoints { obj, hp, mp } => {
                if *obj == self.me.obj && self.me.obj != 0 {
                    if *hp < self.me.hp {
                        self.last_damage_ms = self.now_ms.max(1);
                    }
                    self.me.hp = *hp;
                    self.me.mp = *mp;
                    self.me.hp_max = self.me.hp_max.max(*hp);
                    self.me.mp_max = self.me.mp_max.max(*mp);
                } else if let Some(u) = self.units.get_mut(obj) {
                    u.hp = *hp;
                    if *hp <= 0 {
                        u.flags |= U_DEAD;
                    }
                }
            }
            WorldEvent::UnitDeath {
                obj,
                killer,
                respawn_wait_ms,
                ..
            } => self.apply_death(*obj, *killer, *respawn_wait_ms),
            WorldEvent::Resurrected { obj, pos, yaw } => {
                if *obj == self.me.obj && self.me.obj != 0 {
                    self.me.dead = false;
                    self.me.pos = [pos.0, pos.1, pos.2];
                    self.me.yaw = *yaw;
                    self.me.hp = self.me.hp.max(1);
                    self.reactions.resurrected = Some(self.me.pos);
                } else if let Some(u) = self.units.get_mut(obj) {
                    u.flags &= !U_DEAD;
                    u.pos = [pos.0, pos.1, pos.2];
                }
            }
            WorldEvent::TargetChanged { obj, target } => {
                if *obj == self.me.obj {
                    return;
                }
                if *target == self.me.obj && self.me.obj != 0 {
                    let now = self.now_ms;
                    match self.threats.iter_mut().find(|t| t.0 == *obj) {
                        Some(t) => t.1 = now,
                        None => {
                            if self.threats.len() >= 4 {
                                self.threats.remove(0);
                            }
                            self.threats.push((*obj, now));
                        }
                    }
                } else {
                    self.threats.retain(|t| t.0 != *obj);
                }
            }
            WorldEvent::ExpChanged { obj, delta } => {
                if *obj == self.me.obj {
                    self.me.exp = self.me.exp.saturating_add(*delta);
                }
            }
            WorldEvent::LevelChanged { obj, level } => {
                if *obj == self.me.obj && self.me.obj != 0 {
                    self.me.level = *level;
                } else if let Some(u) = self.units.get_mut(obj) {
                    u.level = *level;
                }
            }
            WorldEvent::FactionChanged { obj, faction, .. } => {
                if *obj == self.me.obj && self.me.obj != 0 {
                    self.me.faction = *faction;
                } else if let Some(u) = self.units.get_mut(obj) {
                    u.faction = *faction;
                }
            }
            WorldEvent::DoodadsCreated(list) => {
                for d in list {
                    if self.doodads.len() >= MAX_DOODADS && !self.doodads.contains_key(&d.obj) {
                        self.dropped = self.dropped.saturating_add(1);
                        continue;
                    }
                    self.doodads.insert(
                        d.obj,
                        Doodad {
                            template: d.template,
                            pos: [d.pos.0, d.pos.1, d.pos.2],
                            phase: d.phase,
                        },
                    );
                }
            }
            WorldEvent::DoodadsRemoved(objs) => {
                for o in objs {
                    self.doodads.remove(o);
                }
            }
            WorldEvent::DoodadPhase { obj, phase } => {
                if let Some(d) = self.doodads.get_mut(obj) {
                    d.phase = *phase;
                }
            }
            WorldEvent::InventoryInit { .. } => {}
            WorldEvent::Inventory {
                slot_type,
                chunk,
                items,
            } => {
                if *slot_type == SLOT_BAG {
                    let lo = u16::from(*chunk) * 10;
                    self.bag.retain(|slot, _| !(lo..lo + 10).contains(slot));
                    for it in items.iter() {
                        self.bag.insert(
                            it.slot,
                            Item {
                                id: it.id,
                                template: it.template,
                                count: it.count,
                            },
                        );
                    }
                }
            }
            WorldEvent::ItemTask(task) => {
                for change in &task.changes {
                    self.apply_item_change(change);
                }
                if !task.force_removed.is_empty() {
                    self.bag
                        .retain(|_, item| !task.force_removed.contains(&item.id));
                }
            }
            WorldEvent::LaborChanged { delta, .. } => {
                let v = i32::from(self.me.labor).saturating_add(*delta);
                self.me.labor = v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            }
            WorldEvent::SkillLearned { skill, level, .. } => {
                match self.skills.iter_mut().find(|s| s.0 == *skill) {
                    Some(s) => s.1 = *level,
                    None => self.skills.push((*skill, *level)),
                }
            }
            WorldEvent::Cooldowns { skills, .. } => {
                let now = self.now_ms;
                for c in skills.iter() {
                    self.set_cooldown(c.id, now + u64::from(c.remaining_ms));
                }
            }
            WorldEvent::CooldownReset { obj, skill, .. } => {
                if *obj == self.me.obj {
                    self.cooldowns.retain(|c| c.0 != *skill);
                }
            }
            WorldEvent::BuffCreated {
                obj,
                index,
                buff_id,
                ..
            } => {
                if *obj == self.me.obj && self.me.obj != 0 && self.buffs.len() < 32 {
                    self.buffs.retain(|b| b.0 != *index);
                    self.buffs.push((*index, *buff_id));
                }
            }
            WorldEvent::BuffRemoved { obj, index } => {
                if *obj == self.me.obj {
                    self.buffs.retain(|b| b.0 != *index);
                }
            }
            WorldEvent::Chat {
                kind,
                sender_obj,
                name,
                msg,
                ..
            } => {
                if *sender_obj != self.me.obj || *sender_obj == 0 {
                    if self.inbox.len() >= 4 {
                        self.inbox.remove(0);
                    }
                    self.inbox.push(ChatIn {
                        kind: *kind,
                        sender_obj: *sender_obj,
                        name: name.clone(),
                        text: msg.chars().take(200).collect::<String>().into_boxed_str(),
                        at_ms: self.now_ms,
                    });
                }
            }
            WorldEvent::ErrorMsg { err, .. } => self.push_error(StateError::Msg(*err)),
            WorldEvent::LootableState {
                owner_obj,
                owner_type,
                has_loot,
            } => {
                if *owner_type == 1 {
                    if let Some(u) = self.units.get_mut(owner_obj) {
                        if *has_loot {
                            u.flags |= U_LOOT;
                        } else {
                            u.flags &= !U_LOOT;
                        }
                    }
                }
            }
            WorldEvent::LootFailed { err, .. } => self.push_error(StateError::Loot(*err)),
            WorldEvent::Kicked { reason, msg } => {
                self.reactions.kicked = Some((*reason, msg.clone()));
            }
            WorldEvent::LeaveWorldGranted { .. } => self.reactions.leave_granted = true,
            // Session steps, combat flow and loot windows are handled by the session code, the
            // combat controller and the goals that watch them.
            _ => {}
        }
    }

    fn apply_unit_state(&mut self, info: &UnitInfo) {
        let is_me = info.unit_type == unit_type::CHARACTER
            && self.me.char_id != 0
            && info.id == self.me.char_id;
        if is_me {
            let me = &mut self.me;
            me.obj = info.obj;
            me.level = info.level;
            me.hp = info.hp;
            me.mp = info.mp;
            me.hp_max = me.hp_max.max(info.hp);
            me.mp_max = me.mp_max.max(info.mp);
            if info.complete {
                me.faction = info.faction_id;
            }
            if !me.spawned {
                me.pos = [info.pos.0, info.pos.1, info.pos.2];
                me.spawned = true;
            }
            if info.complete {
                self.skills = info.skills.to_vec();
                self.buffs.clear();
                for b in info.buffs.iter().take(32) {
                    self.buffs.push((b.index, b.buff_id));
                }
            }
            return;
        }
        let Some(kind) = UnitKind::from_wire(info.unit_type) else {
            return;
        };
        if self.units.len() >= MAX_UNITS && !self.units.contains_key(&info.obj) {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        let mut flags = 0;
        if info.dead {
            flags |= U_DEAD;
        }
        if info.has_loot {
            flags |= U_LOOT;
        }
        if info.owner_id != 0 {
            flags |= U_OWNED;
        }
        self.units.insert(
            info.obj,
            Unit {
                kind,
                id: info.id,
                pos: [info.pos.0, info.pos.1, info.pos.2],
                hp: info.hp,
                faction: info.faction_id,
                level: info.level,
                flags,
                name: (kind == UnitKind::Character).then(|| info.name.clone()),
            },
        );
    }

    fn apply_death(&mut self, obj: u32, killer: u32, respawn_wait_ms: i32) {
        if obj == self.me.obj && self.me.obj != 0 {
            self.me.dead = true;
            self.me.hp = 0;
            let wait = u64::try_from(respawn_wait_ms).unwrap_or(0).max(1000);
            self.me.respawn_at_ms = self.now_ms + wait;
            self.reactions.died = true;
            self.threats.clear();
            return;
        }
        if let Some(u) = self.units.get_mut(&obj) {
            u.flags |= U_DEAD;
            u.hp = 0;
        }
        self.threats.retain(|t| t.0 != obj);
        if killer == self.me.obj && self.me.obj != 0 {
            if self.kills.len() >= 4 {
                self.kills.remove(0);
            }
            self.kills.push((obj, self.now_ms));
        }
    }

    fn apply_item_change(&mut self, change: &ItemChange) {
        match *change {
            ItemChange::Money(delta) => self.me.money += i64::from(delta),
            ItemChange::Added {
                slot_type,
                slot,
                template,
                id,
                count,
                ..
            } => match slot_type {
                SLOT_BAG => {
                    self.bag.insert(
                        u16::from(slot),
                        Item {
                            id,
                            template,
                            count,
                        },
                    );
                }
                SLOT_EQUIPMENT => {
                    self.equipment.retain(|e| e.slot != slot);
                    self.equipment.push(EquipSlot {
                        slot,
                        template,
                        id,
                        grade: 0,
                    });
                }
                _ => {}
            },
            ItemChange::CountUpdate {
                slot_type,
                slot,
                delta,
                id,
                template,
            } if slot_type == SLOT_BAG => {
                let key = u16::from(slot);
                match self.bag.get_mut(&key) {
                    Some(item) => {
                        item.count = item.count.saturating_add(delta);
                        if item.count <= 0 {
                            self.bag.remove(&key);
                        }
                    }
                    None if delta > 0 => {
                        self.bag.insert(
                            key,
                            Item {
                                id,
                                template,
                                count: delta,
                            },
                        );
                    }
                    None => {}
                }
            }
            ItemChange::Removed {
                slot_type, slot, ..
            }
            | ItemChange::RemovedSlot {
                slot_type, slot, ..
            } => match slot_type {
                SLOT_BAG => {
                    self.bag.remove(&u16::from(slot));
                }
                SLOT_EQUIPMENT => self.equipment.retain(|e| e.slot != slot),
                _ => {}
            },
            ItemChange::Moved {
                from_slot_type,
                from_slot,
                to_slot_type,
                to_slot,
                from_id,
                to_id,
            } => self.move_item(
                from_slot_type,
                from_slot,
                to_slot_type,
                to_slot,
                from_id,
                to_id,
            ),
            _ => {}
        }
    }

    /// Bag-to-bag moves and swaps. Moves involving equipment only keep the bag consistent.
    fn move_item(
        &mut self,
        from_type: u8,
        from: u8,
        to_type: u8,
        to: u8,
        from_id: u64,
        to_id: u64,
    ) {
        if from_type == SLOT_BAG && to_type == SLOT_BAG {
            let a = self.bag.remove(&u16::from(from));
            let b = self.bag.remove(&u16::from(to));
            if let Some(a) = a.filter(|a| a.id == from_id || from_id == 0) {
                self.bag.insert(u16::from(to), a);
            }
            if let Some(b) = b.filter(|b| to_id != 0 && b.id == to_id) {
                self.bag.insert(u16::from(from), b);
            }
        } else if from_type == SLOT_BAG {
            self.bag.remove(&u16::from(from));
        } else if to_type == SLOT_BAG && from_type == SLOT_EQUIPMENT {
            if let Some(e) = self.equipment.iter().find(|e| e.id == from_id).cloned() {
                self.equipment.retain(|x| x.id != from_id);
                self.bag.insert(
                    u16::from(to),
                    Item {
                        id: e.id,
                        template: e.template,
                        count: 1,
                    },
                );
            }
        }
    }

    fn apply_activity(&mut self, ev: &ActivityEvent) {
        match ev {
            ActivityEvent::Quests { list } => {
                for row in list {
                    self.quests.upsert(*row);
                }
            }
            ActivityEvent::CompletedQuests { ids } => {
                for id in ids {
                    self.quests.mark_done(*id);
                }
            }
            ActivityEvent::QuestStarted { quest, .. } => self.quests.upsert(**quest),
            ActivityEvent::QuestUpdated { quest, .. } => self.quests.upsert(**quest),
            ActivityEvent::QuestCompleted { quest_id, .. } => {
                self.quests.active.retain(|q| q.template != *quest_id);
                self.quests.mark_done(*quest_id);
            }
            ActivityEvent::QuestReset { quest_id, .. } => self.quests.mark_undone(*quest_id),
            ActivityEvent::QuestFailed { quest_id, err } => self.push_error(StateError::Quest {
                quest: *quest_id,
                err: *err,
            }),
            ActivityEvent::MyHouse(h) => self.upsert_house(h),
            ActivityEvent::LoginCharInfoHouse { owner_id, house } => {
                if *owner_id == self.me.char_id && self.me.char_id != 0 {
                    self.upsert_house(house);
                }
            }
            ActivityEvent::MyHouseRemoved { tl } => self.houses.retain(|h| h.tl != *tl),
            ActivityEvent::HouseBuildProgress { tl, all, cur, .. } => {
                if let Some(h) = self.houses.iter_mut().find(|h| h.tl == *tl) {
                    h.build_left = (*all - *cur).max(0);
                }
            }
            _ => {}
        }
    }

    fn upsert_house(&mut self, h: &aa_proto::sc_activity::HouseInfo) {
        let brief = HouseBrief {
            tl: h.tl,
            id: h.id,
            template: h.template,
            pos: [h.x, h.y, h.z],
            build_left: (h.all_action - h.cur_action).max(0),
        };
        match self.houses.iter_mut().find(|x| x.tl == h.tl) {
            Some(x) => *x = brief,
            None => {
                if self.houses.len() < 4 {
                    self.houses.push(brief);
                }
            }
        }
    }
}

/// Horizontal distance between two positions.
pub fn dist2d(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
    (dx * dx + dy * dy).sqrt()
}

/// Full distance between two positions.
pub fn dist3d(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dz = a[2] - b[2];
    (dist2d(a, b).powi(2) + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_proto::sc_world::{BuffInfo, DoodadInfo, InvItem, ItemTaskInfo, OwnState};

    fn own_state() -> OwnState {
        OwnState {
            id: 500,
            name: "Aria".into(),
            race: 1,
            gender: 2,
            level: 3,
            hp: 1000,
            mp: 400,
            zone_id: 7,
            faction_id: 101,
            expedition_id: 0,
            family_id: 0,
            abilities: [1, 11, 11],
            pos: (15000.0, 15100.0, 126.0),
            yaw: 1.0,
            instance_id: 1,
            exp: 50,
            recoverable_exp: 0,
            labor: 50,
            money: 20_000,
            bank_money: 0,
            loyalty: 0,
            bag_slots: 50,
            bank_slots: 0,
            equipment: Box::new([EquipSlot {
                slot: 15,
                template: 9,
                id: 99,
                grade: 1,
            }]),
        }
    }

    fn unit_info(obj: u32, ty: u8, id: u32) -> UnitInfo {
        UnitInfo {
            obj,
            unit_type: ty,
            id,
            name: "N".into(),
            owner_name: "".into(),
            owner_id: 0,
            tl_id: 0,
            pos: (15010.0, 15100.0, 126.0),
            level: 5,
            hp: 300,
            mp: 0,
            dead: false,
            has_loot: false,
            faction_id: 115,
            expedition_id: 0,
            yaw: 0,
            skills: Box::new([(18132, 1)]),
            buffs: Box::new([BuffInfo {
                list: 0,
                index: 4,
                buff_id: 77,
                duration_ms: 0,
                charge: 0,
            }]),
            complete: true,
        }
    }

    fn world(w: WorldEvent) -> ServerEvent {
        ServerEvent::World(w)
    }

    fn in_world() -> BotState {
        let mut s = BotState::new();
        s.apply(&world(WorldEvent::CharacterState(Box::new(own_state()))));
        s.apply(&world(WorldEvent::UnitState(Box::new(unit_info(
            0x400,
            unit_type::CHARACTER,
            500,
        )))));
        s
    }

    #[test]
    fn character_state_and_own_unit_state_fill_the_self_info() {
        let s = in_world();
        assert_eq!(s.me.char_id, 500);
        assert_eq!(&*s.me.name, "Aria");
        assert_eq!(s.me.obj, 0x400);
        assert!(s.me.spawned);
        assert_eq!((s.me.hp, s.me.hp_max, s.me.mp_max), (300, 1000, 400));
        assert_eq!(s.me.hp_pct(), 30);
        assert_eq!(s.me.money, 20_000);
        assert_eq!(s.equipment.len(), 1);
        assert_eq!(s.skills, vec![(18132, 1)]);
        assert_eq!(s.buffs.as_slice(), &[(4, 77)]);
        // the own unit is not an entity of the view
        assert!(s.units.is_empty());
        // a second own unit state does not move the bot
        let mut again = unit_info(0x400, unit_type::CHARACTER, 500);
        again.pos = (1.0, 2.0, 3.0);
        let mut s2 = s;
        s2.apply(&world(WorldEvent::UnitState(Box::new(again))));
        assert_eq!(s2.me.pos, [15010.0, 15100.0, 126.0]);
    }

    #[test]
    fn units_come_and_go_and_track_moves_points_and_death() {
        let mut s = in_world();
        s.apply(&world(WorldEvent::UnitState(Box::new(unit_info(
            0x401,
            unit_type::NPC,
            3475,
        )))));
        s.apply(&world(WorldEvent::UnitState(Box::new(unit_info(
            0x402,
            unit_type::CHARACTER,
            900,
        )))));
        // houses are not tracked
        s.apply(&world(WorldEvent::UnitState(Box::new(unit_info(
            0x403,
            unit_type::HOUSING,
            1,
        )))));
        assert_eq!(s.units.len(), 2);
        assert_eq!(s.unit(0x401).unwrap().kind, UnitKind::Npc);
        assert!(s.unit(0x401).unwrap().name.is_none());
        assert_eq!(s.unit(0x402).unwrap().name.as_deref(), Some("N"));
        assert_eq!(s.unit(0x401).unwrap().faction, 115);

        s.apply(&world(WorldEvent::UnitMoved {
            obj: 0x401,
            kind: 1,
            pos: (1.0, 2.0, 3.0),
            yaw: 0,
            stance: 0,
        }));
        assert_eq!(s.unit(0x401).unwrap().pos, [1.0, 2.0, 3.0]);

        s.now_ms = 10_000;
        s.apply(&world(WorldEvent::UnitDeath {
            obj: 0x401,
            killer: 0x400,
            respawn_wait_ms: 0,
            lost_exp: 0,
            reason: 0,
        }));
        assert!(s.unit(0x401).unwrap().dead());
        assert_eq!(s.kills.as_slice(), &[(0x401, 10_000)]);
        s.apply(&world(WorldEvent::LootableState {
            owner_obj: 0x401,
            owner_type: 1,
            has_loot: true,
        }));
        assert!(s.unit(0x401).unwrap().has_loot());

        s.apply(&world(WorldEvent::UnitsRemoved {
            objs: [0x401u32, 0x999].into_iter().collect(),
        }));
        assert!(s.unit(0x401).is_none());
        assert_eq!(s.units.len(), 1);
    }

    #[test]
    fn threats_follow_target_changes_and_damage_is_noted() {
        let mut s = in_world();
        s.now_ms = 5000;
        s.apply(&world(WorldEvent::TargetChanged {
            obj: 0x401,
            target: 0x400,
        }));
        assert_eq!(s.threats.as_slice(), &[(0x401, 5000)]);
        s.apply(&world(WorldEvent::TargetChanged {
            obj: 0x401,
            target: 0x777,
        }));
        assert!(s.threats.is_empty());

        s.now_ms = 6000;
        s.apply(&world(WorldEvent::UnitPoints {
            obj: 0x400,
            hp: 200,
            mp: 390,
        }));
        assert_eq!((s.me.hp, s.me.mp), (200, 390));
        assert!(s.damaged_within(1000));
        s.now_ms = 9000;
        assert!(!s.damaged_within(1000));
        // healing is not damage
        s.apply(&world(WorldEvent::UnitPoints {
            obj: 0x400,
            hp: 900,
            mp: 390,
        }));
        assert_eq!(s.last_damage_ms, 6000);
    }

    #[test]
    fn own_death_and_resurrection_leave_reactions() {
        let mut s = in_world();
        s.now_ms = 1000;
        s.apply(&world(WorldEvent::UnitDeath {
            obj: 0x400,
            killer: 0x401,
            respawn_wait_ms: 5000,
            lost_exp: 10,
            reason: 1,
        }));
        assert!(s.me.dead);
        assert_eq!(s.me.respawn_at_ms, 6000);
        assert!(std::mem::take(&mut s.reactions.died));
        s.apply(&world(WorldEvent::Resurrected {
            obj: 0x400,
            pos: (1.0, 2.0, 3.0),
            yaw: 0.5,
        }));
        assert!(!s.me.dead);
        assert_eq!(s.reactions.resurrected, Some([1.0, 2.0, 3.0]));
        assert_eq!(s.me.pos, [1.0, 2.0, 3.0]);
        assert!(s.me.hp >= 1);
        // a death without a wait still waits a second
        s.apply(&world(WorldEvent::UnitDeath {
            obj: 0x400,
            killer: 0,
            respawn_wait_ms: 0,
            lost_exp: 0,
            reason: 0,
        }));
        assert_eq!(s.me.respawn_at_ms, 2000);
    }

    #[test]
    fn teleport_and_kick_are_noted() {
        let mut s = in_world();
        s.apply(&world(WorldEvent::TeleportUnit {
            reason: 0,
            error: 0,
            pos: (10.0, 20.0, 30.0),
        }));
        assert_eq!(s.me.pos, [10.0, 20.0, 30.0]);
        assert_eq!(s.reactions.teleport, Some([10.0, 20.0, 30.0]));
        s.apply(&world(WorldEvent::Kicked {
            reason: 3,
            msg: "bye".into(),
        }));
        assert_eq!(s.reactions.kicked.as_ref().map(|k| k.0), Some(3));
    }

    #[test]
    fn inventory_chunks_and_item_tasks() {
        let mut s = in_world();
        s.apply(&world(WorldEvent::Inventory {
            slot_type: SLOT_BAG,
            chunk: 0,
            items: Box::new([
                InvItem {
                    slot: 0,
                    id: 11,
                    template: 4045,
                    grade: 0,
                    count: 3,
                },
                InvItem {
                    slot: 7,
                    id: 12,
                    template: 417,
                    grade: 0,
                    count: 1,
                },
            ]),
        }));
        s.apply(&world(WorldEvent::Inventory {
            slot_type: SLOT_BAG,
            chunk: 1,
            items: Box::new([InvItem {
                slot: 12,
                id: 13,
                template: 4045,
                grade: 0,
                count: 2,
            }]),
        }));
        // a bank chunk is not the bag
        s.apply(&world(WorldEvent::Inventory {
            slot_type: 3,
            chunk: 0,
            items: Box::new([InvItem {
                slot: 1,
                id: 14,
                template: 1,
                grade: 0,
                count: 1,
            }]),
        }));
        assert_eq!(s.bag.len(), 3);
        assert_eq!(s.item_count(4045), 5);
        assert_eq!(s.find_item(4045).unwrap().0, 0);

        let task = |changes: Vec<ItemChange>| {
            world(WorldEvent::ItemTask(Box::new(ItemTaskInfo {
                reason: 0,
                changes: changes.into_iter().collect(),
                force_removed: Default::default(),
            })))
        };
        s.apply(&task(vec![
            ItemChange::Money(-5000),
            ItemChange::CountUpdate {
                slot_type: SLOT_BAG,
                slot: 0,
                id: 11,
                delta: -3,
                template: 4045,
            },
            ItemChange::Added {
                action: 5,
                slot_type: SLOT_BAG,
                slot: 20,
                template: 555,
                id: 99,
                grade: 0,
                count: 4,
            },
            ItemChange::Removed {
                slot_type: SLOT_BAG,
                slot: 7,
                id: 12,
                template: 417,
            },
        ]));
        assert_eq!(s.me.money, 15_000);
        assert!(!s.bag.contains_key(&0), "an emptied stack leaves the bag");
        assert!(!s.bag.contains_key(&7));
        assert_eq!(s.item_count(555), 4);

        s.apply(&task(vec![ItemChange::Moved {
            from_slot_type: SLOT_BAG,
            from_slot: 20,
            to_slot_type: SLOT_BAG,
            to_slot: 12,
            from_id: 99,
            to_id: 13,
        }]));
        assert_eq!(s.bag.get(&12).map(|i| i.id), Some(99));
        assert_eq!(s.bag.get(&20).map(|i| i.id), Some(13));
    }

    #[test]
    fn quests_are_tracked_through_their_life() {
        let mut s = in_world();
        let row = |template: u32, status: u8| QuestRow {
            id: i64::from(template),
            template,
            status,
            objectives: [0; 5],
            left_time_ms: -1,
            component: 0,
            acceptor_type: 0,
            acceptor_id: 0,
        };
        let act = ServerEvent::Activity;
        s.apply(&act(ActivityEvent::Quests {
            list: vec![
                row(10, quest_status::PROGRESS),
                row(11, quest_status::READY),
            ],
        }));
        s.apply(&act(ActivityEvent::CompletedQuests { ids: vec![3, 5, 9] }));
        assert_eq!(s.quests.active.len(), 2);
        assert!(s.quests.is_done(5) && !s.quests.is_done(6));

        s.apply(&act(ActivityEvent::QuestStarted {
            quest: Box::new(row(12, quest_status::PROGRESS)),
            component: 1,
        }));
        s.apply(&act(ActivityEvent::QuestUpdated {
            quest: Box::new(row(12, quest_status::READY)),
            component: 2,
            p: [0; 4],
        }));
        assert_eq!(s.quests.row(12).unwrap().status, quest_status::READY);
        assert_eq!(s.quests.active.len(), 3);

        s.apply(&act(ActivityEvent::QuestCompleted {
            quest_id: 12,
            component: 0,
            block_mask: 0,
        }));
        assert!(s.quests.row(12).is_none() && s.quests.is_done(12));
        s.apply(&act(ActivityEvent::QuestUpdated {
            quest: Box::new(row(10, quest_status::DROPPED)),
            component: 0,
            p: [0; 4],
        }));
        assert!(s.quests.row(10).is_none() && !s.quests.is_done(10));
        s.apply(&act(ActivityEvent::QuestReset {
            quest_id: 12,
            block_id: 0,
            block_mask: 0,
        }));
        assert!(!s.quests.is_done(12));
        s.apply(&act(ActivityEvent::QuestFailed {
            quest_id: 11,
            err: 4,
        }));
        assert_eq!(
            s.errors.last().map(|e| e.1),
            Some(StateError::Quest { quest: 11, err: 4 })
        );
    }

    #[test]
    fn doodads_are_bounded_and_phase_updates() {
        let mut s = in_world();
        let d = |obj: u32| DoodadInfo {
            obj,
            template: 7,
            pos: (1.0, 2.0, 3.0),
            owner_obj: 0,
            parent_obj: 0,
            phase: 1,
            owner_id: 0,
            quest_glow: 0,
            has_loot: false,
        };
        s.apply(&world(WorldEvent::DoodadsCreated(
            [d(1), d(2)].into_iter().collect(),
        )));
        s.apply(&world(WorldEvent::DoodadPhase { obj: 2, phase: 9 }));
        assert_eq!(s.doodads[&2].phase, 9);
        s.apply(&world(WorldEvent::DoodadsRemoved(
            [1u32].into_iter().collect(),
        )));
        assert_eq!(s.doodads.len(), 1);
        for i in 0..(MAX_DOODADS as u32 + 10) {
            s.apply(&world(WorldEvent::DoodadsCreated(
                [d(1000 + i)].into_iter().collect(),
            )));
        }
        assert_eq!(s.doodads.len(), MAX_DOODADS);
        assert!(s.dropped > 0);
    }

    #[test]
    fn cooldowns_chat_and_errors_are_bounded() {
        let mut s = in_world();
        s.now_ms = 100;
        s.set_cooldown(2, 500);
        assert!(!s.cooldown_ready(2, 400));
        assert!(s.cooldown_ready(2, 500));
        assert!(s.cooldown_ready(99, 0));
        s.apply(&world(WorldEvent::CooldownReset {
            obj: 0x400,
            skill: 2,
            tag: 0,
            gcd: false,
        }));
        assert!(s.cooldown_ready(2, 0));
        for k in 0..40u32 {
            s.set_cooldown(1000 + k, 10_000);
        }
        assert!(s.cooldowns.len() <= 16);

        for i in 0..10 {
            s.apply(&world(WorldEvent::Chat {
                kind: 0,
                sender_obj: 0x500 + i,
                sender_id: 1,
                name: "Bob".into(),
                msg: "hi".repeat(300).into(),
            }));
            s.apply(&world(WorldEvent::ErrorMsg {
                err: i as i16,
                kind: 0,
                notify: false,
            }));
        }
        assert_eq!(s.inbox.len(), 4);
        assert_eq!(s.inbox[0].text.chars().count(), 200);
        assert_eq!(s.errors.len(), 8);
        assert_eq!(s.errors.last().map(|e| e.1), Some(StateError::Msg(9)));
        // own chat echoes are not "heard"
        let before = s.inbox.len();
        s.apply(&world(WorldEvent::Chat {
            kind: 0,
            sender_obj: 0x400,
            sender_id: 500,
            name: "Aria".into(),
            msg: "me".into(),
        }));
        assert_eq!(s.inbox.len(), before);
    }

    #[test]
    fn reset_frees_everything() {
        let mut s = in_world();
        for i in 0..200u32 {
            s.apply(&world(WorldEvent::UnitState(Box::new(unit_info(
                0x1000 + i,
                unit_type::NPC,
                1,
            )))));
        }
        assert_eq!(s.units.len(), 200);
        s.reset();
        assert!(s.units.is_empty() && s.units.capacity() == 0);
        assert_eq!(s.me.obj, 0);
    }
}
