//! Class archetypes.
//!
//! An archetype is a playable class build of the original PlayerBots mod
//! ([NulightJens/ArcheAge-PlayerBots](https://github.com/NulightJens/ArcheAge-PlayerBots),
//! GPL-3.0-or-later): the ability a new character starts with, its role and preferred fighting
//! range, the order skills are learned in and the id of its rotation file. `data/archetypes.json`
//! holds the seven archetypes; the rotations (`data/rotations/*.json`) are copied unchanged and
//! embedded for the rotation engine.
//!
//! The starting `ability` goes into `CSCreateCharacter.ability1`. The server accepts every
//! ability 1..=10 (each has a `character_equip_packs` row), but only 1, 5, 6, 7, 8 and 10 come
//! with a weapon and armor; 2, 3, 4 and 9 start naked, so the archetypes use 1, 6, 7 and 10.

use std::sync::Arc;

use anyhow::{ensure, Context};
use serde::Deserialize;

use crate::bot::rotation::Rotation;

/// The shipped archetypes.
const ARCHETYPES_JSON: &str = include_str!("../data/archetypes.json");

/// Melee auto attack skill (`skills.id` 2).
pub const AUTO_ATTACK_MELEE: u32 = 2;
/// Ranged auto attack skill (`skills.id` 4).
pub const AUTO_ATTACK_RANGED: u32 = 4;

/// The rotation files of `data/rotations`, by rotation id.
const ROTATIONS: &[(&str, &str)] = &[
    (
        "abolisher.tank",
        include_str!("../data/rotations/abolisher.tank.json"),
    ),
    (
        "cleric.support",
        include_str!("../data/rotations/cleric.support.json"),
    ),
    (
        "daggerspell.caster",
        include_str!("../data/rotations/daggerspell.caster.json"),
    ),
    (
        "darkrunner.melee",
        include_str!("../data/rotations/darkrunner.melee.json"),
    ),
    (
        "primeval.archer",
        include_str!("../data/rotations/primeval.archer.json"),
    ),
    (
        "reaper.caster",
        include_str!("../data/rotations/reaper.caster.json"),
    ),
    (
        "templar.support",
        include_str!("../data/rotations/templar.support.json"),
    ),
];

/// What a build does in a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Melee damage dealer.
    Melee,
    /// Ranged physical damage dealer.
    Ranged,
    /// Spell caster.
    Caster,
    /// Healer / support.
    Healer,
    /// Tank.
    Tank,
}

/// Whether a build fights next to its target or from a distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeKind {
    /// Next to the target.
    Melee,
    /// From a distance.
    Ranged,
}

/// One class build.
#[derive(Debug, Clone, Deserialize)]
pub struct Archetype {
    /// Display name ("Abolisher").
    pub name: String,
    /// Id of the rotation file ("abolisher.tank").
    pub rotation: String,
    /// Starting ability (class) id, `CSCreateCharacter.ability1`.
    pub ability: u8,
    /// Role in a group.
    pub role: Role,
    /// Fighting range kind.
    pub range: RangeKind,
    /// Distance in metres the bot keeps to its target.
    pub preferred_range: f32,
    /// The skill a new character of this ability casts (level 1 skill of the ability).
    pub starter_skill: u32,
    /// Relative frequency among new bots.
    pub weight: u32,
    /// Abilities of the final three-ability build.
    #[serde(default)]
    pub required_abilities: Vec<u8>,
    /// Primary stat (0 strength, 1 agility, 2 stamina, 3 intelligence, 4 spirit).
    #[serde(default)]
    pub primary_stat: u8,
    /// Armor type (1 cloth, 2 leather, 3 plate).
    #[serde(default)]
    pub armor_type: u8,
    /// Weapon kinds in order of preference.
    #[serde(default)]
    pub weapon_priority: Vec<String>,
    /// Wears a shield.
    #[serde(default)]
    pub uses_shield: bool,
    /// Character level at which the second ability is added.
    #[serde(default)]
    pub level_to_unlock_second: u8,
    /// Character level at which the third ability is added.
    #[serde(default)]
    pub level_to_unlock_third: u8,
    /// Skills in the order they are learned.
    #[serde(default)]
    pub skill_learn_order: Vec<u32>,
    /// Passive buffs that belong to the build.
    #[serde(default)]
    pub passive_buff_ids: Vec<u32>,
    /// The compiled rotation, shared by all bots of the build (filled in by [`Archetypes`]).
    #[serde(skip)]
    pub compiled: Option<Arc<Rotation>>,
}

impl Archetype {
    /// The basic attack skill of the build: 2 (melee) or 4 (ranged).
    pub fn auto_attack_skill(&self) -> u32 {
        match self.range {
            RangeKind::Melee => AUTO_ATTACK_MELEE,
            RangeKind::Ranged => AUTO_ATTACK_RANGED,
        }
    }

    /// The text of the rotation file (`data/rotations/<rotation>.json`), for the rotation engine.
    pub fn rotation_source(&self) -> Option<&'static str> {
        ROTATIONS
            .iter()
            .find(|(id, _)| *id == self.rotation)
            .map(|(_, text)| *text)
    }

    /// The compiled rotation, if the rotation file exists and parses.
    pub fn rotation(&self) -> Option<&Arc<Rotation>> {
        self.compiled.as_ref()
    }

    /// The skills of the starting ability, ordered by required ability level
    /// (`World::ability_skills`).
    pub fn ability_skills<'w>(&self, world: &'w aa_world::World) -> &'w [u32] {
        world.ability_skills(self.ability)
    }
}

/// Compiles the rotation file of `a`; problems are logged, a missing or broken file leaves the
/// build without a rotation (it fights with its starter skill and the basic attack).
fn compile_rotation(a: &Archetype) -> Option<Arc<Rotation>> {
    let src = a.rotation_source()?;
    match Rotation::compile(src) {
        Ok(r) => {
            for w in r.warnings() {
                tracing::warn!(archetype = %a.name, rotation = %a.rotation, "{w}");
            }
            Some(Arc::new(r))
        }
        Err(e) => {
            tracing::warn!(archetype = %a.name, rotation = %a.rotation, "rotation file: {e}");
            None
        }
    }
}

/// The set of archetypes.
#[derive(Debug, Clone)]
pub struct Archetypes {
    list: Vec<Arc<Archetype>>,
}

impl Archetypes {
    /// The archetypes shipped in `data/archetypes.json`.
    pub fn builtin() -> anyhow::Result<Archetypes> {
        Archetypes::from_json(ARCHETYPES_JSON).context("data/archetypes.json")
    }

    /// Parses and checks an archetype list.
    pub fn from_json(text: &str) -> anyhow::Result<Archetypes> {
        let mut list: Vec<Archetype> = serde_json::from_str(text)?;
        ensure!(!list.is_empty(), "no archetypes");
        for a in &list {
            ensure!(
                (1..=10).contains(&a.ability),
                "{}: ability {} is not a class ability (1..=10)",
                a.name,
                a.ability
            );
            ensure!(
                a.preferred_range > 0.5 && a.preferred_range < 40.0,
                "{}: preferred_range {} is not plausible",
                a.name,
                a.preferred_range
            );
            ensure!(a.starter_skill != 0, "{}: no starter skill", a.name);
        }
        for a in &mut list {
            a.compiled = compile_rotation(a);
        }
        Ok(Archetypes {
            list: list.into_iter().map(Arc::new).collect(),
        })
    }

    /// Every archetype.
    pub fn all(&self) -> &[Arc<Archetype>] {
        &self.list
    }

    /// The archetype called `name` (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&Arc<Archetype>> {
        self.list.iter().find(|a| a.name.eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_archetypes_load() {
        let a = Archetypes::builtin().unwrap();
        assert_eq!(a.all().len(), 7);
        let abolisher = a.get("abolisher").unwrap();
        assert_eq!(abolisher.ability, 1);
        assert_eq!(abolisher.role, Role::Tank);
        assert_eq!(abolisher.starter_skill, 18132);
        assert_eq!(abolisher.auto_attack_skill(), 2);
        let primeval = a.get("Primeval").unwrap();
        assert_eq!(primeval.range, RangeKind::Ranged);
        assert_eq!(primeval.auto_attack_skill(), 4);
        assert!(primeval.preferred_range > 10.0);
        assert!(a.get("nobody").is_none());
    }

    #[test]
    fn every_archetype_has_its_rotation_file_and_uses_a_geared_ability() {
        let a = Archetypes::builtin().unwrap();
        for arch in a.all() {
            let src = arch
                .rotation_source()
                .unwrap_or_else(|| panic!("{}", arch.name));
            let v: serde_json::Value = serde_json::from_str(src).unwrap();
            assert_eq!(v["id"], arch.rotation.as_str());
            assert_eq!(
                v["archetype"].as_str().unwrap().to_ascii_lowercase(),
                arch.name.to_ascii_lowercase()
            );
            // Abilities 2, 3, 4 and 9 have no starting weapon or armor in the 1.2 data.
            assert!(
                [1u8, 5, 6, 7, 8, 10].contains(&arch.ability),
                "{} starts naked with ability {}",
                arch.name,
                arch.ability
            );
            assert!(
                arch.skill_learn_order.contains(&arch.starter_skill),
                "{}",
                arch.name
            );
            assert_eq!(arch.required_abilities.first(), Some(&arch.ability));
        }
    }

    #[test]
    fn every_shipped_rotation_compiles_without_warnings() {
        let a = Archetypes::builtin().unwrap();
        for arch in a.all() {
            let rot = arch
                .rotation()
                .unwrap_or_else(|| panic!("{}: rotation did not compile", arch.name));
            assert!(
                rot.warnings().is_empty(),
                "{}: {:?}",
                arch.name,
                rot.warnings()
            );
            assert_eq!(rot.id(), arch.rotation.as_str());
            assert!(!rot.skills().is_empty(), "{}", arch.name);
        }
    }

    #[test]
    fn bad_archetypes_are_rejected() {
        let bad_ability = r#"[{"name":"X","rotation":"x","ability":11,"role":"melee","range":"melee","preferred_range":3.0,"starter_skill":1,"weight":1}]"#;
        assert!(Archetypes::from_json(bad_ability).is_err());
        let bad_range = r#"[{"name":"X","rotation":"x","ability":1,"role":"melee","range":"melee","preferred_range":0.0,"starter_skill":1,"weight":1}]"#;
        assert!(Archetypes::from_json(bad_range).is_err());
        assert!(Archetypes::from_json("[]").is_err());
        assert!(Archetypes::from_json("{}").is_err());
    }

    /// Checks the archetype data against the real `compact.sqlite3`:
    /// `AA_COMPACT=<path> cargo test -p playerbots -- --ignored archetypes_match_compact`.
    #[test]
    #[ignore = "needs compact.sqlite3: set AA_COMPACT"]
    fn archetypes_match_compact() {
        let path = std::env::var("AA_COMPACT").expect("AA_COMPACT not set");
        let db = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let a = Archetypes::builtin().unwrap();
        for arch in a.all() {
            let packs: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM character_equip_packs WHERE ability_id = ?1 AND newbie_weapon_pack_id IS NOT NULL",
                    [arch.ability],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                packs > 0,
                "{}: ability {} has no weapon pack",
                arch.name,
                arch.ability
            );
            let (ability, level): (u8, u8) = db
                .query_row(
                    "SELECT ability_id, ability_level FROM skills WHERE id = ?1",
                    [arch.starter_skill],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!((ability, level), (arch.ability, 1), "{}", arch.name);
        }
    }
}
