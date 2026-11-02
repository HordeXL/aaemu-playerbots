//! The 2D "funnel" (string pulling) over a corridor of triangle portals.
//!
//! Ported from the AAEmu Rust server (`aaemu-game/src/systems/ai/geodata/funnel.rs`,
//! GPL-3.0-or-later), which in turn follows Mikko Mononen's "simple stupid funnel algorithm".
//! Portals are shrunk by the agent radius so the string keeps its distance from the edge corners.

use crate::geom::Vec3;

type P2 = (f64, f64);

fn p2(v: Vec3) -> P2 {
    (f64::from(v.x), f64::from(v.y))
}

/// Z component of (a - o) x (b - o): positive when `b` is counter-clockwise (left) of `o -> a`.
pub(crate) fn cross(o: P2, a: P2, b: P2) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

/// An edge the route crosses, seen in the direction of travel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Portal {
    pub left: Vec3,
    pub right: Vec3,
    /// The corner is an obstacle (a vertex on the border of the mesh): the path keeps the agent
    /// radius away from it. Corners inside the walkable mesh need no clearance.
    pub left_hard: bool,
    pub right_hard: bool,
}

impl Portal {
    /// A portal with two obstacle corners.
    pub fn hard(left: Vec3, right: Vec3) -> Self {
        Portal {
            left,
            right,
            left_hard: true,
            right_hard: true,
        }
    }

    /// The portal with its obstacle corners moved `radius` towards the other end (both to the
    /// midpoint when both are obstacles and the edge is shorter than the agent): the string
    /// keeps that distance from them. Corners that are not obstacles stay where they are.
    pub fn shrunk(self, radius: f32) -> Self {
        let (l, r) = (p2(self.left), p2(self.right));
        let len = ((l.0 - r.0).powi(2) + (l.1 - r.1).powi(2)).sqrt();
        let lerp = |a: Vec3, b: Vec3, t: f64| {
            Vec3::new(
                (f64::from(a.x) + (f64::from(b.x) - f64::from(a.x)) * t) as f32,
                (f64::from(a.y) + (f64::from(b.y) - f64::from(a.y)) * t) as f32,
                (f64::from(a.z) + (f64::from(b.z) - f64::from(a.z)) * t) as f32,
            )
        };
        let r = f64::from(radius.max(0.0));
        match (self.left_hard, self.right_hard) {
            (false, false) => self,
            (true, true) => {
                if len <= 2.0 * r + 1e-6 {
                    let m = lerp(self.left, self.right, 0.5);
                    return Self {
                        left: m,
                        right: m,
                        ..self
                    };
                }
                let t = r / len;
                Self {
                    left: lerp(self.left, self.right, t),
                    right: lerp(self.right, self.left, t),
                    ..self
                }
            }
            (true, false) => Self {
                left: lerp(
                    self.left,
                    self.right,
                    if len > 0.0 { (r / len).min(1.0) } else { 0.0 },
                ),
                ..self
            },
            (false, true) => Self {
                right: lerp(
                    self.right,
                    self.left,
                    if len > 0.0 { (r / len).min(1.0) } else { 0.0 },
                ),
                ..self
            },
        }
    }

    /// The portal of the edge (`a1`, `a2`) leaving a triangle whose third corner is `apex`:
    /// left and right as seen when walking out across the edge.
    pub fn across(apex: Vec3, (a1, hard1): (Vec3, bool), (a2, hard2): (Vec3, bool)) -> Self {
        let (o, p1, p2_) = (p2(apex), p2(a1), p2(a2));
        let mid = ((p1.0 + p2_.0) / 2.0, (p1.1 + p2_.1) / 2.0);
        if cross(o, mid, p1) >= 0.0 {
            Portal {
                left: a1,
                right: a2,
                left_hard: hard1,
                right_hard: hard2,
            }
        } else {
            Portal {
                left: a2,
                right: a1,
                left_hard: hard2,
                right_hard: hard1,
            }
        }
    }
}

/// Twice the signed area of (a, b, c), negative when `c` is counter-clockwise of `a -> b`
/// (the "simple stupid funnel" convention).
fn tri2(a: P2, b: P2, c: P2) -> f64 {
    (c.0 - a.0) * (b.1 - a.1) - (b.0 - a.0) * (c.1 - a.1)
}

fn same(a: P2, b: P2) -> bool {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) < 1e-10
}

/// The shortest 2D path from `start` to `goal` through the portals: appends the corners the
/// string bends around and finally `goal` to `out` (not `start`). Returns `false` (leaving `out`
/// as it was) only when the funnel does not terminate, which a sane corridor never does.
pub(crate) fn string_pull(
    start: Vec3,
    goal: Vec3,
    portals: &[Portal],
    out: &mut Vec<Vec3>,
) -> bool {
    let first = out.len();
    let n = portals.len() + 2;
    let at = |i: usize| -> Portal {
        if i == 0 {
            Portal::hard(start, start)
        } else if i == n - 1 {
            Portal::hard(goal, goal)
        } else {
            portals[i - 1]
        }
    };
    let (mut apex, mut left, mut right) = (start, start, start);
    let (mut li, mut ri) = (0usize, 0usize);
    let mut i = 1usize;
    let mut budget = n * n + 16;
    while i < n {
        if budget == 0 {
            out.truncate(first);
            return false;
        }
        budget -= 1;
        let p = at(i);
        let (l, r) = (p.left, p.right);
        // tighten the right side
        if tri2(p2(apex), p2(right), p2(r)) <= 0.0 {
            if same(p2(apex), p2(right)) || tri2(p2(apex), p2(left), p2(r)) > 0.0 {
                right = r;
                ri = i;
            } else {
                // the right side crossed over the left: the left corner is a path corner
                out.push(left);
                apex = left;
                right = apex;
                ri = li;
                i = li + 1;
                continue;
            }
        }
        // tighten the left side
        if tri2(p2(apex), p2(left), p2(l)) >= 0.0 {
            if same(p2(apex), p2(left)) || tri2(p2(apex), p2(right), p2(l)) < 0.0 {
                left = l;
                li = i;
            } else {
                out.push(right);
                apex = right;
                left = apex;
                li = ri;
                i = ri + 1;
                continue;
            }
        }
        i += 1;
    }
    if out.len() == first || out.last().is_none_or(|p| !same(p2(*p), p2(goal))) {
        out.push(goal);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32) -> Vec3 {
        Vec3::new(x, y, 0.0)
    }

    fn xy(p: &[Vec3]) -> Vec<(f32, f32)> {
        p.iter().map(|p| (p.x, p.y)).collect()
    }

    fn pull(start: Vec3, goal: Vec3, portals: &[Portal]) -> Vec<Vec3> {
        let mut out = Vec::new();
        assert!(string_pull(start, goal, portals, &mut out));
        out
    }

    #[test]
    fn collinear_portals_give_the_straight_line() {
        // a 10 m wide corridor along +x
        let portals: Vec<Portal> = (1..4)
            .map(|k| Portal::hard(v(10.0 * k as f32, 5.0), v(10.0 * k as f32, -5.0)))
            .collect();
        let p = pull(v(0.0, 0.0), v(40.0, 0.0), &portals);
        assert_eq!(xy(&p), [(40.0, 0.0)]);
    }

    #[test]
    fn single_portal_and_empty_corridor() {
        let portal = Portal::hard(v(5.0, 5.0), v(5.0, -5.0));
        assert_eq!(
            xy(&pull(v(0.0, 0.0), v(10.0, 3.0), &[portal])),
            [(10.0, 3.0)]
        );
        assert_eq!(xy(&pull(v(0.0, 0.0), v(10.0, 3.0), &[])), [(10.0, 3.0)]);
    }

    #[test]
    fn corridor_corner_is_pulled_tight_around_the_corner_vertex() {
        // an L-shaped corridor: east along y in [0, 10], then north through x in [20, 30]
        let portals = [
            Portal::hard(v(10.0, 10.0), v(10.0, 0.0)),
            // travelling east then turning north: the corner is the inner (left) vertex
            Portal::hard(v(20.0, 10.0), v(30.0, 10.0)),
        ];
        let p = pull(v(0.0, 0.0), v(25.0, 40.0), &portals);
        assert_eq!(xy(&p), [(20.0, 10.0), (25.0, 40.0)]);
    }

    #[test]
    fn shrunk_portals_keep_the_radius_from_the_corners() {
        let s = Portal::hard(v(0.0, 10.0), v(0.0, 0.0)).shrunk(0.25);
        assert_eq!((s.left.y, s.right.y), (9.75, 0.25));
        // narrower than the agent: both ends at the midpoint
        let s = Portal::hard(v(0.0, 0.4), v(0.0, 0.0)).shrunk(0.25);
        assert_eq!((s.left.y, s.right.y), (0.2, 0.2));
    }

    #[test]
    fn only_obstacle_corners_are_shrunk() {
        let mk = |lh, rh| Portal {
            left: v(0.0, 10.0),
            right: v(0.0, 0.0),
            left_hard: lh,
            right_hard: rh,
        };
        let s = mk(false, false).shrunk(0.5);
        assert_eq!(
            (s.left.y, s.right.y),
            (10.0, 0.0),
            "open mesh: no clearance needed"
        );
        let s = mk(true, false).shrunk(0.5);
        assert_eq!((s.left.y, s.right.y), (9.5, 0.0));
        let s = mk(false, true).shrunk(0.5);
        assert_eq!((s.left.y, s.right.y), (10.0, 0.5));
        // an edge shorter than the radius: the obstacle end goes to the other end
        let s = Portal {
            left: v(0.0, 0.3),
            right: v(0.0, 0.0),
            left_hard: true,
            right_hard: false,
        }
        .shrunk(0.5);
        assert_eq!((s.left.y, s.right.y), (0.0, 0.0));
    }

    #[test]
    fn across_orients_left_and_right_by_travel_direction() {
        // walking north out of a triangle with the apex at the south
        let apex = v(0.0, 0.0);
        let p = Portal::across(apex, (v(-1.0, 5.0), true), (v(1.0, 5.0), true));
        assert_eq!((p.left.x, p.right.x), (-1.0, 1.0));
        let p = Portal::across(apex, (v(1.0, 5.0), true), (v(-1.0, 5.0), true));
        assert_eq!((p.left.x, p.right.x), (-1.0, 1.0));
    }

    #[test]
    fn pulls_around_a_wall_end() {
        // wall end at (10, 0): the corridor is above it, portal from (10, 0) (left) to (10, 10)
        // travelling east: right is the lower point? facing +x, left is +y.
        let portals = [Portal::hard(v(10.0, 10.0), v(10.0, 0.0))];
        // goal below the wall end line: the string must bend at the wall end
        let p = pull(v(0.0, 5.0), v(20.0, -10.0), &portals);
        assert_eq!(xy(&p), [(10.0, 0.0), (20.0, -10.0)]);
    }
}
