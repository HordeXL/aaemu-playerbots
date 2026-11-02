//! The persistent roster: which accounts and characters the bots are.
//!
//! One JSON file (`bots.roster`) lists every bot with its account, character name, race, gender,
//! archetype, temperament and, once the character exists on the server, its character id. It is
//! written atomically (temp file + rename) whenever something changes (a character was created,
//! a name was rejected and regenerated), so a restart finds the same bots again.

use std::path::Path;

use anyhow::{ensure, Context};
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};

use crate::archetype::Archetype;
use crate::names;

/// File format version of the roster.
pub const ROSTER_VERSION: u32 = 1;

/// How much a bot cares about each kind of activity (0..=1). The brain scales the goal weights
/// of the config with these.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Temperament {
    /// Likes quests.
    pub quester: f32,
    /// Likes fights and duels.
    pub duelist: f32,
    /// Likes trading with other players.
    pub trader: f32,
    /// Likes trade runs (crafting and selling packs).
    pub merchant: f32,
    /// Likes staying around, resting and housing.
    pub homebody: f32,
}

impl Temperament {
    /// All traits at the neutral 0.5.
    pub const NEUTRAL: Temperament = Temperament {
        quester: 0.5,
        duelist: 0.5,
        trader: 0.5,
        merchant: 0.5,
        homebody: 0.5,
    };

    /// A random temperament: every trait 0.1..0.6 and one dominant trait 0.8..1.0, rounded to
    /// two decimals so the roster file stays readable.
    pub fn random(rng: &mut fastrand::Rng) -> Temperament {
        let mut v = [0.0f32; 5];
        for x in &mut v {
            *x = 0.1 + rng.f32() * 0.5;
        }
        v[rng.usize(..5)] = 0.8 + rng.f32() * 0.2;
        let r = |x: f32| (x * 100.0).round() / 100.0;
        Temperament {
            quester: r(v[0]),
            duelist: r(v[1]),
            trader: r(v[2]),
            merchant: r(v[3]),
            homebody: r(v[4]),
        }
    }

    /// The temperament pulled towards neutral: `spread` 1 keeps it, 0 makes it neutral.
    pub fn with_spread(self, spread: f32) -> Temperament {
        let s = spread.clamp(0.0, 1.0);
        let f = |x: f32| (0.5 + (x - 0.5) * s).clamp(0.05, 1.0);
        Temperament {
            quester: f(self.quester),
            duelist: f(self.duelist),
            trader: f(self.trader),
            merchant: f(self.merchant),
            homebody: f(self.homebody),
        }
    }

    /// The most pronounced trait, for display.
    pub fn dominant(&self) -> &'static str {
        let t = [
            (self.quester, "quester"),
            (self.duelist, "duelist"),
            (self.trader, "trader"),
            (self.merchant, "merchant"),
            (self.homebody, "homebody"),
        ];
        t.iter()
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map_or("balanced", |x| x.1)
    }
}

/// One bot of the roster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RosterEntry {
    /// Bot id (1-based); the web UI and the account name use it.
    pub id: u32,
    /// Login name (`<prefix><id>`).
    pub account: String,
    /// Character name; changes when the server rejects it.
    pub char_name: String,
    /// Race id (1 = Nuian).
    pub race: u8,
    /// Gender id (1 = male, 2 = female).
    pub gender: u8,
    /// Name of the [`Archetype`].
    pub archetype: String,
    /// Persona.
    pub temperament: Temperament,
    /// Character id on the server, known once the character exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_id: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct RosterFile {
    version: u32,
    bots: Vec<RosterEntry>,
}

/// What a new roster entry may be made of.
pub struct RosterSpec<'a> {
    /// Account name prefix.
    pub account_prefix: &'a str,
    /// Allowed races.
    pub races: &'a [u8],
    /// Whether the server lets a player create this `(race, gender)` (`World::creatable`).
    pub creatable: &'a dyn Fn(u8, u8) -> bool,
    /// The archetypes to pick from (by weight).
    pub archetypes: &'a [std::sync::Arc<Archetype>],
}

/// The bots, in id order.
#[derive(Debug, Default)]
pub struct Roster {
    entries: Vec<RosterEntry>,
    /// Lower-cased character names in use.
    names: FxHashSet<String>,
}

impl Roster {
    /// An empty roster.
    pub fn new() -> Roster {
        Roster::default()
    }

    /// Reads a roster file; a missing file is an empty roster.
    pub fn load(path: &Path) -> anyhow::Result<Roster> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Roster::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Roster::from_json(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Parses the JSON text of a roster file.
    pub fn from_json(text: &str) -> anyhow::Result<Roster> {
        let file: RosterFile = serde_json::from_str(text)?;
        ensure!(
            file.version == ROSTER_VERSION,
            "roster version {} is not supported (expected {ROSTER_VERSION})",
            file.version
        );
        let mut roster = Roster::new();
        for e in file.bots {
            ensure!(e.id != 0, "roster entry {:?} has id 0", e.char_name);
            ensure!(
                roster.entries.iter().all(|o| o.id != e.id),
                "roster id {} appears twice",
                e.id
            );
            ensure!(
                roster.names.insert(e.char_name.to_ascii_lowercase()),
                "roster character name {:?} appears twice",
                e.char_name
            );
            roster.entries.push(e);
        }
        roster.entries.sort_by_key(|e| e.id);
        Ok(roster)
    }

    /// The roster file text.
    pub fn to_json(&self) -> String {
        let file = RosterFile {
            version: ROSTER_VERSION,
            bots: self.entries.clone(),
        };
        // Serialising plain data cannot fail; an empty file would be detected on the next load.
        serde_json::to_string_pretty(&file).unwrap_or_default()
    }

    /// Writes the roster file atomically: a temp file next to it, then a rename.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        write_atomic(path, &self.to_json())
    }

    /// Number of bots.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the roster has no bots.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries in id order.
    pub fn entries(&self) -> &[RosterEntry] {
        &self.entries
    }

    /// The entry of bot `id`.
    pub fn get(&self, id: u32) -> Option<&RosterEntry> {
        self.entries
            .binary_search_by_key(&id, |e| e.id)
            .ok()
            .map(|i| &self.entries[i])
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut RosterEntry> {
        self.entries
            .binary_search_by_key(&id, |e| e.id)
            .ok()
            .map(|i| &mut self.entries[i])
    }

    /// Adds bots until there are `count` of them; returns how many were added.
    ///
    /// New bots take the next free ids, a random creatable race and gender among `spec.races`,
    /// an archetype by weight, a fresh temperament and a unique pronounceable name.
    pub fn ensure(
        &mut self,
        count: usize,
        spec: &RosterSpec<'_>,
        rng: &mut fastrand::Rng,
    ) -> anyhow::Result<usize> {
        let combos: Vec<(u8, u8)> = spec
            .races
            .iter()
            .flat_map(|&r| [1u8, 2].map(|g| (r, g)))
            .filter(|&(r, g)| (spec.creatable)(r, g))
            .collect();
        let mut added = 0;
        while self.entries.len() < count {
            ensure!(
                !combos.is_empty(),
                "none of the configured races can be created on this server (bots.races = {:?})",
                spec.races
            );
            ensure!(!spec.archetypes.is_empty(), "no archetypes");
            let id = self.entries.last().map_or(0, |e| e.id) + 1;
            let (race, gender) = combos[rng.usize(..combos.len())];
            let arch = pick_archetype(spec.archetypes, rng);
            let name = self.fresh_name(rng);
            self.entries.push(RosterEntry {
                id,
                account: format!("{}{id}", spec.account_prefix),
                char_name: name,
                race,
                gender,
                archetype: arch.name.clone(),
                temperament: Temperament::random(rng),
                char_id: None,
            });
            added += 1;
        }
        Ok(added)
    }

    /// A name no roster entry uses; reserves it.
    fn fresh_name(&mut self, rng: &mut fastrand::Rng) -> String {
        loop {
            let name = names::generate(rng);
            if self.names.insert(name.to_ascii_lowercase()) {
                return name;
            }
        }
    }

    /// Replaces the character name of bot `id` with a fresh one (the server rejected the old
    /// one); returns the new name.
    pub fn regenerate_name(&mut self, id: u32, rng: &mut fastrand::Rng) -> Option<String> {
        let new = self.fresh_name(rng);
        let entry = self.get_mut(id)?;
        let old = std::mem::replace(&mut entry.char_name, new.clone());
        self.names.remove(&old.to_ascii_lowercase());
        Some(new)
    }

    /// Records the server's name for bot `id` (a character found on the account under another
    /// name than the roster's). Returns false when another bot uses that name.
    pub fn adopt_name(&mut self, id: u32, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        let Some(entry) = self.get_mut(id) else {
            return false;
        };
        if entry.char_name.eq_ignore_ascii_case(name) {
            entry.char_name = name.to_string();
            return true;
        }
        let old = entry.char_name.to_ascii_lowercase();
        if self.names.contains(&lower) {
            return false;
        }
        if let Some(e) = self.get_mut(id) {
            e.char_name = name.to_string();
        }
        self.names.remove(&old);
        self.names.insert(lower);
        true
    }

    /// Records the character id of bot `id`; true when something changed.
    pub fn set_char_id(&mut self, id: u32, char_id: u32) -> bool {
        match self.get_mut(id) {
            Some(e) if e.char_id != Some(char_id) => {
                e.char_id = Some(char_id);
                true
            }
            _ => false,
        }
    }
}

fn pick_archetype<'a>(
    archetypes: &'a [std::sync::Arc<Archetype>],
    rng: &mut fastrand::Rng,
) -> &'a Archetype {
    let total: u32 = archetypes.iter().map(|a| a.weight.max(1)).sum();
    let mut roll = rng.u32(..total);
    for a in archetypes {
        let w = a.weight.max(1);
        if roll < w {
            return a;
        }
        roll -= w;
    }
    &archetypes[0]
}

/// Writes `text` to `path` through a temp file in the same directory and a rename, so a crash
/// never leaves a half-written file.
pub fn write_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// The roster shared by the supervisor and the bot tasks: every change is written to disk.
#[derive(Debug)]
pub struct SharedRoster {
    inner: parking_lot::Mutex<Roster>,
    path: std::path::PathBuf,
}

impl SharedRoster {
    /// Wraps `roster`, saved to `path` after each change.
    pub fn new(roster: Roster, path: std::path::PathBuf) -> SharedRoster {
        SharedRoster {
            inner: parking_lot::Mutex::new(roster),
            path,
        }
    }

    /// Runs `f` on the roster; when it returns `true` the roster is saved. A failed save is
    /// logged, never fatal (the next change tries again).
    pub fn update<R>(&self, f: impl FnOnce(&mut Roster) -> (R, bool)) -> R {
        let mut g = self.inner.lock();
        let (r, changed) = f(&mut g);
        if changed {
            if let Err(e) = g.save(&self.path) {
                tracing::warn!("saving the roster: {e:#}");
            }
        }
        r
    }

    /// Runs `f` on the roster without saving.
    pub fn read<R>(&self, f: impl FnOnce(&Roster) -> R) -> R {
        f(&self.inner.lock())
    }

    /// A copy of the entry of bot `id`.
    pub fn entry(&self, id: u32) -> Option<RosterEntry> {
        self.inner.lock().get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archetype::Archetypes;
    use crate::testutil::temp_dir;

    fn spec<'a>(
        arch: &'a Archetypes,
        races: &'a [u8],
        creatable: &'a dyn Fn(u8, u8) -> bool,
    ) -> RosterSpec<'a> {
        RosterSpec {
            account_prefix: "pb",
            races,
            creatable,
            archetypes: arch.all(),
        }
    }

    #[test]
    fn ensure_creates_unique_valid_entries() {
        let arch = Archetypes::builtin().unwrap();
        let ok = |r: u8, g: u8| !(r == 4 && g == 2);
        let races = [1u8, 4, 5, 6];
        let mut roster = Roster::new();
        let mut rng = fastrand::Rng::with_seed(1);
        assert_eq!(
            roster
                .ensure(300, &spec(&arch, &races, &ok), &mut rng)
                .unwrap(),
            300
        );
        assert_eq!(roster.len(), 300);
        let mut names = FxHashSet::default();
        for (i, e) in roster.entries().iter().enumerate() {
            assert_eq!(e.id as usize, i + 1);
            assert_eq!(e.account, format!("pb{}", e.id));
            assert!(crate::names::is_acceptable(&e.char_name), "{}", e.char_name);
            assert!(
                names.insert(e.char_name.to_ascii_lowercase()),
                "duplicate name"
            );
            assert!(races.contains(&e.race));
            assert!(ok(e.race, e.gender), "{} {}", e.race, e.gender);
            assert!(arch.get(&e.archetype).is_some());
            assert!(e.char_id.is_none());
        }
        // asking for fewer or the same number adds nothing
        assert_eq!(
            roster
                .ensure(300, &spec(&arch, &races, &ok), &mut rng)
                .unwrap(),
            0
        );
        assert_eq!(
            roster
                .ensure(10, &spec(&arch, &races, &ok), &mut rng)
                .unwrap(),
            0
        );
        // all archetypes get used
        let used: FxHashSet<&str> = roster
            .entries()
            .iter()
            .map(|e| e.archetype.as_str())
            .collect();
        assert_eq!(used.len(), arch.all().len());
    }

    #[test]
    fn ensure_fails_when_no_race_can_be_created() {
        let arch = Archetypes::builtin().unwrap();
        let none = |_: u8, _: u8| false;
        let mut roster = Roster::new();
        let mut rng = fastrand::Rng::with_seed(1);
        assert!(roster
            .ensure(1, &spec(&arch, &[1], &none), &mut rng)
            .is_err());
    }

    #[test]
    fn json_round_trip_and_atomic_save() {
        let arch = Archetypes::builtin().unwrap();
        let any = |_: u8, _: u8| true;
        let mut roster = Roster::new();
        let mut rng = fastrand::Rng::with_seed(3);
        roster
            .ensure(5, &spec(&arch, &[1, 4], &any), &mut rng)
            .unwrap();
        assert!(roster.set_char_id(2, 777));
        assert!(!roster.set_char_id(2, 777));
        assert!(!roster.set_char_id(99, 1));

        let dir = temp_dir("roster-save");
        let path = dir.join("sub").join("roster.json");
        roster.save(&path).unwrap();
        assert!(!dir.join("sub").join("roster.json.tmp").exists());
        let loaded = Roster::load(&path).unwrap();
        assert_eq!(loaded.entries(), roster.entries());
        assert_eq!(loaded.get(2).unwrap().char_id, Some(777));
        assert_eq!(loaded.get(1).unwrap().char_id, None);

        // saving again replaces the file
        roster.set_char_id(1, 5);
        roster.save(&path).unwrap();
        assert_eq!(
            Roster::load(&path).unwrap().get(1).unwrap().char_id,
            Some(5)
        );
    }

    #[test]
    fn missing_file_is_empty_and_bad_files_are_errors() {
        let dir = temp_dir("roster-load");
        assert!(Roster::load(&dir.join("nope.json")).unwrap().is_empty());
        std::fs::write(dir.join("bad.json"), "{ not json").unwrap();
        assert!(Roster::load(&dir.join("bad.json")).is_err());
        std::fs::write(dir.join("v.json"), r#"{"version":9,"bots":[]}"#).unwrap();
        assert!(Roster::load(&dir.join("v.json")).is_err());
        let dup = r#"{"version":1,"bots":[
            {"id":1,"account":"pb1","char_name":"Aaa","race":1,"gender":1,"archetype":"X","temperament":{"quester":0.5,"duelist":0.5,"trader":0.5,"merchant":0.5,"homebody":0.5}},
            {"id":2,"account":"pb2","char_name":"aaa","race":1,"gender":1,"archetype":"X","temperament":{"quester":0.5,"duelist":0.5,"trader":0.5,"merchant":0.5,"homebody":0.5}}]}"#;
        assert!(Roster::from_json(dup).is_err());
    }

    #[test]
    fn rejected_names_are_regenerated_and_stay_unique() {
        let arch = Archetypes::builtin().unwrap();
        let any = |_: u8, _: u8| true;
        let mut roster = Roster::new();
        let mut rng = fastrand::Rng::with_seed(5);
        roster
            .ensure(3, &spec(&arch, &[1], &any), &mut rng)
            .unwrap();
        let old = roster.get(2).unwrap().char_name.clone();
        let new = roster.regenerate_name(2, &mut rng).unwrap();
        assert_ne!(old, new);
        assert_eq!(roster.get(2).unwrap().char_name, new);
        assert!(roster.regenerate_name(42, &mut rng).is_none());
        // the old name is free again, the new one is taken
        assert!(roster.adopt_name(1, &old));
        assert!(!roster.adopt_name(3, &new));
        assert_eq!(roster.get(1).unwrap().char_name, old);
    }

    #[test]
    fn temperament_spread() {
        let mut rng = fastrand::Rng::with_seed(11);
        for _ in 0..50 {
            let t = Temperament::random(&mut rng);
            for v in [t.quester, t.duelist, t.trader, t.merchant, t.homebody] {
                assert!((0.1..=1.0).contains(&v), "{v}");
            }
            assert!([t.quester, t.duelist, t.trader, t.merchant, t.homebody]
                .iter()
                .any(|v| *v >= 0.8));
            assert_eq!(t.with_spread(0.0), Temperament::NEUTRAL);
            assert!((t.with_spread(1.0).quester - t.quester.clamp(0.05, 1.0)).abs() < 1e-6);
        }
    }
}
