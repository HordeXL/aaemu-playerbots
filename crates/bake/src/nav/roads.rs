//! `ROAD` flags for the main world's navigation mesh.
//!
//! Two sources describe where the roads run:
//!
//! * the road prefabs (object type 13) in the cell files `cells/<cx>_<cy>/client/object.dat`:
//!   a strip of paired left / right edge points (positions relative to the 1024 m cell); the
//!   centre line joins the pair midpoints and the strip width is the pair distance;
//! * the carriage routes of `level_design/zone/<id>/client/transfer_path.xml` (positions
//!   relative to the zone's origin cell). The file also holds air, ship and tiger routes: a
//!   route counts only when most of it lies on the mesh.
//!
//! Every navigation triangle that a road strip passes over (within a vertical tolerance, so a
//! bridge or a cliff above does not count) gets [`flag::ROAD`]; path finding then prefers links
//! between two such triangles ([`aa_world::nav::PathQuery::road_bias`]).
//!
//! The `object.dat` walker only follows the sizes of the prefab records (the same octree and
//! record layouts as the server's `aaemu-game/src/systems/ai/physics/objects_file.rs`) to reach
//! the road records; every size is checked, damaged data ends the walk and is never a panic.

use std::collections::HashMap;

use aa_world::nav::format::{flag, NavData};

use crate::pak::Pak;

/// Object type of the road prefab.
pub const OBJECT_ROAD: i32 = 13;
/// Cell edge length in metres.
pub const CELL_SIZE: f64 = 1024.0;
/// Distance between the sample points along a road in metres.
pub const SAMPLE_STEP: f32 = 2.0;
/// A road only marks triangles within this vertical distance.
pub const MAX_DZ: f32 = 6.0;
/// Triangles larger than this (m^2) are junk planes and never marked.
pub const MAX_TRIANGLE_AREA: f32 = 2000.0;
/// Fraction of a carriage route that must lie on the mesh for the route to count.
pub const MIN_TRANSFER_COVERAGE: f32 = 0.6;
/// Strip half width limits in metres (a road prefab with odd widths is clamped).
pub const MIN_HALF_WIDTH: f32 = 1.5;
pub const MAX_HALF_WIDTH: f32 = 12.0;
/// Half width of a carriage route (it has no width of its own).
pub const TRANSFER_HALF_WIDTH: f32 = 3.5;

/// A road centre line in world coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Road {
    pub points: Vec<[f32; 3]>,
    pub half_width: f32,
}

impl Road {
    /// Length of the centre line in the horizontal plane.
    pub fn length(&self) -> f64 {
        self.points
            .windows(2)
            .map(|w| f64::from(dist2(w[0], w[1])))
            .sum()
    }
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// What flagging the roads found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoadReport {
    /// `object.dat` files read / not completely readable.
    pub cells_scanned: usize,
    pub cells_damaged: usize,
    /// Road prefabs found, the ones used and their centre line length (km).
    pub prefabs: usize,
    pub prefabs_used: usize,
    pub prefab_km: f64,
    /// Sample points of the prefab centre lines and how many lie on a mesh triangle (a check that
    /// the cell origin is right).
    pub prefab_samples: usize,
    pub prefab_samples_on_mesh: usize,
    /// `transfer_path.xml` files, routes in them, routes that lie on the mesh, their length (km).
    pub transfer_files: usize,
    pub transfer_routes: usize,
    pub transfer_used: usize,
    pub transfer_km: f64,
    /// Sample points of the used roads and how many of those lie on a mesh triangle.
    pub samples: usize,
    pub samples_on_mesh: usize,
    pub nodes_flagged: usize,
    pub nodes_total: usize,
    pub warnings: Vec<String>,
}

impl std::fmt::Display for RoadReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "  roads: {} object.dat read ({} damaged), {} road prefabs ({} used, {:.0} km, {}/{} samples on the mesh); {} transfer files, {}/{} carriage routes on the mesh ({:.0} km); {}/{} samples on the mesh -> {} of {} nodes flagged",
            self.cells_scanned,
            self.cells_damaged,
            self.prefabs,
            self.prefabs_used,
            self.prefab_km,
            self.prefab_samples_on_mesh,
            self.prefab_samples,
            self.transfer_files,
            self.transfer_used,
            self.transfer_routes,
            self.transfer_km,
            self.samples_on_mesh,
            self.samples,
            self.nodes_flagged,
            self.nodes_total
        )?;
        for w in &self.warnings {
            writeln!(f, "  warning: {w}")?;
        }
        Ok(())
    }
}

// ---- object.dat ------------------------------------------------------------------------------

fn i32_at(b: &[u8], o: usize) -> Option<i32> {
    let s = b.get(o..o.checked_add(4)?)?;
    Some(i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn f32_at(b: &[u8], o: usize) -> Option<f32> {
    i32_at(b, o).map(|v| f32::from_bits(v as u32))
}

/// Record sizes of the prefab types that have a fixed one (the type itself is in the first four
/// bytes).
fn fixed_size(t: i32) -> usize {
    match t {
        2 => 68,
        4 => 100,
        5 => 124,
        8 => 196,
        9 => 115,
        14 => 83,
        27 => 652,
        _ => 0,
    }
}

/// Walks one octree node's block of prefab records, collecting the road point lists. The walk
/// ends early at an unknown or damaged record; the roads read before it are kept.
fn scan_block(b: &[u8], roads: &mut Vec<Vec<[f32; 3]>>) {
    const BRUSH: usize = 0x84;
    const WATER_START: usize = 0x7B;
    const ROAD_HEADER: usize = 67;
    const VOXEL_RANGES: usize = 198_779;
    let len = b.len();
    let mut off = 0usize;
    while off + 4 <= len {
        let Some(t) = i32_at(b, off) else { break };
        let size = match t {
            1 => BRUSH.min(len - off),
            6 => {
                let ranges_at = off + VOXEL_RANGES;
                let Some(n) = i32_at(b, ranges_at) else { break };
                let mut pos = ranges_at as i64 + 4;
                let mut ok = true;
                for _ in 0..n.max(0) {
                    let Some(sz) = usize::try_from(pos).ok().and_then(|p| i32_at(b, p)) else {
                        ok = false;
                        break;
                    };
                    if sz < 0 {
                        ok = false;
                        break;
                    }
                    pos += 4 + i64::from(sz);
                }
                if !ok {
                    break;
                }
                usize::try_from(pos - off as i64)
                    .unwrap_or(0)
                    .min(len - off)
            }
            11 => {
                if off + WATER_START > len {
                    break;
                }
                let (Some(sc), Some(cc)) = (i32_at(b, off + 0x6B), i32_at(b, off + 0x77)) else {
                    break;
                };
                let total = i64::from(sc) * 12 + i64::from(cc) * 12 + WATER_START as i64;
                if off as i64 + total > len as i64 {
                    len - off
                } else if sc > 0 || cc > 0 {
                    usize::try_from(total).unwrap_or(0)
                } else {
                    WATER_START
                }
            }
            OBJECT_ROAD => {
                if off + ROAD_HEADER > len {
                    break;
                }
                let count = usize::from(b[off + 43]);
                let total = count * 12 + ROAD_HEADER;
                if off + total > len {
                    len - off
                } else {
                    if count > 0 {
                        let pts: Option<Vec<[f32; 3]>> = (0..count)
                            .map(|i| {
                                let o = off + ROAD_HEADER + i * 12;
                                Some([f32_at(b, o)?, f32_at(b, o + 4)?, f32_at(b, o + 8)?])
                            })
                            .collect();
                        if let Some(p) = pts {
                            roads.push(p);
                        }
                    }
                    total
                }
            }
            other => {
                let n = fixed_size(other);
                if n == 0 || off + n > len {
                    break;
                }
                n
            }
        };
        if size == 0 {
            break;
        }
        off += size;
    }
}

/// Deepest octree followed (the real files are about 8 deep).
const MAX_DEPTH: u32 = 32;

/// `(offset after the node, success)`; a node is 33 header bytes (block size at +28, child mask at
/// +32), its block of records and up to eight child nodes.
fn scan_node(d: &[u8], start: usize, depth: u32, roads: &mut Vec<Vec<[f32; 3]>>) -> (usize, bool) {
    const HEADER: usize = 33;
    let len = d.len();
    if depth > MAX_DEPTH || start + HEADER > len {
        return (start.min(len), false);
    }
    let size = i32_at(d, start + 28).unwrap_or(-1);
    let mask = d[start + 32];
    let content = start + HEADER;
    let mut cur = content;
    if size > 0 {
        let size = size as usize;
        if content + size > len {
            return (len, false);
        }
        scan_block(&d[content..content + size], roads);
        cur = content + size;
    }
    if mask > 0 {
        for i in 0..8 {
            if mask & (1 << i) != 0 {
                let (next, ok) = scan_node(d, cur, depth + 1, roads);
                if !ok {
                    return (len, false);
                }
                cur = next;
            }
        }
    }
    (cur, true)
}

/// The road point lists (cell-local positions, as stored) of one `object.dat`, and whether the
/// whole file was readable.
pub fn scan_object_dat(d: &[u8]) -> (Vec<Vec<[f32; 3]>>, bool) {
    let mut roads = Vec::new();
    if d.len() <= 8 {
        return (roads, true);
    }
    let walk = |roads: &mut Vec<Vec<[f32; 3]>>| -> Option<bool> {
        let assets = usize::try_from(i32_at(d, 0)? as u32).ok()?;
        let mut pos = 4usize.checked_add(assets.checked_mul(260)?)?;
        let prefabs = usize::try_from(i32_at(d, pos)? as u32).ok()?;
        pos = pos.checked_add(4)?.checked_add(prefabs.checked_mul(260)?)?;
        for _ in 0..prefabs {
            if pos >= d.len() {
                return Some(true);
            }
            let (next, ok) = scan_node(d, pos, 0, roads);
            if !ok {
                return Some(false);
            }
            pos = next;
        }
        Some(true)
    };
    let ok = walk(&mut roads).unwrap_or(false);
    (roads, ok)
}

/// The centre line of a road prefab: the pair midpoints of its (left, right) edge points, moved
/// from cell-local to world coordinates. `None` for fewer than two pairs.
pub fn prefab_road(points: &[[f32; 3]], cell: (i32, i32)) -> Option<Road> {
    let pairs = points.len() / 2;
    if pairs < 2 {
        return None;
    }
    let (ox, oy) = (
        (f64::from(cell.0) * CELL_SIZE) as f32,
        (f64::from(cell.1) * CELL_SIZE) as f32,
    );
    let mut line = Vec::with_capacity(pairs);
    let mut width = 0.0f32;
    for &[l, r] in points.as_chunks::<2>().0 {
        width += dist2(l, r);
        line.push([
            (l[0] + r[0]) * 0.5 + ox,
            (l[1] + r[1]) * 0.5 + oy,
            (l[2] + r[2]) * 0.5,
        ]);
    }
    let half = (width / pairs as f32 * 0.5).clamp(MIN_HALF_WIDTH, MAX_HALF_WIDTH);
    line.iter()
        .all(|p| p.iter().all(|v| v.is_finite()))
        .then_some(Road {
            points: line,
            half_width: half,
        })
}

/// `cx_cy` of a cell folder name.
fn parse_cell(name: &str) -> Option<(i32, i32)> {
    let (x, y) = name.split_once('_')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

// ---- transfer_path.xml -----------------------------------------------------------------------

fn parse_vec3(v: &str) -> Option<[f32; 3]> {
    let mut it = v
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<f32>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

/// The routes of a `transfer_path.xml` in world coordinates: every child of the root with the
/// `Pos` attributes of its grandchildren (positions relative to the zone's origin cell).
pub fn parse_transfer_routes(xml: &str, origin: (i32, i32)) -> Vec<Vec<[f32; 3]>> {
    let Ok(doc) = roxmltree::Document::parse(xml.trim_start_matches('\u{feff}')) else {
        return Vec::new();
    };
    let (ox, oy) = (
        (f64::from(origin.0) * CELL_SIZE) as f32,
        (f64::from(origin.1) * CELL_SIZE) as f32,
    );
    let mut out = Vec::new();
    for route in doc.root_element().children().filter(|n| n.is_element()) {
        let mut pts = Vec::new();
        for child in route.children().filter(|n| n.is_element()) {
            for node in child.children().filter(|n| n.is_element()) {
                if let Some(p) = node.attribute("Pos").and_then(parse_vec3) {
                    pts.push([p[0] + ox, p[1] + oy, p[2]]);
                }
            }
        }
        if pts.len() >= 2 {
            out.push(pts);
        }
    }
    out
}

// ---- flagging --------------------------------------------------------------------------------

/// What walking a road over the mesh found.
#[derive(Debug, Default)]
pub struct Probe {
    pub samples: usize,
    pub samples_on_mesh: usize,
    /// Triangles the strip passes over (may repeat).
    pub nodes: Vec<u32>,
}

/// The triangles under `(x, y, z)`: calls `hit(node)` for every triangle of the mesh that
/// contains the point (within 5 cm) and whose plane is within [`MAX_DZ`] of `z`. Returns the
/// number of hits.
fn triangles_at(d: &NavData, x: f32, y: f32, z: f32, mut hit: impl FnMut(u32)) -> usize {
    if d.grid_w == 0
        || d.grid_h == 0
        || d.grid_off.len() != d.grid_w as usize * d.grid_h as usize + 1
    {
        return 0;
    }
    let [min_x, min_y, max_x, max_y] = d.bounds;
    if x < min_x - 1.0 || x > max_x + 1.0 || y < min_y - 1.0 || y > max_y + 1.0 {
        return 0;
    }
    let col = (((f64::from(x) - f64::from(min_x)) / f64::from(d.bucket))
        .floor()
        .max(0.0) as usize)
        .min(d.grid_w as usize - 1);
    let row = (((f64::from(y) - f64::from(min_y)) / f64::from(d.bucket))
        .floor()
        .max(0.0) as usize)
        .min(d.grid_h as usize - 1);
    let cell = row * d.grid_w as usize + col;
    let (a, b) = (d.grid_off[cell] as usize, d.grid_off[cell + 1] as usize);
    let Some(items) = d.grid_items.get(a..b) else {
        return 0;
    };
    let mut hits = 0;
    for &n in items {
        let Some(t) = d.triangle(n as usize) else {
            continue;
        };
        let v = [d.vertices[t[0]], d.vertices[t[1]], d.vertices[t[2]]];
        // barycentric coordinates in the horizontal plane
        let det =
            (v[1][1] - v[2][1]) * (v[0][0] - v[2][0]) + (v[2][0] - v[1][0]) * (v[0][1] - v[2][1]);
        let area = det.abs() * 0.5;
        if area.is_nan() || area <= 1e-6 || area > MAX_TRIANGLE_AREA {
            continue;
        }
        let l0 = ((v[1][1] - v[2][1]) * (x - v[2][0]) + (v[2][0] - v[1][0]) * (y - v[2][1])) / det;
        let l1 = ((v[2][1] - v[0][1]) * (x - v[2][0]) + (v[0][0] - v[2][0]) * (y - v[2][1])) / det;
        let l2 = 1.0 - l0 - l1;
        // 5 cm tolerance: the barycentric weight is distance / height of the triangle
        let tol = -0.05 / (2.0 * area / edge_max(&v)).max(0.1);
        if l0 < tol || l1 < tol || l2 < tol {
            continue;
        }
        let pz = l0 * v[0][2] + l1 * v[1][2] + l2 * v[2][2];
        if (pz - z).abs() <= MAX_DZ {
            hit(n);
            hits += 1;
        }
    }
    hits
}

/// Longest edge of a triangle (the smallest height is `2 * area / longest edge`).
fn edge_max(v: &[[f32; 3]; 3]) -> f32 {
    dist2(v[0], v[1])
        .max(dist2(v[1], v[2]))
        .max(dist2(v[2], v[0]))
}

/// Walks the centre line of `road` (and a lane on either side) over the mesh.
pub fn probe(d: &NavData, road: &Road) -> Probe {
    let mut out = Probe::default();
    let offsets = [0.0f32, road.half_width * 0.6, -road.half_width * 0.6];
    for (si, seg) in road.points.windows(2).enumerate() {
        let (a, b) = (seg[0], seg[1]);
        let len = dist2(a, b);
        if !len.is_finite() {
            continue;
        }
        let n = (len / SAMPLE_STEP).ceil().max(1.0) as usize;
        let (dx, dy) = if len > 1e-4 {
            ((b[0] - a[0]) / len, (b[1] - a[1]) / len)
        } else {
            (1.0, 0.0)
        };
        // the first point of a segment is the last of the previous one
        let from = usize::from(si > 0);
        for i in from..=n {
            let t = i as f32 / n as f32;
            let p = [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ];
            out.samples += 1;
            for (k, off) in offsets.iter().enumerate() {
                let hits = triangles_at(d, p[0] - dy * off, p[1] + dx * off, p[2], |n| {
                    out.nodes.push(n)
                });
                if k == 0 && hits > 0 {
                    out.samples_on_mesh += 1;
                }
            }
        }
    }
    out
}

/// Marks the triangles of `nodes` as road. Returns how many were not marked before.
fn mark(d: &mut NavData, nodes: &[u32]) -> usize {
    let mut new = 0;
    for &n in nodes {
        if let Some(f) = d.node_flags.get_mut(n as usize) {
            if *f & flag::ROAD == 0 {
                *f |= flag::ROAD;
                new += 1;
            }
        }
    }
    new
}

/// Reads the roads of `world` from the pak and flags the mesh. Problems are collected as
/// warnings in the report; a world without road data just flags nothing.
pub fn flag_roads(
    pak: &mut Pak,
    world: &str,
    zone_origins: &HashMap<u32, (i32, i32)>,
    d: &mut NavData,
) -> RoadReport {
    let mut r = RoadReport {
        nodes_total: d.node_count(),
        ..Default::default()
    };
    let warn = |r: &mut RoadReport, m: String| {
        if r.warnings.len() < 10 {
            r.warnings.push(m);
        }
    };
    let prefix = format!("game/worlds/{world}/");

    // road prefabs of the cells
    let cells: Vec<(String, (i32, i32))> = pak
        .names_with_prefix(&format!("{prefix}cells/"))
        .into_iter()
        .filter_map(|n| {
            let rest = n.strip_prefix(&prefix)?.strip_prefix("cells/")?;
            let (folder, file) = rest.split_once('/')?;
            (file == "client/object.dat")
                .then(|| parse_cell(folder))
                .flatten()
                .map(|c| (n.to_string(), c))
        })
        .collect();
    for (name, cell) in &cells {
        let bytes = match pak.read(name) {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(e) => {
                warn(&mut r, format!("{name}: {e:#}"));
                continue;
            }
        };
        r.cells_scanned += 1;
        let (raw, ok) = scan_object_dat(&bytes);
        if !ok {
            r.cells_damaged += 1;
        }
        for pts in raw {
            r.prefabs += 1;
            let Some(road) = prefab_road(&pts, *cell) else {
                continue;
            };
            let p = probe(d, &road);
            r.prefabs_used += 1;
            r.prefab_km += road.length() / 1000.0;
            r.prefab_samples += p.samples;
            r.prefab_samples_on_mesh += p.samples_on_mesh;
            r.samples += p.samples;
            r.samples_on_mesh += p.samples_on_mesh;
            r.nodes_flagged += mark(d, &p.nodes);
        }
    }

    // carriage routes
    let transfers: Vec<(String, u32)> = pak
        .names_with_prefix(&format!("{prefix}level_design/zone/"))
        .into_iter()
        .filter_map(|n| {
            let rest = n
                .strip_prefix(&prefix)?
                .strip_prefix("level_design/zone/")?;
            let (zone, file) = rest.split_once('/')?;
            (file == "client/transfer_path.xml")
                .then(|| zone.parse::<u32>().ok())
                .flatten()
                .map(|z| (n.to_string(), z))
        })
        .collect();
    for (name, zone) in &transfers {
        let Some(origin) = zone_origins.get(zone) else {
            warn(&mut r, format!("{name}: zone {zone} is not in world.xml"));
            continue;
        };
        let text = match pak.read_text(name) {
            Ok(Some(t)) => t,
            Ok(None) => continue,
            Err(e) => {
                warn(&mut r, format!("{name}: {e:#}"));
                continue;
            }
        };
        r.transfer_files += 1;
        for pts in parse_transfer_routes(&text, *origin) {
            r.transfer_routes += 1;
            let road = Road {
                points: pts,
                half_width: TRANSFER_HALF_WIDTH,
            };
            let p = probe(d, &road);
            if p.samples == 0
                || (p.samples_on_mesh as f32) < MIN_TRANSFER_COVERAGE * p.samples as f32
            {
                continue;
            }
            r.transfer_used += 1;
            r.transfer_km += road.length() / 1000.0;
            r.samples += p.samples;
            r.samples_on_mesh += p.samples_on_mesh;
            r.nodes_flagged += mark(d, &p.nodes);
        }
    }
    r.nodes_total = d.node_count();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_i32(v: &mut Vec<u8>, x: i32) {
        v.extend_from_slice(&x.to_le_bytes());
    }

    fn put_f32(v: &mut Vec<u8>, x: f32) {
        v.extend_from_slice(&x.to_le_bytes());
    }

    /// A road record: 67 header bytes (type at 0, point count at 43) and the points.
    fn road_record(points: &[[f32; 3]]) -> Vec<u8> {
        let mut v = vec![0u8; 67];
        v[0..4].copy_from_slice(&OBJECT_ROAD.to_le_bytes());
        v[43] = points.len() as u8;
        for p in points {
            for c in p {
                put_f32(&mut v, *c);
            }
        }
        v
    }

    /// An octree node: 33 header bytes (block size at 28, child mask at 32), the block, children.
    fn node(block: &[u8], children: &[Vec<u8>]) -> Vec<u8> {
        let mut v = vec![0u8; 33];
        v[28..32].copy_from_slice(&(block.len() as i32).to_le_bytes());
        let mut mask = 0u8;
        for i in 0..children.len() {
            mask |= 1 << (i * 3 % 8);
        }
        v[32] = mask;
        v.extend_from_slice(block);
        for c in children {
            v.extend_from_slice(c);
        }
        v
    }

    /// An `object.dat` with no assets and `nodes.len()` root nodes.
    fn object_dat(nodes: &[Vec<u8>]) -> Vec<u8> {
        let mut v = Vec::new();
        put_i32(&mut v, 0); // assets
        put_i32(&mut v, nodes.len() as i32);
        v.extend(std::iter::repeat_n(0u8, 260 * nodes.len()));
        for n in nodes {
            v.extend_from_slice(n);
        }
        v
    }

    fn water_record(shape: usize, contour: usize) -> Vec<u8> {
        let mut v = vec![0u8; 0x7B + (shape + contour) * 12];
        v[0..4].copy_from_slice(&11i32.to_le_bytes());
        v[0x6B..0x6F].copy_from_slice(&(shape as i32).to_le_bytes());
        v[0x77..0x7B].copy_from_slice(&(contour as i32).to_le_bytes());
        v
    }

    fn strip() -> Vec<[f32; 3]> {
        // three pairs along +x, 8 m wide
        (0..3)
            .flat_map(|i| {
                let x = 100.0 + i as f32 * 10.0;
                [[x, 96.0, 5.0], [x, 104.0, 5.0]]
            })
            .collect()
    }

    #[test]
    fn finds_roads_behind_other_records_and_in_children() {
        let strip = strip();
        let mut block = Vec::new();
        let mut generic = vec![0u8; 68];
        generic[0..4].copy_from_slice(&2i32.to_le_bytes());
        block.extend_from_slice(&generic);
        block.extend_from_slice(&water_record(2, 1));
        block.extend_from_slice(&road_record(&strip));
        let child = node(&road_record(&strip[..4]), &[]);
        let root = node(&block, &[child]);
        let (roads, ok) = scan_object_dat(&object_dat(&[root]));
        assert!(ok);
        assert_eq!(roads.len(), 2);
        assert_eq!(roads[0], strip);
        assert_eq!(roads[1], strip[..4].to_vec());
    }

    #[test]
    fn damaged_files_end_the_walk_without_panicking() {
        let strip = strip();
        let root = node(&road_record(&strip), &[]);
        let good = object_dat(&[root]);
        assert_eq!(scan_object_dat(&good).0.len(), 1);
        // every truncation is handled
        for cut in 0..good.len() {
            let _ = scan_object_dat(&good[..cut]);
        }
        // an unknown record type keeps what was read before it
        let mut block = road_record(&strip);
        block.extend_from_slice(&999i32.to_le_bytes());
        block.extend_from_slice(&[0u8; 8]);
        let (roads, _) = scan_object_dat(&object_dat(&[node(&block, &[])]));
        assert_eq!(roads.len(), 1);
        // absurd counts
        let mut bad = vec![0xFFu8; 64];
        bad[0..4].copy_from_slice(&0i32.to_le_bytes());
        let _ = scan_object_dat(&bad);
        assert_eq!(scan_object_dat(&[1, 2, 3]), (Vec::new(), true));
        // a truncated road record (count says more points than the block holds)
        let mut rec = road_record(&strip);
        rec.truncate(rec.len() - 5);
        assert!(scan_object_dat(&object_dat(&[node(&rec, &[])]))
            .0
            .is_empty());
    }

    #[test]
    fn prefab_centre_line_and_width() {
        let road = prefab_road(&strip(), (2, 3)).unwrap();
        assert_eq!(road.points.len(), 3);
        assert_eq!(road.points[0], [2048.0 + 100.0, 3072.0 + 100.0, 5.0]);
        assert!((road.half_width - 4.0).abs() < 1e-4);
        assert!((road.length() - 20.0).abs() < 1e-3);
        assert!(prefab_road(&strip()[..2], (0, 0)).is_none());
        assert_eq!(parse_cell("007_012"), Some((7, 12)));
        assert_eq!(parse_cell("x_1"), None);
    }

    #[test]
    fn transfer_routes_use_the_zone_origin() {
        let xml =
            "\u{feff}<Objects>\n<Transfer Name=\"a\" Type=\"0\" cellX=\"0\" cellY=\"0\"><Points>\
            <Point Pos=\"1.5,2.5,3\"/><Point Pos=\"11.5,2.5,3\"/></Points></Transfer>\
            <Transfer Name=\"b\"><Points><Point Pos=\"bad\"/></Points></Transfer></Objects>";
        let r = parse_transfer_routes(xml, (1, 2));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0][0], [1024.0 + 1.5, 2048.0 + 2.5, 3.0]);
        assert_eq!(r[0][1][0], 1024.0 + 11.5);
        assert!(parse_transfer_routes("nonsense", (0, 0)).is_empty());
    }

    /// Two squares side by side (x 0..10 and 10..20), each two triangles, plus a huge junk
    /// triangle; the second square is 20 m higher.
    fn mesh() -> NavData {
        let mut d = NavData::default();
        let sq = |x: f32, z: f32| {
            [
                [x, 0.0, z],
                [x + 10.0, 0.0, z],
                [x + 10.0, 10.0, z],
                [x, 10.0, z],
            ]
        };
        let mut tris = Vec::new();
        for (x, z) in [(0.0, 1.0), (10.0, 21.0)] {
            let base = d.vertices.len() as u32;
            d.vertices.extend(sq(x, z));
            tris.push([base, base + 1, base + 2]);
            tris.push([base, base + 2, base + 3]);
        }
        let base = d.vertices.len() as u32;
        d.vertices.extend([
            [-500.0, -500.0, 1.0],
            [500.0, -500.0, 1.0],
            [0.0, 500.0, 1.0],
        ]);
        tris.push([base, base + 1, base + 2]);
        for t in tris {
            let c = t.map(|i| d.vertices[i as usize]);
            d.node_pos.push([
                (c[0][0] + c[1][0] + c[2][0]) / 3.0,
                (c[0][1] + c[1][1] + c[2][1]) / 3.0,
                c[0][2],
            ]);
            d.node_tri.push(t);
            d.node_flags.push(0);
        }
        d.compute_bounds();
        d.build_grid(8.0).unwrap();
        d
    }

    #[test]
    fn roads_mark_the_triangles_under_them_only() {
        let mut d = mesh();
        let on_first = Road {
            points: vec![[1.0, 5.0, 1.0], [9.0, 5.0, 1.0]],
            half_width: 3.0,
        };
        let p = probe(&d, &on_first);
        assert_eq!(p.samples, p.samples_on_mesh);
        assert!(p.samples >= 4);
        let new = mark(&mut d, &p.nodes);
        assert_eq!(new, 2, "both triangles of the first square");
        assert_eq!(d.node_flags, vec![flag::ROAD, flag::ROAD, 0, 0, 0]);
        // marking again adds nothing
        assert_eq!(mark(&mut d, &p.nodes), 0);

        // over the 20 m higher square at ground height of the first: not on the mesh
        let below = Road {
            points: vec![[11.0, 5.0, 1.0], [19.0, 5.0, 1.0]],
            half_width: 3.0,
        };
        let p = probe(&d, &below);
        assert_eq!(p.samples_on_mesh, 0);
        assert!(p.nodes.is_empty(), "the 500 m junk triangle is ignored too");

        // a road that leaves the mesh is partly on it
        let half = Road {
            points: vec![[5.0, 5.0, 1.0], [45.0, 5.0, 1.0]],
            half_width: 3.0,
        };
        let p = probe(&d, &half);
        assert!(p.samples_on_mesh > 0 && p.samples_on_mesh < p.samples);
        // outside the grid entirely
        let far = Road {
            points: vec![[5000.0, 5000.0, 1.0], [5010.0, 5000.0, 1.0]],
            half_width: 3.0,
        };
        assert_eq!(probe(&d, &far).samples_on_mesh, 0);
    }
}
