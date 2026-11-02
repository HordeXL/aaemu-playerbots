//! Boats: summon scrolls and the slaves (vehicles) they summon.
//!
//! A scroll is an item whose use skill carries the `SpawnSlave` special effect (type 60: skills
//! 15802, 19622, 22267); `item_summon_slaves` names the slave it summons
//! (`systems/skills/effects/special/misc.rs: spawn_slave` -> `systems/vehicles/slaves.rs:
//! create_from_skill`). Only scrolls of *boat* kinds are kept (`SlaveTemplate::is_a_boat`,
//! `vehicles/data.rs:282`): big / small sailing ship, speedboat, boat, fishboat, merchant ship.
//!
//! The summon needs water at the chosen spot (a ship "must be summoned on water") within
//! [`SlaveTemplate::summon_range`] of the caster; the caster then boards the driver seat
//! (attach point 1) and steers with throttle / steering input, passengers take points 2 and up.
//!
//! The top speed follows the server's ship physics (`systems/ai/physics/ship.rs`):
//! `ship_models.velocity * MoveSpeedMul / 2`, where `MoveSpeedMul` is the unit attribute (base
//! 1000, so 1.0) plus the slave's `unit_modifiers` rows (the rowboat and the clipper both carry
//! +1000, so 2.0), clamped to `unit_attribute_limits`.

use anyhow::{Context, Result};
use rustc_hash::{FxHashMap, FxHashSet};

use super::{localized, str_bytes, SkillTiming, TravelInputs};
use crate::geom::IdMap;
use crate::sql::{each_row, RowExt};
use crate::text::TextTable;

/// `SlaveKind` values of boats (`vehicles/data.rs:167`).
pub mod slave_kind {
    pub const BIG_SAILING_SHIP: u8 = 1;
    pub const SMALL_SAILING_SHIP: u8 = 2;
    pub const SPEEDBOAT: u8 = 3;
    pub const BOAT: u8 = 4;
    pub const FISHBOAT: u8 = 9;
    pub const MERCHANT_SHIP: u8 = 10;
}

/// The `SpawnSlave` special effect type.
const SPAWN_SLAVE_EFFECT: u32 = 60;
/// The skill that casts the common boat scrolls.
pub const SUMMON_BOAT_SKILL: u32 = 15802;
/// Driver seat attach point.
pub const DRIVER_POINT: u8 = 1;
/// Passenger attach points are `PASSENGER_FIRST..=PASSENGER_LAST` (`Passenger0..Passenger6`).
pub const PASSENGER_FIRST: u8 = 2;
/// Last passenger attach point.
pub const PASSENGER_LAST: u8 = 8;

// `UnitAttribute` ids in `unit_modifiers` / `unit_attribute_limits`.
const ATTR_STA: u32 = 2;
const ATTR_MAX_HEALTH: u32 = 6;
const ATTR_MOVE_SPEED_MUL: u32 = 10;
/// `unit_formulas` rows (owner type 2 = slave) the hit point estimate mirrors.
const MAX_HEALTH_FORMULA: &str = "(level * 70 + sta * 12)";
const STA_FORMULA: &str = "10 + (level - 1) * 2";

/// Whether a `slave_kind_id` is a boat.
pub fn is_boat_kind(kind: u8) -> bool {
    use slave_kind::*;
    matches!(
        kind,
        BIG_SAILING_SHIP | SMALL_SAILING_SHIP | SPEEDBOAT | BOAT | FISHBOAT | MERCHANT_SHIP
    )
}

/// A boat slave template.
#[derive(Clone, Debug, PartialEq)]
pub struct SlaveTemplate {
    /// `slaves.id`.
    pub id: u32,
    /// English name (Korean if there is no translation).
    pub name: Box<str>,
    /// `slaves.model_id`.
    pub model: u32,
    /// `slaves.slave_kind_id`; see [`slave_kind`].
    pub kind: u8,
    /// `slaves.level` (the level the server gives the slave unit).
    pub level: u8,
    /// Maximum hit points the server computes for the slave, estimated from its formulas and
    /// flat / percent modifiers (no buffs, no gear).
    pub max_hp: u32,
    /// `ship_models.velocity` of the model (0 without a ship model).
    pub velocity: f32,
    /// Final `MoveSpeedMul` of the template (1.0 = no modifier).
    pub move_speed_mul: f32,
    /// `slaves.spawn_valid_area_range`: how far from the caster the boat may be placed, metres.
    pub summon_range: u16,
    /// `slaves.mountable`.
    pub mountable: bool,
    /// Bit `n` set = the model declares attach point `n` (`model_bindings`), for `n` in 1..=8.
    seats: u16,
}

impl SlaveTemplate {
    /// Top speed in metres per second (`velocity * MoveSpeedMul / 2`, the server's physics cap
    /// without wind).
    pub fn top_speed(&self) -> f32 {
        self.velocity * self.move_speed_mul / 2.0
    }

    /// Whether the model has a driver seat (attach point 1).
    pub fn has_driver_seat(&self) -> bool {
        self.seats & (1 << DRIVER_POINT) != 0
    }

    /// Passenger seats the model declares (attach points 2..=8).
    pub fn passenger_seats(&self) -> u8 {
        (PASSENGER_FIRST..=PASSENGER_LAST)
            .filter(|p| self.seats & (1 << p) != 0)
            .count() as u8
    }

    /// The seat attach points (driver first) the model declares among 1..=8.
    pub fn seat_points(&self) -> impl Iterator<Item = u8> + '_ {
        (DRIVER_POINT..=PASSENGER_LAST).filter(|p| self.seats & (1 << p) != 0)
    }

    /// The slave is a boat (it is for every template in this table).
    pub fn is_boat(&self) -> bool {
        is_boat_kind(self.kind)
    }
}

/// An item that summons a boat.
#[derive(Clone, Debug, PartialEq)]
pub struct BoatScroll {
    /// Item template.
    pub item: u32,
    /// English item name.
    pub name: Box<str>,
    /// `items.price` (the vendor sell value, copper; 0 for event / test items).
    pub price: u32,
    /// The skill using the item casts, with its cast time and cooldown.
    pub skill: SkillTiming,
    /// The summoned slave template (`item_summon_slaves`).
    pub slave: u32,
    /// NPC templates whose merchant goods list the item (empty = not sold).
    pub merchants: Box<[u32]>,
}

/// Boat data.
#[derive(Debug, Default)]
pub struct Boats {
    /// Sorted by `(price, item)`.
    scrolls: Vec<BoatScroll>,
    slaves: IdMap<SlaveTemplate>,
}

impl Boats {
    pub(crate) fn load(inp: &TravelInputs<'_>) -> Result<Boats> {
        let conn = inp.conn;

        // Skills with the SpawnSlave effect, and the items that cast them.
        let mut summon_skills: FxHashSet<u32> = FxHashSet::default();
        each_row(
            conn,
            &format!(
                "SELECT DISTINCT sk.skill_id FROM skill_effects sk \
                 JOIN effects e ON e.id = sk.effect_id AND e.actual_type = 'SpecialEffect' \
                 JOIN special_effects se ON se.id = e.actual_id \
                 WHERE se.special_effect_type_id = {SPAWN_SLAVE_EFFECT}"
            ),
            |r| {
                summon_skills.insert(r.u32(0));
            },
        )
        .context("read SpawnSlave skills")?;
        if !summon_skills.contains(&SUMMON_BOAT_SKILL) {
            tracing::warn!("travel: skill {SUMMON_BOAT_SKILL} has no SpawnSlave effect");
        }

        let mut items: Vec<(u32, u32, u32, u32)> = Vec::new(); // item, skill, price, slave
        each_row(
            conn,
            "SELECT i.id, i.use_skill_id, i.price, m.slave_id FROM items i \
             JOIN item_summon_slaves m ON m.item_id = i.id WHERE i.use_skill_id != 0 ORDER BY i.id",
            |r| {
                if summon_skills.contains(&r.u32(1)) {
                    items.push((r.u32(0), r.u32(1), r.u32(2), r.u32(3)));
                }
            },
        )
        .context("read summon scrolls")?;
        items.dedup_by_key(|i| i.0);

        // The slaves they summon, keeping boats only.
        let wanted: FxHashSet<u32> = items.iter().map(|i| i.3).collect();
        let slave_names = localized(conn, "slaves", "name");
        let mut rows: Vec<SlaveRow> = Vec::new();
        each_row(
            conn,
            "SELECT id, name, model_id, mountable, level, slave_kind_id, spawn_valid_area_range \
             FROM slaves",
            |r| {
                let id = r.u32(0);
                if wanted.contains(&id) && is_boat_kind(r.u8(5)) {
                    rows.push(SlaveRow {
                        id,
                        name: slave_names
                            .get(&id)
                            .map(|n| &**n)
                            .unwrap_or_else(|| r.text(1))
                            .into(),
                        model: r.u32(2),
                        mountable: r.flag(3),
                        level: r.u8(4),
                        kind: r.u8(5),
                        range: r.u16(6),
                    });
                }
            },
        )
        .context("read slaves")?;
        let boat_slaves: FxHashSet<u32> = rows.iter().map(|r| r.id).collect();
        let models: FxHashSet<u32> = rows.iter().map(|r| r.model).collect();

        let velocity = ship_velocities(conn, &models)?;
        let seats = model_seats(conn, &models)?;
        let mods = SlaveModifiers::load(conn, &boat_slaves)?;
        check_hp_formulas(conn);

        let mut templates = Vec::new();
        for row in rows {
            let m = row.id;
            let level = i32::from(row.level);
            let sta = mods.apply(ATTR_STA, 10 + (level - 1) * 2, m);
            let hp = mods.apply(ATTR_MAX_HEALTH, level * 70 + sta * 12, m);
            templates.push((
                row.id,
                SlaveTemplate {
                    id: row.id,
                    name: row.name,
                    model: row.model,
                    kind: row.kind,
                    level: row.level,
                    max_hp: hp.max(0) as u32,
                    velocity: velocity.get(&row.model).copied().unwrap_or(0.0),
                    move_speed_mul: mods.move_speed_mul(m),
                    summon_range: row.range,
                    mountable: row.mountable,
                    seats: seats.get(&row.model).copied().unwrap_or(0),
                },
            ));
        }
        let slaves = IdMap::build(templates);

        // Vendors per scroll.
        let scroll_ids: FxHashSet<u32> = items
            .iter()
            .filter(|i| slaves.contains(i.3))
            .map(|i| i.0)
            .collect();
        let mut sold_by: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        each_row(
            conn,
            "SELECT m.npc_id, g.item_id FROM merchants m \
             JOIN merchant_goods g ON g.merchant_pack_id = m.merchant_pack_id",
            |r| {
                if scroll_ids.contains(&r.u32(1)) {
                    sold_by.entry(r.u32(1)).or_default().push(r.u32(0));
                }
            },
        )
        .context("read merchant goods")?;

        let mut scrolls: Vec<BoatScroll> = items
            .iter()
            .filter(|i| slaves.contains(i.3))
            .map(|&(item, skill, price, slave)| {
                let mut merchants = sold_by.remove(&item).unwrap_or_default();
                merchants.sort_unstable();
                merchants.dedup();
                BoatScroll {
                    item,
                    name: inp
                        .items
                        .get(item)
                        .unwrap_or_else(|| inp.texts.get(TextTable::Item, item).unwrap_or(""))
                        .into(),
                    price,
                    skill: SkillTiming::of(inp.skills, skill, "boat summon"),
                    slave,
                    merchants: merchants.into_boxed_slice(),
                }
            })
            .collect();
        scrolls.sort_by_key(|s| (s.price, s.item));
        Ok(Boats { scrolls, slaves })
    }

    /// Boat summon scrolls, cheapest first (ties by item id).
    pub fn scrolls(&self) -> &[BoatScroll] {
        &self.scrolls
    }

    /// A scroll by item template.
    pub fn scroll(&self, item: u32) -> Option<&BoatScroll> {
        self.scrolls.iter().find(|s| s.item == item)
    }

    /// A boat slave template (those some scroll summons).
    pub fn slave_template(&self, id: u32) -> Option<&SlaveTemplate> {
        self.slaves.get(id)
    }

    /// The boat a scroll item summons.
    pub fn boat_for_scroll(&self, item: u32) -> Option<&SlaveTemplate> {
        self.slave_template(self.scroll(item)?.slave)
    }

    /// All boat slave templates in id order.
    pub fn slave_templates(&self) -> &[SlaveTemplate] {
        self.slaves.values()
    }

    /// The cheapest scroll whose boat has a driver seat and a (non-zero) top speed of at least
    /// `min_speed` metres per second.
    pub fn cheapest_scroll(&self, min_speed: f32) -> Option<&BoatScroll> {
        self.scrolls.iter().find(|s| {
            self.slave_template(s.slave).is_some_and(|b| {
                b.has_driver_seat() && b.top_speed() > 0.0 && b.top_speed() >= min_speed
            })
        })
    }

    /// Approximate heap bytes held.
    pub fn bytes(&self) -> usize {
        self.scrolls.capacity() * std::mem::size_of::<BoatScroll>()
            + str_bytes(self.scrolls.iter().map(|s| &*s.name))
            + self
                .scrolls
                .iter()
                .map(|s| s.merchants.len() * 4)
                .sum::<usize>()
            + self.slaves.shallow_bytes()
            + str_bytes(self.slaves.values().iter().map(|s| &*s.name))
    }
}

struct SlaveRow {
    id: u32,
    name: Box<str>,
    model: u32,
    mountable: bool,
    level: u8,
    kind: u8,
    range: u16,
}

/// `model id -> ship_models.velocity` for the models of the boats (`models.sub_type =
/// 'ShipModel'`, `sub_id` = the ship model).
fn ship_velocities(
    conn: &rusqlite::Connection,
    models: &FxHashSet<u32>,
) -> Result<FxHashMap<u32, f32>> {
    let mut ship_of: FxHashMap<u32, u32> = FxHashMap::default();
    each_row(
        conn,
        "SELECT id, sub_id FROM models WHERE sub_type = 'ShipModel'",
        |r| {
            if models.contains(&r.u32(0)) {
                ship_of.insert(r.u32(0), r.u32(1));
            }
        },
    )
    .context("read ship models of boats")?;
    let mut velocity_of_ship: FxHashMap<u32, f32> = FxHashMap::default();
    each_row(conn, "SELECT id, velocity FROM ship_models", |r| {
        velocity_of_ship.insert(r.u32(0), r.f32(1));
    })
    .context("read ship_models")?;
    Ok(ship_of
        .into_iter()
        .filter_map(|(model, ship)| Some((model, *velocity_of_ship.get(&ship)?)))
        .collect())
}

/// `model id -> seat mask` from `model_bindings` (attach points 1..=8).
fn model_seats(
    conn: &rusqlite::Connection,
    models: &FxHashSet<u32>,
) -> Result<FxHashMap<u32, u16>> {
    let mut seats: FxHashMap<u32, u16> = FxHashMap::default();
    each_row(
        conn,
        "SELECT owner_id, attach_point_id FROM model_bindings WHERE owner_type = 'Model'",
        |r| {
            let (model, point) = (r.u32(0), r.int(1));
            if models.contains(&model)
                && (i64::from(DRIVER_POINT)..=i64::from(PASSENGER_LAST)).contains(&point)
            {
                *seats.entry(model).or_default() |= 1u16 << point;
            }
        },
    )
    .context("read model_bindings")?;
    Ok(seats)
}

/// The slaves' `unit_modifiers` for Sta, MaxHealth and MoveSpeedMul and the attribute limits.
struct SlaveModifiers {
    /// `(slave, attribute, percent, value)` in table order.
    rows: Vec<(u32, u32, bool, i64)>,
    /// `(attribute, minimum, maximum)`.
    limits: Vec<(u32, i64, i64)>,
}

impl SlaveModifiers {
    fn load(conn: &rusqlite::Connection, slaves: &FxHashSet<u32>) -> Result<SlaveModifiers> {
        let mut rows = Vec::new();
        each_row(
            conn,
            &format!(
                "SELECT owner_id, unit_attribute_id, unit_modifier_type_id, value FROM unit_modifiers \
                 WHERE owner_type = 'Slave' AND unit_attribute_id IN \
                 ({ATTR_STA}, {ATTR_MAX_HEALTH}, {ATTR_MOVE_SPEED_MUL}) ORDER BY id"
            ),
            |r| {
                if slaves.contains(&r.u32(0)) {
                    rows.push((r.u32(0), r.u32(1), r.int(2) == 1, r.int(3)));
                }
            },
        )
        .context("read slave unit_modifiers")?;
        let mut limits = Vec::new();
        each_row(
            conn,
            &format!(
                "SELECT unit_attribute_id, minimum, maximum FROM unit_attribute_limits \
                 WHERE unit_attribute_id IN ({ATTR_STA}, {ATTR_MAX_HEALTH}, {ATTR_MOVE_SPEED_MUL})"
            ),
            |r| limits.push((r.u32(0), r.int(1), r.int(2))),
        )
        .context("read unit_attribute_limits")?;
        Ok(SlaveModifiers { rows, limits })
    }

    fn clamp(&self, attr: u32, value: f64) -> f64 {
        match self.limits.iter().find(|l| l.0 == attr) {
            Some(&(_, min, max)) => value.clamp(min as f64, max as f64),
            None => value,
        }
    }

    /// An integer attribute with the slave's modifiers (the server's `int_bonuses`: in row
    /// order, a percent adds the truncated share, a value adds itself; then the limits).
    fn apply(&self, attr: u32, base: i32, slave: u32) -> i32 {
        let mut res = f64::from(base);
        for &(_, _, percent, value) in self.rows.iter().filter(|r| r.0 == slave && r.1 == attr) {
            if percent {
                res += f64::from((res as f32 * value as f32 / 100.0).trunc());
            } else {
                res += value as f64;
            }
        }
        self.clamp(attr, res) as i32
    }

    /// `CalculateWithBonuses(1000, MoveSpeedMul) / 1000`: values first, then percents.
    fn move_speed_mul(&self, slave: u32) -> f32 {
        let mut res = 1000.0f64;
        let rows = || {
            self.rows
                .iter()
                .filter(move |r| r.0 == slave && r.1 == ATTR_MOVE_SPEED_MUL)
        };
        for &(_, _, _, value) in rows().filter(|r| !r.2) {
            res += value as f64;
        }
        for &(_, _, _, value) in rows().filter(|r| r.2) {
            res += res * value as f64 / 100.0;
        }
        (self.clamp(ATTR_MOVE_SPEED_MUL, res) / 1000.0) as f32
    }
}

/// Warns when the data's slave hit point formulas are not the ones [`Boats::load`] mirrors.
fn check_hp_formulas(conn: &rusqlite::Connection) {
    let mut seen = [false, false];
    let mut other = false;
    let read = each_row(
        conn,
        "SELECT kind_id, formula FROM unit_formulas WHERE owner_type_id = 2 AND kind_id IN (14, 24)",
        |r| {
            let (slot, want) = match r.int(0) {
                14 => (0, MAX_HEALTH_FORMULA),
                _ => (1, STA_FORMULA),
            };
            seen[slot] = true;
            other |= r.text(1).trim() != want;
        },
    );
    if read.is_ok() && (other || seen != [true, true]) {
        tracing::warn!(
            "travel: the slave hit point formulas in unit_formulas differ from the built-in ones; \
             SlaveTemplate::max_hp may be off"
        );
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::doodad::DoodadData;
    use crate::geom::StrTable;
    use crate::npc::NpcTemplate;
    use crate::skill::Skills;
    use crate::spatial::SpawnIndex;
    use crate::text::Texts;

    /// Rowboat (slave 15, model 129: driver 1 + passenger 2), clipper (slave 52, model 128:
    /// driver only), a tank and scrolls for them, one sold by an NPC.
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE skill_effects (id INT, skill_id INT, effect_id INT);
             CREATE TABLE effects (id INT, actual_id INT, actual_type TEXT);
             CREATE TABLE special_effects (id INT, special_effect_type_id INT);
             CREATE TABLE items (id INT, name TEXT, use_skill_id INT, price INT);
             CREATE TABLE item_summon_slaves (id INT, item_id INT, slave_id INT);
             CREATE TABLE slaves (id INT, name TEXT, model_id INT, mountable TEXT, level INT,
               slave_kind_id INT, spawn_valid_area_range INT);
             CREATE TABLE models (id INT, sub_id INT, sub_type TEXT);
             CREATE TABLE ship_models (id INT, velocity REAL);
             CREATE TABLE model_bindings (id INT, owner_id INT, owner_type TEXT, attach_point_id INT);
             CREATE TABLE unit_modifiers (id INT, owner_id INT, owner_type TEXT, unit_attribute_id INT,
               unit_modifier_type_id INT, value INT);
             CREATE TABLE unit_attribute_limits (id INT, unit_attribute_id INT, minimum INT, maximum INT);
             CREATE TABLE unit_formulas (id INT, formula TEXT, kind_id INT, owner_type_id INT);
             CREATE TABLE merchants (id INT, npc_id INT, merchant_pack_id INT);
             CREATE TABLE merchant_goods (id INT, merchant_pack_id INT, item_id INT, grade_id INT);
             CREATE TABLE localized_texts (id INT, tbl_name TEXT, tbl_column_name TEXT, idx INT, en_us TEXT);
             INSERT INTO skill_effects VALUES (1, 15802, 10), (2, 12345, 11);
             INSERT INTO effects VALUES (10, 7, 'SpecialEffect'), (11, 8, 'SpecialEffect');
             INSERT INTO special_effects VALUES (7, 60), (8, 25);
             INSERT INTO items VALUES (17906, 's', 15802, 0), (15817, 's', 15802, 10000), (17626, 's', 15802, 50000),
               (150, 'siege', 15802, 200000), (999, 'x', 12345, 5), (4045, 'book', 11216, 3000);
             INSERT INTO item_summon_slaves VALUES (1, 17906, 15), (2, 15817, 15), (3, 17626, 52), (4, 150, 100), (5, 999, 15);
             INSERT INTO slaves VALUES (15, 'rowboat', 129, 't', 50, 4, 50), (52, 'clipper', 128, 't', 50, 3, 50),
               (100, 'tank', 5, 't', 50, 5, 50);
             INSERT INTO models VALUES (129, 4, 'ShipModel'), (128, 3, 'ShipModel'), (5, 9, 'VehicleModel');
             INSERT INTO ship_models VALUES (4, 6.4), (3, 13.0);
             INSERT INTO model_bindings VALUES (1, 129, 'Model', 1), (2, 129, 'Model', 2), (3, 129, 'Model', 35),
               (4, 128, 'Model', 1), (5, 128, 'Model', 9), (6, 129, 'Object', 3);
             INSERT INTO unit_modifiers VALUES (1, 15, 'Slave', 10, 0, 1000), (2, 15, 'Slave', 6, 0, 1420),
               (3, 52, 'Slave', 10, 0, 1000), (4, 52, 'Slave', 6, 0, 2420), (5, 15, 'Item', 10, 0, 7777),
               (6, 52, 'Slave', 10, 1, 50);
             INSERT INTO unit_attribute_limits VALUES (1, 10, -10000, 3000), (2, 6, -2000000000, 10000000);
             INSERT INTO unit_formulas VALUES (48, '(level * 70 + sta * 12)', 14, 2), (141, '10 + (level - 1) * 2', 24, 2);
             INSERT INTO merchants VALUES (1, 4242, 7), (2, 4243, 7);
             INSERT INTO merchant_goods VALUES (1, 7, 17626, 1), (2, 7, 17626, 1), (3, 8, 15817, 1);
             INSERT INTO localized_texts VALUES (1, 'slaves', 'name', 15, 'Rowboat');",
        )
        .unwrap();
        conn
    }

    fn load(conn: &Connection) -> Boats {
        let dir = std::env::temp_dir();
        let npcs = IdMap::<NpcTemplate>::build(Vec::new());
        let doodads = DoodadData::default();
        let texts = Texts::empty();
        let skills = Skills::default();
        let spawns = SpawnIndex::default();
        let items = StrTable::build(vec![
            (17906, "Scroll: Sharani's Rowboat"),
            (15817, "Scroll: Rowboat"),
            (17626, "Scroll: Clipper"),
        ]);
        let inp = TravelInputs {
            conn,
            texts: &texts,
            server_data: &dir,
            main_world: "main_world",
            npcs: &npcs,
            doodads: &doodads,
            skills: &skills,
            doodad_spawns: &spawns,
            items: &items,
        };
        Boats::load(&inp).unwrap()
    }

    #[test]
    fn scrolls_are_sorted_cheapest_first_and_only_boats_remain() {
        let b = load(&fixture());
        let order: Vec<(u32, u32)> = b.scrolls().iter().map(|s| (s.item, s.price)).collect();
        assert_eq!(order, vec![(17906, 0), (15817, 10_000), (17626, 50_000)]);
        // the siege weapon scroll, the non-SpawnSlave skill and the book are not boat scrolls
        assert!(b.scroll(150).is_none() && b.scroll(999).is_none() && b.scroll(4045).is_none());
        assert_eq!(&*b.scroll(17906).unwrap().name, "Scroll: Sharani's Rowboat");
        assert_eq!(b.scroll(15817).unwrap().skill.id, 15802);
        assert_eq!(b.boat_for_scroll(15817).map(|s| s.id), Some(15));
        assert_eq!(b.boat_for_scroll(17626).map(|s| s.id), Some(52));
        assert!(b.boat_for_scroll(150).is_none());
        assert!(b.slave_template(100).is_none(), "the tank is not a boat");
        // vendors, deduplicated
        assert_eq!(&*b.scroll(17626).unwrap().merchants, &[4242, 4243]);
        assert!(b.scroll(17906).unwrap().merchants.is_empty());
    }

    #[test]
    fn slave_templates_have_seats_speed_and_hit_points() {
        let b = load(&fixture());
        let row = b.slave_template(15).unwrap();
        assert_eq!(&*row.name, "Rowboat", "the localized name wins");
        assert_eq!(
            (row.model, row.kind, row.level, row.summon_range),
            (129, 4, 50, 50)
        );
        // driver 1 + passenger 2; point 35 and the 'Object' row are not seats
        assert!(row.has_driver_seat());
        assert_eq!(row.passenger_seats(), 1);
        assert_eq!(row.seat_points().collect::<Vec<_>>(), vec![1, 2]);
        // +1000 flat -> MoveSpeedMul 2.0, the Item modifier is not the slave's
        assert_eq!(row.velocity, 6.4);
        assert_eq!(row.move_speed_mul, 2.0);
        assert!((row.top_speed() - 6.4).abs() < 1e-5);
        // level 50: sta 108, 50 * 70 + 108 * 12 = 4796, + 1420
        assert_eq!(row.max_hp, 6216);

        let clipper = b.slave_template(52).unwrap();
        assert!(clipper.has_driver_seat() && clipper.passenger_seats() == 0);
        // +1000 flat, then +50 percent: 2000 * 1.5 = 3000 (the limit)
        assert_eq!(clipper.move_speed_mul, 3.0);
        assert!((clipper.top_speed() - 13.0 * 3.0 / 2.0).abs() < 1e-4);
        assert_eq!(clipper.max_hp, 3500 + 108 * 12 + 2420);
        assert!(row.is_boat() && b.slave_templates().len() == 2);
    }

    #[test]
    fn cheapest_scroll_needs_a_driver_seat_and_speed() {
        let b = load(&fixture());
        assert_eq!(b.cheapest_scroll(0.0).map(|s| s.item), Some(17906));
        assert_eq!(b.cheapest_scroll(7.0).map(|s| s.item), Some(17626));
        assert!(b.cheapest_scroll(100.0).is_none());
        assert!(b.bytes() > 0);
    }

    #[test]
    fn boat_kinds_match_the_servers_is_a_boat() {
        for k in [1, 2, 3, 4, 9, 10] {
            assert!(is_boat_kind(k), "{k}");
        }
        for k in [0, 5, 6, 7, 8, 11] {
            assert!(!is_boat_kind(k), "{k}");
        }
    }
}
