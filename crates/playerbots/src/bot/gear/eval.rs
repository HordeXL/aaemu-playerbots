//! What a piece of gear is worth to a bot, and the best move from the bag to the equipment.
//!
//! Pure: everything here reads plain values (the bot's [`Fit`], the [`Facts`] of the items, the
//! [`Worn`] pieces and the bag), never a clock or a socket, so it is tested with made-up items.
//!
//! * A piece's **value** is `(item level + 4) * grade factor * affinity * (1 + 4 % per gem)`:
//!   the item level carries most of the weight, the grade multiplies it like the game's stat
//!   multiplier (common 1.0, mythic 2.0), the affinity says how well the piece suits the build
//!   (its armor kind and weapon kind, [`Fit::affinity`]), the gems add a little.
//! * The two hands count together: a two-handed weapon is worth [`TWO_HANDS`] times its value
//!   (it empties the off hand), a one-handed set is the main hand plus the off hand.
//! * A bag piece replaces a worn one when it is worth at least [`MIN_GAIN_PCT`] percent more, the
//!   bot meets its level requirement ([`GearItem::wearable_by`]) and the piece suits the build at
//!   all (affinity above 0). Rings and earrings go over the weaker of the two.
//! * **Tiers** are level bands of ten levels ([`Tier::of_level`]): each asks for a lowest item
//!   level and a grade. Worn gear below the band is the gear goal's reason to run
//!   ([`deficit`]).

use aa_world::gear::{
    armor_kind, equip_slot, holdable, slot_type, GearClass, GearData, GearItem, Lunagem,
};
use smallvec::SmallVec;

use crate::archetype::Archetype;

/// The equipment slots the bots manage (0..=18: head to instrument).
pub const SLOTS: usize = 19;
/// A two-handed weapon is worth this many times its value (it takes both hands).
pub const TWO_HANDS: f32 = 1.8;
/// A bag piece must be worth this much more (percent) than the worn one to be put on.
pub const MIN_GAIN_PCT: f32 = 3.0;
/// The least gain that counts at all (a level-1 piece over nothing is worth more).
pub const MIN_GAIN: f32 = 0.5;
/// Value added per socketed gem (fraction of the piece's value).
pub const GEM_BONUS: f32 = 0.04;
/// The affinity of armor of another kind than the build's.
pub const OTHER_ARMOR: f32 = 0.55;
/// The affinity of a bow for a build that does not shoot.
pub const SPARE_BOW: f32 = 0.35;
/// The affinity of an instrument.
pub const INSTRUMENT: f32 = 0.4;
/// The slots the tier deficit looks at: the armor and the main hand.
pub const CORE_SLOTS: [u8; 8] = [
    equip_slot::HEAD,
    equip_slot::CHEST,
    equip_slot::WAIST,
    equip_slot::LEGS,
    equip_slot::HANDS,
    equip_slot::FEET,
    equip_slot::ARMS,
    equip_slot::MAINHAND,
];
/// The slots a minting bot fills besides the core slots.
pub const EXTRA_SLOTS: [u8; 6] = [
    equip_slot::BACK,
    equip_slot::NECK,
    equip_slot::EAR1,
    equip_slot::EAR2,
    equip_slot::FINGER1,
    equip_slot::FINGER2,
];

/// What the item facts the evaluation needs are read from: the world's [`GearData`], or a table
/// in a test.
pub trait Facts {
    /// The gear facts of a template.
    fn item(&self, template: u32) -> Option<GearItem>;
    /// The stat factor of a grade (common 1.0).
    fn grade_factor(&self, grade: u8) -> f32;
    /// The order of a grade (poor 0, common 1, ... mythic 11).
    fn grade_order(&self, grade: u8) -> u8;
    /// The lunagem facts of a template.
    fn gem(&self, template: u32) -> Option<Lunagem>;
    /// Sockets of a piece of `slot_type` at `grade`.
    fn sockets(&self, slot_type: u8, grade: u8) -> u8;
    /// The chance, per 10 000, that the next gem goes into a piece holding `held`.
    fn socket_chance(&self, held: u8) -> u16;
}

impl Facts for GearData {
    fn item(&self, template: u32) -> Option<GearItem> {
        GearData::item(self, template).copied()
    }

    fn grade_factor(&self, grade: u8) -> f32 {
        GearData::grade_factor(self, grade)
    }

    fn grade_order(&self, grade: u8) -> u8 {
        self.grade(grade).order
    }

    fn gem(&self, template: u32) -> Option<Lunagem> {
        GearData::gem(self, template).copied()
    }

    fn sockets(&self, slot_type: u8, grade: u8) -> u8 {
        GearData::sockets(self, slot_type, grade)
    }

    fn socket_chance(&self, held: u8) -> u16 {
        GearData::socket_chance(self, held)
    }
}

/// What suits a build: its armor kind and weapon kinds (from the class build,
/// [`Archetype::armor_type`], [`Archetype::weapon_priority`], [`Archetype::uses_shield`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fit {
    /// The armor kind ([`armor_kind`]); 0 takes any.
    pub armor: u8,
    /// The weapon holdables, the most preferred first.
    pub weapons: SmallVec<[u8; 12]>,
    /// Wears a shield.
    pub shield: bool,
    /// Shoots a bow.
    pub bow: bool,
}

impl Fit {
    /// The fit of a class build.
    pub fn of(a: &Archetype) -> Fit {
        let mut weapons: SmallVec<[u8; 12]> = SmallVec::new();
        for name in &a.weapon_priority {
            for &h in holdable::of_name(name) {
                if !weapons.contains(&h) && weapons.len() < 12 {
                    weapons.push(h);
                }
            }
        }
        Fit {
            armor: a.armor_type,
            bow: weapons.contains(&holdable::BOW[0]),
            weapons,
            shield: a.uses_shield,
        }
    }

    /// How well `item` suits the build, 0 (never wear it) to 1.
    pub fn affinity(&self, item: &GearItem) -> f32 {
        if !item.character_gear() {
            return 0.0;
        }
        match item.class {
            GearClass::Accessory => 1.0,
            GearClass::Armor => {
                if self.armor == 0
                    || item.kind == self.armor
                    || item.slot_type == slot_type::BACK
                    || !matches!(
                        item.kind,
                        armor_kind::CLOTH | armor_kind::LEATHER | armor_kind::PLATE
                    )
                {
                    1.0
                } else {
                    OTHER_ARMOR
                }
            }
            GearClass::Weapon => match item.slot_type {
                slot_type::SHIELD => {
                    if self.shield {
                        1.0
                    } else {
                        0.0
                    }
                }
                slot_type::RANGED => {
                    if self.bow {
                        1.0
                    } else {
                        SPARE_BOW
                    }
                }
                slot_type::INSTRUMENT => INSTRUMENT,
                _ => match self.weapons.iter().position(|&h| h == item.kind) {
                    Some(rank) => (1.0 - 0.06 * rank as f32).max(0.6),
                    None => 0.0,
                },
            },
        }
    }

    /// The holdables of the build's first weapon kind.
    pub fn main_weapons(&self) -> &[u8] {
        let Some(&first) = self.weapons.first() else {
            return &[];
        };
        holdable::NAMES
            .iter()
            .map(|(_, ids)| *ids)
            .find(|ids| ids.contains(&first))
            .unwrap_or(&[])
    }
}

/// The value of a piece (see the module docs); 0 when it does not suit the build.
pub fn value(item: &GearItem, grade_factor: f32, gems: u8, affinity: f32) -> f32 {
    if affinity <= 0.0 {
        return 0.0;
    }
    (f32::from(item.level) + 4.0) * grade_factor * affinity * (1.0 + GEM_BONUS * f32::from(gems))
}

/// One worn piece.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Piece {
    /// Item instance id.
    pub id: u64,
    /// Template.
    pub template: u32,
    /// Grade.
    pub grade: u8,
    /// Gems socketed; `None` when the bot does not know.
    pub gems: Option<u8>,
    /// The facts (`None` for a template that is no gear the bot knows).
    pub item: Option<GearItem>,
    /// [`value`] for the bot.
    pub value: f32,
}

/// The worn pieces by equipment slot (0..[`SLOTS`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Worn {
    /// The piece in each slot.
    pub slots: [Option<Piece>; SLOTS],
}

impl Worn {
    /// The worn set from the equipment list (`(slot, id, template, grade)`) and the known gems.
    pub fn build(
        equipment: impl IntoIterator<Item = (u8, u64, u32, u8)>,
        gems: impl Fn(u64) -> Option<u8>,
        facts: &dyn Facts,
        fit: &Fit,
    ) -> Worn {
        let mut w = Worn::default();
        for (slot, id, template, grade) in equipment {
            let Some(cell) = w.slots.get_mut(usize::from(slot)) else {
                continue;
            };
            let item = facts.item(template);
            let g = gems(id);
            let v = item.map_or(0.0, |i| {
                value(
                    &i,
                    facts.grade_factor(grade),
                    g.unwrap_or(0),
                    fit.affinity(&i),
                )
            });
            *cell = Some(Piece {
                id,
                template,
                grade,
                gems: g,
                item,
                value: v,
            });
        }
        w
    }

    /// The piece in `slot`.
    pub fn get(&self, slot: u8) -> Option<&Piece> {
        self.slots.get(usize::from(slot)).and_then(Option::as_ref)
    }

    /// The value of the piece in `slot`, 0 when empty.
    pub fn value_of(&self, slot: u8) -> f32 {
        self.get(slot).map_or(0.0, |p| p.value)
    }

    /// Whether the main hand holds a two-handed weapon.
    pub fn two_handed(&self) -> bool {
        self.get(equip_slot::MAINHAND)
            .and_then(|p| p.item)
            .is_some_and(|i| i.two_handed())
    }

    /// The value of both hands together.
    pub fn hands(&self) -> f32 {
        let main = self.value_of(equip_slot::MAINHAND);
        if self.two_handed() {
            main * TWO_HANDS
        } else {
            main + self.value_of(equip_slot::OFFHAND)
        }
    }

    /// The sum of the values of every worn piece (both hands counted together).
    pub fn total(&self) -> f32 {
        let mut t = self.hands();
        for slot in 0..SLOTS as u8 {
            if slot != equip_slot::MAINHAND && slot != equip_slot::OFFHAND {
                t += self.value_of(slot);
            }
        }
        t
    }
}

/// A gear piece in the bag the bot could put on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BagPiece {
    /// Bag slot.
    pub slot: u16,
    /// Item instance id.
    pub id: u64,
    /// Template.
    pub template: u32,
    /// Grade.
    pub grade: u8,
    /// Gems, `None` when unknown.
    pub gems: Option<u8>,
}

/// A move from the bag to the equipment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Move {
    /// The bag item to put on.
    pub id: u64,
    /// Its template.
    pub template: u32,
    /// Its bag slot.
    pub bag_slot: u16,
    /// The equipment slot.
    pub slot: u8,
    /// The worn piece it replaces (0: the slot is empty).
    pub worn_id: u64,
    /// The value it adds.
    pub gain: f32,
}

/// What a bag piece would add in `slot` (both hands counted together): `(gain, before)`.
fn gain_in(worn: &Worn, slot: u8, item: &GearItem, v: f32) -> (f32, f32) {
    if slot == equip_slot::MAINHAND || slot == equip_slot::OFFHAND {
        let before = worn.hands();
        let off = worn.value_of(equip_slot::OFFHAND);
        let main = worn.value_of(equip_slot::MAINHAND);
        let after = if slot == equip_slot::MAINHAND {
            if item.two_handed() {
                v * TWO_HANDS
            } else if worn.two_handed() {
                v
            } else {
                v + off
            }
        } else if worn.two_handed() {
            // the server takes the two-handed weapon off for the off-hand piece
            v
        } else {
            main + v
        };
        (after - before, before)
    } else {
        let before = worn.value_of(slot);
        (v - before, before)
    }
}

/// The best piece of the bag to put on now, if any is worth it. `level` and `gender` are the
/// character's; `free_bag` the free bag slots (a two-handed weapon pushes the off-hand piece into
/// one, `None` when unknown); `skip` the items not to try (refused ones).
#[allow(clippy::too_many_arguments)]
pub fn best_move(
    worn: &Worn,
    bag: impl IntoIterator<Item = BagPiece>,
    facts: &dyn Facts,
    fit: &Fit,
    level: u8,
    gender: u8,
    free_bag: Option<usize>,
    skip: impl Fn(u64) -> bool,
) -> Option<Move> {
    let mut best: Option<Move> = None;
    for p in bag {
        if skip(p.id) {
            continue;
        }
        if let Some(m) = place(worn, &p, facts, fit, level, gender, free_bag, true) {
            if best.is_none_or(|b| m.gain > b.gain) {
                best = Some(m);
            }
        }
    }
    best
}

/// Where the bag piece `p` goes: the slot where it adds the most. With `worth_it`, only a slot
/// where it adds at least [`MIN_GAIN_PCT`] percent (and [`MIN_GAIN`]) counts; without, the piece
/// goes on even when it is worse (an operator asked for it). Never a piece above the bot's level
/// requirement, one that does not suit the build at all, or a slot that holds something the bot
/// does not know as gear (a costume, a tool).
#[allow(clippy::too_many_arguments)]
pub fn place(
    worn: &Worn,
    p: &BagPiece,
    facts: &dyn Facts,
    fit: &Fit,
    level: u8,
    gender: u8,
    free_bag: Option<usize>,
    worth_it: bool,
) -> Option<Move> {
    if worn.slots.iter().flatten().any(|w| w.id == p.id) {
        return None;
    }
    let item = facts.item(p.template)?;
    if !item.wearable_by(level, gender) {
        return None;
    }
    let v = value(
        &item,
        facts.grade_factor(p.grade),
        p.gems.unwrap_or(0),
        fit.affinity(&item),
    );
    if v <= 0.0 {
        return None;
    }
    let mut best: Option<Move> = None;
    for &slot in item.slots() {
        if worn.get(slot).is_some_and(|w| w.item.is_none()) {
            continue;
        }
        // a two-handed weapon (or an off-hand piece under one) needs a bag slot for the other
        let pushes_out = (item.two_handed() && worn.get(equip_slot::OFFHAND).is_some())
            || (slot == equip_slot::OFFHAND && worn.two_handed());
        if pushes_out && free_bag == Some(0) {
            continue;
        }
        let (gain, before) = gain_in(worn, slot, &item, v);
        if worth_it && !worth(gain, before) {
            continue;
        }
        if best.is_none_or(|b| gain > b.gain) {
            best = Some(Move {
                id: p.id,
                template: p.template,
                bag_slot: p.slot,
                slot,
                worn_id: worn.get(slot).map_or(0, |w| w.id),
                gain,
            });
        }
    }
    best
}

/// What a piece of `item` at value `v` would add in its best slot (both hands counted together),
/// `None` when no slot takes it.
pub fn gain_of(worn: &Worn, item: &GearItem, v: f32) -> Option<(u8, f32, f32)> {
    let mut best: Option<(u8, f32, f32)> = None;
    for &slot in item.slots() {
        if worn.get(slot).is_some_and(|w| w.item.is_none()) {
            continue;
        }
        let (gain, before) = gain_in(worn, slot, item, v);
        if best.is_none_or(|b| gain > b.1) {
            best = Some((slot, gain, before));
        }
    }
    best
}

/// Whether `gain` over a piece worth `before` is worth a change.
pub fn worth(gain: f32, before: f32) -> bool {
    gain >= MIN_GAIN.max(before * MIN_GAIN_PCT / 100.0)
}

/// What a level band asks of the worn gear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier {
    /// The band: level / 10 (0..=5).
    pub band: u8,
    /// The lowest item level the band is content with.
    pub min_item_level: u8,
    /// The grade the band wishes for (minted pieces get it, regrades aim for it).
    pub grade: u8,
}

impl Tier {
    /// The tier of a character level: bands of ten levels; the lowest item level is three below
    /// the band's start (the starter sets are level 7), the grade climbs from common (bands 0 and
    /// 1) through uncommon, rare and ancient to heroic (band 5, level 50 and up).
    pub fn of_level(level: u8) -> Tier {
        let band = (level / 10).min(5);
        let grade = match band {
            0 | 1 => 0,
            2 => 2,
            3 => 3,
            4 => 4,
            _ => 5,
        };
        Tier {
            band,
            min_item_level: (band * 10).saturating_sub(3).max(1),
            grade,
        }
    }
}

/// How far the core slots ([`CORE_SLOTS`]) are below the tier, 0 (all fine) to 1 (all empty):
/// an empty slot counts 1, a piece below the band's item level a tenth per missing level (at most
/// 1), a piece that suits the build badly a half.
pub fn deficit(worn: &Worn, tier: &Tier, fit: &Fit) -> f32 {
    let mut sum = 0.0;
    for &slot in &CORE_SLOTS {
        sum += match worn.get(slot).and_then(|p| p.item) {
            None => 1.0,
            Some(item) => {
                let below = f32::from(tier.min_item_level.saturating_sub(item.level)) / 10.0;
                let poor_fit = if fit.affinity(&item) < 0.6 { 0.5 } else { 0.0 };
                below.min(1.0).max(poor_fit)
            }
        };
    }
    sum / CORE_SLOTS.len() as f32
}

/// The slots a bot fills when it mints gear: the core slots, the off hand when it wears a shield
/// (and the main weapon is one-handed), the ranged slot when it shoots, and the accessories.
pub fn wanted_slots(fit: &Fit, main_two_handed: bool) -> SmallVec<[u8; 16]> {
    let mut out: SmallVec<[u8; 16]> = CORE_SLOTS.iter().copied().collect();
    if fit.shield && !main_two_handed {
        out.push(equip_slot::OFFHAND);
    }
    if fit.bow {
        out.push(equip_slot::RANGED);
    }
    out.extend(EXTRA_SLOTS.iter().copied());
    out
}

/// The slot types that go into an equipment slot, for picking a new piece for it.
pub fn slot_types_for(slot: u8, fit: &Fit) -> &'static [u8] {
    match slot {
        equip_slot::HEAD => &[slot_type::HEAD],
        equip_slot::NECK => &[slot_type::NECK],
        equip_slot::CHEST => &[slot_type::CHEST],
        equip_slot::WAIST => &[slot_type::WAIST],
        equip_slot::LEGS => &[slot_type::LEGS],
        equip_slot::HANDS => &[slot_type::HANDS],
        equip_slot::FEET => &[slot_type::FEET],
        equip_slot::ARMS => &[slot_type::ARMS],
        equip_slot::BACK => &[slot_type::BACK],
        equip_slot::EAR1 | equip_slot::EAR2 => &[slot_type::EAR],
        equip_slot::FINGER1 | equip_slot::FINGER2 => &[slot_type::FINGER],
        equip_slot::MAINHAND => &[
            slot_type::TWO_HANDED,
            slot_type::ONE_HANDED,
            slot_type::MAINHAND,
        ],
        equip_slot::OFFHAND if fit.shield => &[slot_type::SHIELD],
        equip_slot::OFFHAND => &[slot_type::ONE_HANDED, slot_type::OFFHAND],
        equip_slot::RANGED => &[slot_type::RANGED],
        _ => &[],
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use aa_world::gear::{F_ENCHANTABLE, F_GRADABLE, NO_GRADE};
    use std::collections::HashMap;

    /// Items, gems and the socket tables of a test.
    #[derive(Default)]
    pub struct Table {
        pub items: HashMap<u32, GearItem>,
        pub gems: HashMap<u32, Lunagem>,
    }

    impl Facts for Table {
        fn item(&self, template: u32) -> Option<GearItem> {
            self.items.get(&template).copied()
        }
        fn grade_factor(&self, grade: u8) -> f32 {
            match grade {
                1 => 0.8,
                2 => 1.08,
                3 => 1.16,
                4 => 1.24,
                5 => 1.32,
                _ => 1.0,
            }
        }
        fn grade_order(&self, grade: u8) -> u8 {
            match grade {
                0 => 1,
                1 => 0,
                g => g,
            }
        }
        fn gem(&self, template: u32) -> Option<Lunagem> {
            self.gems.get(&template).copied()
        }
        fn sockets(&self, slot_type: u8, grade: u8) -> u8 {
            match (slot_type, grade) {
                (_, 0 | 1) => 0,
                (slot_type::CHEST, g) => g - 1,
                (_, g) => (g - 1).min(2),
            }
        }
        fn socket_chance(&self, held: u8) -> u16 {
            match held {
                0 => 10_000,
                1..=6 => 5_000,
                _ => 0,
            }
        }
    }

    pub fn armor(slot: u8, kind: u8, level: u8) -> GearItem {
        GearItem {
            slot_type: slot,
            class: GearClass::Armor,
            kind,
            level,
            req: level.saturating_sub(2),
            fixed_grade: NO_GRADE,
            gender: 0,
            flags: F_GRADABLE | F_ENCHANTABLE,
        }
    }

    pub fn weapon(slot: u8, holdable: u8, level: u8) -> GearItem {
        GearItem {
            slot_type: slot,
            class: GearClass::Weapon,
            kind: holdable,
            level,
            req: level.saturating_sub(2),
            fixed_grade: NO_GRADE,
            gender: 0,
            flags: F_GRADABLE | F_ENCHANTABLE,
        }
    }

    pub fn plate_sword_and_shield() -> Fit {
        Fit {
            armor: armor_kind::PLATE,
            weapons: [1, 5, 6].into_iter().collect(),
            shield: true,
            bow: false,
        }
    }

    pub fn nodachi_leather() -> Fit {
        Fit {
            armor: armor_kind::LEATHER,
            weapons: [8, 3, 1].into_iter().collect(),
            shield: false,
            bow: false,
        }
    }

    fn bag(id: u64, template: u32) -> BagPiece {
        BagPiece {
            slot: id as u16,
            id,
            template,
            grade: 0,
            gems: Some(0),
        }
    }

    #[test]
    fn the_fit_of_a_build_follows_its_armor_and_weapons() {
        let a: Vec<Archetype> = serde_json::from_str(include_str!("../../../data/archetypes.json"))
            .expect("archetypes");
        let runner = a.iter().find(|a| a.name == "Darkrunner").unwrap();
        let fit = Fit::of(runner);
        assert_eq!(fit.armor, armor_kind::LEATHER);
        assert_eq!(&fit.weapons[..2], &[8, 3], "nodachi, then greatsword");
        assert!(!fit.shield && !fit.bow);
        assert_eq!(fit.main_weapons(), holdable::NODACHI);
        let archer = a.iter().find(|a| a.name == "Primeval").unwrap();
        assert!(Fit::of(archer).bow);
        let templar = Fit::of(a.iter().find(|a| a.name == "Templar").unwrap());
        assert!(templar.shield);
        assert_eq!(templar.weapons.as_slice(), holdable::CLUB);
    }

    #[test]
    fn affinity_prefers_the_build_and_refuses_the_wrong_weapons() {
        let fit = plate_sword_and_shield();
        assert_eq!(fit.affinity(&armor(slot_type::HEAD, 3, 10)), 1.0);
        assert_eq!(fit.affinity(&armor(slot_type::HEAD, 1, 10)), OTHER_ARMOR);
        assert_eq!(fit.affinity(&armor(slot_type::BACK, 1, 10)), 1.0, "cloaks");
        assert_eq!(
            fit.affinity(&armor(slot_type::HEAD, 4, 10)),
            0.0,
            "pet gear"
        );
        assert_eq!(fit.affinity(&weapon(slot_type::ONE_HANDED, 1, 10)), 1.0);
        assert!(fit.affinity(&weapon(slot_type::ONE_HANDED, 6, 10)) < 1.0);
        assert_eq!(fit.affinity(&weapon(slot_type::TWO_HANDED, 8, 10)), 0.0);
        assert_eq!(fit.affinity(&weapon(slot_type::SHIELD, 24, 10)), 1.0);
        assert_eq!(
            nodachi_leather().affinity(&weapon(slot_type::SHIELD, 24, 10)),
            0.0
        );
        assert_eq!(fit.affinity(&weapon(slot_type::RANGED, 19, 10)), SPARE_BOW);
    }

    #[test]
    fn a_better_bag_piece_goes_on_and_a_worse_one_stays() {
        let mut t = Table::default();
        t.items.insert(1, armor(slot_type::CHEST, 3, 10));
        t.items.insert(2, armor(slot_type::CHEST, 3, 13));
        t.items.insert(3, armor(slot_type::CHEST, 3, 9));
        let fit = plate_sword_and_shield();
        let worn = Worn::build([(equip_slot::CHEST, 100, 1, 0)], |_| Some(0), &t, &fit);
        let m = best_move(&worn, [bag(7, 3), bag(8, 2)], &t, &fit, 20, 1, None, |_| {
            false
        })
        .expect("the level 13 shirt is better");
        assert_eq!((m.id, m.slot, m.worn_id), (8, equip_slot::CHEST, 100));
        assert!(m.gain > 0.0);
        // only a worse piece: nothing
        assert!(best_move(&worn, [bag(7, 3)], &t, &fit, 20, 1, None, |_| false).is_none());
        // the better piece is above the level requirement
        assert!(best_move(&worn, [bag(8, 2)], &t, &fit, 10, 1, None, |_| false).is_none());
        // a refused piece is skipped
        assert!(best_move(&worn, [bag(8, 2)], &t, &fit, 20, 1, None, |id| id == 8).is_none());
    }

    #[test]
    fn a_grade_can_beat_a_level_and_the_build_beats_both() {
        let mut t = Table::default();
        t.items.insert(1, armor(slot_type::HEAD, 3, 12));
        t.items.insert(2, armor(slot_type::HEAD, 1, 14)); // cloth: poor fit for plate
        let fit = plate_sword_and_shield();
        let worn = Worn::build([(equip_slot::HEAD, 100, 1, 0)], |_| Some(0), &t, &fit);
        assert!(best_move(&worn, [bag(2, 2)], &t, &fit, 20, 1, None, |_| false).is_none());
        // the same plate helmet at grade 3 is worth more than the common one
        let mut b = bag(3, 1);
        b.grade = 3;
        let m = best_move(&worn, [b], &t, &fit, 20, 1, None, |_| false).unwrap();
        assert_eq!(m.id, 3);
    }

    #[test]
    fn two_hands_are_weighed_together() {
        let mut t = Table::default();
        t.items.insert(1, weapon(slot_type::ONE_HANDED, 1, 10)); // sword
        t.items.insert(2, weapon(slot_type::SHIELD, 24, 10));
        t.items.insert(3, weapon(slot_type::TWO_HANDED, 8, 12)); // nodachi
        t.items.insert(4, weapon(slot_type::TWO_HANDED, 3, 10)); // greatsword
                                                                 // sword and board: a nodachi does not suit the build at all
        let fit = plate_sword_and_shield();
        let worn = Worn::build(
            [
                (equip_slot::MAINHAND, 100, 1, 0),
                (equip_slot::OFFHAND, 101, 2, 0),
            ],
            |_| Some(0),
            &t,
            &fit,
        );
        assert!(best_move(&worn, [bag(3, 3)], &t, &fit, 20, 1, None, |_| false).is_none());
        // a nodachi build drops the one-handed sword for the nodachi (the shield goes to the bag)
        let fit = nodachi_leather();
        let worn = Worn::build([(equip_slot::MAINHAND, 100, 1, 0)], |_| Some(0), &t, &fit);
        let m = best_move(&worn, [bag(3, 3)], &t, &fit, 20, 1, None, |_| false).unwrap();
        assert_eq!((m.slot, m.worn_id), (equip_slot::MAINHAND, 100));
        // a two-handed weapon with an off-hand piece needs a free bag slot
        let worn = Worn::build(
            [
                (equip_slot::MAINHAND, 100, 1, 0),
                (equip_slot::OFFHAND, 101, 1, 0),
            ],
            |_| Some(0),
            &t,
            &fit,
        );
        assert!(best_move(&worn, [bag(3, 3)], &t, &fit, 20, 1, Some(0), |_| false).is_none());
        assert!(best_move(&worn, [bag(3, 3)], &t, &fit, 20, 1, Some(3), |_| false).is_some());
        // the preferred kind wins over a slightly higher level of the second kind
        let worn = Worn::default();
        let m = best_move(&worn, [bag(4, 4), bag(3, 3)], &t, &fit, 20, 1, None, |_| {
            false
        });
        assert_eq!(m.unwrap().id, 3);
    }

    #[test]
    fn rings_go_over_the_weaker_finger() {
        let mut t = Table::default();
        let ring = |level| GearItem {
            class: GearClass::Accessory,
            kind: 5,
            ..armor(slot_type::FINGER, 5, level)
        };
        t.items.insert(1, ring(10));
        t.items.insert(2, ring(20));
        t.items.insert(3, ring(15));
        let fit = plate_sword_and_shield();
        let worn = Worn::build(
            [
                (equip_slot::FINGER1, 100, 2, 0),
                (equip_slot::FINGER2, 101, 1, 0),
            ],
            |_| Some(0),
            &t,
            &fit,
        );
        let m = best_move(&worn, [bag(3, 3)], &t, &fit, 30, 1, None, |_| false).unwrap();
        assert_eq!((m.slot, m.worn_id), (equip_slot::FINGER2, 101));
    }

    #[test]
    fn tiers_climb_with_the_level_and_the_deficit_shows_the_gap() {
        assert_eq!(
            Tier::of_level(5),
            Tier {
                band: 0,
                min_item_level: 1,
                grade: 0
            }
        );
        assert_eq!(Tier::of_level(14).min_item_level, 7);
        assert_eq!(Tier::of_level(25).grade, 2);
        assert_eq!(Tier::of_level(55).band, 5);
        assert_eq!(Tier::of_level(55).grade, 5);
        let mut t = Table::default();
        t.items.insert(1, armor(slot_type::CHEST, 3, 9));
        let fit = plate_sword_and_shield();
        let naked = Worn::default();
        assert_eq!(deficit(&naked, &Tier::of_level(20), &fit), 1.0);
        let worn = Worn::build([(equip_slot::CHEST, 100, 1, 0)], |_| Some(0), &t, &fit);
        // level 25 wants item level 17: the level-9 shirt is 0.8 below, seven slots are empty
        let d = deficit(&worn, &Tier::of_level(25), &fit);
        assert!((d - (7.0 + 0.8) / 8.0).abs() < 1e-4, "{d}");
        // at level 12 the shirt is fine
        let d = deficit(&worn, &Tier::of_level(12), &fit);
        assert!((d - 7.0 / 8.0).abs() < 1e-4, "{d}");
        let slots = wanted_slots(&fit, false);
        assert!(slots.contains(&equip_slot::OFFHAND) && !slots.contains(&equip_slot::RANGED));
        assert!(!wanted_slots(&fit, true).contains(&equip_slot::OFFHAND));
        assert_eq!(
            slot_types_for(equip_slot::OFFHAND, &fit),
            [slot_type::SHIELD]
        );
    }
}
