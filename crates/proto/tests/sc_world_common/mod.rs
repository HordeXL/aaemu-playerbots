//! Shared body builders for the `sc_world*` tests. Every builder writes in the order of the
//! server writer it is named after (AAEmu-1.2-Rust `rust/crates/aaemu-game/src`, GPL-3.0-or-later),
//! independently of the parser under test.
#![allow(dead_code)]

use aa_proto::sc_world::{parse, WorldEvent};
use aa_proto::PacketStream;

/// Detail body lengths by `ItemDetailType` (models/item.rs:158), repeated here on purpose.
const DETAILS: [usize; 10] = [0, 55, 29, 6, 9, 24, 16, 16, 8, 4];

/// Parses a level-1 body that must be handled and well-formed.
pub fn game(opcode: u16, s: &PacketStream) -> WorldEvent {
    parse(1, opcode, s.as_bytes())
        .expect("body parses")
        .expect("packet is handled")
}

/// Runs every strict prefix of `body` through the parser: none may panic.
pub fn no_panic_on_truncation(opcode: u16, body: &[u8]) {
    for end in 0..body.len() {
        let _ = parse(1, opcode, &body[..end]);
    }
}

/// Every prefix shorter than `needed` bytes must be rejected, none may panic.
pub fn truncation_is_rejected(opcode: u16, body: &[u8], needed: usize) {
    for end in 0..body.len() {
        let result = parse(1, opcode, &body[..end]);
        if end < needed {
            assert!(result.is_err(), "prefix of {end} bytes parsed: {result:?}");
        }
    }
}

/// `Item.Write` (models/item.rs:1061) of a non-empty item.
pub fn write_item(s: &mut PacketStream, template: u32, id: u64, grade: u8, count: i32, detail: u8) {
    s.write_u32(template)
        .write_u64(id)
        .write_u8(grade)
        .write_u8(0) // flags
        .write_i32(count)
        .write_u8(detail)
        .write_raw(&vec![0xAB; DETAILS[usize::from(detail)]])
        .write_i64(1) // create time
        .write_i32(2) // lifespan
        .write_u32(3) // made unit
        .write_u8(1) // world id
        .write_i64(4) // unsecure time
        .write_i64(5); // unpack time
}

/// `UnitCustomModelParams.write` (models/unit_model.rs:143) with the given kind (0..=3).
pub fn write_model_params(s: &mut PacketStream, kind: u8) {
    s.write_u8(kind);
    if kind >= 1 {
        s.write_u32(11); // hair color
    }
    if kind >= 2 {
        s.write_u32(12).write_u32(13); // skin color, model id
    }
    if kind >= 3 {
        // FaceModel (models/unit_model.rs:83)
        s.write_u32(1).write_f32(0.5).write_f32(1.0).write_f32(2.0);
        s.write_i16(-3).write_i16(4);
        for i in 0..4 {
            s.write_u32(i).write_f32(0.25);
        }
        s.write_u32(21).write_u32(22).write_u32(23).write_f32(0.75);
        for i in 0..5 {
            s.write_u32(100 + i); // lip, pupils, eyebrow, deco colors
        }
        s.write_bytes_sized(&[7u8; 128]);
    }
}

/// One equipment slot of a [`CharSpec`].
#[derive(Clone)]
pub struct Equip {
    pub slot: usize,
    pub template: u32,
    pub id: u64,
    pub grade: u8,
    pub detail: u8,
}

/// The fields `Character.Write` (models/character.rs:449) writes that the tests vary.
#[derive(Clone)]
pub struct CharSpec {
    pub id: u32,
    pub name: &'static str,
    pub race: u8,
    pub gender: u8,
    pub level: u8,
    pub hp: i32,
    pub mp: i32,
    pub zone: u32,
    pub faction: u32,
    pub expedition: u32,
    pub family: u32,
    pub equip: Vec<Equip>,
    pub abilities: [u8; 3],
    pub pos: (f32, f32, f32),
    pub model_kind: u8,
    pub labor: i16,
    pub money: i64,
    pub loyalty: i64,
    pub bank_money: i64,
}

impl CharSpec {
    pub fn new(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            race: 3,
            gender: 1,
            level: 25,
            hp: 1200,
            mp: 800,
            zone: 7,
            faction: 101,
            expedition: 0,
            family: 0,
            equip: Vec::new(),
            abilities: [1, 2, 11],
            pos: (12000.5, 9000.25, 120.0),
            model_kind: 0,
            labor: 50,
            money: 123_456,
            loyalty: 9,
            bank_money: 777,
        }
    }
}

/// `Character.Write` (models/character.rs:449).
pub fn write_character_record(s: &mut PacketStream, c: &CharSpec) {
    s.write_u32(c.id)
        .write_str(c.name)
        .write_u8(c.race)
        .write_u8(c.gender)
        .write_u8(c.level)
        .write_i32(c.hp)
        .write_i32(c.mp)
        .write_u32(c.zone)
        .write_u32(c.faction)
        .write_str("Nuian Alliance")
        .write_u32(c.expedition)
        .write_u32(c.family);
    for slot in 0..28 {
        match c.equip.iter().find(|e| e.slot == slot) {
            None => {
                s.write_i32(0);
            }
            // appearance parts (19..=25) are written as the template only
            Some(e) if (19..=25).contains(&slot) => {
                s.write_u32(e.template);
            }
            Some(e) => write_item(s, e.template, e.id, e.grade, 1, e.detail),
        }
    }
    s.write_u8(c.abilities[0])
        .write_u8(c.abilities[1])
        .write_u8(c.abilities[2]);
    s.write_i64(((c.pos.0 * 4096.0) as i64) << 32)
        .write_i64(((c.pos.1 * 4096.0) as i64) << 32)
        .write_f32(c.pos.2);
    write_model_params(s, c.model_kind);
    s.write_i16(c.labor)
        .write_i64(1) // labor modified
        .write_i16(2) // dead count
        .write_i64(3) // dead time
        .write_i32(4) // rez wait
        .write_i64(5) // rez time
        .write_i32(6) // rez penalty
        .write_i64(7) // last world leave
        .write_i64(c.money)
        .write_i64(0) // money amount
        .write_i16(8) // crime
        .write_i32(9) // infamy
        .write_i16(10) // trials
        .write_i64(11) // delete request
        .write_i64(12) // transfer request
        .write_i64(13) // delete delay
        .write_i32(14) // consumed labor
        .write_i64(c.loyalty)
        .write_i64(c.bank_money)
        .write_i64(0) // money amount
        .write_bool(true) // auto AA point
        .write_i32(15) // prev point
        .write_i32(16) // point
        .write_i32(17) // gift
        .write_i64(18) // updated
        .write_u8(0); // force name change
}
