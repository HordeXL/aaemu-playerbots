//! Which monster of an event a bot fights. Pure: the goal builds the candidates from what it sees.

use aa_world::event::EventDef;
use aa_world::Events;

use super::plan::{ENGAGE_M, VIEW_M};

/// How much a monster is the event's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// A kill target of the current wave: what the wave waits for.
    Target,
    /// A monster of another wave of the event (a straggler of the one before).
    Wave,
}

/// What the goal knows about a monster in view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cand {
    /// Object id.
    pub obj: u32,
    /// Its class.
    pub class: Class,
    /// Distance from the bot (m).
    pub d_bot: f32,
    /// Distance from the defended target (m).
    pub d_rally: f32,
    /// Another bot has claimed it.
    pub held: bool,
}

/// The class of an NPC of `template` for wave `step` of `def`; `None` for what the bots leave
/// alone: the event's own controllers (the Rift itself, the summoners) and every monster that is
/// not one of the event's own (the camps around a rift are not the bots' business, and a wave that
/// names no kill targets is waited out at the target).
pub fn classify(events: &Events, def: &EventDef, step: u32, template: u32) -> Option<Class> {
    if events.is_controller(template) {
        return None;
    }
    if def.is_kill_target(step, template) {
        return Some(Class::Target);
    }
    def.is_wave_npc(template).then_some(Class::Wave)
}

/// The monster to fight: the best class first, one nobody holds before one somebody holds, then
/// the nearest to the bot. Monsters out of reach ([`ENGAGE_M`] from the target, [`VIEW_M`] from
/// the bot) are not considered.
pub fn choose(cands: &[Cand]) -> Option<u32> {
    cands
        .iter()
        .filter(|c| c.d_rally <= ENGAGE_M && c.d_bot <= VIEW_M)
        .min_by(|a, b| {
            (a.class, a.held)
                .cmp(&(b.class, b.held))
                .then(a.d_bot.total_cmp(&b.d_bot))
        })
        .map(|c| c.obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_world::event::{Controller, Kill, Step};
    use aa_world::Vec3;

    const CTRL: u32 = 900;
    const ARCHER: u32 = 10;
    const SOLDIER: u32 = 11;

    fn fixture() -> (Events, EventDef) {
        let step = |prog, kills: &[(u32, u32)]| Step {
            prog,
            timer_s: 0.0,
            all: true,
            kills: kills
                .iter()
                .map(|&(template, count)| Kill { template, count })
                .collect(),
            spawners: Box::default(),
        };
        let def = EventDef {
            id: 3,
            name: "Rift".into(),
            first_wave_after_s: 0.0,
            force_end_s: 3600.0,
            target_spawner: 1,
            end_kill: (0, 0),
            steps: vec![
                step(1, &[]),
                step(2, &[(ARCHER, 3)]),
                step(3, &[(SOLDIER, 3)]),
                step(4, &[]),
            ]
            .into(),
            controllers: vec![Controller {
                spawner: 1,
                step: 0,
                template: CTRL,
                pos: Vec3::new(0.0, 0.0, 0.0),
                zone_group: 20,
            }]
            .into(),
            min_level: 40,
            max_level: 45,
            war_zone_group: None,
        };
        (Events::from_defs(vec![def.clone()]), def)
    }

    fn cand(obj: u32, class: Class, d_bot: f32, held: bool) -> Cand {
        Cand {
            obj,
            class,
            d_bot,
            d_rally: 10.0,
            held,
        }
    }

    #[test]
    fn the_current_wave_decides_what_is_a_target() {
        let (ev, d) = fixture();
        // wave 2 waits for archers; a soldier is a straggler of the event
        assert_eq!(classify(&ev, &d, 2, ARCHER), Some(Class::Target));
        assert_eq!(classify(&ev, &d, 2, SOLDIER), Some(Class::Wave));
        assert_eq!(classify(&ev, &d, 3, SOLDIER), Some(Class::Target));
        // the controllers are never attacked
        for step in 0..6 {
            assert_eq!(classify(&ev, &d, step, CTRL), None, "step {step}");
        }
    }

    #[test]
    fn monsters_that_are_not_the_events_are_left_alone() {
        let (ev, d) = fixture();
        // the camps around a rift, in a wave with kill targets and in one without (waves 1 and 4
        // name none; the bots wait those out at the target), and before the first wave
        for step in 0..6 {
            assert_eq!(classify(&ev, &d, step, 77), None, "step {step}");
        }
    }

    #[test]
    fn the_best_class_wins_then_a_free_monster_then_the_nearest() {
        let c = [
            cand(1, Class::Wave, 2.0, false),
            cand(2, Class::Wave, 3.0, false),
            cand(3, Class::Target, 30.0, true),
            cand(4, Class::Target, 40.0, false),
            cand(5, Class::Target, 20.0, false),
        ];
        // targets first; of those the free ones; of those the nearest
        assert_eq!(choose(&c), Some(5));
        // when every target is held, the nearest held one
        let c = [
            cand(3, Class::Target, 30.0, true),
            cand(6, Class::Target, 25.0, true),
            cand(2, Class::Wave, 3.0, false),
        ];
        assert_eq!(choose(&c), Some(6));
        // no target in reach: a straggler of another wave, the nearest first
        let c = [
            cand(1, Class::Wave, 40.0, false),
            cand(2, Class::Wave, 30.0, false),
        ];
        assert_eq!(choose(&c), Some(2));
        assert_eq!(choose(&c[..1]), Some(1));
        assert_eq!(choose(&[]), None);
    }

    #[test]
    fn monsters_out_of_reach_are_not_considered() {
        let far_from_target = Cand {
            d_rally: ENGAGE_M + 1.0,
            ..cand(1, Class::Target, 5.0, false)
        };
        let far_from_bot = cand(2, Class::Target, VIEW_M + 1.0, false);
        let edge = Cand {
            d_rally: ENGAGE_M,
            ..cand(3, Class::Wave, VIEW_M, false)
        };
        assert_eq!(choose(&[far_from_target, far_from_bot]), None);
        assert_eq!(choose(&[far_from_target, far_from_bot, edge]), Some(3));
    }
}
