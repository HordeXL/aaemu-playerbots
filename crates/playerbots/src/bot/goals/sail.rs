//! Sail: summon a boat, sail it across the sea and come ashore.
//!
//! A bot gets a boat summon scroll (the cheapest boat's, added with the GM `/item add self` line
//! when GM commands are allowed; without them a bot sails only when its bag already holds one),
//! picks a trip on the sea map (a crossing of 1 to 3 km to another beach, or a loop out to sea and
//! back; see [`trip`]), walks to the launch beach, faces the water and casts the scroll's skill on
//! itself. The server puts the boat on the water ahead; the bot swims to it, binds the driver's seat,
//! probes which way the wheel turns, sails the planned route with a heading controller
//! ([`crate::bot::vehicle`]), stops at the landing, leaves the boat, despawns it and swims ashore.
//! Lakes and rivers are out of scope: the map only knows the sea.
//!
//! The state machine is [`machine`] (pure: an `Input` in, `Act`s out, tested against a scripted
//! server); this file connects it to the bot: it builds the input from [`Ctx`], applies the acts
//! (packets, movement, GM lines, issue reports) and holds the scoring. Whatever ends the goal (done,
//! failed, interrupted by a fight, a rest or the operator) passes through [`Sail::stop`], which
//! leaves the seat and despawns the boat, so no boat is left behind (the server also removes a boat
//! whose owner logs out or changes the instance).
//!
//! Boats are small and turn slowly (a rowboat 4.35 degrees per second), so the goal asks the bot task
//! for fast ticks while the bot is on a boat (`VehicleState::fast_tick`).

mod machine;
pub mod trip;

#[cfg(test)]
mod tests;

use std::cell::RefCell;

use aa_proto::cs;
use aa_world::water::{find_shore, Disc, ShoreSpot, WaterMap, NAV_FLOOR_M};
use aa_world::World;
use pb_web::Activity;

use crate::bot::ctx::Ctx;
use crate::bot::motion::{MotionStatus, Pace};
use crate::bot::state::BotState;
use crate::bot::vehicle::helm::Hull;
use crate::config::BehaviorCfg;
use crate::roster::Temperament;

use self::machine::{Act, Input, Machine, Ops, Pick, MIN_SPEED};
use self::trip::{Plan, PlanError, TripCfg};
use super::{FailReason, GoalStatus};

/// A bot sails at most once in this long (bot clock milliseconds).
const COOLDOWN_MS: u64 = 30 * 60_000;
/// A bot that has been online for less than this does not sail yet.
const SETTLE_MS: u64 = 3 * 60_000;
/// Hit points (percent) below which a bot does not set out.
const MIN_HP_PCT: u8 = 60;
/// A beach is "near" when the terrain dips under the sea at one of these distances.
const COAST_PROBES_M: [f32; 3] = [100.0, 250.0, 450.0];

/// Summoning a boat and sailing it. See the module docs.
#[derive(Debug, Default)]
pub struct Sail {
    trip: Option<Box<Machine>>,
}

impl Sail {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, ctx: &mut Ctx<'_>) {
        ctx.state.vehicle.sailor_mut().active = true;
        self.trip = Some(Box::new(Machine::new(ctx.now_ms, ctx.state.me.pos)));
    }

    /// One tick.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        let Some(machine) = self.trip.as_mut() else {
            return GoalStatus::Failed(FailReason::Other("not started"));
        };
        if !ctx.cfg.behavior.sailing {
            return GoalStatus::Failed(FailReason::Other("sailing is switched off"));
        }
        let world = ctx.world;
        let Some(map) = world.water() else {
            return GoalStatus::Failed(FailReason::Other("no sea map"));
        };
        let Some(pick) = pick_scroll(world, |item| ctx.state.item_count(item)) else {
            return GoalStatus::Failed(FailReason::Other("no boat scroll in the data"));
        };
        let mut acts = Vec::new();
        let status = {
            let ops = RealOps {
                world,
                map,
                faction: ctx.state.me.faction,
                rng: RefCell::new(&mut *ctx.rng),
            };
            let io = input(
                ctx.state,
                ctx.now_ms,
                ctx.cfg.bots.gm_bootstrap,
                ctx.motion.status(),
                &pick,
                &ops,
            );
            machine.step(&io, &mut acts)
        };
        apply(ctx, acts);
        status
    }

    /// Called when the goal ends, fails or is interrupted: leaves the seat and despawns the boat.
    pub fn stop(&mut self, ctx: &mut Ctx<'_>) {
        if let Some(mut machine) = self.trip.take() {
            let world = ctx.world;
            if let (Some(map), Some(pick)) = (
                world.water(),
                pick_scroll(world, |item| ctx.state.item_count(item)),
            ) {
                let mut acts = Vec::new();
                {
                    let ops = RealOps {
                        world,
                        map,
                        faction: ctx.state.me.faction,
                        rng: RefCell::new(&mut *ctx.rng),
                    };
                    let io = input(
                        ctx.state,
                        ctx.now_ms,
                        ctx.cfg.bots.gm_bootstrap,
                        ctx.motion.status(),
                        &pick,
                        &ops,
                    );
                    machine.stop(&io, &mut acts);
                }
                apply(ctx, acts);
            }
            let now = ctx.now_ms;
            let sailor = ctx.state.vehicle.sailor_mut();
            sailor.active = false;
            sailor.last_trip_end_ms = now.max(1);
            sailor.trips = sailor.trips.saturating_add(u16::from(machine.sailed));
        }
        ctx.motion.stop();
    }

    /// Whether the goal drives the combat controller itself (see [`super::Goal::owns_combat`]):
    /// on the boat it must not be paused by a fight.
    pub fn owns_combat(&self) -> bool {
        self.trip.as_ref().is_some_and(|m| m.owns_combat())
    }

    /// The activity the web UI shows while the goal runs.
    pub fn activity(&self, moving: bool, fighting: bool) -> Activity {
        if fighting {
            Activity::Fighting
        } else if moving || self.owns_combat() {
            Activity::Traveling
        } else {
            Activity::Idle
        }
    }

    /// Appends a one-line description of what the goal is doing to `out`.
    pub fn describe(&self, out: &mut String) {
        out.push_str("sail");
        if let Some(m) = &self.trip {
            out.push_str(": ");
            m.describe(out);
        }
    }
}

/// The goal's weight before the situation is looked at: 0 (the goal scores itself, see [`score`],
/// from the temperament and the situation).
pub fn base_weight(_behavior: &BehaviorCfg, _temper: &Temperament) -> f32 {
    0.0
}

/// The weight of a sailing trip for a bot with this temperament: explorer and merchant types like
/// the sea, homebodies do not. About a fifth of the quest weight for a neutral bot.
pub fn weight(temper: &Temperament) -> f32 {
    0.2 * (0.3 + 0.7 * (1.0 - temper.homebody) + 0.7 * temper.merchant)
}

/// The score of the goal for this tick: 0 unless sailing is on, the bot is settled, healthy, in the
/// main world, off its cooldown, near a coast and able to get a scroll; the temperament's weight
/// otherwise. Cheap (a few terrain probes): the brain asks whenever it scores the goals.
pub fn score(ctx: &Ctx<'_>, _base: f32) -> f32 {
    if !ctx.cfg.behavior.sailing
        || ctx.state.place.in_instance()
        || ctx.now_ms < SETTLE_MS
        || ctx.state.me.hp_pct() < MIN_HP_PCT
    {
        return 0.0;
    }
    if let Some(s) = ctx.state.vehicle.sailor() {
        if s.last_trip_end_ms != 0 && ctx.now_ms < s.last_trip_end_ms + COOLDOWN_MS {
            return 0.0;
        }
    }
    let world = ctx.world;
    let boats = &world.travel().boats;
    let has_scroll = boats
        .scrolls()
        .iter()
        .any(|s| ctx.state.item_count(s.item) > 0);
    if !(ctx.gm_allowed() || has_scroll) || boats.cheapest_scroll(MIN_SPEED).is_none() {
        return 0.0;
    }
    if !coast_near(world, ctx.state.me.pos) {
        return 0.0;
    }
    weight(&ctx.persona.temper)
}

/// Whether the terrain dips under the sea within a few hundred metres of `pos` (a coast, or the
/// bot is in the water).
fn coast_near(world: &World, pos: [f32; 3]) -> bool {
    if world.baked().terrain.is_none() {
        return false;
    }
    COAST_PROBES_M.iter().any(|&r| {
        (0..12).any(|i| {
            let a = i as f32 * std::f32::consts::TAU / 12.0;
            world
                .ground_z(pos[0] + a.cos() * r, pos[1] + a.sin() * r)
                .is_some_and(|z| z > 0.0 && z < NAV_FLOOR_M)
        })
    })
}

/// The scroll to use: one the bag holds (cheapest first), else the cheapest boat's scroll that has
/// a driver seat and is fast enough.
fn pick_scroll(world: &World, bag_count: impl Fn(u32) -> i64) -> Option<Pick> {
    let boats = &world.travel().boats;
    let fits = |s: &&aa_world::travel::BoatScroll| {
        boats
            .slave_template(s.slave)
            .is_some_and(|b| b.has_driver_seat() && b.top_speed() >= MIN_SPEED)
    };
    let scroll = boats
        .scrolls()
        .iter()
        .filter(fits)
        .find(|s| bag_count(s.item) > 0)
        .or_else(|| boats.cheapest_scroll(MIN_SPEED))?;
    let boat = boats.slave_template(scroll.slave)?;
    Some(Pick {
        item: scroll.item,
        skill: scroll.skill.id,
        cast_ms: scroll.skill.cast_ms,
        hull: Hull::of(boat),
        top_speed: boat.top_speed(),
    })
}

/// What the bot knows this tick, for the state machine.
fn input<'a>(
    st: &'a BotState,
    now_ms: u64,
    gm: bool,
    motion: MotionStatus,
    pick: &Pick,
    ops: &'a dyn Ops,
) -> Input<'a> {
    let sailor = st.vehicle.sailor();
    let boat_unit = sailor
        .and_then(|s| s.boat.as_ref())
        .and_then(|b| st.units.get(&b.obj))
        .map(|u| u.pos);
    Input {
        now_ms,
        char_id: st.me.char_id,
        pos: st.me.pos,
        gm,
        pick: *pick,
        held: st.find_item(pick.item).map(|(_, i)| (i.id, i.template)),
        sailor,
        boat_unit,
        errors: st.errors.as_slice(),
        motion,
        ops,
    }
}

/// Carries out what the machine asked for.
fn apply(ctx: &mut Ctx<'_>, acts: Vec<Act>) {
    let me = ctx.state.me.obj;
    for act in acts {
        match act {
            Act::Say(text) => ctx.log.say(text),
            Act::Gm(line) => {
                ctx.gm(&line);
            }
            Act::Travel { x, y, radius } => ctx.travel(x, y, radius, Pace::Run),
            Act::Face { x, y } => ctx.motion.face(x, y),
            Act::StopMotion => ctx.motion.stop(),
            Act::Sync(pos) => ctx.motion.teleported(pos),
            Act::Travelling => ctx.combat.note_travelling(ctx.now_ms),
            Act::Pin => {
                if let Some(pin) = ctx.motion.pin(ctx.now_ms, false) {
                    ctx.send(pin);
                }
            }
            Act::Summon {
                skill,
                item,
                template,
            } => ctx.send(cs::start_skill_item(skill, me, item, template, me)),
            Act::Bind(tl) => ctx.send(cs::bind_slave(tl)),
            Act::Discard(tl) => ctx.send(cs::discard_slave(tl)),
            Act::Despawn(obj) => ctx.send(cs::despawn_slave(obj)),
            Act::Helm { ship, cmd } => ctx.send(cs::ship_request(
                ship,
                ctx.now_ms as u32,
                cmd.throttle,
                cmd.steering,
            )),
            Act::Report(report) => ctx.report_issue(*report),
            Act::Find(finding) => ctx.find(finding),
        }
    }
}

/// The machine's view of the world: the sea map, the terrain and the monsters.
struct RealOps<'a> {
    world: &'a World,
    map: &'a WaterMap,
    faction: u32,
    rng: RefCell<&'a mut fastrand::Rng>,
}

impl Ops for RealOps<'_> {
    fn danger(&self, from: (f32, f32)) -> Vec<Disc> {
        trip::danger_zones(self.world, self.map, self.faction, from, 5000.0)
    }

    fn plan(&self, from: (f32, f32), avoid: &[Disc]) -> Result<Plan, PlanError> {
        let ground = |x: f32, y: f32| self.world.ground_z(x, y);
        trip::plan_trip(
            self.map,
            &ground,
            &mut self.rng.borrow_mut(),
            from,
            &TripCfg::default(),
            avoid,
        )
    }

    fn route(&self, start: (f32, f32), plan: &Plan, avoid: &[Disc]) -> Option<Vec<(f32, f32)>> {
        trip::sail_route(self.map, start, plan, avoid)
    }

    fn shore_near(&self, from: (f32, f32), max_r: f32) -> Option<ShoreSpot> {
        let ground = |x: f32, y: f32| self.world.ground_z(x, y);
        find_shore(self.map, &ground, from, max_r, &[])
    }

    fn ground(&self, x: f32, y: f32) -> Option<f32> {
        self.world.ground_z(x, y)
    }
}
