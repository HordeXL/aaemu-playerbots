//! `sc_world` parsers of the gear feature: the gems of the equipped pieces at login, the gems in
//! an item task's details, the lunagem and regrade results. Bodies are built in the order of the
//! server writers cited per test (AAEmu-1.2-Rust `rust/crates/aaemu-game/src`).

mod sc_world_common;

use aa_proto::opcodes::sc;
use aa_proto::sc_world::{grade_enchant_result, ItemChange, WorldEvent};
use aa_proto::PacketStream;
use sc_world_common::{game, truncation_is_rejected, write_character_record, CharSpec, Equip};

/// The 55-byte equipment detail body (`write_details`, models/item.rs:603) with `gems`.
fn equipment_details(gems: [u32; 7]) -> Vec<u8> {
    let mut s = PacketStream::new();
    s.write_u32(0) // image item
        .write_u8(100) // durability
        .write_i16(0)
        .write_u32(31_000) // lunastone
        .write_i64(-1) // charge time
        .write_u32(0); // dye
    for g in gems {
        s.write_u32(g);
    }
    s.write_u16(7).write_u16(9); // temper
    let body = s.as_bytes().to_vec();
    assert_eq!(body.len(), 55);
    body
}

/// `write_padded_details` (models/item_task.rs:374): `(short)128`, the detail type, the details,
/// zero padding to 128 bytes.
fn padded(detail: u8, body: &[u8]) -> Vec<u8> {
    let mut s = PacketStream::new();
    s.write_i16(128).write_u8(detail).write_raw(body);
    s.write_raw(&vec![0; 127 - body.len()]);
    s.as_bytes().to_vec()
}

#[test]
fn the_item_task_update_of_a_socketing_carries_the_gems() {
    // systems/economy/item_effects.rs:745: task SOCKETING (99) with ItemTask::update, action 9
    let mut s = PacketStream::new();
    s.write_u8(99).write_u8(2); // reason, two tasks
    s.write_u8(9).write_u8(1).write_u8(2).write_u64(0xC0FFEE); // equipment slot 2 (chest)
    s.write_raw(&padded(
        1,
        &equipment_details([30_915, 30_916, 0, 0, 0, 0, 0]),
    ));
    s.write_u8(9).write_u8(2).write_u8(5).write_u64(0xBEEF); // a pet's details: no gems
    s.write_raw(&padded(3, &[1, 2, 3, 4, 5, 6]));
    s.write_u8(0); // nothing force-removed
    let WorldEvent::ItemTask(task) = game(sc::SCItemTaskSuccessPacket, &s) else {
        panic!("not an item task");
    };
    assert_eq!(
        task.changes.as_slice(),
        [
            ItemChange::Updated {
                slot_type: 1,
                slot: 2,
                id: 0xC0FFEE,
                gems: Some(2)
            },
            ItemChange::Updated {
                slot_type: 2,
                slot: 5,
                id: 0xBEEF,
                gems: None
            },
        ]
    );
    // a wiped item: every gem id is 0
    let mut s = PacketStream::new();
    s.write_u8(99).write_u8(1);
    s.write_u8(9).write_u8(1).write_u8(15).write_u64(7);
    s.write_raw(&padded(1, &equipment_details([0; 7])));
    s.write_u8(0);
    let WorldEvent::ItemTask(task) = game(sc::SCItemTaskSuccessPacket, &s) else {
        panic!("not an item task");
    };
    assert!(matches!(
        task.changes[0],
        ItemChange::Updated { gems: Some(0), .. }
    ));
    truncation_is_rejected(
        sc::SCItemTaskSuccessPacket,
        s.as_bytes(),
        s.as_bytes().len() - 1,
    );
}

#[test]
fn the_character_state_counts_the_gems_of_every_worn_piece() {
    // world/unit_state.rs:43 around Character.Write (models/character.rs:449); the test writer
    // fills every detail body with 0xAB, so an equipment detail holds seven gems
    let mut c = CharSpec::new(1001, "Aria");
    c.equip = vec![
        Equip {
            slot: 2,
            template: 22_257,
            id: 0x51,
            grade: 3,
            detail: 1,
        },
        Equip {
            slot: 15,
            template: 21_907,
            id: 0x52,
            grade: 0,
            detail: 0,
        },
        Equip {
            slot: 19,
            template: 55_550,
            id: 0,
            grade: 0,
            detail: 0,
        },
    ];
    let mut s = PacketStream::new();
    s.write_u32(1)
        .write_u16(16)
        .write_raw(&[0x11; 16])
        .write_i32(0);
    write_character_record(&mut s, &c);
    s.write_f32(0.0).write_f32(0.0).write_f32(1.5); // rotation
    s.write_i32(4242).write_i32(100); // experience, recoverable
    s.write_u32(0).write_i32(0).write_u32(0).write_u32(9);
    for _ in 0..11 {
        s.write_u32(0); // ability exp
    }
    s.write_i32(1).write_i32(2).write_i32(3); // unread mail
    s.write_u8(50).write_i16(100); // bag and bank slots
    let WorldEvent::CharacterState(own) = game(sc::SCCharacterStatePacket, &s) else {
        panic!("not a character state");
    };
    let slots: Vec<u8> = own.equipment.iter().map(|e| e.slot).collect();
    assert_eq!(slots, [2, 15, 19]);
    assert_eq!(own.equipment_gems.as_ref(), [7, 0, 0]);
    assert_eq!(own.equipment[0].grade, 3);
}

#[test]
fn the_lunagem_result() {
    // systems/economy/item_effect_packets.rs:91: u8 result, u64 item id, u32 gem, bool install
    let mut s = PacketStream::new();
    s.write_u8(1)
        .write_u64(0xC0FFEE)
        .write_u32(30_915)
        .write_bool(true);
    assert_eq!(sc::SCItemSocketingLunagemResultPacket, 0x09d);
    assert_eq!(
        game(sc::SCItemSocketingLunagemResultPacket, &s),
        WorldEvent::LunagemResult {
            result: 1,
            item_id: 0xC0FFEE,
            gem: 30_915,
            installed: true,
        }
    );
    assert_eq!(s.as_bytes().len(), 14);
    truncation_is_rejected(sc::SCItemSocketingLunagemResultPacket, s.as_bytes(), 14);
    // a failure took the gems out
    let mut s = PacketStream::new();
    s.write_u8(0)
        .write_u64(5)
        .write_u32(30_916)
        .write_bool(true);
    assert!(matches!(
        game(sc::SCItemSocketingLunagemResultPacket, &s),
        WorldEvent::LunagemResult {
            result: 0,
            installed: true,
            ..
        }
    ));
}

#[test]
fn the_regrade_result() {
    // systems/economy/item_effect_packets.rs:51: u8 result, Item.Write, u8 grade before, u8 after
    let mut s = PacketStream::new();
    s.write_u8(grade_enchant_result::SUCCESS);
    sc_world_common::write_item(&mut s, 22_257, 0x51, 3, 1, 1);
    s.write_u8(2).write_u8(3);
    assert_eq!(sc::SCGradeEnchantResultPacket, 0x09c);
    assert_eq!(
        game(sc::SCGradeEnchantResultPacket, &s),
        WorldEvent::GradeEnchantResult {
            result: 3,
            item_id: 0x51,
            template: 22_257,
            before: 2,
            after: 3,
        }
    );
    truncation_is_rejected(
        sc::SCGradeEnchantResultPacket,
        s.as_bytes(),
        s.as_bytes().len(),
    );
    assert_eq!(
        (
            grade_enchant_result::BREAK,
            grade_enchant_result::DOWNGRADE,
            grade_enchant_result::FAIL,
            grade_enchant_result::GREAT_SUCCESS
        ),
        (0, 1, 2, 4)
    );
}
