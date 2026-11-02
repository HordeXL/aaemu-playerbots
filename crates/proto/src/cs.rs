//! Client → server (`CS*`) packet builders.
//!
//! One function per packet a bot sends. Each returns an [`Out`] (level, opcode and body) that
//! [`crate::game::GameConn::send`] frames. Every layout matches the server's `read()` byte for
//! byte; the doc comment of each builder cites it (paths are relative to
//! `AAEmu-1.2-Rust/rust/crates/aaemu-game/src/`) and `tests/cs_*.rs` pins the bytes.
//!
//! Notation: `bc` is a 3-byte little-endian object id, `fx64` a coordinate as the
//! server's `i64` fixed point ([`crate::pos::convert_coord_to_long`]), `str` a `u16`-length-prefixed
//! string ([`PacketStream::write_str`]).

use crate::game::{cs_name, LEVEL_GAME, LEVEL_PROXY};
use crate::opcodes::{cs, pp};
use crate::pos::{convert_coord_to_long, encode_position};
use crate::stream::PacketStream;

/// An outbound packet, ready for [`crate::game::GameConn::send`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Out {
    /// 1 = game (`CS*`), 2 = proxy.
    pub level: u8,
    /// The `cs::` (level 1) or `pp::` (level 2) opcode.
    pub opcode: u16,
    /// The packet body, without the `DD <level> ...` header.
    pub body: Vec<u8>,
}

impl Out {
    /// C# constant name of the opcode, for logging.
    pub fn name(&self) -> &'static str {
        cs_name(self.level, self.opcode)
    }
}

/// Builds a level-1 packet from `fill`.
fn game(opcode: u16, fill: impl FnOnce(&mut PacketStream)) -> Out {
    let mut s = PacketStream::new();
    fill(&mut s);
    Out {
        level: LEVEL_GAME,
        opcode,
        body: s.into_bytes(),
    }
}

/// Builds a level-2 (proxy) packet from `fill`.
fn proxy(opcode: u16, fill: impl FnOnce(&mut PacketStream)) -> Out {
    let mut o = game(opcode, fill);
    o.level = LEVEL_PROXY;
    o
}

fn fx64(v: f32) -> i64 {
    convert_coord_to_long(v)
}

// ---- session --------------------------------------------------------------------------------

/// `X2EnterWorldPacket` (level 1): presents the login server's world cookie.
/// Server read: `packets/c2g.rs:30`.
///
/// Body: `u32 0, u32 0, u32 account_id, u32 cookie, i32 0 zone, u8 0 tb, u64 0 revision`.
pub fn enter_world(account_id: u32, cookie: u32) -> Out {
    game(cs::X2EnterWorldPacket, |s| {
        s.write_u32(0)
            .write_u32(0)
            .write_u32(account_id)
            .write_u32(cookie)
            .write_i32(0)
            .write_u8(0)
            .write_u64(0);
    })
}

/// `FinishStatePacket` (proxy 0x001): drives the CryNetwork state machine; state 0 asks for the
/// lobby configuration burst. Server read: `packets/proxy.rs:127`.
///
/// Body: `i32 state`.
pub fn finish_state(state: i32) -> Out {
    proxy(pp::FinishStatePacket, |s| {
        s.write_i32(state);
    })
}

/// `PingPacket` (proxy 0x012), the keep-alive (the server drops silent connections after 30 s).
/// Server read: `packets/proxy.rs:207`.
///
/// Body: `i64 ms, i64 ms, u32 ms` (`tm`, `when`, `local`); `ms` is the bot's milliseconds since
/// it connected. The server echoes the three values in a `PongPacket`.
pub fn ping(ms: i64) -> Out {
    proxy(pp::PingPacket, |s| {
        s.write_i64(ms).write_i64(ms).write_u32(ms as u32);
    })
}

/// `CSListCharacterPacket`. Server read: `packets/c2g.rs:174`.
///
/// Body: `i32 0, u16 0` (a size and an empty sized byte array).
pub fn list_characters() -> Out {
    game(cs::CSListCharacterPacket, |s| {
        s.write_i32(0).write_bytes_sized(&[]);
    })
}

/// The fields of `CSCreateCharacterPacket` a bot chooses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCharacter<'a> {
    /// Character name.
    pub name: &'a str,
    /// Race id (1 = Nuian).
    pub race: u8,
    /// Gender id (1 = male, 2 = female).
    pub gender: u8,
    /// Body part item templates (face, hair, ...); 0 = the race template's default.
    pub body_items: [u32; 7],
    /// Starting ability (class) id, e.g. 1 = Fight.
    pub ability1: u8,
}

/// `CSCreateCharacterPacket`. Server read: `packets/c2g_world.rs:22`.
///
/// Body: `str name, u8 race, u8 gender, 7 x u32 body items, UnitCustomModelParams, u8 ability1,
/// u8 11 ability2 (none), u8 11 ability3 (none), u8 1 level`.
///
/// The appearance is a type-3 (`Face`) `UnitCustomModelParams` with every field zero and a
/// 128-byte zero modifier, 231 bytes: the form the real client sends and the one proven against
/// the server (`models/unit_model.rs:106` for `FaceModel`, `:161` for the type gating). The
/// server also accepts the 1-byte type-0 form; it is not used so a bot never gets a model
/// without a face.
pub fn create_character(c: &CreateCharacter<'_>) -> Out {
    game(cs::CSCreateCharacterPacket, |s| {
        s.write_str(c.name).write_u8(c.race).write_u8(c.gender);
        for item in c.body_items {
            s.write_u32(item);
        }
        write_default_face(s);
        s.write_u8(c.ability1).write_u8(11).write_u8(11).write_u8(1);
    })
}

/// A zero `UnitCustomModelParams` of type 3 (231 bytes).
fn write_default_face(s: &mut PacketStream) {
    s.write_u8(3) // UnitCustomModelType.Face
        .write_u32(0) // hair color
        .write_u32(0) // skin color
        .write_u32(0); // model id
                       // FaceModel
    s.write_u32(0) // movable decal asset
        .write_f32(0.0) // weight
        .write_f32(0.0) // scale
        .write_f32(0.0) // rotate
        .write_i16(0) // move x
        .write_i16(0); // move y
    for _ in 0..4 {
        s.write_u32(0).write_f32(0.0); // fixed decal assets
    }
    for _ in 0..3 {
        s.write_u32(0); // diffuse, normal, eyelash maps
    }
    s.write_f32(0.0); // normal map weight
    for _ in 0..5 {
        s.write_u32(0); // lip, pupils, eyebrow, deco colors
    }
    s.write_bytes_sized(&[0u8; 128]); // modifier
}

/// `CSSelectCharacterPacket`. Server read: `packets/c2g_world.rs:124`.
///
/// Body: `u32 character_id, bool false gm, u8 0`.
pub fn select_character(character_id: u32) -> Out {
    game(cs::CSSelectCharacterPacket, |s| {
        s.write_u32(character_id).write_bool(false).write_u8(0);
    })
}

/// `CSSpawnCharacterPacket` with the visual options the real client sends. Server read:
/// `packets/c2g_world.rs:150` and `VisualOptions::read` at `models/char_data.rs:507`.
///
/// Body: `u8 0x1F` (stp, helmet, back holdable, cosplay, cosplay backpack; bit 0x01 is required
/// for the server to keep a visual-options record), 6 zero `stp` bytes, 4 x `bool true`.
pub fn spawn_character() -> Out {
    game(cs::CSSpawnCharacterPacket, |s| {
        s.write_u8(0x1F)
            .write_raw(&[0u8; 6])
            .write_bool(true)
            .write_bool(true)
            .write_bool(true)
            .write_bool(true);
    })
}

/// `CSNotifyInGamePacket` (empty). Server read: `packets/c2g_world.rs:175`.
pub fn notify_in_game() -> Out {
    game(cs::CSNotifyInGamePacket, |_| {})
}

/// `CSNotifyInGameCompletedPacket` (empty). Server read: `packets/c2g_world.rs:198`.
pub fn notify_in_game_completed() -> Out {
    game(cs::CSNotifyInGameCompletedPacket, |_| {})
}

/// `CSLeaveWorldPacket` target: log out of the game.
pub const LEAVE_QUIT: u8 = 0;
/// `CSLeaveWorldPacket` target: back to the character list.
pub const LEAVE_CHARACTER_SELECT: u8 = 1;
/// `CSLeaveWorldPacket` target: back to the server list.
pub const LEAVE_SERVER_SELECT: u8 = 2;

/// `CSLeaveWorldPacket`. Server read: `packets/c2g.rs:229` (`LeaveWorldTargetType`, `:206`).
///
/// Body: `u8 target` ([`LEAVE_QUIT`], [`LEAVE_CHARACTER_SELECT`], [`LEAVE_SERVER_SELECT`]).
pub fn leave_world(target: u8) -> Out {
    game(cs::CSLeaveWorldPacket, |s| {
        s.write_u8(target);
    })
}

/// `CSInstanceLoadedPacket` (empty): the answer to `SCLoadInstance`; the server then files the
/// character in the instance and repopulates its view. Server flow:
/// `systems/instances/flow_tests.rs:1051`.
pub fn instance_loaded() -> Out {
    game(cs::CSInstanceLoadedPacket, |_| {})
}

/// `CSTeleportEndedPacket`: the answer to a server teleport; the server then accepts movement
/// again. Server read: `systems/npcs/portal/packets.rs:207`.
///
/// Body: `fx64 x, fx64 y, f32 z`, then a 16-byte orientation (the identity quaternion
/// `0, 0, 0, 1.0f32`).
pub fn teleport_ended(x: f32, y: f32, z: f32) -> Out {
    game(cs::CSTeleportEndedPacket, |s| {
        s.write_i64(fx64(x))
            .write_i64(fx64(y))
            .write_f32(z)
            .write_f32(0.0)
            .write_f32(0.0)
            .write_f32(0.0)
            .write_f32(1.0);
    })
}

// ---- movement -------------------------------------------------------------------------------

/// `GameStanceType` Combat (weapons drawn).
pub const STANCE_COMBAT: u8 = 0;
/// `GameStanceType` Relaxed (the normal out-of-combat stance).
pub const STANCE_RELAXED: u8 = 1;
/// `MoveTypeAlertness` Idle.
pub const ALERT_IDLE: u8 = 0;
/// `MoveTypeAlertness` Combat.
pub const ALERT_COMBAT: u8 = 2;

/// `MoveTypeFlags.Moving`.
const FLAGS_MOVING: u8 = 0x02;
/// `MoveTypeFlags.Stopping`.
const FLAGS_STOPPING: u8 = 0x04;
/// `MoveTypeActorFlags.StandingOnSolid` (running).
const ACTOR_RUNNING: u8 = 0x04;
/// `FLAG1 | StandingOnSolid` (walking).
const ACTOR_WALKING: u8 = 0x05;
/// Horizontal speed above which a moving update is announced as running (m/s).
const RUN_SPEED: f32 = 3.0;
/// `DeltaMovement` of a unit pushing the stick fully forward (`[0, 127, 0]`).
const DELTA_FORWARD: i8 = 127;
/// Wire units per m/s of a unit's velocity shorts.
const VELOCITY_WIRE_PER_MPS: f32 = 1000.0;
/// Degrees per step of the direction byte (`360 / 128`).
const DIRECTION_STEP_DEG: f64 = 360.0 / 128.0;

/// One `CSMoveUnitPacket` for a unit the bot controls (its own character).
///
/// # Encoding
///
/// A type-1 (`MoveTypeEnum.Unit`) body: `bc obj, u8 1, u32 time, u8 flags, position(9),
/// i16 vel x3, i8 rotation x3, i8 delta movement x3, u8 stance, u8 alertness, u8 actor flags`
/// (server read: `packets/c2g_world.rs:224`, `MoveType::read` `models/movement.rs:261`,
/// `UnitMove::read` `models/movement.rs:418`). The server broadcasts it unchanged as
/// `SCOneUnitMovementPacket` to real clients, so the values follow what the server's own NPC
/// and skill-controller movement sends (`systems/ai/simulation.rs:521-560`,
/// `systems/ai/npc.rs:370-430`, stop: `:592-606`) and what the C# PlayerBots broadcaster
/// announced for bots:
///
/// | | moving (`moving = true`) | stopped |
/// | --- | --- | --- |
/// | header flags | `0x02` Moving | `0x04` Stopping |
/// | velocity shorts | `round(v * 1000)` per axis, clamped to `i16` (m/s) | 0 |
/// | actor flags | `0x04` when speed > 3 m/s (running), else `0x05` (walking) | 0 |
/// | delta movement | `[0, 127, 0]` (forward) | `[0, 0, 0]` |
///
/// The server only needs the position (`systems/skills::on_move_request` treats a non-zero
/// velocity as "moving" for buff removal); the rest makes the bot look right to real players.
/// `time` must change from packet to packet; the server's NPCs send milliseconds of the day.
///
/// # Yaw
///
/// `yaw` is in radians: **0 faces +Y, positive turns counter-clockwise** (so +π/2 faces −X,
/// −π/2 faces +X, ±π faces −Y). It is the server's `rot_z` (an NPC walking from `a` to `b`
/// has `yaw = atan2(dy, dx) - π/2`); [`yaw_towards`] computes it. The direction byte is
/// `round(yaw / (2π / 128))` taken modulo 128 into `-42..=85` ([`yaw_to_direction`]), written
/// as rotation z; rotation x and y (roll, pitch) are 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveUnit {
    /// Object id of the moved unit (the bot's character).
    pub obj: u32,
    /// Movement timestamp; must keep changing (ms is fine).
    pub time: u32,
    /// World position `(x, y, z)`.
    pub pos: (f32, f32, f32),
    /// Velocity in m/s; ignored when `moving` is false.
    pub vel: (f32, f32, f32),
    /// Facing in radians, see the type docs.
    pub yaw: f32,
    /// Whether this update announces motion or a stop.
    pub moving: bool,
    /// `GameStanceType`: [`STANCE_RELAXED`] or [`STANCE_COMBAT`].
    pub stance: u8,
    /// `MoveTypeAlertness`: [`ALERT_IDLE`] or [`ALERT_COMBAT`].
    pub alertness: u8,
}

/// Builds the `CSMoveUnitPacket` of `m`; see [`MoveUnit`] for the encoding.
pub fn move_unit(m: &MoveUnit) -> Out {
    let wire = |v: f32| (v * VELOCITY_WIRE_PER_MPS).round().clamp(-32768.0, 32767.0) as i16;
    let (flags, vel, actor, delta) = if m.moving {
        let speed = (m.vel.0 * m.vel.0 + m.vel.1 * m.vel.1 + m.vel.2 * m.vel.2).sqrt();
        (
            FLAGS_MOVING,
            [wire(m.vel.0), wire(m.vel.1), wire(m.vel.2)],
            if speed > RUN_SPEED {
                ACTOR_RUNNING
            } else {
                ACTOR_WALKING
            },
            DELTA_FORWARD,
        )
    } else {
        (FLAGS_STOPPING, [0; 3], 0, 0)
    };
    game(cs::CSMoveUnitPacket, |s| {
        s.write_bc(m.obj)
            .write_u8(1) // MoveTypeEnum.Unit
            .write_u32(m.time)
            .write_u8(flags)
            .write_raw(&encode_position(m.pos.0, m.pos.1, m.pos.2))
            .write_i16(vel[0])
            .write_i16(vel[1])
            .write_i16(vel[2])
            .write_i8(0) // rotation x (roll)
            .write_i8(0) // rotation y (pitch)
            .write_i8(yaw_to_direction(m.yaw)) // rotation z (yaw)
            .write_i8(0) // delta movement
            .write_i8(delta)
            .write_i8(0)
            .write_u8(m.stance)
            .write_u8(m.alertness)
            .write_u8(actor);
    })
}

/// The yaw (radians, 0 = facing +Y, positive = counter-clockwise, in `(-π, π]`) that faces `to`
/// from `from`; 0 when the points coincide. The server's NPCs use the same convention
/// (`systems/ai/npc.rs:398-401`: `yaw = angle - 90°`, `angle = atan2(dy, dx)`).
pub fn yaw_towards(from: (f32, f32), to: (f32, f32)) -> f32 {
    let (dx, dy) = (f64::from(to.0 - from.0), f64::from(to.1 - from.1));
    if dx == 0.0 && dy == 0.0 {
        return 0.0;
    }
    let yaw = (dy.atan2(dx) - std::f64::consts::FRAC_PI_2 + std::f64::consts::PI)
        .rem_euclid(std::f64::consts::TAU)
        - std::f64::consts::PI;
    // `rem_euclid` yields [-π, π); fold -π onto +π so a half turn has one representation.
    (if yaw <= -std::f64::consts::PI {
        yaw + std::f64::consts::TAU
    } else {
        yaw
    }) as f32
}

/// The direction byte for `yaw` (radians): one step is 2.8125 degrees, 128 steps a full turn,
/// folded into `-42..=85` like the server's `convert_radian_to_direction`
/// (`models/movement.rs:756`). 0 = +Y, 32 = −X, 64 = −Y, −32 = +X.
pub fn yaw_to_direction(yaw: f32) -> i8 {
    let steps = (f64::from(yaw).to_degrees() / DIRECTION_STEP_DEG).round() as i64;
    let n = steps.rem_euclid(128);
    (if n > 85 { n - 128 } else { n }) as i8
}

// ---- combat and interaction -----------------------------------------------------------------

/// Writes a `CSStartSkillPacket` header and caster `0` (unit) + the object id.
fn start_skill(
    skill_id: u32,
    caster: impl FnOnce(&mut PacketStream),
    target: impl FnOnce(&mut PacketStream),
) -> Out {
    game(cs::CSStartSkillPacket, |s| {
        s.write_u32(skill_id);
        caster(s);
        target(s);
        s.write_u8(0); // flag: no skill object
    })
}

/// `CSStartSkillPacket`: a unit casting on a unit (the common attack / buff / heal).
/// Server read: `systems/skills/handlers.rs:33`, `SkillCaster::read` `systems/skills/types.rs:66`,
/// `SkillCastTarget::read` `:236`.
///
/// Body: `u32 skill, u8 0 caster type, bc caster, u8 0 target type, bc target, u8 0 flag`.
pub fn start_skill_unit(skill_id: u32, caster: u32, target: u32) -> Out {
    start_skill(
        skill_id,
        |s| {
            s.write_u8(0).write_bc(caster);
        },
        |s| {
            s.write_u8(0).write_bc(target);
        },
    )
}

/// `CSStartSkillPacket`: a unit casting on a doodad (harvesting, interaction skills).
/// Server read as [`start_skill_unit`].
///
/// Body: `u32 skill, u8 0, bc caster, u8 4 target type, bc doodad, u8 0`.
pub fn start_skill_doodad(skill_id: u32, caster: u32, doodad: u32) -> Out {
    start_skill(
        skill_id,
        |s| {
            s.write_u8(0).write_bc(caster);
        },
        |s| {
            s.write_u8(4).write_bc(doodad);
        },
    )
}

/// `CSStartSkillPacket`: a unit casting on a ground position (area and placement skills).
/// Server read as [`start_skill_unit`] (`SkillCastTarget::read` position variant).
///
/// Body: `u32 skill, u8 0, bc caster, u8 1 target type, fx64 x, fx64 y, f32 z, f32 rot, bc 0,
/// bc 0, u8 0`.
pub fn start_skill_position(
    skill_id: u32,
    caster: u32,
    pos: (f32, f32, f32),
    rotation: f32,
) -> Out {
    start_skill(
        skill_id,
        |s| {
            s.write_u8(0).write_bc(caster);
        },
        |s| {
            s.write_u8(1)
                .write_i64(fx64(pos.0))
                .write_i64(fx64(pos.1))
                .write_f32(pos.2)
                .write_f32(rotation)
                .write_bc(0)
                .write_bc(0);
        },
    )
}

/// `CSStartSkillPacket`: a skill cast by an inventory item (potions, scrolls, summon items) on a
/// unit (often the caster itself). Server read as [`start_skill_unit`].
///
/// Body: `u32 skill, u8 2 caster type, bc caster, u64 item id, u32 item template, u8 0, u32 0,
/// u8 0 target type, bc target, u8 0`.
pub fn start_skill_item(
    skill_id: u32,
    caster: u32,
    item_id: u64,
    item_template: u32,
    target: u32,
) -> Out {
    start_skill(
        skill_id,
        |s| {
            s.write_u8(2)
                .write_bc(caster)
                .write_u64(item_id)
                .write_u32(item_template)
                .write_u8(0)
                .write_u32(0);
        },
        |s| {
            s.write_u8(0).write_bc(target);
        },
    )
}

/// `CSStopCastingPacket`. Server read: `systems/skills/handlers.rs:762`.
///
/// Body: `u16 tl, u16 plot_tl, bc obj`.
pub fn stop_casting(tl: u16, plot_tl: u16, obj: u32) -> Out {
    game(cs::CSStopCastingPacket, |s| {
        s.write_u16(tl).write_u16(plot_tl).write_bc(obj);
    })
}

/// `CSChangeTargetPacket`: selects a unit (0 clears). Server read: `systems/skills/handlers.rs:1068`.
///
/// Body: `bc target`.
pub fn change_target(target: u32) -> Out {
    game(cs::CSChangeTargetPacket, |s| {
        s.write_bc(target);
    })
}

/// `CSLearnSkillPacket`. Server read: `systems/skills/handlers.rs:950`.
///
/// Body: `u32 skill`.
pub fn learn_skill(skill_id: u32) -> Out {
    game(cs::CSLearnSkillPacket, |s| {
        s.write_u32(skill_id);
    })
}

/// `CSSwapAbilityPacket`: replaces ability `old` with `new` in the character's slots.
/// Server read: `systems/skills/handlers.rs:977`.
///
/// Body: `bc obj, u8 old, u8 new, bool false`.
pub fn swap_ability(obj: u32, old: u8, new: u8) -> Out {
    game(cs::CSSwapAbilityPacket, |s| {
        s.write_bc(obj)
            .write_u8(old)
            .write_u8(new)
            .write_bool(false);
    })
}

/// `CSResurrectCharacterPacket`. Server read: `systems/skills/resurrect.rs:42`.
///
/// Body: `bool in_place` (false = at the respawn point).
pub fn resurrect(in_place: bool) -> Out {
    game(cs::CSResurrectCharacterPacket, |s| {
        s.write_bool(in_place);
    })
}

/// `CSInteractNPCPacket`: the first click on an NPC. Server read: `systems/npcs/npc/packets.rs:375`.
///
/// Body: `bc npc, bool target_changed`.
pub fn interact_npc(npc: u32, target_changed: bool) -> Out {
    game(cs::CSInteractNPCPacket, |s| {
        s.write_bc(npc).write_bool(target_changed);
    })
}

/// `CSStartInteractionPacket`: asks the NPC for its interaction (skill) list, sent after
/// [`interact_npc`]. Server read: `systems/npcs/npc/packets.rs:293`.
///
/// Body: `bc npc, bc npc, i32 0 extra info, i32 0 pick id, u8 1 mouse button, i32 0 modifier keys`.
pub fn start_interaction(npc: u32) -> Out {
    game(cs::CSStartInteractionPacket, |s| {
        s.write_bc(npc)
            .write_bc(npc)
            .write_i32(0)
            .write_i32(0)
            .write_u8(1)
            .write_i32(0);
    })
}

/// `CSSetForceAttackPacket`: toggles attacking non-hostile units. Server read:
/// `systems/skills/handlers.rs:986`.
///
/// Body: `bool on`.
pub fn set_force_attack(on: bool) -> Out {
    game(cs::CSSetForceAttackPacket, |s| {
        s.write_bool(on);
    })
}

// ---- loot -----------------------------------------------------------------------------------

/// Loot owner type of a unit's corpse (`SCLootItemTookPacket.loot_owner_type` = 1).
pub const LOOT_OWNER_UNIT: u16 = 1;

/// `CSLootOpenBagPacket`: opens (or with `loot_all`, takes everything from) a corpse's bag.
/// Server read: `systems/economy/looting.rs:1204`.
///
/// Body: `bc owner, bc 0, bool loot_all`.
pub fn loot_open_bag(owner: u32, loot_all: bool) -> Out {
    game(cs::CSLootOpenBagPacket, |s| {
        s.write_bc(owner).write_bc(0).write_bool(loot_all);
    })
}

/// `CSLootItemPacket`: takes one item of an open bag. Server read:
/// `systems/economy/looting.rs:1257`.
///
/// Body: `u16 index, u16 owner_type, bc owner, u16 0, u16 0`; the owner type of a unit's corpse
/// is [`LOOT_OWNER_UNIT`].
pub fn loot_item(index: u16, owner_type: u16, owner: u32) -> Out {
    game(cs::CSLootItemPacket, |s| {
        s.write_u16(index)
            .write_u16(owner_type)
            .write_bc(owner)
            .write_u16(0)
            .write_u16(0);
    })
}

/// `CSLootCloseBagPacket`. Server read: `systems/economy/looting.rs:1289`.
///
/// Body: `u16 index, u16 owner_type, bc owner, u8 0`.
pub fn loot_close_bag(index: u16, owner_type: u16, owner: u32) -> Out {
    game(cs::CSLootCloseBagPacket, |s| {
        s.write_u16(index)
            .write_u16(owner_type)
            .write_bc(owner)
            .write_u8(0);
    })
}

// ---- chat -----------------------------------------------------------------------------------

/// Chat channel ids of `CSSendChatMessagePacket` (`systems/social/chat.rs:17`).
pub mod chat_type {
    /// Private message to the named target.
    pub const WHISPER: i16 = -3;
    /// Local ("white") chat.
    pub const SAY: i16 = 0;
    /// Shout.
    pub const SHOUT: i16 = 1;
    /// Party chat.
    pub const PARTY: i16 = 4;
}

/// Longest chat line (in characters) the builders accept.
pub const MAX_CHAT_CHARS: usize = 200;

/// Characters the server (or a client) could take for the command slash.
pub fn is_slash_like(c: char) -> bool {
    matches!(
        c,
        '/' | '\\'
            | '\u{2044}'
            | '\u{2215}'
            | '\u{2216}'
            | '\u{29F5}'
            | '\u{29F8}'
            | '\u{2571}'
            | '\u{2572}'
            | '\u{27CB}'
            | '\u{27CD}'
            | '\u{2AFB}'
            | '\u{2AFD}'
            | '\u{FF0F}'
            | '\u{FF3C}'
    )
}

/// A character the command check looks past: whitespace, control characters (the server strips
/// NULs at both ends of a string before it looks for the slash) and invisible format characters.
fn is_ignorable(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '\u{200b}'..='\u{200f}' | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{ad}')
}

/// Whether the first visible character of `text` is a slash (or looks like one): the server
/// would run the line as a GM command, whatever the chat type.
pub fn is_command_like(text: &str) -> bool {
    text.chars()
        .find(|c| !is_ignorable(*c))
        .is_some_and(is_slash_like)
}

/// `text` as a plain chat line, or `None` when it must not be sent: empty, longer than
/// [`MAX_CHAT_CHARS`], with a control character (a NUL could hide a leading slash from a check)
/// or command-like. Returns the trimmed text.
pub fn plain_chat_text(text: &str) -> Option<&str> {
    let t = text.trim();
    let ok = !t.is_empty()
        && t.chars().count() <= MAX_CHAT_CHARS
        && !t.chars().any(char::is_control)
        && !is_command_like(t);
    ok.then_some(t)
}

/// What a refused chat line is turned into: control characters removed, the leading run of
/// invisible and slash-like characters dropped, cut to [`MAX_CHAT_CHARS`]; "." if nothing is left.
fn neutralize(text: &str) -> String {
    let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
    let rest = cleaned
        .trim_start_matches(|c: char| is_ignorable(c) || is_slash_like(c))
        .trim_end();
    if rest.is_empty() {
        ".".to_string()
    } else {
        rest.chars().take(MAX_CHAT_CHARS).collect()
    }
}

fn chat_packet(chat_type: i16, target: &str, message: &str) -> Out {
    game(cs::CSSendChatMessagePacket, |s| {
        s.write_i16(chat_type)
            .write_i16(0)
            .write_i32(0)
            .write_str(target)
            .write_str(message)
            .write_u8(0)
            .write_i32(0);
    })
}

/// `CSSendChatMessagePacket`. Server read: `packets/c2g_world.rs:260`.
///
/// Body: `i16 chat_type, i16 0, i32 0, str target, str message, u8 0 language, i32 0 ability`.
/// `target` is the recipient of a [`chat_type::WHISPER`], empty otherwise.
///
/// The server runs every line that starts with `/` (after it stripped the NULs at both ends of
/// the string) as a GM command, in any chat type. This builder is for *chat*: a command-like
/// `message` (see [`is_command_like`]) is neutralised into plain text and control characters are
/// removed, so no caller can turn text into a command by accident. The deliberate command path is
/// [`gm_command`]; a caller that must know whether the line was acceptable uses [`try_chat`].
pub fn chat(chat_type: i16, target: &str, message: &str) -> Out {
    match plain_chat_text(message) {
        Some(plain) => chat_packet(chat_type, target, plain),
        None => chat_packet(chat_type, target, &neutralize(message)),
    }
}

/// Like [`chat`], but `None` where [`chat`] would neutralise the line (see [`plain_chat_text`]).
pub fn try_chat(chat_type: i16, target: &str, message: &str) -> Option<Out> {
    Some(chat_packet(chat_type, target, plain_chat_text(message)?))
}

/// A GM command line (`/item add self 1 1`) as local chat: the only builder that lets a message
/// start with `/`. `None` unless `line` starts with an ASCII `/`, has at most [`MAX_CHAT_CHARS`]
/// characters and no control character. Which commands are sent, and who they are addressed to,
/// is the caller's decision (`playerbots::gm`).
pub fn gm_command(line: &str) -> Option<Out> {
    let ok = line.starts_with('/')
        && line.chars().count() <= MAX_CHAT_CHARS
        && !line.chars().any(char::is_control);
    ok.then(|| chat_packet(chat_type::SAY, "", line))
}

// ---- quests ---------------------------------------------------------------------------------

/// `CSStartQuestContextPacket`: accepts a quest from an NPC, a doodad or a sign sphere (the
/// first non-zero source wins on the server). Server read: `systems/quests/packets.rs:235`.
///
/// Body: `u32 quest, bc npc, bc doodad, u32 sphere`.
pub fn start_quest_context(quest: u32, npc: u32, doodad: u32, sphere: u32) -> Out {
    game(cs::CSStartQuestContextPacket, |s| {
        s.write_u32(quest)
            .write_bc(npc)
            .write_bc(doodad)
            .write_u32(sphere);
    })
}

/// `CSQuestTalkMadePacket`: a dialog step of a quest was completed. Server read:
/// `systems/quests/packets.rs:364`.
///
/// Body: `bc npc, u32 quest, u32 component, u32 act`.
pub fn quest_talk_made(npc: u32, quest: u32, component: u32, act: u32) -> Out {
    game(cs::CSQuestTalkMadePacket, |s| {
        s.write_bc(npc)
            .write_u32(quest)
            .write_u32(component)
            .write_u32(act);
    })
}

/// `CSCompleteQuestContextPacket`: turns a quest in. Server read: `systems/quests/packets.rs:284`.
///
/// Body: `u32 quest, bc npc, bc doodad, i32 selected` (the chosen reward index, 0 if none).
pub fn complete_quest_context(quest: u32, npc: u32, doodad: u32, selected: i32) -> Out {
    game(cs::CSCompleteQuestContextPacket, |s| {
        s.write_u32(quest)
            .write_bc(npc)
            .write_bc(doodad)
            .write_i32(selected);
    })
}

/// `CSDropQuestContextPacket`. Server read: `systems/quests/packets.rs:332`.
///
/// Body: `u32 quest`.
pub fn drop_quest_context(quest: u32) -> Out {
    game(cs::CSDropQuestContextPacket, |s| {
        s.write_u32(quest);
    })
}

// ---- duel -----------------------------------------------------------------------------------

/// `CSStartDuelPacket` answer: accept the challenge.
pub const DUEL_ACCEPT: i16 = 0;
/// `CSStartDuelPacket` answer: decline (`TargetRejectedDuel`).
pub const DUEL_DECLINE: i16 = 507;

/// `CSChallengeDuelPacket`. Server read: `systems/social/packets/duel.rs:70`.
///
/// Body: `u32 challenged_character_id`.
pub fn challenge_duel(challenged_character_id: u32) -> Out {
    game(cs::CSChallengeDuelPacket, |s| {
        s.write_u32(challenged_character_id);
    })
}

/// `CSStartDuelPacket`: the challenged player's answer. Server read:
/// `systems/social/packets/duel.rs:82`.
///
/// Body: `u32 challenger_character_id, i16 error` ([`DUEL_ACCEPT`] or [`DUEL_DECLINE`]).
pub fn start_duel(challenger_character_id: u32, error: i16) -> Out {
    game(cs::CSStartDuelPacket, |s| {
        s.write_u32(challenger_character_id).write_i16(error);
    })
}

// ---- trade ----------------------------------------------------------------------------------

/// `CSCanStartTradePacket`: asks `target` (an object id) to trade. Server read:
/// `systems/economy/trade.rs:1181`.
///
/// Body: `bc target`.
pub fn can_start_trade(target: u32) -> Out {
    game(cs::CSCanStartTradePacket, |s| {
        s.write_bc(target);
    })
}

/// `CSStartTradePacket`: accepts the trade requested by `requester`. Server read:
/// `systems/economy/trade.rs:1234`.
///
/// Body: `bc requester`.
pub fn start_trade(requester: u32) -> Out {
    game(cs::CSStartTradePacket, |s| {
        s.write_bc(requester);
    })
}

/// `CSCannotStartTradePacket`: declines the trade requested by `requester`. Server read:
/// `systems/economy/trade.rs:1205`.
///
/// Body: `bc requester, i32 reason`.
pub fn cannot_start_trade(requester: u32, reason: i32) -> Out {
    game(cs::CSCannotStartTradePacket, |s| {
        s.write_bc(requester).write_i32(reason);
    })
}

/// `CSPutupTradeItemPacket`: puts the item at `(slot_type, slot)` (1 = equipment, 2 = bag) into
/// the trade window. Server read: `systems/economy/trade.rs:1283`.
///
/// Body: `u8 slot_type, u8 slot, i32 amount`.
pub fn putup_trade_item(slot_type: u8, slot: u8, amount: i32) -> Out {
    game(cs::CSPutupTradeItemPacket, |s| {
        s.write_u8(slot_type).write_u8(slot).write_i32(amount);
    })
}

/// `CSTakedownTradeItemPacket`. Server read: `systems/economy/trade.rs:1326`.
///
/// Body: `u8 slot_type, u8 slot`.
pub fn takedown_trade_item(slot_type: u8, slot: u8) -> Out {
    game(cs::CSTakedownTradeItemPacket, |s| {
        s.write_u8(slot_type).write_u8(slot);
    })
}

/// `CSPutupTradeMoneyPacket` (copper). Server read: `systems/economy/trade.rs:1305`.
///
/// Body: `i32 amount`.
pub fn putup_trade_money(amount: i32) -> Out {
    game(cs::CSPutupTradeMoneyPacket, |s| {
        s.write_i32(amount);
    })
}

/// `CSTradeLockPacket`: locks (or unlocks) the own offer. Server read:
/// `systems/economy/trade.rs:1347`.
///
/// Body: `bool lock`.
pub fn trade_lock(lock: bool) -> Out {
    game(cs::CSTradeLockPacket, |s| {
        s.write_bool(lock);
    })
}

/// `CSTradeOkPacket`: confirms the trade (empty). Server read: `systems/economy/trade.rs:1365`.
pub fn trade_ok() -> Out {
    game(cs::CSTradeOkPacket, |_| {})
}

/// `CSCancelTradePacket`. Server read: `systems/economy/trade.rs:1260`.
///
/// Body: `i32 reason`.
pub fn cancel_trade(reason: i32) -> Out {
    game(cs::CSCancelTradePacket, |s| {
        s.write_i32(reason);
    })
}

// ---- team -----------------------------------------------------------------------------------

/// `CSAskRiskyTeamActionPacket` action: leave the team.
pub const RISKY_LEAVE: u8 = 0;
/// `CSAskRiskyTeamActionPacket` action: kick a member (owner only).
pub const RISKY_KICK: u8 = 1;
/// `CSAskRiskyTeamActionPacket` action: dismiss the team (owner only).
pub const RISKY_DISMISS: u8 = 2;

/// `CSInviteToTeamPacket`: invites the character `name`; `team_id` 0 starts a new team.
/// Server read: `systems/social/packets/team.rs:293`.
///
/// Body: `u32 team_id, bool is_party, str name`.
pub fn invite_to_team(team_id: u32, is_party: bool, name: &str) -> Out {
    game(cs::CSInviteToTeamPacket, |s| {
        s.write_u32(team_id).write_bool(is_party).write_str(name);
    })
}

/// `CSReplyToJoinTeamPacket`: answers an invitation (`SCAskToJoinTeamPacket`). Server read:
/// `systems/social/packets/team.rs:315`.
///
/// Body: `u32 team_id, bool is_party, u32 owner_character_id, bool reject, str inviter_name,
/// bool is_area`. The values echo the invitation.
pub fn reply_to_join_team(
    team_id: u32,
    is_party: bool,
    owner_character_id: u32,
    reject: bool,
    char_name: &str,
    is_area: bool,
) -> Out {
    game(cs::CSReplyToJoinTeamPacket, |s| {
        s.write_u32(team_id)
            .write_bool(is_party)
            .write_u32(owner_character_id)
            .write_bool(reject)
            .write_str(char_name)
            .write_bool(is_area);
    })
}

/// `CSAskRiskyTeamActionPacket`: leave / kick / dismiss ([`RISKY_LEAVE`], [`RISKY_KICK`],
/// [`RISKY_DISMISS`]). Server read: `systems/social/packets/team.rs:517`.
///
/// Body: `u32 team_id, u32 target_character_id, u8 action`.
pub fn ask_risky_team_action(team_id: u32, target_character_id: u32, action: u8) -> Out {
    game(cs::CSAskRiskyTeamActionPacket, |s| {
        s.write_u32(team_id)
            .write_u32(target_character_id)
            .write_u8(action);
    })
}

// ---- housing --------------------------------------------------------------------------------

/// `CSConstructHouseTaxPacket`: quotes the tax of building `design_id` at a position.
/// Server read: `systems/housing/packets.rs:221`.
///
/// Body: `u32 design, fx64 x, fx64 y, f32 z`.
pub fn construct_house_tax(design_id: u32, pos: (f32, f32, f32)) -> Out {
    game(cs::CSConstructHouseTaxPacket, |s| {
        s.write_u32(design_id)
            .write_i64(fx64(pos.0))
            .write_i64(fx64(pos.1))
            .write_f32(pos.2);
    })
}

/// The fields of `CSCreateHousePacket`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CreateHouse {
    /// House design (template) id.
    pub design_id: u32,
    /// World position of the house.
    pub pos: (f32, f32, f32),
    /// Rotation around Z in radians.
    pub z_rot: f32,
    /// Id of the design item (blueprint) consumed.
    pub item_id: u64,
    /// `money_amount` (copper). Read by the server, but `systems/housing/ops.rs::build` does not
    /// use it (the server computes the deposit itself); 0 is fine.
    pub money: i32,
    /// The packet's `ht` field; read but unused by `ops::build`; 0 is fine.
    pub ht: i32,
    /// `auto_use_aa_point`; read but unused by `ops::build`.
    pub auto_use_aa_point: bool,
}

/// `CSCreateHousePacket`: places a house. Server read: `systems/housing/packets.rs:240`.
///
/// Body: `u32 design, fx64 x, fx64 y, f32 z, f32 z_rot, u64 item_id, i32 money, i32 ht,
/// bool auto_use_aa_point`.
pub fn create_house(h: &CreateHouse) -> Out {
    game(cs::CSCreateHousePacket, |s| {
        s.write_u32(h.design_id)
            .write_i64(fx64(h.pos.0))
            .write_i64(fx64(h.pos.1))
            .write_f32(h.pos.2)
            .write_f32(h.z_rot)
            .write_u64(h.item_id)
            .write_i32(h.money)
            .write_i32(h.ht)
            .write_bool(h.auto_use_aa_point);
    })
}

/// `CSRequestHouseTaxPacket`: asks for the tax bill of the house with transport id `tl`.
/// Server read: `systems/housing/packets.rs:360`.
///
/// Body: `u16 tl`.
pub fn request_house_tax(tl: u16) -> Out {
    game(cs::CSRequestHouseTaxPacket, |s| {
        s.write_u16(tl);
    })
}

// ---- economy --------------------------------------------------------------------------------

/// `CSExecuteCraftPacket` (`CSExecuteCraft`): crafts `count` times at a workbench.
/// Server read: `systems/economy/craft.rs:1003`.
///
/// Body: `u32 craft, bc bench_obj, i32 count`.
pub fn execute_craft(craft_id: u32, bench_obj: u32, count: i32) -> Out {
    game(cs::CSExecuteCraft, |s| {
        s.write_u32(craft_id).write_bc(bench_obj).write_i32(count);
    })
}

/// `CSSellBackpackGoodsPacket`: sells the carried trade pack to the specialty NPC `npc_obj`.
/// Server read: `systems/economy/store.rs:901` and `systems/npcs/specialty.rs:755`.
///
/// Body: `bc npc`.
pub fn sell_backpack_goods(npc_obj: u32) -> Out {
    game(cs::CSSellBackpackGoodsPacket, |s| {
        s.write_bc(npc_obj);
    })
}

/// `CSListMailPacket` (empty): opens the mailbox. Server read: `systems/economy/mail_packets.rs:470`.
pub fn list_mail() -> Out {
    game(cs::CSListMailPacket, |_| {})
}

/// `CSReadMailPacket`. Server read: `systems/economy/mail_packets.rs:505`.
///
/// Body: `bool is_sent, i64 mail_id`.
pub fn read_mail(is_sent: bool, mail_id: i64) -> Out {
    game(cs::CSReadMailPacket, |s| {
        s.write_bool(is_sent).write_i64(mail_id);
    })
}

/// `CSTakeAttachmentSequentially`: takes the next attachment (items, then money) of a mail.
/// Server read: `systems/economy/mail_packets.rs:599`.
///
/// Body: `i64 mail_id`.
pub fn take_attachment_sequentially(mail_id: i64) -> Out {
    game(cs::CSTakeAttachmentSequentially, |s| {
        s.write_i64(mail_id);
    })
}
