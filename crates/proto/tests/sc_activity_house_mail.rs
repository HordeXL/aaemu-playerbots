//! `sc_activity` housing, mail and specialty packets, plus the dispatch rules of `parse`: every
//! body is built in the order the AAEmu server writes it (writer citations are in the variant
//! docs of `ActivityEvent`).

use aa_proto::opcodes::sc;
use aa_proto::pos::convert_coord_to_long;
use aa_proto::sc_activity::{
    mail_type, parse, ActivityEvent, HouseInfo, ItemBrief, MailBodyInfo, MailHeader, TakenItem,
};
use aa_proto::stream::MarshalError;
use aa_proto::PacketStream;

fn event(opcode: u16, s: &PacketStream) -> ActivityEvent {
    parse(1, opcode, s.as_bytes())
        .expect("layout matches")
        .expect("opcode is handled")
}

// ---- housing --------------------------------------------------------------------------------

/// `House.Write` (G/models/house.rs:95-131).
fn write_house(s: &mut PacketStream, h: &HouseInfo) {
    s.write_u16(h.tl)
        .write_u32(h.id)
        .write_bc(h.obj)
        .write_u32(h.template)
        .write_u32(h.model)
        .write_u32(h.co_owner)
        .write_u32(h.owner)
        .write_str(&h.owner_name)
        .write_u32(h.account)
        .write_u8(h.permission)
        .write_i32(h.all_action)
        .write_i32(h.cur_action)
        .write_i32(h.tax)
        .write_i64(convert_coord_to_long(h.x))
        .write_i64(convert_coord_to_long(h.y))
        .write_f32(h.z)
        .write_str(&h.name)
        .write_bool(h.allow_recover)
        .write_u32(h.sell_price)
        .write_u32(h.sell_to_id)
        .write_str(&h.sell_to_name);
}

fn house() -> HouseInfo {
    HouseInfo {
        tl: 7,
        id: 0x0A0B_0C0D,
        obj: 0x01_0203,
        template: 300,
        model: 0x10,
        co_owner: 0,
        owner: 5,
        owner_name: "Nick".into(),
        account: 9,
        permission: 1,
        all_action: 12,
        cur_action: 3,
        tax: 1_000,
        // exactly representable in the 1/4096 fixed point of the wire
        x: 12_345.5,
        y: 6_789.25,
        z: 101.5,
        name: "Hut".into(),
        allow_recover: true,
        sell_price: 50,
        sell_to_id: 6,
        sell_to_name: "Beta".into(),
    }
}

#[test]
fn my_house() {
    // SCMyHousePacket: the House.Write body (G/systems/housing/packets.rs:177).
    let mut s = PacketStream::new();
    write_house(&mut s, &house());
    assert_eq!(
        event(sc::SCMyHousePacket, &s),
        ActivityEvent::MyHouse(Box::new(house()))
    );
}

#[test]
fn login_char_info_house() {
    // SCLoginCharInfoHousePacket: u32 owner, then House.Write (housing/ops.rs:55-64).
    let mut s = PacketStream::new();
    s.write_u32(5);
    write_house(&mut s, &house());
    assert_eq!(
        event(sc::SCLoginCharInfoHousePacket, &s),
        ActivityEvent::LoginCharInfoHouse {
            owner_id: 5,
            house: Box::new(house()),
        }
    );
}

#[test]
fn my_house_removed_and_demolished() {
    // SCMyHouseRemovedPacket / SCHouseDemolishedPacket: u16 tl (housing/packets.rs:75, 172).
    let mut s = PacketStream::new();
    s.write_u16(0x1234);
    assert_eq!(
        event(sc::SCMyHouseRemovedPacket, &s),
        ActivityEvent::MyHouseRemoved { tl: 0x1234 }
    );
    assert_eq!(
        event(sc::SCHouseDemolishedPacket, &s),
        ActivityEvent::HouseDemolished { tl: 0x1234 }
    );
}

#[test]
fn construct_house_tax() {
    // SCConstructHouseTaxPacket (housing/packets.rs:37-54).
    let mut s = PacketStream::new();
    s.write_u32(300)
        .write_i32(1)
        .write_i32(2)
        .write_bool(true)
        .write_i32(1_000)
        .write_i32(2_000)
        .write_i32(3_500);
    assert_eq!(
        event(sc::SCConstructHouseTaxPacket, &s),
        ActivityEvent::ConstructHouseTax {
            design: 300,
            heavy_count: 1,
            normal_count: 2,
            heavy: true,
            base: 1_000,
            deposit: 2_000,
            total: 3_500,
        }
    );
}

#[test]
fn house_build_progress() {
    // SCHouseBuildProgressPacket: u16 tl, u32 model, i32 all, i32 cur (packets.rs:65).
    let mut s = PacketStream::new();
    s.write_u16(7).write_u32(0x10).write_i32(12).write_i32(3);
    assert_eq!(
        event(sc::SCHouseBuildProgressPacket, &s),
        ActivityEvent::HouseBuildProgress {
            tl: 7,
            model: 0x10,
            all: 12,
            cur: 3
        }
    );
}

#[test]
fn house_tax_info() {
    // SCHouseTaxInfoPacket (housing/packets.rs:142-160).
    let mut s = PacketStream::new();
    s.write_u16(7)
        .write_i32(15)
        .write_i32(2_000)
        .write_i32(3_500)
        .write_i64(1_800_000_000)
        .write_bool(false)
        .write_i32(1)
        .write_bool(true);
    assert_eq!(
        event(sc::SCHouseTaxInfoPacket, &s),
        ActivityEvent::HouseTaxInfo {
            tl: 7,
            dominion_tax_rate: 15,
            deposit: 2_000,
            total: 3_500,
            due: 1_800_000_000,
            already_paid: false,
            weeks_without_pay: 1,
            heavy: true,
        }
    );
}

// ---- mail -----------------------------------------------------------------------------------

/// `MailHeader.Write` (G/systems/economy/mail.rs:370-383).
fn write_header(s: &mut PacketStream, h: &MailHeader) {
    s.write_i64(h.mail_id)
        .write_u8(h.mail_type)
        .write_u8(h.status)
        .write_str(&h.title)
        .write_str(&h.sender)
        .write_u8(h.attachments)
        .write_str(&h.receiver)
        .write_i64(h.open_date)
        .write_bool(h.returned)
        .write_i64(h.extra);
}

fn header(id: i64, kind: u8, title: &str) -> MailHeader {
    MailHeader {
        mail_id: id,
        mail_type: kind,
        status: 0,
        title: title.into(),
        sender: "Merchant".into(),
        attachments: 1,
        receiver: "Nick".into(),
        open_date: 0,
        returned: false,
        extra: 0,
    }
}

/// `Item.Write` of a plain item (G/models/item.rs:1061-1082).
fn write_item(s: &mut PacketStream, template: u32, id: u64, count: i32) {
    s.write_u32(template)
        .write_u64(id)
        .write_u8(1)
        .write_u8(0)
        .write_i32(count)
        .write_u8(0)
        .write_i64(0)
        .write_i32(0)
        .write_u32(0)
        .write_u8(1)
        .write_i64(0)
        .write_i64(0);
}

/// `MailBody.Write` with the given attachments in the first slots (mail.rs:402-424).
fn write_body(s: &mut PacketStream, mail_id: i64, items: &[(u32, u64, i32)]) {
    s.write_i64(mail_id)
        .write_u8(mail_type::SYS_SELL_BACKPACK)
        .write_str("Nick")
        .write_str("Payment")
        .write_str("Your pack was sold.")
        .write_i32(120_000) // copper
        .write_i32(0) // billing
        .write_i32(0) // money 2
        .write_i64(100)
        .write_i64(200)
        .write_i64(0);
    for i in 0..10 {
        match items.get(i) {
            Some(&(template, id, count)) => write_item(s, template, id, count),
            None => {
                s.write_i32(0);
            }
        }
    }
}

fn body_info(mail_id: i64, items: Vec<ItemBrief>) -> MailBodyInfo {
    MailBodyInfo {
        mail_id,
        mail_type: mail_type::SYS_SELL_BACKPACK,
        receiver: "Nick".into(),
        title: "Payment".into(),
        text: "Your pack was sold.".into(),
        money: 120_000,
        billing: 0,
        money2: 0,
        send_date: 100,
        recv_date: 200,
        open_date: 0,
        items,
    }
}

fn brief(template_id: u32, id: u64, count: i32) -> ItemBrief {
    ItemBrief {
        template_id,
        id,
        grade: 1,
        flags: 0,
        count,
    }
}

#[test]
fn count_unread_mail() {
    // SCCountUnreadMailPacket: 4 x i32 (G/packets/world.rs:189, G/models/char_data.rs:552).
    let mut s = PacketStream::new();
    s.write_i32(1).write_i32(2).write_i32(3).write_i32(4);
    assert_eq!(
        event(sc::SCCountUnreadMailPacket, &s),
        ActivityEvent::CountUnreadMail {
            unread: [1, 2, 3, 4]
        }
    );
}

#[test]
fn mail_received_without_body() {
    // SCGotMailPacket: header, 4 counters, bool has_body, bool is_cancel
    // (G/systems/economy/mail_packets.rs:108-118): the payment mail of a sold trade pack.
    let h = header(900, mail_type::SYS_SELL_BACKPACK, "Payment");
    let mut s = PacketStream::new();
    write_header(&mut s, &h);
    s.write_i32(0)
        .write_i32(1)
        .write_i32(0)
        .write_i32(0)
        .write_bool(false) // no body
        .write_bool(false); // is_cancel
    assert_eq!(
        event(sc::SCGotMailPacket, &s),
        ActivityEvent::MailReceived {
            mail: Box::new(h),
            unread: [0, 1, 0, 0],
            is_cancel: false,
            body: None,
        }
    );
}

#[test]
fn mail_received_with_body_reads_the_cancel_flag_after_it() {
    let h = header(901, mail_type::CHARGED, "Cash on delivery");
    let mut s = PacketStream::new();
    write_header(&mut s, &h);
    s.write_i32(0)
        .write_i32(1)
        .write_i32(0)
        .write_i32(0)
        .write_bool(true);
    write_body(&mut s, 901, &[(8001, 33, 5)]);
    s.write_bool(true); // is_cancel
    assert_eq!(
        event(sc::SCGotMailPacket, &s),
        ActivityEvent::MailReceived {
            mail: Box::new(h),
            unread: [0, 1, 0, 0],
            is_cancel: true,
            body: Some(Box::new(body_info(901, vec![brief(8001, 33, 5)]))),
        }
    );
}

#[test]
fn mail_list_and_end() {
    // SCMailListPacket: bool is_sent, i32 n, headers (mail_packets.rs:131); SCMailListEndPacket:
    // i32, i32 (mail_packets.rs:152).
    let (a, b) = (header(1, 1, "Hello"), header(2, 19, "Payment"));
    let mut s = PacketStream::new();
    s.write_bool(false).write_i32(2);
    write_header(&mut s, &a);
    write_header(&mut s, &b);
    assert_eq!(
        event(sc::SCMailListPacket, &s),
        ActivityEvent::MailList {
            is_sent: false,
            mails: vec![a, b]
        }
    );
    let mut s = PacketStream::new();
    s.write_i32(2).write_i32(0);
    assert_eq!(
        event(sc::SCMailListEndPacket, &s),
        ActivityEvent::MailListEnd {
            total_headers: 2,
            total_bodies: 0
        }
    );
    // A count beyond the body is refused before anything is allocated.
    let mut s = PacketStream::new();
    s.write_bool(false).write_i32(i32::MAX);
    assert!(parse(1, sc::SCMailListPacket, s.as_bytes()).is_err());
}

#[test]
fn mail_body_with_attachments() {
    // SCMailBodyPacket: bool prepare, bool is_sent, MailBody.Write, bool open date modified,
    // 4 counters (mail_packets.rs:172-178). Two items, eight empty slots.
    let mut s = PacketStream::new();
    s.write_bool(false).write_bool(false);
    write_body(&mut s, 900, &[(8001, 33, 5), (8002, 34, 1)]);
    s.write_bool(true)
        .write_i32(0)
        .write_i32(0)
        .write_i32(0)
        .write_i32(0);
    assert_eq!(
        event(sc::SCMailBodyPacket, &s),
        ActivityEvent::MailBody {
            is_prepare: false,
            is_sent: false,
            body: Box::new(body_info(900, vec![brief(8001, 33, 5), brief(8002, 34, 1)])),
            open_date_modified: true,
            unread: [0, 0, 0, 0],
        }
    );
}

#[test]
fn mail_sent_and_failed_take_their_item_pairs_from_the_packet_length() {
    // SCMailSentPacket: header, (u8, u8) per item (mail_packets.rs:86).
    let h = header(5, 1, "Gift");
    let mut s = PacketStream::new();
    write_header(&mut s, &h);
    s.write_u8(1).write_u8(4).write_u8(1).write_u8(5);
    assert_eq!(
        event(sc::SCMailSentPacket, &s),
        ActivityEvent::MailSent {
            mail: Box::new(h),
            items: vec![(1, 4), (1, 5)],
        }
    );
    // SCMailFailedPacket: u8 err, (u8, u8) per item, bool money (mail_packets.rs:65).
    let mut s = PacketStream::new();
    s.write_u8(9).write_u8(1).write_u8(4).write_bool(true);
    assert_eq!(
        event(sc::SCMailFailedPacket, &s),
        ActivityEvent::MailFailed {
            err: 9,
            items: vec![(1, 4)],
            money: true,
        }
    );
    let mut s = PacketStream::new();
    s.write_u8(3).write_bool(false);
    assert_eq!(
        event(sc::SCMailFailedPacket, &s),
        ActivityEvent::MailFailed {
            err: 3,
            items: vec![],
            money: false,
        }
    );
}

#[test]
fn attachment_taken_keeps_the_valid_entries() {
    // SCAttachmentTakenPacket: i64 mail, 3 bools, u8 n, 10 x (u64, u8, u8) (mail_packets.rs:220).
    let mut s = PacketStream::new();
    s.write_i64(900)
        .write_bool(true)
        .write_bool(false)
        .write_bool(true)
        .write_u8(2);
    for i in 0..10u64 {
        if i < 2 {
            s.write_u64(100 + i).write_u8(1).write_u8(10 + i as u8);
        } else {
            s.write_u64(0).write_u8(0).write_u8(0);
        }
    }
    assert_eq!(
        event(sc::SCAttachmentTakenPacket, &s),
        ActivityEvent::AttachmentTaken {
            mail_id: 900,
            money: true,
            aa_point: false,
            take_sequentially: true,
            items: vec![
                TakenItem {
                    id: 100,
                    slot_type: 1,
                    slot: 10
                },
                TakenItem {
                    id: 101,
                    slot_type: 1,
                    slot: 11
                }
            ]
            .into_boxed_slice(),
        }
    );
}

#[test]
fn charge_money_paid_deleted_and_status() {
    // SCChargeMoneyPaidPacket: i64 (mail_packets.rs:250).
    let mut s = PacketStream::new();
    s.write_i64(42);
    assert_eq!(
        event(sc::SCChargeMoneyPaidPacket, &s),
        ActivityEvent::ChargeMoneyPaid { mail_id: 42 }
    );
    // SCMailDeletedPacket: bool is_sent, i64 mail, bool modified, 4 counters (mail_packets.rs:269).
    let mut s = PacketStream::new();
    s.write_bool(false)
        .write_i64(42)
        .write_bool(true)
        .write_i32(0)
        .write_i32(7)
        .write_i32(0)
        .write_i32(0);
    assert_eq!(
        event(sc::SCMailDeletedPacket, &s),
        ActivityEvent::MailDeleted {
            is_sent: false,
            mail_id: 42,
            unread_modified: true,
            unread: [0, 7, 0, 0],
        }
    );
    // SCMailStatusUpdatedPacket: bool is_sent, i64 mail, u8 status (mail_packets.rs:308).
    let mut s = PacketStream::new();
    s.write_bool(true).write_i64(42).write_u8(1);
    assert_eq!(
        event(sc::SCMailStatusUpdatedPacket, &s),
        ActivityEvent::MailStatusUpdated {
            is_sent: true,
            mail_id: 42,
            status: 1
        }
    );
}

// ---- specialty ------------------------------------------------------------------------------

#[test]
fn specialty_ratio() {
    // SCSpecialtyRatioPacket: i32 (G/systems/economy/specialty.rs:582).
    let mut s = PacketStream::new();
    s.write_i32(130);
    assert_eq!(
        event(sc::SCSpecialtyRatioPacket, &s),
        ActivityEvent::SpecialtyRatio { ratio: 130 }
    );
}

#[test]
fn specialty_current() {
    // SCSpecialtyCurrentPacket: i32 n, u16 from, u16 to, (u32, u32) x n (specialty.rs:600).
    let mut s = PacketStream::new();
    s.write_i32(2)
        .write_u16(3)
        .write_u16(4)
        .write_u32(8000)
        .write_u32(110)
        .write_u32(8001)
        .write_u32(95);
    assert_eq!(
        event(sc::SCSpecialtyCurrentPacket, &s),
        ActivityEvent::SpecialtyCurrent {
            from_zone_group: 3,
            to_zone_group: 4,
            rates: vec![(8000, 110), (8001, 95)].into_boxed_slice(),
        }
    );
    let mut s = PacketStream::new();
    s.write_i32(1_000_000).write_u16(3).write_u16(4);
    assert!(parse(1, sc::SCSpecialtyCurrentPacket, s.as_bytes()).is_err());
}

// ---- dispatch and robustness ----------------------------------------------------------------

#[test]
fn other_levels_and_opcodes_are_ignored() {
    let mut s = PacketStream::new();
    s.write_u32(1);
    // The proxy level (2) reuses opcode numbers.
    assert_eq!(
        parse(2, sc::SCDuelChallengedPacket, s.as_bytes()).unwrap(),
        None
    );
    assert_eq!(
        parse(4, sc::SCDuelChallengedPacket, s.as_bytes()).unwrap(),
        None
    );
    // An opcode of another family.
    assert_eq!(
        parse(1, sc::SCChatMessagePacket, s.as_bytes()).unwrap(),
        None
    );
    assert_eq!(parse(1, 0xFFE, &[]).unwrap(), None);
}

#[test]
fn events_stay_small() {
    // Hundreds of bots queue these: large payloads are boxed.
    assert!(
        std::mem::size_of::<ActivityEvent>() <= 56,
        "ActivityEvent is {} bytes",
        std::mem::size_of::<ActivityEvent>()
    );
}

#[test]
fn truncated_bodies_fail_and_never_panic() {
    let mut h = PacketStream::new();
    write_house(&mut h, &house());
    let mut got = PacketStream::new();
    write_header(&mut got, &header(901, mail_type::CHARGED, "COD"));
    got.write_i32(0)
        .write_i32(1)
        .write_i32(0)
        .write_i32(0)
        .write_bool(true);
    write_body(&mut got, 901, &[(8001, 33, 5)]);
    got.write_bool(false);
    let mut taken = PacketStream::new();
    taken
        .write_i64(900)
        .write_bool(true)
        .write_bool(false)
        .write_bool(true)
        .write_u8(1);
    for _ in 0..10 {
        taken.write_u64(1).write_u8(1).write_u8(1);
    }
    let mut tax = PacketStream::new();
    tax.write_u16(7)
        .write_i32(15)
        .write_i32(2_000)
        .write_i32(3_500)
        .write_i64(1)
        .write_bool(false)
        .write_i32(1)
        .write_bool(true);
    let samples: [(u16, &PacketStream); 4] = [
        (sc::SCMyHousePacket, &h),
        (sc::SCGotMailPacket, &got),
        (sc::SCAttachmentTakenPacket, &taken),
        (sc::SCHouseTaxInfoPacket, &tax),
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

#[test]
fn arbitrary_bytes_never_panic() {
    // Deterministic pseudo-random bodies against every opcode: parse may fail but must not
    // panic or allocate from a wire count.
    let mut x: u32 = 0x1234_5678;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    let mut bodies: Vec<Vec<u8>> = vec![vec![], vec![0xFF; 64], vec![0; 64], vec![0x7F; 300]];
    for len in [1usize, 3, 7, 12, 40, 100, 400] {
        bodies.push((0..len).map(|_| next() as u8).collect());
    }
    for opcode in 0..0x400u16 {
        for body in &bodies {
            let _ = parse(1, opcode, body);
        }
    }
}
