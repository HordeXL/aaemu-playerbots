//! `sc_activity` player-trade and team packets: every body is built in the order the AAEmu
//! server writes it (writer citations are in the variant docs of `ActivityEvent`).

use aa_proto::opcodes::sc;
use aa_proto::sc_activity::{parse, ActivityEvent, ItemBrief, TeamMemberBrief};
use aa_proto::sc_world::{ItemChange, ItemTaskInfo};
use aa_proto::stream::MarshalError;
use aa_proto::PacketStream;

fn event(opcode: u16, s: &PacketStream) -> ActivityEvent {
    parse(1, opcode, s.as_bytes())
        .expect("layout matches")
        .expect("opcode is handled")
}

/// `Item.Write` (G/models/item.rs:1061-1082) for a plain item (detail type 0).
fn write_plain_item(s: &mut PacketStream, template: u32, id: u64, grade: u8, count: i32) {
    s.write_u32(template)
        .write_u64(id)
        .write_u8(grade)
        .write_u8(0x01) // flags
        .write_i32(count)
        .write_u8(0); // detail type: none
    write_item_tail(s);
}

/// `Item.Write` of an equipment item: detail type 1 with the 55 byte detail body
/// (`write_details`, G/models/item.rs:601-619).
fn write_equip_item(s: &mut PacketStream, template: u32, id: u64) {
    s.write_u32(template)
        .write_u64(id)
        .write_u8(2)
        .write_u8(0)
        .write_i32(1)
        .write_u8(1);
    s.write_u32(0) // image item template
        .write_u8(90) // durability
        .write_i16(0)
        .write_u32(0) // rune
        .write_i64(0) // charge / unpack time
        .write_u32(0); // dye
    for g in 0..7u32 {
        s.write_u32(g);
    }
    s.write_u16(3).write_u16(4);
    write_item_tail(s);
}

fn write_item_tail(s: &mut PacketStream) {
    s.write_i64(1_700_000_000) // create time
        .write_i32(0) // lifespan
        .write_u32(0) // made unit
        .write_u8(1) // world
        .write_i64(0) // unsecure time
        .write_i64(0); // unpack time
}

#[test]
fn can_start_trade() {
    // SCCanStartTradePacket: bc requester (G/systems/economy/trade.rs:941).
    let mut s = PacketStream::new();
    s.write_bc(0x01_0203);
    assert_eq!(
        event(sc::SCCanStartTradePacket, &s),
        ActivityEvent::CanStartTrade {
            requester_obj: 0x01_0203
        }
    );
}

#[test]
fn cannot_start_trade() {
    // SCCannotStartTradePacket: bc obj, i32 reason (trade.rs:958).
    let mut s = PacketStream::new();
    s.write_bc(0x44).write_i32(-5);
    assert_eq!(
        event(sc::SCCannotStartTradePacket, &s),
        ActivityEvent::CannotStartTrade {
            obj: 0x44,
            reason: -5
        }
    );
}

#[test]
fn trade_started() {
    // SCTradeStartedPacket: bc peer (trade.rs:974).
    let mut s = PacketStream::new();
    s.write_bc(0x55);
    assert_eq!(
        event(sc::SCTradeStartedPacket, &s),
        ActivityEvent::TradeStarted { peer_obj: 0x55 }
    );
}

#[test]
fn trade_canceled() {
    // SCTradeCanceledPacket: i32 reason, bool caused_by_me (trade.rs:991).
    let mut s = PacketStream::new();
    s.write_i32(3).write_bool(true);
    assert_eq!(
        event(sc::SCTradeCanceledPacket, &s),
        ActivityEvent::TradeCanceled {
            reason: 3,
            caused_by_me: true
        }
    );
}

#[test]
fn trade_item_putup() {
    // SCTradeItemPutupPacket: u8 slot type, u8 slot, i32 amount (trade.rs:1009).
    let mut s = PacketStream::new();
    s.write_u8(1).write_u8(7).write_i32(20);
    assert_eq!(
        event(sc::SCTradeItemPutupPacket, &s),
        ActivityEvent::TradeItemPutup {
            slot_type: 1,
            slot: 7,
            amount: 20
        }
    );
}

#[test]
fn other_trade_item_putup_reads_the_item_head() {
    // SCOtherTradeItemPutupPacket: Item.Write (trade.rs:1027); the count is the offered amount.
    let mut s = PacketStream::new();
    write_plain_item(&mut s, 8000, 0x0102_0304_0506_0708, 3, 20);
    assert_eq!(
        event(sc::SCOtherTradeItemPutupPacket, &s),
        ActivityEvent::OtherTradeItemPutup {
            item: ItemBrief {
                template_id: 8000,
                id: 0x0102_0304_0506_0708,
                grade: 3,
                flags: 1,
                count: 20,
            }
        }
    );
    // An equipment item carries a 55 byte detail body that has to be skipped to validate the tail.
    let mut s = PacketStream::new();
    write_equip_item(&mut s, 9000, 77);
    assert_eq!(
        event(sc::SCOtherTradeItemPutupPacket, &s),
        ActivityEvent::OtherTradeItemPutup {
            item: ItemBrief {
                template_id: 9000,
                id: 77,
                grade: 2,
                flags: 0,
                count: 1,
            }
        }
    );
}

#[test]
fn trade_item_tookdown() {
    // SCTradeItemTookdownPacket: u8 slot type, u8 slot (trade.rs:1044).
    let mut s = PacketStream::new();
    s.write_u8(1).write_u8(9);
    assert_eq!(
        event(sc::SCTradeItemTookdownPacket, &s),
        ActivityEvent::TradeItemTookdown {
            slot_type: 1,
            slot: 9
        }
    );
}

#[test]
fn other_trade_item_tookdown() {
    // SCOtherTradeItemTookdownPacket: Item.Write (trade.rs:1060).
    let mut s = PacketStream::new();
    write_plain_item(&mut s, 8001, 5, 1, 2);
    assert_eq!(
        event(sc::SCOtherTradeItemTookdownPacket, &s),
        ActivityEvent::OtherTradeItemTookdown {
            item: ItemBrief {
                template_id: 8001,
                id: 5,
                grade: 1,
                flags: 1,
                count: 2,
            }
        }
    );
}

#[test]
fn trade_money() {
    // SCTradeMoneyPutupPacket / SCOtherTradeMoneyPutupPacket: i32 amount (trade.rs:1076, 1092).
    let mut s = PacketStream::new();
    s.write_i32(12_345);
    assert_eq!(
        event(sc::SCTradeMoneyPutupPacket, &s),
        ActivityEvent::TradeMoneyPutup { amount: 12_345 }
    );
    assert_eq!(
        event(sc::SCOtherTradeMoneyPutupPacket, &s),
        ActivityEvent::OtherTradeMoneyPutup { amount: 12_345 }
    );
}

#[test]
fn trade_lock_and_ok_updates() {
    // SCTradeLockUpdatePacket / SCTradeOkUpdatePacket: bool mine, bool other (trade.rs:1109, 1126).
    let mut s = PacketStream::new();
    s.write_bool(true).write_bool(false);
    assert_eq!(
        event(sc::SCTradeLockUpdatePacket, &s),
        ActivityEvent::TradeLockUpdate {
            mine: true,
            other: false
        }
    );
    assert_eq!(
        event(sc::SCTradeOkUpdatePacket, &s),
        ActivityEvent::TradeOkUpdate {
            mine: true,
            other: false
        }
    );
}

#[test]
fn trade_made_carries_its_item_tasks() {
    // SCTradeMadePacket: u8 action, u8 n, n item tasks, u8 force-remove count, ids, u32 type id
    // (trade.rs:1144); the head layout is that of SCItemTaskSuccess.
    let mut s = PacketStream::new();
    s.write_u8(48).write_u8(0).write_u8(0).write_u32(0);
    assert_eq!(
        event(sc::SCTradeMadePacket, &s),
        ActivityEvent::TradeMade {
            action: 48,
            tasks: 0,
            items: ItemTaskInfo {
                reason: 48,
                ..ItemTaskInfo::default()
            }
            .into(),
        }
    );
    // one task: money -250 (ItemAction 1), one forced removal, the trailing u32
    let mut s = PacketStream::new();
    s.write_u8(48)
        .write_u8(1)
        .write_u8(1)
        .write_i32(-250)
        .write_u8(1)
        .write_u64(0x1122_3344_5566_7788)
        .write_u32(0);
    let ActivityEvent::TradeMade {
        action,
        tasks,
        items,
    } = event(sc::SCTradeMadePacket, &s)
    else {
        panic!("expected TradeMade");
    };
    assert_eq!((action, tasks), (48, 1));
    assert_eq!(items.reason, 48);
    assert_eq!(items.changes.as_slice(), [ItemChange::Money(-250)]);
    assert_eq!(items.force_removed.as_slice(), [0x1122_3344_5566_7788]);
    // a body the item reader cannot follow still signals the completion
    let mut s = PacketStream::new();
    s.write_u8(48).write_u8(2).write_raw(&[0xAA; 40]);
    assert_eq!(
        event(sc::SCTradeMadePacket, &s),
        ActivityEvent::TradeMade {
            action: 48,
            tasks: 2,
            items: Default::default(),
        }
    );
}

#[test]
fn ask_to_join_team() {
    // SCAskToJoinTeamPacket: u32 team, u32 sender, string name, bool party
    // (G/systems/social/packets/team.rs:108).
    let mut s = PacketStream::new();
    s.write_u32(0)
        .write_u32(1234)
        .write_str("Nick")
        .write_bool(true);
    assert_eq!(
        event(sc::SCAskToJoinTeamPacket, &s),
        ActivityEvent::AskToJoinTeam {
            team_id: 0,
            sender_char_id: 1234,
            name: "Nick".into(),
            is_party: true,
        }
    );
}

/// `TeamMember.Write` (team.rs:35-43).
fn write_member(s: &mut PacketStream, id: u32, name: &str, level: u8, role: u8, obj: u32) {
    s.write_u32(id)
        .write_str(name)
        .write_u8(1) // race
        .write_u8(2) // gender
        .write_u8(level)
        .write_u8(role)
        .write_bc(obj);
}

fn member(char_id: u32, name: &str, level: u8, role: u8, obj_id: u32) -> TeamMemberBrief {
    TeamMemberBrief {
        char_id,
        name: name.into(),
        level,
        role,
        obj_id,
    }
}

#[test]
fn joined_team_reads_head_and_members() {
    // SCJoinedTeamPacket: Team.Write + TeamMember.Write per member (team.rs:82-104, 129-134).
    let mut s = PacketStream::new();
    s.write_u32(77).write_u32(1001).write_bool(true);
    s.write_raw(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // party counts
    for slot in 0..50u32 {
        // two occupied slots, 50 in total
        match slot {
            0 => s.write_u32(1001).write_bool(true),
            1 => s.write_u32(1002).write_bool(false),
            _ => s.write_u32(0).write_bool(false),
        };
    }
    for mark in 0..12 {
        // marks: kind 1 + u32 character id, kind 2 + bc object id, otherwise only the kind
        match mark {
            3 => s.write_u8(1).write_u32(1002),
            5 => s.write_u8(2).write_bc(0x0A0B0C),
            _ => s.write_u8(0),
        };
    }
    s.write_u8(1).write_u8(2).write_u32(0).write_bool(true); // LootingRule.Write
    write_member(&mut s, 1001, "Alpha", 30, 0, 0x100);
    write_member(&mut s, 1002, "Beta", 25, 1, 0x200);
    match event(sc::SCJoinedTeamPacket, &s) {
        ActivityEvent::JoinedTeam {
            team_id,
            owner_id,
            is_party,
            members,
        } => {
            assert_eq!((team_id, owner_id, is_party), (77, 1001, true));
            assert_eq!(
                &*members,
                [
                    member(1001, "Alpha", 30, 0, 0x100),
                    member(1002, "Beta", 25, 1, 0x200)
                ]
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn team_member_joined() {
    // SCTeamMemberJoinedPacket: u32 team, TeamMember.Write, i32 party (team.rs:227).
    let mut s = PacketStream::new();
    s.write_u32(77);
    write_member(&mut s, 1003, "Gamma", 41, 2, 0x300);
    s.write_i32(1);
    assert_eq!(
        event(sc::SCTeamMemberJoinedPacket, &s),
        ActivityEvent::TeamMemberJoined {
            team_id: 77,
            member: member(1003, "Gamma", 41, 2, 0x300),
            party: 1,
        }
    );
}

#[test]
fn team_member_left() {
    // SCTeamMemberLeavedPacket: u32 team, u32 character, bool kicked (team.rs:236).
    let mut s = PacketStream::new();
    s.write_u32(77).write_u32(1002).write_bool(true);
    assert_eq!(
        event(sc::SCTeamMemberLeavedPacket, &s),
        ActivityEvent::TeamMemberLeft {
            team_id: 77,
            char_id: 1002,
            kicked: true
        }
    );
}

#[test]
fn left_team() {
    // SCLeavedTeamPacket: u32 team, bool kicked, bool dismissed (team.rs:139).
    let mut s = PacketStream::new();
    s.write_u32(77).write_bool(false).write_bool(true);
    assert_eq!(
        event(sc::SCLeavedTeamPacket, &s),
        ActivityEvent::LeftTeam {
            team_id: 77,
            kicked: false,
            dismissed: true
        }
    );
}

#[test]
fn team_dismissed() {
    // SCTeamDismissedPacket: u32 team (team.rs:203).
    let mut s = PacketStream::new();
    s.write_u32(77);
    assert_eq!(
        event(sc::SCTeamDismissedPacket, &s),
        ActivityEvent::TeamDismissed { team_id: 77 }
    );
}

#[test]
fn rejected_team() {
    // SCRejectedTeamPacket: string name, bool party (team.rs:171).
    let mut s = PacketStream::new();
    s.write_str("Beta").write_bool(false);
    assert_eq!(
        event(sc::SCRejectedTeamPacket, &s),
        ActivityEvent::RejectedTeam {
            name: "Beta".into(),
            party: false
        }
    );
}

#[test]
fn team_owner_changed() {
    // SCTeamOwnerChangedPacket: u32 team, u32 new owner (team.rs:265).
    let mut s = PacketStream::new();
    s.write_u32(77).write_u32(1002);
    assert_eq!(
        event(sc::SCTeamOwnerChangedPacket, &s),
        ActivityEvent::TeamOwnerChanged {
            team_id: 77,
            owner_id: 1002
        }
    );
}

#[test]
fn truncated_bodies_fail_and_never_panic() {
    let mut item = PacketStream::new();
    write_equip_item(&mut item, 9000, 77);
    let mut ask = PacketStream::new();
    ask.write_u32(0)
        .write_u32(1234)
        .write_str("Nick")
        .write_bool(true);
    let mut joined = PacketStream::new();
    joined.write_u32(77);
    write_member(&mut joined, 1003, "Gamma", 41, 2, 0x300);
    joined.write_i32(1);
    let mut cannot = PacketStream::new();
    cannot.write_bc(0x44).write_i32(-5);
    let samples: [(u16, &PacketStream); 4] = [
        (sc::SCOtherTradeItemPutupPacket, &item),
        (sc::SCAskToJoinTeamPacket, &ask),
        (sc::SCTeamMemberJoinedPacket, &joined),
        (sc::SCCannotStartTradePacket, &cannot),
    ];
    for (opcode, full) in samples {
        assert!(parse(1, opcode, full.as_bytes()).unwrap().is_some());
        for end in 0..full.len() {
            let r = parse(1, opcode, &full.as_bytes()[..end]);
            assert!(
                matches!(r, Err(MarshalError::EndOfStream { .. })),
                "opcode {opcode:#x} cut at {end}: {r:?}"
            );
        }
    }
}
