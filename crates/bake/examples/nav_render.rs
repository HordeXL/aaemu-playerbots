//! Renders a part of the baked navigation mesh, and optionally one planned route, to a PNG.
//!
//! ```text
//! nav_render <nav_main_world.bin> <out.png> [--center X,Y] [--width METRES] [--size WxH]
//!            [--from X,Y --to X,Y]
//! ```
//!
//! Defaults: centred on Marianople (11230,11960), 440 m wide, 1600x1000 pixels, no route.
//!
//! * Dark background: no mesh there (water, cliffs, anything the client left out).
//! * Light triangles are the walkable network, shaded by their slope and drawn with thin edges;
//!   triangles near a road are amber. Slate triangles are not part of the main network (building
//!   footprints, islands). Dark lines are the walls: triangle edges that an agent cannot cross.
//!   Teal dots are indoor waypoints (building interiors, stairs, bridges).
//! * With `--from` / `--to` the route the bots walk (`find_path`: weighted A*, road bias, funnel
//!   smoothing) is the bold line, from the green start to the blue goal, above the dashed
//!   straight line for comparison.
//!
//! ```text
//! cargo run --release -p aa-bake --example nav_render -- \
//!     <data dir>/nav_main_world.bin docs/images/navmesh-route.png \
//!     --from 11380,11900 --to 11080,12030
//! ```

use aa_bake::map::Canvas;
use aa_world::geom::Vec3;
use aa_world::nav::{NavMesh, NodeRef, PathQuery, PathResult, DEFAULT_AGENT_RADIUS};
use anyhow::{bail, ensure, Context};
use std::collections::HashMap;

/// Supersampling factor per axis (anti-aliasing by averaging).
const SS: u32 = 3;

const BLOCKED: [u8; 3] = [16, 20, 28];
const WALKABLE: [f32; 3] = [172.0, 184.0, 198.0];
const WALL: [u8; 3] = [22, 28, 44];
const CLOSED: [f32; 3] = [70.0, 82.0, 104.0];
const ROAD: [f32; 3] = [226.0, 178.0, 84.0];
const WAYPOINT: [u8; 3] = [62, 168, 152];
const ROUTE: [u8; 3] = [255, 45, 117];
const CASING: [u8; 3] = [255, 255, 255];
const START: [u8; 3] = [46, 204, 113];
const GOAL: [u8; 3] = [41, 121, 255];

/// A triangle of the mesh that touches the view.
struct Tri {
    node: NodeRef,
    v: [Vec3; 3],
    road: bool,
    /// In the main component (the walkable network) and linked to a neighbour.
    main: bool,
}

/// World metres to pixels of the (supersampled) canvas.
struct View {
    min_x: f64,
    max_y: f64,
    px_per_m: f64,
}

impl View {
    fn px(&self, x: f32, y: f32) -> (f64, f64) {
        (
            (f64::from(x) - self.min_x) * self.px_per_m,
            (self.max_y - f64::from(y)) * self.px_per_m,
        )
    }
}

fn pair(s: &str) -> anyhow::Result<(f32, f32)> {
    let (a, b) = s.split_once(',').context("expected X,Y")?;
    Ok((a.trim().parse()?, b.trim().parse()?))
}

fn main() -> anyhow::Result<()> {
    let mut pos = Vec::new();
    let (mut center, mut width_m, mut size) = ((11230.0, 11960.0), 440.0f32, (1600u32, 1000u32));
    let (mut from, mut to) = (None, None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--center" => center = pair(&value()?)?,
            "--width" => width_m = value()?.parse()?,
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context("--size WxH")?;
                size = (w.parse()?, h.parse()?);
            }
            "--from" => from = Some(pair(&value()?)?),
            "--to" => to = Some(pair(&value()?)?),
            _ if a.starts_with("--") => bail!("unknown option {a}"),
            _ => pos.push(a),
        }
    }
    ensure!(
        pos.len() == 2 && from.is_some() == to.is_some() && width_m > 1.0,
        "usage: nav_render <nav_main_world.bin> <out.png> [--center X,Y] [--width METRES] \
         [--size WxH] [--from X,Y --to X,Y]"
    );
    let mesh = NavMesh::open(std::path::Path::new(&pos[0]))?;

    let (w, h) = (size.0 * SS, size.1 * SS);
    let view = View {
        min_x: f64::from(center.0 - width_m / 2.0),
        max_y: f64::from(center.1)
            + f64::from(width_m) * f64::from(size.1) / f64::from(size.0) / 2.0,
        px_per_m: f64::from(w) / f64::from(width_m),
    };
    let height_m = f64::from(h) / view.px_per_m;
    let (min_y, max_x) = (view.max_y - height_m, view.min_x + f64::from(width_m));

    // Collect the nodes that touch the view: triangles (with their centroid height for the
    // painter's order) and indoor waypoints.
    let mut tris: Vec<Tri> = Vec::new();
    let mut waypoints: Vec<Vec3> = Vec::new();
    for i in 0..mesh.node_count() {
        let n = NodeRef(i as u32);
        let in_view = |x: f32, y: f32| {
            f64::from(x) >= view.min_x - 1.0
                && f64::from(x) <= max_x + 1.0
                && f64::from(y) >= min_y - 1.0
                && f64::from(y) <= view.max_y + 1.0
        };
        match mesh.triangle(n) {
            Some(t) => {
                let (lo_x, hi_x) = t
                    .iter()
                    .fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(v.x), h.max(v.x)));
                let (lo_y, hi_y) = t
                    .iter()
                    .fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(v.y), h.max(v.y)));
                let overlaps = f64::from(hi_x) >= view.min_x
                    && f64::from(lo_x) <= max_x
                    && f64::from(hi_y) >= min_y
                    && f64::from(lo_y) <= view.max_y;
                if overlaps {
                    let road = mesh.node_flags(n) & aa_world::nav::format::flag::ROAD != 0;
                    let main = mesh.component_of(n) == 0 && mesh.degree(n) > 0;
                    tris.push(Tri {
                        node: n,
                        v: t,
                        road,
                        main,
                    });
                }
            }
            None => {
                let p = mesh.node_pos(n);
                if mesh.is_waypoint(n) && in_view(p.x, p.y) {
                    waypoints.push(p);
                }
            }
        }
    }
    tris.sort_by(|a, b| {
        let (za, zb) = (
            a.v.iter().map(|v| v.z).sum::<f32>(),
            b.v.iter().map(|v| v.z).sum::<f32>(),
        );
        za.total_cmp(&zb)
    });
    eprintln!(
        "{} triangles, {} waypoints in view",
        tris.len(),
        waypoints.len()
    );

    let mut canvas = Canvas::new(w, h, BLOCKED);
    for t in &tris {
        fill_triangle(&mut canvas, &view, t);
    }
    for (a, b) in blocked_edges(&mesh, &tris) {
        let (a, b) = (view.px(a.x, a.y), view.px(b.x, b.y));
        line(&mut canvas, a, b, f64::from(SS) * 1.3, WALL);
    }
    for p in &waypoints {
        let (x, y) = view.px(p.x, p.y);
        dot(&mut canvas, (x, y), f64::from(SS) * 0.9, WAYPOINT, WAYPOINT);
    }

    if let (Some(f), Some(t)) = (from, to) {
        let (f, t) = (lift(&tris, f), lift(&tris, t));
        let q = PathQuery::new(f, t);
        let path = match mesh.find_path(&q) {
            PathResult::Found(p) => p,
            other => bail!("no route from {f:?} to {t:?}: {other:?}"),
        };
        eprintln!(
            "route: {} corners, {:.0} m, {} expansions (straight line {:.0} m)",
            path.points.len(),
            path.length,
            path.expansions,
            f.dist2d(t)
        );
        let s = f64::from(SS);
        dashed(&mut canvas, &view, f, t, s * 1.1);
        for pair in path.points.windows(2) {
            let (a, b) = (view.px(pair[0].x, pair[0].y), view.px(pair[1].x, pair[1].y));
            line(&mut canvas, a, b, s * 3.6, CASING);
        }
        for pair in path.points.windows(2) {
            let (a, b) = (view.px(pair[0].x, pair[0].y), view.px(pair[1].x, pair[1].y));
            line(&mut canvas, a, b, s * 2.3, ROUTE);
        }
        let (x, y) = view.px(f.x, f.y);
        dot(&mut canvas, (x, y), s * 7.0, START, CASING);
        let (x, y) = view.px(t.x, t.y);
        dot(&mut canvas, (x, y), s * 7.0, GOAL, CASING);
    }

    let mut out = downsample(&canvas, SS);
    scale_bar(&mut out, width_m);
    out.write_png(std::path::Path::new(&pos[1]))?;
    eprintln!("wrote {} ({}x{})", pos[1], out.width, out.height);
    Ok(())
}

/// The triangle edges that an agent cannot cross: mesh borders, and edges whose two triangles are
/// not linked (cliffs, water, walls, anything the client marked blocked).
fn blocked_edges(mesh: &NavMesh, tris: &[Tri]) -> Vec<(Vec3, Vec3)> {
    let key = |a: Vec3, b: Vec3| {
        let (a, b) = (
            (a.x.to_bits(), a.y.to_bits()),
            (b.x.to_bits(), b.y.to_bits()),
        );
        if a <= b {
            (a, b)
        } else {
            (b, a)
        }
    };
    let mut by_edge: HashMap<_, Vec<usize>> = HashMap::new();
    for (i, t) in tris.iter().enumerate() {
        for e in 0..3 {
            by_edge
                .entry(key(t.v[e], t.v[(e + 1) % 3]))
                .or_default()
                .push(i);
        }
    }
    let mut walls = Vec::new();
    for (i, t) in tris.iter().enumerate() {
        let mut open = mesh.neighbours(t.node, DEFAULT_AGENT_RADIUS);
        open.sort();
        for e in 0..3 {
            let (a, b) = (t.v[e], t.v[(e + 1) % 3]);
            let linked = by_edge[&key(a, b)]
                .iter()
                .any(|&j| j != i && open.binary_search(&tris[j].node).is_ok());
            // each edge once: only the lower triangle of a pair draws it
            let lowest = by_edge[&key(a, b)].iter().all(|&j| j >= i);
            if !linked && lowest {
                walls.push((a, b));
            }
        }
    }
    walls
}

/// Adds the height of the triangle under (x, y) (the nearest corner's when none is).
fn lift(tris: &[Tri], (x, y): (f32, f32)) -> Vec3 {
    let mut best = (f32::MAX, 0.0);
    for tri in tris {
        let [a, b, c] = tri.v;
        let d = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
        if d.abs() > 1e-6 {
            let l1 = ((b.y - c.y) * (x - c.x) + (c.x - b.x) * (y - c.y)) / d;
            let l2 = ((c.y - a.y) * (x - c.x) + (a.x - c.x) * (y - c.y)) / d;
            if l1 >= 0.0 && l2 >= 0.0 && l1 + l2 <= 1.0 {
                return Vec3::new(x, y, l1 * a.z + l2 * b.z + (1.0 - l1 - l2) * c.z);
            }
        }
        for v in &tri.v {
            let dist = (v.x - x).hypot(v.y - y);
            if dist < best.0 {
                best = (dist, v.z);
            }
        }
    }
    Vec3::new(x, y, best.1)
}

/// A shaded triangle (light from the north-west) with a slightly darker outline.
fn fill_triangle(c: &mut Canvas, view: &View, tri: &Tri) {
    let (t, road) = (&tri.v, tri.road);
    let u = [t[1].x - t[0].x, t[1].y - t[0].y, t[1].z - t[0].z];
    let v = [t[2].x - t[0].x, t[2].y - t[0].y, t[2].z - t[0].z];
    let mut n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    if n[2] < 0.0 {
        n = n.map(|x| -x);
    }
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
    let shade = ((-0.5 * n[0] + 0.5 * n[1] + 0.7 * n[2]) / len / 0.99).max(0.0);
    let k = 0.62 + 0.5 * shade;
    let base = if !tri.main {
        CLOSED
    } else if road {
        ROAD
    } else {
        WALKABLE
    };
    let fill = base.map(|v| (v * k).clamp(0.0, 255.0) as u8);
    let edge = fill.map(|v| (f32::from(v) * 0.7) as u8);

    let p = t.map(|v| view.px(v.x, v.y));
    let (lo_x, hi_x) = (
        p.iter()
            .map(|q| q.0)
            .fold(f64::MAX, f64::min)
            .floor()
            .max(0.0),
        p.iter()
            .map(|q| q.0)
            .fold(f64::MIN, f64::max)
            .ceil()
            .min(f64::from(c.width)),
    );
    let (lo_y, hi_y) = (
        p.iter()
            .map(|q| q.1)
            .fold(f64::MAX, f64::min)
            .floor()
            .max(0.0),
        p.iter()
            .map(|q| q.1)
            .fold(f64::MIN, f64::max)
            .ceil()
            .min(f64::from(c.height)),
    );
    let area = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[2].0 - p[0].0) * (p[1].1 - p[0].1);
    if area.abs() < 1e-9 {
        return;
    }
    let edge_fn = |a: (f64, f64), b: (f64, f64), x: f64, y: f64| {
        (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0)
    };
    for y in lo_y as i64..hi_y as i64 {
        for x in lo_x as i64..hi_x as i64 {
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let w0 = edge_fn(p[1], p[2], fx, fy) / area;
            let w1 = edge_fn(p[2], p[0], fx, fy) / area;
            let w2 = edge_fn(p[0], p[1], fx, fy) / area;
            if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                c.put(x, y, fill);
            }
        }
    }
    for i in 0..3 {
        line(c, p[i], p[(i + 1) % 3], 0.5 * f64::from(SS) / 2.0, edge);
    }
}

/// A line with round caps; `r` is the half width in canvas pixels.
fn line(c: &mut Canvas, a: (f64, f64), b: (f64, f64), r: f64, color: [u8; 3]) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let lo_x = (a.0.min(b.0) - r - 1.0).floor().max(0.0) as i64;
    let hi_x = (a.0.max(b.0) + r + 1.0).ceil().min(f64::from(c.width)) as i64;
    let lo_y = (a.1.min(b.1) - r - 1.0).floor().max(0.0) as i64;
    let hi_y = (a.1.max(b.1) + r + 1.0).ceil().min(f64::from(c.height)) as i64;
    for y in lo_y..hi_y {
        for x in lo_x..hi_x {
            let (px, py) = (x as f64 + 0.5 - a.0, y as f64 + 0.5 - a.1);
            let t = if len2 > 0.0 {
                ((px * dx + py * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            if (px - t * dx).hypot(py - t * dy) <= r {
                c.put(x, y, color);
            }
        }
    }
}

/// A filled circle of radius `r` with an outline about as wide as the route line.
fn dot(c: &mut Canvas, at: (f64, f64), r: f64, fill: [u8; 3], outline: [u8; 3]) {
    line(c, at, at, r + f64::from(SS) * 1.4, outline);
    line(c, at, at, r, fill);
}

/// The straight line start-goal as dashes.
fn dashed(c: &mut Canvas, view: &View, from: Vec3, to: Vec3, r: f64) {
    let (a, b) = (view.px(from.x, from.y), view.px(to.x, to.y));
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    let (dash, gap) = (9.0 * r, 6.0 * r);
    let mut d = 0.0;
    while d < len {
        let e = (d + dash).min(len);
        let at = |s: f64| (a.0 + (b.0 - a.0) * s / len, a.1 + (b.1 - a.1) * s / len);
        line(c, at(d), at(e), r, CASING);
        d += dash + gap;
    }
}

/// Averages `ss` x `ss` blocks.
fn downsample(c: &Canvas, ss: u32) -> Canvas {
    let (w, h) = (c.width / ss, c.height / ss);
    let mut out = Canvas::new(w, h, [0, 0, 0]);
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0u32; 3];
            for sy in 0..ss {
                for sx in 0..ss {
                    let px = c.get(x * ss + sx, y * ss + sy);
                    for i in 0..3 {
                        sum[i] += u32::from(px[i]);
                    }
                }
            }
            out.put(
                i64::from(x),
                i64::from(y),
                sum.map(|s| (s / (ss * ss)) as u8),
            );
        }
    }
    out
}

/// 3x5 glyphs of the digits and "m" (one row per entry, bit 2 = left column).
const GLYPHS: [[u8; 5]; 11] = [
    [7, 5, 5, 5, 7],
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
    [7, 1, 1, 1, 1],
    [7, 5, 7, 5, 7],
    [7, 5, 7, 1, 7],
    [5, 7, 7, 5, 5],
];

/// A scale bar of a round length (about a fifth of the width) with its label, bottom left.
fn scale_bar(c: &mut Canvas, width_m: f32) {
    let metres = [10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 500.0, 1000.0]
        .into_iter()
        .rev()
        .find(|m| *m <= width_m / 4.0)
        .unwrap_or(10.0);
    let len = (metres / width_m * c.width as f32) as i64;
    let (x0, y0) = (36, c.height as i64 - 40);
    let ink = [235, 240, 245];
    for x in x0..=x0 + len {
        for dy in 0..3 {
            c.put(x, y0 + dy, ink);
        }
    }
    for x in [x0, x0 + len] {
        for dy in -6..=3 {
            c.put(x, y0 + dy, ink);
        }
    }
    let label: Vec<usize> = format!("{metres:.0}")
        .bytes()
        .map(|b| (b - b'0') as usize)
        .chain([10])
        .collect();
    let px = 3;
    for (i, g) in label.iter().enumerate() {
        for (row, bits) in GLYPHS[*g].iter().enumerate() {
            for col in 0..3 {
                if bits & (4 >> col) != 0 {
                    for dy in 0..px {
                        for dx in 0..px {
                            c.put(
                                x0 + 6 + (i as i64 * 4 + col) * px + dx,
                                y0 - 12 - 5 * px + row as i64 * px + dy,
                                ink,
                            );
                        }
                    }
                }
            }
        }
    }
}
