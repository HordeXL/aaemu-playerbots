//! A small model of the server's ship physics for the tests of the helm and the sail goal (test
//! builds only). It mirrors the parts of `systems/ai/physics/ship.rs` that matter for steering:
//! the helm input smoothed by 0.12 per 25 Hz tick, the steering value as an angular acceleration
//! request with an approach factor towards the turn cap, the faster counter-steer, the damping
//! with a centred helm, and a turn factor that falls with speed. `sign` is the *true* direction of
//! the wheel: +1 means a positive steering lowers the yaw (the documented direction).

use std::collections::VecDeque;

use aa_proto::sc_travel::ShipMotion;

const DT: f32 = 0.04;

/// The simulated hull.
#[derive(Debug, Clone)]
pub struct Plant {
    pub pos: (f32, f32),
    pub yaw: f32,
    pub speed: f32,
    /// Turn rate the helm builds up (positive = the way a positive steering turns).
    pub rot: f32,
    pub time_ms: u32,
    sign: i8,
    accel: f32,
    cap: f32,
    top_speed: f32,
    req: (f32, f32),
    smooth: (f32, f32),
    hist: VecDeque<(f32, f32)>,
}

impl Plant {
    /// A hull at rest at the origin heading `yaw`, with the given wheel direction.
    pub fn new(yaw: f32, sign: i8) -> Plant {
        Plant {
            pos: (0.0, 0.0),
            yaw,
            speed: 0.0,
            rot: 0.0,
            time_ms: 0,
            sign,
            accel: 0.125,
            cap: 0.0759,
            top_speed: 6.4,
            req: (0.0, 0.0),
            smooth: (0.0, 0.0),
            hist: VecDeque::new(),
        }
    }

    /// Sets the turn acceleration at full helm and full turn factor (rad/s^2) and the turn rate
    /// the helm settles at (rad/s); the default is the rowboat's.
    pub fn hull(mut self, accel: f32, cap: f32) -> Plant {
        self.accel = accel;
        self.cap = cap;
        self
    }

    /// Sets the top speed (m/s).
    pub fn top(mut self, top_speed: f32) -> Plant {
        self.top_speed = top_speed;
        self
    }

    /// Places the hull.
    pub fn at(mut self, x: f32, y: f32) -> Plant {
        self.pos = (x, y);
        self
    }

    /// The helm request the server stores until the next one.
    pub fn request(&mut self, throttle: i8, steering: i8) {
        self.req = (f32::from(throttle), f32::from(steering));
    }

    /// The applied (smoothed) throttle and steering, as `ShipMoved` reports them.
    pub fn applied(&self) -> (i8, i8) {
        (self.smooth.0.round() as i8, self.smooth.1.round() as i8)
    }

    /// Runs the physics for `secs` seconds in 25 Hz ticks.
    pub fn advance(&mut self, secs: f32) {
        let ticks = (secs / DT).round() as u32;
        for _ in 0..ticks {
            self.tick();
        }
    }

    fn tick(&mut self) {
        self.smooth.0 += (self.req.0 - self.smooth.0) * 0.12;
        self.smooth.1 += (self.req.1 - self.smooth.1) * 0.12;
        let steering = self.smooth.1.round();
        let norm = steering / 127.0;
        // (the server bumps a hull that is asked to go ahead to 1 m/s)
        let moving = if self.smooth.0.round() > 0.0 {
            self.speed.abs().max(1.0)
        } else {
            self.speed.abs()
        };
        let turn_factor = 0.5 + 0.5 * (moving / 2.5).clamp(0.0, 1.0);
        let cap = self.cap * turn_factor;
        let mut delta = norm * self.accel * DT * turn_factor;
        if self.rot != 0.0 && self.rot.signum() != norm.signum() && norm != 0.0 {
            delta *= 1.35;
        }
        if norm != 0.0 && (self.rot == 0.0 || self.rot.signum() == delta.signum()) {
            let n = (self.rot.abs() / cap).clamp(0.0, 1.0);
            delta *= (1.0 - n * n).clamp(0.1, 1.0);
        }
        self.rot = (self.rot + delta).clamp(-2.0 * cap, 2.0 * cap);
        if steering != 0.0 {
            if self.rot.abs() > cap {
                let a = 1.0 - (-8.0 * DT).exp();
                self.rot += (self.rot.signum() * cap - self.rot) * a;
            }
        } else {
            self.rot *= 0.975;
        }
        self.yaw = super::wrap_pi(self.yaw - f32::from(self.sign) * self.rot * DT);
        let target = self.smooth.0 / 127.0 * self.top_speed;
        self.speed += (target - self.speed) * (1.0 - (-DT / 2.0).exp());
        self.pos.0 += -self.yaw.sin() * self.speed * DT;
        self.pos.1 += self.yaw.cos() * self.speed * DT;
        self.time_ms += 40;
        self.hist
            .push_back((self.time_ms as f32 / 1000.0, self.yaw));
        while self.hist.len() > 16 {
            self.hist.pop_front();
        }
    }

    /// The yaw rate as the bot measures it: the heading change over the last 0.3 s or more.
    pub fn yaw_rate_estimate(&self) -> f32 {
        let Some(&(t1, y1)) = self.hist.back() else {
            return 0.0;
        };
        let older = self
            .hist
            .iter()
            .rev()
            .find(|(t, _)| t1 - t >= 0.3)
            .or_else(|| self.hist.front());
        match older {
            Some(&(t0, y0)) if t1 > t0 => super::wrap_pi(y1 - y0) / (t1 - t0),
            _ => 0.0,
        }
    }

    /// The `ShipMoved` payload of the hull now, for ship object `obj`.
    pub fn motion(&self, obj: u32) -> ShipMotion {
        let (throttle, steering) = self.applied();
        ShipMotion {
            obj,
            time: self.time_ms,
            pos: (self.pos.0, self.pos.1, 100.5),
            velocity: [
                -self.yaw.sin() * self.speed,
                self.yaw.cos() * self.speed,
                0.0,
            ],
            speed: self.speed.abs(),
            yaw: self.yaw,
            ang_vel: [0.0, 0.0, -f32::from(self.sign) * self.rot],
            steering,
            throttle,
            zone: 1,
            stuck: false,
        }
    }
}
