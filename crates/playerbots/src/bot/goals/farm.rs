//! Farm: buy seeds, plant, wait, harvest, and grow what the server is short of.
//!
//! A bot that farms keeps plots on the land of its own houses and on the public farms (see
//! [`crate::bot::farm`]). The goal does one of two jobs per run:
//!
//! * **harvest** when a plot is due (ripe, or its growth time is over): walk to the crop, cast the
//!   harvest skill, wait for the crop to change, plant the seeds the harvest gave back on the
//!   same spot;
//! * **plant** otherwise, when the bot has room for plots: choose a crop ([`plan::choose`]: the
//!   items the operator or the population director wishes for first, then the scarce ones, else
//!   what pays), buy its seeds from the nearest vendor (or, with `bots.gm_bootstrap` and no
//!   vendor or money, mint them), walk to a free spot on its own land or a public farm and plant.
//!
//! The state machine is [`machine`] (pure, tested against a scripted server); this file builds its
//! input from [`Ctx`], applies the acts (packets, movement, GM lines), and holds the scoring.
//!
//! # Honesty
//!
//! A harvest is the bot's own to sell only when the seeds were bought: the machine opens the
//! ledger's harvest window with the plot's flag ([`FarmMem::harvest_honest`]) for the length of
//! a harvest cast, and a plot is honest when the ledger counted the seed it planted as clean.
//! GM-minted seeds, and plots found again after a reconnect, are not. See
//! [`crate::bot::market::ledger`].
//!
//! # Scoring
//!
//! [`score`] is 0 unless `farm.enabled`, the bot is settled, healthy and in the main world, and
//! there is something to do: a due plot (three times the goal's [`weight`]), or room for plots, a
//! crop the bot can get seeds for and a place to plant it (the weight, up to two and a half times
//! that when the crop is wished for or scarce). The population director's `farm` bias and its
//! wish nudge apply on top (`brain::scores_now`).

pub mod machine;

#[cfg(test)]
mod tests;

use std::cell::RefCell;

use aa_proto::cs::{self, BuyItem, SHOP_MONEY};
use aa_world::{Crop, FarmKind, World};
use pb_web::Activity;

use crate::bot::ctx::Ctx;
use crate::bot::farm::plan::{self, Env, Plan, Vendor};
use crate::bot::farm::site::{self, Site};
use crate::bot::farm::FarmMem;
use crate::bot::goals::housing::garden_radius;
use crate::bot::motion::Pace;
use crate::bot::state::{dist2d, BotState, UnitKind};
use crate::config::BehaviorCfg;
use crate::contest::plot_key;
use crate::hub::Hub;
use crate::population::economy::ScarcityView;
use crate::roster::Temperament;

use self::machine::{Act, Input, Machine, Ops};
use super::{FailReason, GoalStatus};

/// A bot that has been online for less than this does not farm yet.
const SETTLE_MS: u64 = 90_000;
/// Hit points (percent) below which a bot does not set out.
const MIN_HP_PCT: u8 = 60;
/// A bot plants again at most this often (a harvest is not held back).
const RUN_COOLDOWN_MS: u64 = 5 * 60_000;
/// The score of a due plot, as a multiple of the goal's weight.
const HARVEST_BOOST: f32 = 3.0;
/// How much of the need of the best crop is added to the weight, per point (up to
/// [`NEED_CAP`]).
const NEED_GAIN: f32 = 0.5;
/// The most need that counts towards the weight.
const NEED_CAP: f32 = 3.0;

/// Buying seeds, planting and harvesting. See the module docs.
#[derive(Debug, Default)]
pub struct Farm {
    run: Option<Box<Machine>>,
}

impl Farm {
    /// A fresh goal (the brain starts it with [`Goal::new`](super::Goal::new)).
    pub fn new() -> Farm {
        Farm::default()
    }

    /// A fresh goal started with `args` (`GoalArgs::wild`: plant on unprotected land, `item`: the
    /// seed to grow). The wild-farm feature routes them; until then they are ignored.
    pub fn with_args(_args: Option<Box<crate::bot::cmd::GoalArgs>>) -> Farm {
        Farm::new()
    }

    /// Called once when the brain picks the goal: decides between harvesting and planting.
    pub fn start(&mut self, ctx: &mut Ctx<'_>) {
        self.run = choose_job(ctx).map(Box::new);
        match &self.run {
            Some(m) => ctx.log.say(format!(
                "goal: farm ({})",
                if m.plants() { "plant" } else { "harvest" }
            )),
            None => ctx.log.say("goal: farm (nothing to do)"),
        }
    }

    /// One tick.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        let Some(machine) = self.run.as_mut() else {
            return GoalStatus::Failed(FailReason::NoTarget);
        };
        if !ctx.cfg.farm.enabled {
            return GoalStatus::Failed(FailReason::Other("farming is switched off"));
        }
        let mut mem = ctx.state.market.take_farm();
        let harvested_before = mem.harvested;
        let mut acts = Vec::new();
        let status = {
            let world = ctx.world;
            let ops = RealOps {
                world,
                hub: ctx.hub,
                bot: ctx.persona.id,
                now_ms: ctx.now_ms,
                fallback_z: ctx.state.me.pos[2],
                rng: RefCell::new(&mut *ctx.rng),
            };
            let st: &BotState = ctx.state;
            let count = |t: u32| st.item_count(t);
            let item = |t: u32| st.find_item(t).map(|(_, i)| (i.id, i64::from(i.count)));
            let clean = |t: u32| st.market.clean_count(t);
            let pos = st.me.pos;
            let merchant = |npc: u32| nearest_merchant(st, npc, pos);
            let io = Input {
                now_ms: ctx.now_ms,
                pos,
                me_obj: st.me.obj,
                gm: ctx.cfg.bots.gm_bootstrap,
                replant: ctx.cfg.farm.replant,
                money: st.me.money,
                labor: st.me.labor,
                motion: ctx.motion.status(),
                errors: st.errors.as_slice(),
                count: &count,
                item: &item,
                clean: &clean,
                merchant: &merchant,
                max_trip_m: ctx.cfg.farm.max_trip_m,
                ops: &ops,
            };
            machine.step(&io, &mut mem, &mut acts)
        };
        let last_crop = (mem.harvested != harvested_before).then_some(mem.last_crop);
        ctx.state.market.put_farm(mem);
        apply(ctx, acts);
        if let Some(doodad) = last_crop {
            log_produce(ctx, doodad);
        }
        status
    }

    /// Called when the goal ends, fails or is interrupted: the harvest window closes, the walk
    /// stops, the board is told what grows.
    pub fn stop(&mut self, ctx: &mut Ctx<'_>) {
        let mut mem = ctx.state.market.take_farm();
        if let Some(mut machine) = self.run.take() {
            let mut acts = Vec::new();
            machine.stop(&mut mem, &mut acts);
            mem.last_end_ms = ctx.now_ms.max(1);
            ctx.state.market.put_farm(mem);
            apply(ctx, acts);
        } else {
            ctx.state.market.put_farm(mem);
        }
        ctx.motion.stop();
    }

    /// Whether the goal drives the combat controller itself (see [`super::Goal::owns_combat`]).
    pub fn owns_combat(&self) -> bool {
        false
    }

    /// The activity the web UI shows while the goal runs.
    pub fn activity(&self, moving: bool, fighting: bool) -> Activity {
        if fighting {
            Activity::Fighting
        } else if moving {
            Activity::Traveling
        } else {
            Activity::Idle
        }
    }

    /// Appends a one-line description of what the goal is doing to `out`.
    pub fn describe(&self, out: &mut String) {
        out.push_str("farm");
        if let Some(m) = &self.run {
            out.push_str(": ");
            out.push_str(m.note());
            if let Some(p) = m.plan() {
                out.push_str(&format!(" (seed {}, {})", p.seed, p.site.describe()));
            }
        }
    }
}

/// The goal's weight before the situation is looked at: 0 (the goal scores itself, see [`score`],
/// from its [`weight`] and the situation).
pub fn base_weight(_behavior: &BehaviorCfg, _temper: &Temperament) -> f32 {
    0.0
}

/// The weight of a farm run for a bot with this temperament: a farmer is a homebody or a merchant
/// at heart. About a third of the quest weight for a neutral bot.
pub fn weight(temper: &Temperament) -> f32 {
    0.35 * (0.5 + 0.5 * temper.homebody + 0.5 * temper.merchant)
}

/// The score of the goal for this tick. 0 keeps the bot from picking it. Called whenever the brain
/// scores the goals, so the cheap checks come first.
pub fn score(ctx: &Ctx<'_>, _base: f32) -> f32 {
    let cfg = &ctx.cfg.farm;
    let base = weight(&ctx.persona.temper);
    if !cfg.enabled
        || ctx.state.place.in_instance()
        || ctx.now_ms < SETTLE_MS
        || ctx.state.me.hp_pct() < MIN_HP_PCT
        || ctx.world.farm().crops().is_empty()
    {
        return 0.0;
    }
    let mem = ctx.state.market.farm();
    let now = ctx.now_ms;
    let pos = ctx.state.me.pos;
    if mem.is_some_and(|m| {
        m.plots
            .iter()
            .any(|p| p.due(now) && dist2d(pos, p.pos) <= cfg.max_trip_m)
    }) {
        return base * HARVEST_BOOST;
    }
    if mem.is_some_and(|m| m.last_end_ms != 0 && now < m.last_end_ms + RUN_COOLDOWN_MS) {
        return 0.0;
    }
    let mut rng = fastrand::Rng::with_seed(u64::from(ctx.persona.id) ^ (now / 60_000));
    match plan_now(
        ctx.world,
        ctx.state,
        ctx.hub,
        ctx.cfg,
        ctx.persona.id,
        now,
        &mut rng,
    ) {
        Some((pick, _)) => base * (1.0 + NEED_GAIN * pick.need.min(NEED_CAP)),
        None => 0.0,
    }
}

/// Harvest if a plot is due and reachable, else plant if there is a plan.
fn choose_job(ctx: &mut Ctx<'_>) -> Option<Machine> {
    let cfg = &ctx.cfg.farm;
    let now = ctx.now_ms;
    let pos = ctx.state.me.pos;
    let mem = ctx.state.market.farm();
    let fetch = cfg.public_farms && mem.is_none_or(|m| !m.list_asked);
    if mem.is_some_and(|m| {
        m.plots
            .iter()
            .any(|p| p.due(now) && dist2d(pos, p.pos) <= cfg.max_trip_m)
    }) {
        return Some(Machine::harvest(fetch, now));
    }
    let (_, plan) = plan_now(
        ctx.world,
        ctx.state,
        ctx.hub,
        ctx.cfg,
        ctx.persona.id,
        now,
        ctx.rng,
    )?;
    ctx.log.say(format!(
        "farm: plan {:?} seed {} x{} (buy {}, mint {}) at {}",
        plan.why,
        plan.seed,
        plan.want,
        plan.buy,
        plan.mint,
        plan.site.describe()
    ));
    Some(Machine::plant(plan, fetch, now))
}

/// The best crop for the bot now and the plan to plant it.
fn plan_now(
    world: &World,
    state: &BotState,
    hub: &Hub,
    cfg: &crate::config::Config,
    bot: u32,
    now_ms: u64,
    rng: &mut fastrand::Rng,
) -> Option<(plan::Pick, Plan)> {
    let fcfg = &cfg.farm;
    let mem = state.market.farm();
    let sites = candidate_sites(world, state, mem, cfg);
    if sites.is_empty() {
        return None;
    }
    let env = RealEnv {
        world,
        state,
        hub,
        bot,
        gm: cfg.bots.gm_bootstrap,
        max_trip_m: fcfg.max_trip_m,
        wished: hub.pop.wished(),
        sites: &sites,
        mem,
        now_ms,
    };
    let pick = plan::choose(world.farm().crops(), &env, fcfg.growth_rate, rng)?;
    let crop = world.farm().crop_of_seed(pick.seed)?;
    let site = *sites.iter().find(|s| takes(crop, s))?;
    let room = site::room(mem, site.land, fcfg.max_plots);
    let plan = plan::make_plan(&pick, site, room, state.me.money, cfg.bots.gm_bootstrap)?;
    Some((pick, plan))
}

/// Whether `site` takes `crop`: a garden takes any crop that grows on land, a farm the crops of
/// its kind.
fn takes(crop: &Crop, site: &Site) -> bool {
    match site.kind() {
        None => true,
        Some(kind) => crop.allowed_in(kind),
    }
}

/// The places the bot may plant on now: its gardens and the public farms within reach, with room
/// left, nearest first.
fn candidate_sites(
    world: &World,
    state: &BotState,
    mem: Option<&FarmMem>,
    cfg: &crate::config::Config,
) -> Vec<Site> {
    let fcfg = &cfg.farm;
    let mut all: Vec<Site> = Vec::new();
    if fcfg.own_land {
        all.extend(site::gardens(&state.houses, |template| {
            world.housing_design(template).map_or(4.0, garden_radius)
        }));
    }
    if fcfg.public_farms {
        all.extend(site::farms(
            world.farm().areas(),
            FarmKind::Farm.bit() | FarmKind::Nursery.bit(),
        ));
    }
    all.retain(|s| site::room(mem, s.land, fcfg.max_plots) > 0);
    site::nearest(all, state.me.pos, fcfg.max_trip_m)
}

/// The nearest merchant NPC of `template` that is in view and alive: `(object id, position)`.
fn nearest_merchant(state: &BotState, template: u32, me: [f32; 3]) -> Option<(u32, [f32; 3])> {
    state
        .units
        .iter()
        .filter(|(_, u)| u.kind == UnitKind::Npc && u.id == template && !u.dead())
        .map(|(&obj, u)| (obj, u.pos, u.dist2d(me)))
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(obj, pos, _)| (obj, pos))
}

/// Logs what the bot holds of the produce of the crop it just harvested, and how much of it the
/// ledger counts as its own (the operator's evidence that the honest window worked).
fn log_produce(ctx: &mut Ctx<'_>, doodad: u32) {
    let Some(crop) = ctx.world.farm().crop_of_doodad(doodad) else {
        return;
    };
    for y in crop.produce.iter().take(4) {
        let held = ctx.state.item_count(y.item);
        let clean = ctx.state.market.clean_count(y.item);
        let name = ctx.world.item_name(y.item).unwrap_or("?");
        ctx.log.say(format!(
            "farm: holds {held} of {name} ({}), the ledger counts {clean} as its own",
            y.item
        ));
    }
}

/// Carries out what the machine asked for.
fn apply(ctx: &mut Ctx<'_>, acts: Vec<Act>) {
    let mut publish = false;
    for act in acts {
        match act {
            Act::Say(text) => ctx.log.say(text),
            Act::Gm(line) => {
                ctx.gm(&line);
            }
            Act::Travel { x, y, radius } => ctx.travel(x, y, radius, Pace::Run),
            Act::Face { x, y } => ctx.motion.face(x, y),
            Act::StopMotion => {
                if ctx.motion.is_moving() {
                    ctx.motion.stop();
                }
            }
            Act::RequestList => ctx.send(cs::request_common_farm_list()),
            Act::Buy { npc, item, count } => ctx.send(cs::buy_items(
                npc,
                &[BuyItem {
                    item,
                    grade: 0,
                    count,
                    currency: SHOP_MONEY,
                }],
            )),
            Act::Plant {
                doodad,
                pos,
                z_rot,
                item_id,
            } => ctx.send(cs::create_doodad(
                doodad,
                (pos[0], pos[1], pos[2]),
                z_rot,
                1.0,
                item_id,
            )),
            Act::Harvest { skill, obj } => {
                let me = ctx.state.me.obj;
                ctx.send(cs::start_skill_doodad(skill, me, obj));
            }
            Act::Publish => publish = true,
        }
    }
    if publish {
        crate::bot::farm::publish(ctx.hub, ctx.persona.id, ctx.state, ctx.world);
    }
}

/// What the planner reads about the bot and the world.
struct RealEnv<'a> {
    world: &'a World,
    state: &'a BotState,
    hub: &'a Hub,
    bot: u32,
    gm: bool,
    max_trip_m: f32,
    wished: Vec<u32>,
    sites: &'a [Site],
    mem: Option<&'a FarmMem>,
    now_ms: u64,
}

impl Env for RealEnv<'_> {
    fn have(&self, seed: u32) -> i64 {
        self.state.item_count(seed)
    }

    fn price(&self, item: u32) -> u32 {
        self.world.item_facts(item).map_or(0, |f| f.price)
    }

    fn value(&self, item: u32) -> f32 {
        self.world.item_facts(item).map_or(0.0, |f| {
            let sold = f.refund as f32;
            if f.tradable() {
                sold.max(f.price as f32 * 0.5)
            } else {
                sold
            }
        })
    }

    fn scarcity(&self, item: u32) -> f32 {
        self.hub.pop.scarcity(item)
    }

    fn wished(&self, item: u32) -> bool {
        self.wished.contains(&item)
    }

    fn growing(&self, item: u32) -> f32 {
        let _ = self.bot;
        self.hub.farms.units(item)
    }

    fn vendor(&self, seed: u32) -> Option<Vendor> {
        let pos = self.state.me.pos;
        let from = aa_world::Vec3::new(pos[0], pos[1], pos[2]);
        self.world
            .farm()
            .vendors_of(seed)
            .iter()
            .filter_map(|&npc| {
                let at = self.world.nearest_npc_spawn(npc, from)?;
                let dist = at.dist2d(from);
                (dist <= self.max_trip_m).then_some(Vendor {
                    npc,
                    pos: [at.x, at.y, at.z],
                    dist,
                })
            })
            .min_by(|a, b| a.dist.total_cmp(&b.dist))
    }

    fn place_for(&self, crop: &Crop) -> bool {
        self.sites.iter().any(|s| takes(crop, s))
    }

    fn bad_seed(&self, seed: u32) -> bool {
        self.mem.is_some_and(|m| m.seed_is_bad(seed, self.now_ms))
    }

    fn money(&self) -> i64 {
        self.state.me.money
    }

    fn gm(&self) -> bool {
        self.gm
    }
}

/// The machine's view of the world: the terrain, the farm areas, the skills.
struct RealOps<'a> {
    world: &'a World,
    hub: &'a Hub,
    bot: u32,
    now_ms: u64,
    fallback_z: f32,
    rng: RefCell<&'a mut fastrand::Rng>,
}

impl Ops for RealOps<'_> {
    fn spot(&self, mem: &FarmMem, site: &Site, radius_mm: u16) -> Option<[f32; 3]> {
        let spacing = site::spacing_m(radius_mm);
        let (world, hub, bot, now) = (self.world, self.hub, self.bot, self.now_ms);
        let fits = |x: f32, y: f32| {
            super::walkable(world, x, y)
                && !mem.spot_is_bad(x, y, now)
                && !mem
                    .plots
                    .iter()
                    .any(|p| dist2d(p.pos, [x, y, 0.0]) < spacing)
                && hub.contest.others(plot_key(x, y), bot) == 0
        };
        let mut rng = self.rng.borrow_mut();
        let [x, y] = site::pick_spot(site, world.farm().areas(), &mut rng, &fits)?;
        hub.contest.claim(plot_key(x, y), bot);
        Some([x, y, world.ground_z(x, y).unwrap_or(self.fallback_z)])
    }

    fn harvest_skill(&self, doodad: u32, phase: u32) -> Option<u32> {
        self.world
            .farm()
            .crop_of_doodad(doodad)?
            .harvest_skill(phase)
    }

    fn cast_ms(&self, skill: u32) -> u64 {
        self.world
            .skill(skill)
            .map_or(0, |s| u64::from(s.casting_time))
    }

    fn radius_mm(&self, doodad: u32) -> u16 {
        self.world
            .farm()
            .crop_of_doodad(doodad)
            .map_or(500, |c| c.radius_mm)
    }
}
