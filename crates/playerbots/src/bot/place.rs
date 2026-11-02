//! Which world a bot is in: the main world or an instance world (a dungeon).
//!
//! The server moves a character between worlds with `SCLoadInstance` (instance id, zone key,
//! arrival position) and answers a dungeon entry that takes a while with `SCProcessingInstance`
//! first. The bot answers a load with `CSInstanceLoaded`, after which the server shows it the
//! units and doodads of the new world. [`Place`] is the bot's memory of that: it is kept current
//! by [`BotState::apply`](super::state::BotState::apply) (the events) and by the bot task (the
//! name of the instance world, see [`Place::resolve`], and the answer, see [`Place::loaded`]).
//!
//! The main world is instance id [`MAIN_WORLD_INSTANCE`] (the server's `DEFAULT_INSTANCE_ID`),
//! also when the bot comes back from a dungeon. An instance is told apart from the main world by
//! that id alone; its [`Place::world_name`] is the directory name of the matching
//! `aa_world::travel::InstanceWorld` (`instance_cuttingwind_deadmine`), when the zone key of the
//! load is one of the instance worlds the static data knows.
//!
//! Inside an instance the baked terrain heights (the main world's) do not apply: motion gets
//! [`NoGround`] there ([`Place::ground`]), so the bot keeps the height it arrived at and the
//! navigation mesh (`World::nav_of(name)`) is what the movement feature walks on.

use aa_world::World;

use super::motion::Ground;

/// Instance id of the main world.
pub const MAIN_WORLD_INSTANCE: u32 = 0;

/// The instance the bot is in. Boxed in [`Place`]: bots spend nearly all their time in the main
/// world, where this costs a null pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Instance {
    /// Instance id of the load (never [`MAIN_WORLD_INSTANCE`]).
    id: u32,
    /// Zone key of the load.
    zone_key: u32,
    /// Name of the matching instance world, once [`Place::resolve`] found it.
    world: Option<Box<str>>,
    /// [`Place::resolve`] has looked (whatever it found).
    resolved: bool,
}

/// The world a bot is in. See the module docs.
#[derive(Debug, Default)]
pub struct Place {
    /// `None` in the main world.
    instance: Option<Box<Instance>>,
    /// Bot clock (ms) at which a load started, 0 when none is under way.
    loading_since_ms: u64,
}

impl Place {
    /// Whether the bot is in an instance world (not the main world).
    pub fn in_instance(&self) -> bool {
        self.instance.is_some()
    }

    /// The directory name of the instance world the bot is in (`instance_...`). `None` in the
    /// main world, before [`Place::resolve`] ran, and when the zone key matches no instance
    /// world the static data knows (then [`Place::in_instance`] is still true).
    pub fn world_name(&self) -> Option<&str> {
        self.instance.as_ref()?.world.as_deref()
    }

    /// The instance id of the last `SCLoadInstance` ([`MAIN_WORLD_INSTANCE`] in the main world).
    pub fn instance_id(&self) -> u32 {
        self.instance.as_ref().map_or(MAIN_WORLD_INSTANCE, |i| i.id)
    }

    /// The zone key of the last `SCLoadInstance` that moved the bot into an instance, 0 in the
    /// main world.
    pub fn zone_key(&self) -> u32 {
        self.instance.as_ref().map_or(0, |i| i.zone_key)
    }

    /// Whether the server is building an instance for the bot or the bot has not answered the
    /// load yet (between `SCProcessingInstance` or `SCLoadInstance` and `CSInstanceLoaded`).
    pub fn loading(&self) -> bool {
        self.loading_since_ms != 0
    }

    /// The bot clock at which the current load started. A load the server never finishes (an
    /// error instead of `SCLoadInstance`) stays "loading": time it out with this.
    pub fn loading_since_ms(&self) -> Option<u64> {
        self.loading().then_some(self.loading_since_ms)
    }

    /// `SCProcessingInstance`: the server is building the dungeon.
    pub fn on_processing(&mut self, now_ms: u64) {
        if !self.loading() {
            self.loading_since_ms = now_ms.max(1);
        }
    }

    /// `SCLoadInstance`: the bot is moved to world `instance_id` (zone `zone_key`). Returns
    /// whether that is another world than before, in which case the server shows the bot the new
    /// world's units afresh and the caller must drop its picture of the old one.
    pub fn on_load(&mut self, instance_id: u32, zone_key: u32, now_ms: u64) -> bool {
        let changed = instance_id != self.instance_id();
        if changed {
            self.instance = (instance_id != MAIN_WORLD_INSTANCE).then(|| {
                Box::new(Instance {
                    id: instance_id,
                    zone_key,
                    world: None,
                    resolved: false,
                })
            });
        } else if let Some(i) = self.instance.as_mut() {
            // (the same instance again, e.g. through a second entrance)
            if i.zone_key != zone_key {
                i.zone_key = zone_key;
                i.world = None;
                i.resolved = false;
            }
        }
        // The answer, `CSInstanceLoaded`, is still to be sent.
        self.loading_since_ms = now_ms.max(1);
        changed
    }

    /// The bot answered `CSInstanceLoaded`: the load is over.
    pub fn loaded(&mut self) {
        self.loading_since_ms = 0;
    }

    /// Whether [`Place::resolve`] has something to look up.
    pub fn needs_resolve(&self) -> bool {
        self.instance.as_ref().is_some_and(|i| !i.resolved)
    }

    /// Finds the instance world of the zone key in the static data (once per load).
    pub fn resolve(&mut self, world: &World) {
        self.resolve_with(|zone_key| {
            let key = u16::try_from(zone_key).ok()?;
            world
                .instance_world_by_zone_key(key)
                .map(|w| w.name.clone())
        });
    }

    /// [`Place::resolve`] with the lookup `find` (zone key to world name) given.
    pub fn resolve_with(&mut self, find: impl FnOnce(u32) -> Option<Box<str>>) {
        if let Some(i) = self.instance.as_mut().filter(|i| !i.resolved) {
            i.world = find(i.zone_key);
            i.resolved = true;
        }
    }

    /// The terrain motion stands on: the main world's baked heights, or none inside an instance
    /// (the baked grid is the main world's, so it would put the bot at the wrong height).
    pub fn ground<'a>(&self, world: &'a World) -> &'a dyn Ground {
        if self.in_instance() {
            &NoGround
        } else {
            world
        }
    }
}

/// A [`Ground`] that knows no terrain: motion keeps the height it has.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoGround;

impl Ground for NoGround {
    fn ground_z(&self, _x: f32, _y: f32) -> Option<f32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_bot_is_in_the_main_world() {
        let p = Place::default();
        assert!(!p.in_instance());
        assert_eq!(p.world_name(), None);
        assert_eq!((p.instance_id(), p.zone_key()), (MAIN_WORLD_INSTANCE, 0));
        assert!(!p.loading() && p.loading_since_ms().is_none());
        assert!(!p.needs_resolve());
    }

    #[test]
    fn a_dungeon_load_goes_processing_then_loading_then_loaded() {
        let mut p = Place::default();
        p.on_processing(1_000);
        assert!(p.loading());
        assert_eq!(p.loading_since_ms(), Some(1_000));
        assert!(
            !p.in_instance(),
            "still in the main world while it is built"
        );
        // SCLoadInstance moves the bot; the load is not over before it answered
        assert!(p.on_load(7, 301, 4_000));
        assert!(p.in_instance() && p.loading());
        assert_eq!((p.instance_id(), p.zone_key()), (7, 301));
        assert_eq!(p.loading_since_ms(), Some(4_000), "restarted by the load");
        p.loaded();
        assert!(p.in_instance() && !p.loading());
    }

    #[test]
    fn processing_twice_keeps_the_first_start() {
        let mut p = Place::default();
        p.on_processing(500);
        p.on_processing(900);
        assert_eq!(p.loading_since_ms(), Some(500));
        // a clock that reads 0 (tests) still counts as loading
        let mut q = Place::default();
        q.on_processing(0);
        assert!(q.loading());
    }

    #[test]
    fn the_world_name_comes_from_the_zone_key_once() {
        let mut p = Place::default();
        p.on_load(3, 90, 10);
        assert!(p.needs_resolve());
        assert_eq!(p.world_name(), None);
        let mut asked = Vec::new();
        p.resolve_with(|k| {
            asked.push(k);
            Some("instance_a".into())
        });
        assert_eq!(asked, [90]);
        assert_eq!(p.world_name(), Some("instance_a"));
        assert!(!p.needs_resolve());
        // asking again does not look again
        p.resolve_with(|_| panic!("looked twice"));
        // a zone key no instance world has: an instance without a name
        let mut q = Place::default();
        q.on_load(4, 91, 10);
        q.resolve_with(|_| None);
        assert!(q.in_instance());
        assert_eq!(q.world_name(), None);
        assert!(!q.needs_resolve());
    }

    #[test]
    fn leaving_for_the_main_world_clears_the_instance() {
        let mut p = Place::default();
        assert!(p.on_load(5, 90, 10));
        p.resolve_with(|_| Some("instance_a".into()));
        p.loaded();
        assert!(p.on_load(MAIN_WORLD_INSTANCE, 12, 20), "another world");
        assert!(!p.in_instance());
        assert_eq!(p.world_name(), None);
        assert_eq!(p.zone_key(), 0);
        assert!(p.loading(), "the answer is still to be sent");
        // main to main (a teleport the server calls a load) is no world change
        p.loaded();
        assert!(!p.on_load(MAIN_WORLD_INSTANCE, 13, 30));
    }

    #[test]
    fn the_same_instance_again_is_no_world_change() {
        let mut p = Place::default();
        assert!(p.on_load(5, 90, 10));
        p.resolve_with(|_| Some("instance_a".into()));
        p.loaded();
        assert!(!p.on_load(5, 90, 20));
        assert_eq!(p.world_name(), Some("instance_a"), "the name is kept");
        // another zone of the same world id is looked up again
        assert!(!p.on_load(5, 95, 30));
        assert!(p.needs_resolve() && p.world_name().is_none());
        assert_eq!(p.zone_key(), 95);
    }

    #[test]
    fn instances_have_no_ground() {
        assert_eq!(NoGround.ground_z(1.0, 2.0), None);
    }

    #[test]
    fn place_stays_two_words() {
        assert!(std::mem::size_of::<Place>() <= 16);
    }
}
