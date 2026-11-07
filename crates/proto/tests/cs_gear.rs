//! Byte-exact tests of the gear builders: equipping (`CSSwapItemsPacket`) and the item-on-item
//! cast that sockets a lunagem or regrades a piece (`CSStartSkillPacket` with an item caster and
//! an item target).
//!
//! Every expected body is written out by hand from the layout the server's `read()` expects
//! (AAEmu-1.2-Rust `rust/crates/aaemu-game/src`): `MoveArgs::read`
//! (`systems/economy/item_handlers.rs:91`), `CSStartSkillPacket::read`
//! (`systems/skills/handlers.rs:36`), `SkillCaster::read` and `SkillCastTarget::read`
//! (`systems/skills/types.rs:66`, `:256`).

use aa_proto::cs::{self, Out};
use aa_proto::game::LEVEL_GAME;
use aa_proto::opcodes::cs as op;

#[track_caller]
fn game(out: Out, opcode: u16, body: &[u8]) {
    assert_eq!(out.level, LEVEL_GAME, "level of {}", out.name());
    assert_eq!(out.opcode, opcode, "opcode of {}", out.name());
    assert_eq!(out.body, body, "body of {}", out.name());
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

const WORN: [u8; 8] = [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01];
const BAG: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0, 0, 0, 0];

#[test]
fn swap_items_writes_both_ids_then_both_slots() {
    assert_eq!(op::CSSwapItemsPacket, 0x03a);
    game(
        cs::swap_items(0x0102_0304_0506_0708, 0x4433_2211, 2, 7, 3, 12),
        op::CSSwapItemsPacket,
        &cat(&[&WORN, &BAG, &[2, 7, 3, 12]]),
    );
}

#[test]
fn equipping_into_an_empty_slot_goes_from_the_slot_to_the_bag_item() {
    // the client sends FROM the empty equipment slot 4 (legs, item id 0) TO bag slot 9
    game(
        cs::equip_into_empty(4, 9, 0x4433_2211),
        op::CSSwapItemsPacket,
        &cat(&[&[0; 8], &BAG, &[1, 4, 2, 9]]),
    );
}

#[test]
fn a_swap_and_an_unequip() {
    // the worn chest piece swaps with the bag item of slot 30
    game(
        cs::equip_swap(0x0102_0304_0506_0708, 2, 0x4433_2211, 30),
        op::CSSwapItemsPacket,
        &cat(&[&WORN, &BAG, &[1, 2, 2, 30]]),
    );
    // the main-hand weapon goes into the free bag slot 41
    game(
        cs::unequip(0x0102_0304_0506_0708, 15, 41),
        op::CSSwapItemsPacket,
        &cat(&[&WORN, &[0; 8], &[1, 15, 2, 41]]),
    );
    assert_eq!((cs::SLOT_EQUIPMENT, cs::SLOT_BAG), (1, 2));
}

#[test]
fn socketing_casts_the_gem_on_the_target_item() {
    // the lunagem 30915 (bag item 0x4433_2211) cast by object 0x030201 with skill 23728 on the
    // worn piece 0x0102030405060708
    let mut body = Vec::new();
    body.extend(23_728u32.to_le_bytes());
    body.extend([2, 0x01, 0x02, 0x03]); // item caster, bc caster
    body.extend(BAG); // gem item id
    body.extend(30_915u32.to_le_bytes()); // gem template
    body.extend([0, 0, 0, 0, 0]); // type1 u8, type2 u32
    body.extend([3, 0x01, 0x02, 0x03]); // item target, bc obj
    body.extend(WORN); // target item id
    body.extend([0, 0, 0, 0, 0]); // type1 u32, type2 u8
    body.push(0); // no skill object
    game(
        cs::start_skill_item_on_item(
            23_728,
            0x03_0201,
            0x4433_2211,
            30_915,
            0x0102_0304_0506_0708,
        ),
        op::CSStartSkillPacket,
        &body,
    );
    assert_eq!(body.len(), 4 + 4 + 8 + 4 + 5 + 4 + 8 + 5 + 1);
}
