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
//!
//! The throttle is an acceleration request, not a speed: the server lets the hull accelerate
//! (to its cap) while the throttle is ahead, coast while it is 0 and brake while it is astern.
//! [`throttle`] therefore tracks a *speed plan* ([`crate::bot::vehicle::follow::Guide::v_limit`]
//! and the heading error) with ahead, 0 and astern, which is what lets a clipper or a cutter
//! slow down for a corner and stop at the berth.

use aa_world::travel::{ShipClass, SlaveTemplate};

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
/// The hull turns against the helm when its yaw rate is this many turn caps the wrong way.
const AGAINST_CAPS: f32 = 0.15;

/// How a hull turns and moves: the angular acceleration at full helm (rad/s^2, at full turn
/// factor), the turn rate the helm settles at (rad/s) - both halved at standstill (see
/// [`turn_factor`]) - and the speed figures of the server's physics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hull {
    /// Angular acceleration at full helm.
    pub accel: f32,
    /// Turn rate cap.
    pub cap: f32,
    /// Top speed in m/s.
    pub top: f32,
    /// Deceleration in m/s^2 with full astern.
    pub brake: f32,
    /// Internal speed units per real m/s (the server's turn factor works on the internal speed).
    pub int: f32,
}

impl Hull {
    /// The rowboat's: model 129 `turn_accel` 9.9, kind boat (4.35 degrees a second), 3.2 m/s.
    pub const ROWBOAT: Hull = Hull {
        accel: 0.1254,
        cap: 0.0759,
        top: 3.2,
        brake: 0.6,
        int: 2.0,
    };

    /// A hull with these turn figures and the rowboat's speeds (tests of other hulls).
    pub const fn turning(accel: f32, cap: f32) -> Hull {
        Hull {
            accel,
            cap,
            ..Hull::ROWBOAT
        }
    }

    /// The hull of a ship class.
    pub const fn of_class(c: &ShipClass) -> Hull {
        Hull {
            accel: c.turn_accel,
            cap: c.turn_rate,
            top: c.top_speed,
            brake: c.brake,
            int: c.int_per_real,
        }
    }

    /// The hull of a slave template (the rowboat's turn when the template has no ship model).
    pub fn of(t: &SlaveTemplate) -> Hull {
        Hull::of_class(&t.class())
    }

    /// Turn radius at top speed in metres.
    pub fn radius(&self) -> f32 {
        self.top / self.cap.max(1e-4)
    }
}

/// The server's turn factor: 0.5 at standstill, rising to 1 at an internal speed of 2.5 (and
/// above).
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
    probe_ms: u64,
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
        // long enough for the slowest hull to swing its bow by a few degrees: 2.5 s for the
        // rowboat, 6 s for a cutter
        let a = (hull.accel * 0.63 * 0.6).max(1e-4);
        let probe_ms = ((2.0 * 0.05 / a).sqrt() * 1000.0).clamp(PROBE_MS as f32, 7000.0) as u64;
        Helm {
            hull,
            probe_ms,
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
                if now_ms.saturating_sub(t0_ms) < self.probe_ms {
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
        let tf = turn_factor(speed * self.hull.int);
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

    /// The divergence watch: call once per control step with the heading error, the steering that
    /// was sent and the measured yaw rate (rad/s, positive = the yaw grows). When the wheel has
    /// been hard over for a few seconds, the error only grew and the hull turns *against* the
    /// helm, the sign was wrong: flip it (at most [`MAX_FLIPS`] times). A hull that turns the way
    /// it is steered but not fast enough for the route (the error grows on a tight curve) keeps its
    /// sign. Returns whether it flipped.
    pub fn watch(&mut self, now_ms: u64, err: f32, steering: i8, rate: f32) -> bool {
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
        // the way the yaw changes when the wheel is right: a positive steering lowers it with
        // sign +1
        let expected = -f32::from(self.sign) * f32::from(steering.signum());
        if now_ms.saturating_sub(self.hard_since_ms) >= DIVERGE_MS
            && err.abs() >= self.hard_err + DIVERGE_CAPS * self.hull.cap * secs
            && rate * expected < -AGAINST_CAPS * self.hull.cap
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

/// The speed (m/s) the hull should have: its top speed (a little over it, the server caps the
/// hull itself) unless the route ahead asks for less (`v_limit` of the guide: corners and the
/// berth), slower the further the bow is off the wanted heading (`err` radians), where a tight
/// turn is needed.
pub fn wanted_speed(hull: &Hull, err: f32, v_limit: f32) -> f32 {
    let a = err.abs();
    let by_heading = (1.0 - 0.6 * ((a - 0.3) / 0.9).clamp(0.0, 1.0)).clamp(0.4, 1.0);
    (hull.top * 1.1 * by_heading).min(v_limit)
}

/// The throttle that brings the hull from `speed` to `wanted` m/s (both forward): ahead in
/// proportion to the shortfall (the hull accelerates for as long as the throttle is ahead), 0 to
/// coast within a small band, astern in proportion to the excess (only while moving ahead: astern
/// at a standstill would back the hull up). A multiple of five.
pub fn throttle(speed: f32, wanted: f32) -> i8 {
    let e = wanted - speed;
    let t = if e > 0.25 {
        (35.0 + 75.0 * e).min(f32::from(CRUISE))
    } else if e < -0.35 && speed > 0.6 {
        -(25.0 + 60.0 * (-e - 0.35)).min(90.0)
    } else {
        0.0
    };
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

    /// The hulls the tests turn: the rowboat, the clipper's (slow to turn, quick once turning),
    /// the cutter's (slower still) and a nimble one.
    const HULLS: [Hull; 4] = [
        Hull::ROWBOAT,
        Hull::of_class(&ShipClass::CLIPPER),
        Hull::of_class(&ShipClass::CUTTER),
        Hull::turning(0.4, 0.15),
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
            helm.watch(now, err, s, plant.yaw_rate_estimate());
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
            if helm.watch(now, err, s, p.yaw_rate_estimate()) && flipped.is_none() {
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
            assert!(!h.watch(i * 250, 2.0 - i as f32 * 0.04, 127, -0.05));
        }
        // a centred wheel resets it
        assert!(!h.watch(10_000, 2.0, 0, 0.0));
        assert_eq!(h.flips(), 0);
    }

    #[test]
    fn a_hull_that_turns_the_way_it_is_steered_keeps_its_sign_while_the_error_grows() {
        // hard over for ten seconds on a curve the hull cannot follow: the error grows, but the
        // yaw falls as a positive steering should make it
        let mut h = Helm::default();
        for i in 0..40 {
            assert!(!h.watch(i * 250, 0.1 + i as f32 * 0.05, 127, -0.07));
        }
        assert_eq!(h.flips(), 0);
        // the same with a hull that turns against the helm: flipped after the four seconds
        let mut h = Helm::default();
        let flipped = (0..40).any(|i| h.watch(i * 250, 0.1 + i as f32 * 0.05, 127, 0.07));
        assert!(flipped);
        assert_eq!(h.sign(), -1);
    }

    #[test]
    fn the_hull_comes_from_the_template() {
        assert_eq!(Hull::ROWBOAT.cap.to_degrees().round(), 4.0);
        let tf = turn_factor(0.0);
        assert_eq!((tf, turn_factor(2.5), turn_factor(-9.0)), (0.5, 1.0, 1.0));
    }

    #[test]
    fn the_throttle_tracks_the_wanted_speed() {
        // from rest it goes flat out; it eases as the hull nears the wanted speed
        assert_eq!(throttle(0.0, 3.5), CRUISE);
        assert!(throttle(3.2, 3.5) > 0 && throttle(3.2, 3.5) < CRUISE);
        // within a small band it lets the hull coast
        assert_eq!(throttle(3.4, 3.5), 0);
        assert_eq!(throttle(3.6, 3.5), 0);
        // too fast: astern, the more the faster, and never at a standstill
        assert!(throttle(5.0, 2.0) < 0);
        assert!(throttle(5.0, 2.0) < throttle(3.0, 2.0));
        assert_eq!(throttle(0.3, 0.0), 0);
        assert_eq!(throttle(0.0, 0.0), 0);
        // always a multiple of five, within the range
        for v in [0.0, 0.7, 2.0, 6.0] {
            for w in [0.0, 1.0, 3.0, 8.0] {
                let t = throttle(v, w);
                assert_eq!(t % 5, 0);
                assert!((-90..=CRUISE).contains(&t));
            }
        }
    }

    #[test]
    fn the_wanted_speed_follows_the_heading_and_the_plan() {
        let h = Hull::ROWBOAT;
        // on course it asks for a little over the top speed (the server caps the hull)
        assert!(wanted_speed(&h, 0.0, f32::INFINITY) > h.top);
        // the further off the bow, the slower; never below 40 percent
        let straight = wanted_speed(&h, 0.2, f32::INFINITY);
        assert!(wanted_speed(&h, 0.8, f32::INFINITY) < straight);
        assert!(wanted_speed(&h, 3.0, f32::INFINITY) >= 0.4 * h.top);
        // the route's plan wins when it asks for less
        assert_eq!(wanted_speed(&h, 0.0, 1.0), 1.0);
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
