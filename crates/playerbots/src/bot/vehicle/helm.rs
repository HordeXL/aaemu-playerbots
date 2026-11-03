//! The helm: the heading controller of a boat and the detection of which way the wheel turns.
//!
//! A ship's steering input is an angular *acceleration* request: every physics tick the server adds
//! to the hull's turn rate in proportion to the helm and only damps the rate with the helm
//! centred, and the helm value itself is smoothed with a time constant of about 0.3 s. The turn
//! rate settles at a small cap (4.35 degrees per second for a boat, half of it at standstill), so
//! a rowboat needs twenty seconds for a quarter turn. The controller therefore tracks a desired
//! yaw *rate* instead of the heading error:
//!
//! ```text
//! r_des    = sign(err) * min(r_max, sqrt(2 * a_dec * |err|), KP * |err|)
//! steering = -sign * clamp((r_des - yaw_rate) / (accel * tf * TAU), -1, 1) * 127
//! ```
//!
//! `err` is the wrapped target heading minus the heading (positive: the bow must turn
//! counter-clockwise), `accel` and the cap `r_max` come from the hull ([`Hull`], read from the
//! ship model of the slave template), `tf` is the speed's turn factor and `sign` is +1 when a
//! positive steering value lowers the yaw (what the protocol docs derive from the server's physics
//! code, "starboard turn") and -1 when it raises it. The sign is found out on the water, not
//! assumed: a probe at the start ([`Helm::probe`]) holds the wheel over for a moment and reads
//! which way the bow went, and a divergence watch ([`Helm::watch`]) flips the sign if the error
//! keeps growing with the wheel hard over.

use aa_world::travel::SlaveTemplate;

use super::wrap_pi;

/// Full helm or throttle.
pub const FULL: f32 = 127.0;
/// Throttle on a straight run.
pub const CRUISE: i8 = 110;
/// Desired yaw rate (rad/s) per radian of error near the target heading.
const KP: f32 = 0.5;
/// Time constant (s) the rate tracking aims for.
const TAU: f32 = 1.2;
/// The probe holds this steering for [`PROBE_MS`]...
pub const PROBE_STEERING: i8 = 80;
/// ... and this throttle (the hull turns at standstill too, only slower).
pub const PROBE_THROTTLE: i8 = 100;
/// How long the probe lasts.
pub const PROBE_MS: u64 = 2500;
/// The probe says nothing when the bow moved less than this many radians.
pub const PROBE_MIN_TURN: f32 = 0.02;
/// Steering at or above this counts as "hard over" for the divergence watch.
const HARD_OVER: u8 = 100;
/// The watch needs the wheel hard over this long, with the error grown by this many hull caps'
/// worth of turning, to flip the sign.
const DIVERGE_MS: u64 = 4000;
const DIVERGE_CAPS: f32 = 0.6;
/// Sign flips a helm makes at most (the sign is a fact of the server, not a mood).
const MAX_FLIPS: u8 = 2;

/// How a hull turns: the angular acceleration at full helm (rad/s^2, at full turn factor) and the
/// turn rate the helm settles at (rad/s). Both are halved at standstill (see [`turn_factor`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hull {
    /// Angular acceleration at full helm.
    pub accel: f32,
    /// Turn rate cap.
    pub cap: f32,
}

impl Hull {
    /// The rowboat's: model 129 `turn_accel` 9.9, kind boat (4.35 degrees a second).
    pub const ROWBOAT: Hull = Hull {
        accel: 0.1254,
        cap: 0.0759,
    };

    /// The hull of a slave template (the rowboat's when the template has no ship model).
    pub fn of(t: &SlaveTemplate) -> Hull {
        t.turn()
            .map_or(Hull::ROWBOAT, |(accel, cap)| Hull { accel, cap })
    }
}

/// The server's turn factor: 0.5 at standstill, rising to 1 at 2.5 m/s (and above).
pub fn turn_factor(speed: f32) -> f32 {
    0.5 + 0.5 * (speed.abs() / 2.5).clamp(0.0, 1.0)
}

/// A throttle and steering request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cmd {
    /// -127 (astern) ..= 127 (ahead).
    pub throttle: i8,
    /// -127 ..= 127, see the module docs for the sign.
    pub steering: i8,
}

/// Where the probe stands.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Probe {
    NotStarted,
    Running { t0_ms: u64, yaw0: f32 },
    Done,
}

/// The heading controller of one boat. See the module docs.
#[derive(Debug, Clone)]
pub struct Helm {
    hull: Hull,
    sign: i8,
    probe: Probe,
    /// Whether the probe or the watch has settled the sign.
    known: bool,
    flips: u8,
    /// Divergence watch: since when the wheel has been hard over, and the error then.
    hard_since_ms: u64,
    hard_err: f32,
}

impl Default for Helm {
    fn default() -> Self {
        Helm::new(Hull::ROWBOAT)
    }
}

impl Helm {
    /// A helm for `hull` that assumes the documented sign until the probe says otherwise.
    pub fn new(hull: Hull) -> Helm {
        Helm {
            hull,
            sign: 1,
            probe: Probe::NotStarted,
            known: false,
            flips: 0,
            hard_since_ms: 0,
            hard_err: 0.0,
        }
    }

    /// +1: a positive steering value lowers the yaw (the documented direction); -1: it raises it.
    pub fn sign(&self) -> i8 {
        self.sign
    }

    /// Whether the sign was measured (by the probe or after a flip) and not just assumed.
    pub fn known(&self) -> bool {
        self.known
    }

    /// How often the divergence watch flipped the sign.
    pub fn flips(&self) -> u8 {
        self.flips
    }

    /// Whether the probe has finished (or was given up).
    pub fn probed(&self) -> bool {
        self.probe == Probe::Done
    }

    /// The probe: while it returns a [`Cmd`], send it instead of steering; it holds the wheel over
    /// for [`PROBE_MS`], reads the heading change and settles the sign (or leaves it assumed when
    /// the bow barely moved). Returns `None` once it is done. `yaw` is the ship's heading.
    pub fn probe(&mut self, now_ms: u64, yaw: f32) -> Option<Cmd> {
        let cmd = Cmd {
            throttle: PROBE_THROTTLE,
            steering: PROBE_STEERING,
        };
        match self.probe {
            Probe::NotStarted => {
                self.probe = Probe::Running {
                    t0_ms: now_ms,
                    yaw0: yaw,
                };
                Some(cmd)
            }
            Probe::Running { t0_ms, yaw0 } => {
                if now_ms.saturating_sub(t0_ms) < PROBE_MS {
                    return Some(cmd);
                }
                let turned = wrap_pi(yaw - yaw0);
                if turned.abs() >= PROBE_MIN_TURN {
                    // documented: a positive steering lowers the yaw
                    self.sign = if turned < 0.0 { 1 } else { -1 };
                    self.known = true;
                }
                self.probe = Probe::Done;
                None
            }
            Probe::Done => None,
        }
    }

    /// The steering that turns the bow by `err` radians (positive = counter-clockwise) given the
    /// measured yaw rate in rad/s (positive = counter-clockwise) and the forward speed in m/s.
    pub fn steer(&self, err: f32, yaw_rate: f32, speed: f32) -> i8 {
        let tf = turn_factor(speed);
        let r_max = 0.9 * self.hull.cap * tf;
        let a_dec = 0.5 * self.hull.accel * tf;
        let a = err.abs();
        let r_des = err.signum() * r_max.min((2.0 * a_dec * a).sqrt()).min(KP * a);
        let mut u = ((r_des - yaw_rate) / (self.hull.accel * tf * TAU)).clamp(-1.0, 1.0);
        if u.abs() < 0.03 {
            u = 0.0;
        }
        if speed < -0.3 {
            // going astern the helm works the other way round
            u = -u;
        }
        let s = -f32::from(self.sign) * u * FULL;
        s.round().clamp(-FULL, FULL) as i8
    }

    /// The divergence watch: call once per control step with the heading error and the steering
    /// that was sent. When the wheel has been hard over for a few seconds and the error only grew,
    /// the sign was wrong: flip it (at most [`MAX_FLIPS`] times). Returns whether it flipped.
    pub fn watch(&mut self, now_ms: u64, err: f32, steering: i8) -> bool {
        if steering.unsigned_abs() < HARD_OVER {
            self.hard_since_ms = 0;
            return false;
        }
        if self.hard_since_ms == 0 || err.abs() < self.hard_err {
            // a fresh stretch, or the error is shrinking: the helm is doing its job
            self.hard_since_ms = now_ms.max(1);
            self.hard_err = err.abs();
            return false;
        }
        let secs = DIVERGE_MS as f32 / 1000.0;
        if now_ms.saturating_sub(self.hard_since_ms) >= DIVERGE_MS
            && err.abs() >= self.hard_err + DIVERGE_CAPS * self.hull.cap * secs
            && self.flips < MAX_FLIPS
        {
            self.sign = -self.sign;
            self.flips += 1;
            self.known = true;
            self.hard_since_ms = 0;
            return true;
        }
        false
    }
}

/// The throttle for a run: `err` heading error (rad), `dist_end` metres to the end of the route
/// when this is the last leg (`None` otherwise), `speed` the forward speed in m/s.
pub fn throttle(err: f32, dist_end: Option<f32>, speed: f32) -> i8 {
    let a = err.abs();
    let mut t = if a > 1.2 {
        35.0
    } else if a > 0.6 {
        65.0
    } else if a > 0.3 {
        90.0
    } else {
        f32::from(CRUISE)
    };
    if let Some(d) = dist_end {
        // slow down over the last stretch (about six seconds of way), brake when still too fast
        let slow = 40.0 + speed.max(0.0) * 6.0;
        let ramp = ((d - 12.0) / (slow - 12.0)).clamp(0.0, 1.0);
        t = t.min(30.0 + 80.0 * ramp);
        if d < 40.0 && speed > 2.5 {
            t = -50.0;
        }
    }
    ((t / 5.0).round() * 5.0).clamp(-FULL, FULL) as i8
}

/// Whether a new request is worth a packet: the helm moved by `STEER_STEP` or more, the throttle
/// changed, or the last one is a second old (the server keeps a request until the next one, so the
/// repeat is only a heartbeat).
pub fn worth_sending(last: Option<(Cmd, u64)>, new: Cmd, now_ms: u64) -> bool {
    const STEER_STEP: i16 = 6;
    const HEARTBEAT_MS: u64 = 1000;
    match last {
        None => true,
        Some((old, at)) => {
            now_ms.saturating_sub(at) >= HEARTBEAT_MS
                || old.throttle != new.throttle
                || (i16::from(old.steering) - i16::from(new.steering)).abs() >= STEER_STEP
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::sim::Plant;
    use super::*;

    /// The hulls the tests turn: the rowboat, the clipper's (slow to turn, quick once turning)
    /// and a nimble one.
    const HULLS: [Hull; 3] = [
        Hull::ROWBOAT,
        Hull {
            accel: 0.019,
            cap: 0.102,
        },
        Hull {
            accel: 0.4,
            cap: 0.15,
        },
    ];

    fn plant(h: Hull, sign: i8) -> Plant {
        Plant::new(0.0, sign).hull(h.accel, h.cap)
    }

    /// Runs the probe like the goal does and returns the time it took.
    fn probe(plant: &mut Plant, helm: &mut Helm) -> u64 {
        let mut now = 0u64;
        while let Some(cmd) = helm.probe(now, plant.yaw) {
            plant.request(cmd.throttle, cmd.steering);
            plant.advance(0.25);
            now += 250;
        }
        now
    }

    /// Runs the closed loop for `secs` seconds with a control step of 250 ms (the brain's tick)
    /// turning towards `target` (a function of time) and returns the heading errors.
    fn run(plant: &mut Plant, helm: &mut Helm, target: impl Fn(f32) -> f32, secs: f32) -> Vec<f32> {
        let mut errs = Vec::new();
        let mut now = probe(plant, helm);
        let t0 = now;
        let end = now + (secs * 1000.0) as u64;
        while now < end {
            let err = wrap_pi(target((now - t0) as f32 / 1000.0) - plant.yaw);
            let s = helm.steer(err, plant.yaw_rate_estimate(), plant.speed);
            helm.watch(now, err, s);
            plant.request(CRUISE, s);
            plant.advance(0.25);
            now += 250;
            errs.push(err);
        }
        errs
    }

    #[test]
    fn the_probe_finds_the_documented_sign() {
        for h in HULLS {
            let mut p = plant(h, 1);
            let mut helm = Helm::new(h);
            let took = probe(&mut p, &mut helm);
            assert!(helm.known() && helm.sign() == 1 && helm.probed(), "{h:?}");
            assert!(took >= PROBE_MS);
        }
    }

    #[test]
    fn the_probe_finds_an_inverted_sign() {
        for h in HULLS {
            let mut p = plant(h, -1);
            let mut helm = Helm::new(h);
            probe(&mut p, &mut helm);
            assert!(
                helm.known(),
                "the bow turned, so the sign is settled: {h:?}"
            );
            assert_eq!(helm.sign(), -1, "{h:?}");
        }
    }

    #[test]
    fn a_probe_without_effect_leaves_the_sign_assumed() {
        let mut h = Helm::default();
        let mut now = 0;
        while h.probe(now, 0.3).is_some() {
            now += 250;
        }
        assert!(!h.known() && h.sign() == 1 && h.probed());
        assert_eq!(h.probe(now + 1, 0.3), None);
    }

    #[test]
    fn the_steering_follows_the_error_with_the_documented_sign() {
        let h = Helm::default();
        // the bow must turn counter-clockwise (+): a negative steering value
        assert!(h.steer(0.5, 0.0, 6.0) < 0);
        assert!(h.steer(-0.5, 0.0, 6.0) > 0);
        assert_eq!(h.steer(0.0, 0.0, 6.0), 0);
        // turning at the wanted rate: nothing more to ask; faster than wanted: back off
        let want = 0.9 * Hull::ROWBOAT.cap;
        assert_eq!(h.steer(1.0, want, 6.0), 0);
        assert!(h.steer(1.0, 2.0 * want, 6.0) > 0);
        // close to the heading and still turning: counter-steer before the error is gone
        assert!(h.steer(0.02, 0.07, 6.0) > 0);
        // a big error asks for the cap rate from rest: a good half of the wheel
        assert!((-80..=-40).contains(&h.steer(3.0, 0.0, 6.0)));
        assert_eq!(h.steer(3.0, 0.0, 6.0), h.steer(1.0, 0.0, 6.0));
        // an inverted wheel and a hull going astern turn the answer round
        let inv = Helm {
            sign: -1,
            ..Helm::default()
        };
        assert_eq!(inv.steer(0.5, 0.0, 6.0), -h.steer(0.5, 0.0, 6.0));
        assert_eq!(h.steer(0.5, 0.0, -3.0), -h.steer(0.5, 0.0, 3.0));
    }

    #[test]
    fn the_loop_turns_to_a_heading_for_either_sign_and_any_hull() {
        for sign in [1, -1] {
            for h in HULLS {
                let mut p = plant(h, sign);
                let mut helm = Helm::new(h);
                // a 60 degree turn to port, then round to a heading 100 degrees to starboard
                let target = |t: f32| if t < 120.0 { 1.05 } else { -1.75 };
                let errs = run(&mut p, &mut helm, target, 420.0);
                // before the second turn it settled on the first heading ...
                let first = &errs[(100.0 / 0.25) as usize..(120.0 / 0.25) as usize];
                assert!(
                    first.iter().all(|e| e.abs() < 0.1),
                    "sign {sign} hull {h:?}: {:?}",
                    &first[first.len() - 8..]
                );
                // ... and at the end on the second, without swinging about it
                let last = &errs[errs.len() - 40..];
                assert!(
                    last.iter().all(|e| e.abs() < 0.1),
                    "sign {sign} hull {h:?}: {:?}",
                    &last[last.len() - 8..]
                );
                let overshoot = errs[(130.0 / 0.25) as usize..]
                    .iter()
                    .fold(0.0f32, |m, e| m.max(e.abs()));
                assert!(overshoot < 3.2, "sign {sign} hull {h:?}: {overshoot}");
                assert_eq!(helm.sign(), sign, "hull {h:?}");
            }
        }
    }

    #[test]
    fn a_wrong_sign_is_fixed_by_the_divergence_watch() {
        // the probe is skipped, so the helm starts with the documented sign on an inverted hull
        let h = Hull::ROWBOAT;
        let mut p = plant(h, -1);
        let mut helm = Helm::new(h);
        let mut now = 0u64;
        let mut flipped = None;
        for step in 0..1200 {
            let err = wrap_pi(1.5 - p.yaw);
            let s = helm.steer(err, p.yaw_rate_estimate(), p.speed);
            if helm.watch(now, err, s) && flipped.is_none() {
                flipped = Some(step);
            }
            p.request(CRUISE, s);
            p.advance(0.25);
            now += 250;
        }
        assert!(flipped.is_some(), "the sign was never corrected");
        assert_eq!(helm.sign(), -1);
        assert!(wrap_pi(1.5 - p.yaw).abs() < 0.1, "yaw {}", p.yaw);
    }

    #[test]
    fn the_watch_leaves_a_working_helm_alone() {
        let mut h = Helm::default();
        // hard over for ten seconds while the error shrinks
        for i in 0..40 {
            assert!(!h.watch(i * 250, 2.0 - i as f32 * 0.04, 127));
        }
        // a centred wheel resets it
        assert!(!h.watch(10_000, 2.0, 0));
        assert_eq!(h.flips(), 0);
    }

    #[test]
    fn the_hull_comes_from_the_template() {
        assert_eq!(Hull::ROWBOAT.cap.to_degrees().round(), 4.0);
        let tf = turn_factor(0.0);
        assert_eq!((tf, turn_factor(2.5), turn_factor(-9.0)), (0.5, 1.0, 1.0));
    }

    #[test]
    fn the_throttle_follows_the_turn_and_the_distance() {
        assert_eq!(throttle(0.0, None, 6.0), CRUISE);
        assert!(throttle(0.8, None, 6.0) < throttle(0.4, None, 6.0));
        assert!(throttle(1.5, None, 6.0) <= 40);
        // far from the end it cruises, close it eases, very close at speed it brakes
        assert_eq!(throttle(0.0, Some(500.0), 6.0), CRUISE);
        assert!(throttle(0.0, Some(60.0), 6.0) < 100);
        assert!(throttle(0.0, Some(14.0), 1.0) > 0);
        assert!(throttle(0.0, Some(20.0), 5.0) < 0);
        // always a multiple of five, within the range
        for e in [0.0, 0.5, 1.0, 2.0] {
            for d in [None, Some(10.0), Some(80.0), Some(900.0)] {
                let t = throttle(e, d, 4.0);
                assert_eq!(t % 5, 0);
            }
        }
    }

    #[test]
    fn requests_are_sent_on_change_and_as_a_heartbeat() {
        let a = Cmd {
            throttle: 100,
            steering: 20,
        };
        assert!(worth_sending(None, a, 0));
        assert!(!worth_sending(Some((a, 0)), a, 500));
        assert!(worth_sending(Some((a, 0)), a, 1000));
        let nudge = Cmd { steering: 24, ..a };
        assert!(!worth_sending(Some((a, 0)), nudge, 500));
        let turn = Cmd { steering: 40, ..a };
        assert!(worth_sending(Some((a, 0)), turn, 500));
        let slow = Cmd { throttle: 60, ..a };
        assert!(worth_sending(Some((a, 0)), slow, 500));
    }
}
