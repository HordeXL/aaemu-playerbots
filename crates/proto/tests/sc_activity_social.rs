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

/// `TeamMember.WritePerson` (team.rs:46-66): 50 bytes.
#[allow(clippy::too_many_arguments)]
fn write_person(
    s: &mut PacketStream,
    id: u32,
    zone: u32,
    instance: u32,
    level: u8,
    hp: (i32, i32),
    mp: (i32, i32),
    pos: (f32, f32, f32),
    obj: u32,
) {
    s.write_u32(id)
        .write_u32(zone)
        .write_u32(instance)
        .write_u8(level)
        .write_i32(hp.0)
        .write_i32(hp.1)
        .write_i32(mp.0)
        .write_i32(mp.1)
        .write_raw(&aa_proto::pos::encode_position(pos.0, pos.1, pos.2))
        .write_bc(obj)
        .write_raw(&[0, 1, 2, 3, 0, 0, 0, 0, 0]); // flag, three abilities, reserved
}

/// The position the codec gives back (the codec rounds to its fixed point).
fn close(a: [f32; 3], b: [f32; 3]) {
    for i in 0..3 {
        assert!((a[i] - b[i]).abs() < 0.01, "{a:?} vs {b:?}");
    }
}

#[test]
fn team_remote_members_read_zone_instance_vitals_and_position() {
    // SCTeamRemoteMembersExPacket: i32 count, WritePerson each (team.rs:282-288).
    let mut s = PacketStream::new();
    s.write_i32(2);
    write_person(
        &mut s,
        1001,
        100,
        0,
        30,
        (900, 1200),
        (40, 60),
        (15_346.5, 14_922.25, 117.5),
        0x0A0B0C,
    );
    write_person(
        &mut s,
        1002,
        301,
        7,
        30,
        (900, 1200),
        (40, 60),
        (-12.5, 640.0, 150.0),
        0,
    );
    assert_eq!(s.len(), 4 + 2 * 50, "WritePerson is 50 bytes");
    match event(sc::SCTeamRemoteMembersExPacket, &s) {
        ActivityEvent::TeamRemoteMembers { members } => {
            assert_eq!(members.len(), 2);
            let (a, b) = (members[0], members[1]);
            assert_eq!((a.char_id, a.zone_id, a.instance_id), (1001, 100, 0));
            assert_eq!(
                (a.level, a.hp, a.hp_max, a.mp, a.mp_max),
                (30, 900, 1200, 40, 60)
            );
            assert_eq!(a.obj_id, 0x0A0B0C);
            close(a.pos, [15_346.5, 14_922.25, 117.5]);
            assert_eq!((b.char_id, b.zone_id, b.instance_id), (1002, 301, 7));
            assert_eq!(b.obj_id, 0, "an offline member has object id 0");
            close(b.pos, [-12.5, 640.0, 150.0]);
        }
        other => panic!("unexpected {other:?}"),
    }
    // a count that promises more members than the body holds is refused before any allocation
    let mut lie = PacketStream::new();
    lie.write_i32(i32::MAX);
    write_person(&mut lie, 1, 1, 0, 1, (1, 1), (1, 1), (0.0, 0.0, 0.0), 1);
    assert!(parse(1, sc::SCTeamRemoteMembersExPacket, lie.as_bytes()).is_err());
    // a negative count is no members
    let mut negative = PacketStream::new();
    negative.write_i32(-1);
    assert_eq!(
        event(sc::SCTeamRemoteMembersExPacket, &negative),
        ActivityEvent::TeamRemoteMembers {
            members: Default::default()
        }
    );
    // zero members is fine
    let mut none = PacketStream::new();
    none.write_i32(0);
    assert_eq!(
        event(sc::SCTeamRemoteMembersExPacket, &none),
        ActivityEvent::TeamRemoteMembers {
            members: Default::default()
        }
    );
}

#[test]
fn team_member_disconnected_carries_the_last_state() {
    // SCTeamMemberDisconnectedPacket: u32 team, u32 character, WritePerson (team.rs:218-224).
    let mut s = PacketStream::new();
    s.write_u32(77).write_u32(1002);
    write_person(
        &mut s,
        1002,
        100,
        0,
        25,
        (500, 800),
        (10, 20),
        (100.0, 200.0, 50.0),
        0,
    );
    match event(sc::SCTeamMemberDisconnectedPacket, &s) {
        ActivityEvent::TeamMemberDisconnected {
            team_id,
            char_id,
            person,
        } => {
            assert_eq!((team_id, char_id), (77, 1002));
            assert_eq!((person.char_id, person.hp, person.hp_max), (1002, 500, 800));
            assert_eq!(person.obj_id, 0);
            close(person.pos, [100.0, 200.0, 50.0]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn team_member_role_refresh_and_looting_rule() {
    // SCTeamMemberRoleChangedPacket: u32 team, u32 member, u8 role (team.rs:255-262).
    let mut s = PacketStream::new();
    s.write_u32(77).write_u32(1002).write_u8(2);
    assert_eq!(
        event(sc::SCTeamMemberRoleChangedPacket, &s),
        ActivityEvent::TeamMemberRoleChanged {
            team_id: 77,
            char_id: 1002,
            role: 2
        }
    );
    // SCRefreshTeamMemberPacket: u32 team, u32 member, bc object id (team.rs:161-168).
    let mut s = PacketStream::new();
    s.write_u32(77).write_u32(1002).write_bc(0x0A0B0C);
    assert_eq!(
        event(sc::SCRefreshTeamMemberPacket, &s),
        ActivityEvent::TeamRefreshMember {
            team_id: 77,
            char_id: 1002,
            obj_id: 0x0A0B0C
        }
    );
    // SCTeamLootingRuleChangedPacket: u32 team, LootingRule (u8, u8, u32, bool), u8 flags.
    let mut s = PacketStream::new();
    s.write_u32(77)
        .write_u8(2)
        .write_u8(3)
        .write_u32(1001)
        .write_bool(true)
        .write_u8(5);
    assert_eq!(
        event(sc::SCTeamLootingRuleChangedPacket, &s),
        ActivityEvent::TeamLootingRuleChanged {
            team_id: 77,
            method: 2,
            min_grade: 3,
            master: 1001,
            roll_bop: true,
            flags: 5
        }
    );
}

#[test]
fn the_new_team_packets_fail_cleanly_when_cut_short() {
    let mut remote = PacketStream::new();
    remote.write_i32(1);
    write_person(
        &mut remote,
        1,
        2,
        3,
        4,
        (5, 6),
        (7, 8),
        (9.0, 10.0, 11.0),
        12,
    );
    let mut role = PacketStream::new();
    role.write_u32(77).write_u32(1002).write_u8(2);
    let mut refresh = PacketStream::new();
    refresh.write_u32(77).write_u32(1002).write_bc(5);
    let mut rule = PacketStream::new();
    rule.write_u32(77)
        .write_u8(2)
        .write_u8(3)
        .write_u32(1001)
        .write_bool(true)
        .write_u8(5);
    let samples: [(u16, &PacketStream); 4] = [
        (sc::SCTeamRemoteMembersExPacket, &remote),
        (sc::SCTeamMemberRoleChangedPacket, &role),
        (sc::SCRefreshTeamMemberPacket, &refresh),
        (sc::SCTeamLootingRuleChangedPacket, &rule),
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
