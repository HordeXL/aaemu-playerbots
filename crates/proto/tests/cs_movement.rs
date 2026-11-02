//! Byte-exact tests of `CSMoveUnitPacket` and the yaw conventions.
//!
//! Server read: `aaemu-game/src/packets/c2g_world.rs:224`, `UnitMove::read`
//! `models/movement.rs:418`; the facing values are the server's own compass checks
//! (`+Y = 0, -X = 32, +X = -32, -Y = 64`, test client `game.rs` `facing_tests`).

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use aa_proto::cs::{self, MoveUnit};
use aa_proto::game::LEVEL_GAME;
use aa_proto::opcodes::cs as op;
use aa_proto::pos::encode_position;

fn base() -> MoveUnit {
    MoveUnit {
        obj: 0x03_0201,
        time: 0x0A0B_0C0D,
        pos: (100.0, 200.0, 50.0),
        vel: (0.0, 0.0, 0.0),
        yaw: 0.0,
        moving: false,
        stance: cs::STANCE_RELAXED,
        alertness: cs::ALERT_IDLE,
    }
}

/// `bc obj, u8 1, u32 time, u8 flags, position, 3 x i16 vel, 3 x i8 rot, 3 x i8 delta,
/// u8 stance, u8 alertness, u8 actor`.
#[allow(clippy::too_many_arguments)]
fn expected(
    flags: u8,
    pos: (f32, f32, f32),
    vel: [i16; 3],
    rot_z: i8,
    delta: [i8; 3],
    stance: u8,
    alertness: u8,
    actor: u8,
) -> Vec<u8> {
    let mut b = vec![1, 2, 3, 1, 0x0D, 0x0C, 0x0B, 0x0A, flags];
    b.extend_from_slice(&encode_position(pos.0, pos.1, pos.2));
    for v in vel {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&[0, 0, rot_z as u8]);
    b.extend(delta.map(|d| d as u8));
    b.extend_from_slice(&[stance, alertness, actor]);
    b
}

#[track_caller]
fn check(m: &MoveUnit, body: Vec<u8>) {
    let out = cs::move_unit(m);
    assert_eq!(out.level, LEVEL_GAME);
    assert_eq!(out.opcode, op::CSMoveUnitPacket);
    assert_eq!(out.body, body);
    assert_eq!(out.body.len(), 33);
}

#[test]
fn running_forward_along_y() {
    // 5.4 m/s north: velocity 5400, running actor flags 4, forward stick.
    let m = MoveUnit {
        vel: (0.0, 5.4, 0.0),
        moving: true,
        ..base()
    };
    check(
        &m,
        expected(0x02, m.pos, [0, 5400, 0], 0, [0, 127, 0], 1, 0, 4),
    );
}

#[test]
fn walking_is_announced_with_actor_flags_5() {
    let m = MoveUnit {
        vel: (1.0, 0.0, 0.0),
        yaw: -FRAC_PI_2,
        moving: true,
        ..base()
    };
    check(
        &m,
        expected(0x02, m.pos, [1000, 0, 0], -32, [0, 127, 0], 1, 0, 5),
    );
    // 3.0 m/s exactly is still a walk, anything above runs.
    let at_threshold = MoveUnit {
        vel: (3.0, 0.0, 0.0),
        moving: true,
        ..base()
    };
    assert_eq!(cs::move_unit(&at_threshold).body[32], 5);
    let above = MoveUnit {
        vel: (3.01, 0.0, 0.0),
        moving: true,
        ..base()
    };
    assert_eq!(cs::move_unit(&above).body[32], 4);
}

#[test]
fn negative_and_clamped_velocities() {
    let m = MoveUnit {
        vel: (-2.0, 100.0, -100.0),
        moving: true,
        ..base()
    };
    // -2000 = 0xF830; +/- 100 m/s saturate the i16.
    check(
        &m,
        expected(0x02, m.pos, [-2000, 32767, -32768], 0, [0, 127, 0], 1, 0, 4),
    );
}

#[test]
fn a_stop_has_no_velocity_and_no_input() {
    let m = MoveUnit {
        // The velocity of a stopped update is ignored.
        vel: (5.0, 5.0, 0.0),
        yaw: FRAC_PI_2,
        moving: false,
        ..base()
    };
    check(&m, expected(0x04, m.pos, [0, 0, 0], 32, [0, 0, 0], 1, 0, 0));
}

#[test]
fn combat_stance_and_alertness_are_passed_through() {
    let m = MoveUnit {
        stance: cs::STANCE_COMBAT,
        alertness: cs::ALERT_COMBAT,
        ..base()
    };
    check(&m, expected(0x04, m.pos, [0, 0, 0], 0, [0, 0, 0], 0, 2, 0));
}

#[test]
fn the_position_uses_the_packed_world_encoding() {
    let m = MoveUnit {
        pos: (-1234.5, 16000.25, 123.5),
        ..base()
    };
    let out = cs::move_unit(&m);
    assert_eq!(&out.body[9..18], &encode_position(-1234.5, 16000.25, 123.5));
}

#[test]
fn yaw_towards_follows_the_server_compass() {
    let o = (0.0, 0.0);
    assert_eq!(cs::yaw_towards(o, (0.0, 1.0)), 0.0);
    assert!((cs::yaw_towards(o, (-1.0, 0.0)) - FRAC_PI_2).abs() < 1e-6);
    assert!((cs::yaw_towards(o, (1.0, 0.0)) + FRAC_PI_2).abs() < 1e-6);
    assert!((cs::yaw_towards(o, (0.0, -1.0)) - PI).abs() < 1e-6);
    assert!((cs::yaw_towards(o, (-1.0, 1.0)) - FRAC_PI_4).abs() < 1e-6);
    assert!((cs::yaw_towards(o, (1.0, 1.0)) + FRAC_PI_4).abs() < 1e-6);
    // Same point: no direction.
    assert_eq!(cs::yaw_towards((5.0, 5.0), (5.0, 5.0)), 0.0);
    // The result is relative to the start, not the origin.
    assert!((cs::yaw_towards((100.0, 50.0), (99.0, 50.0)) - FRAC_PI_2).abs() < 1e-5);
}

#[test]
fn the_direction_byte_matches_the_server_compass_values() {
    let o = (0.0, 0.0);
    let dir = |to| cs::yaw_to_direction(cs::yaw_towards(o, to));
    assert_eq!(dir((0.0, 1.0)), 0);
    assert_eq!(dir((-1.0, 0.0)), 32);
    assert_eq!(dir((1.0, 0.0)), -32);
    assert_eq!(dir((0.0, -1.0)), 64);
    assert_eq!(dir((-1.0, 1.0)), 16);
    assert_eq!(dir((1.0, 1.0)), -16);
    // One step is 2.8125 degrees; rounding, not truncation.
    let step = (2.0 * PI) / 128.0;
    assert_eq!(cs::yaw_to_direction(step * 0.49), 0);
    assert_eq!(cs::yaw_to_direction(step * 0.51), 1);
    assert_eq!(cs::yaw_to_direction(-step * 0.51), -1);
    // Angles outside (-pi, pi] and the whole circle fold into -42..=85 like the server's.
    assert_eq!(cs::yaw_to_direction(2.0 * PI), 0);
    assert_eq!(cs::yaw_to_direction(-PI), 64);
    assert_eq!(cs::yaw_to_direction(PI), 64);
    for i in -300..300 {
        let d = cs::yaw_to_direction(i as f32 * 0.037);
        assert!((-42..=85).contains(&d), "{i}: {d}");
    }
}

#[test]
fn facing_a_target_writes_the_direction_into_rotation_z() {
    let from = (10.0, 10.0);
    let m = MoveUnit {
        yaw: cs::yaw_towards(from, (9.0, 10.0)), // west
        ..base()
    };
    // Rotation z is the third of the three rotation bytes after the position and velocity.
    assert_eq!(cs::move_unit(&m).body[9 + 9 + 6 + 2] as i8, 32);
}
