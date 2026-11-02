//! Merging the per-block navigation files of a world into one graph.
//!
//! Like the engine (10.0.2.13 `cryaisystem_dedicate.dll`, see the AAEmu Rust server's
//! `aaemu-game/src/systems/ai/geodata/graph.rs`, GPL-3.0-or-later) neighbouring 256 m blocks
//! carry copies of their border triangles:
//!
//! * vertices are the same when their world x/y is closer than 1e-4 m;
//! * nodes are the same when their world x/y is closer than 1e-4 m and their navType is equal
//!   (the first copy's position and triangle are kept; a later copy only fills in a missing
//!   triangle);
//! * a link is re-attached to the merged nodes; a second link between the same two nodes
//!   overwrites the first (last read wins) when the links are later de-duplicated.
//!
//! Everything lives in flat vectors; the de-duplication uses one spatial hash with chains in a
//! `Vec<u32>`, not a map of vectors. Positions are compared in f64 (file position + block offset
//! is exact there; the f32 sum is rounded to 4 mm at x = 36 000 m).

use rustc_hash::FxHashMap;

use aa_world::nav::format::{radius_to_units, NO_TRI};

use super::bai::{NetFile, VertFile, NAV_TYPE_TRIANGLE, NAV_TYPE_WAYPOINT};

/// Squared x/y distance below which two points are the same (1e-4 m).
const MERGE_DIST_SQ: f64 = 1e-8;
/// Hash cell size in cm-units (x * 100): 1 cm cells, far larger than the merge distance.
const CELL_SCALE: f64 = 100.0;
/// The merge distance in cells.
const TOL_CELLS: f64 = 1e-4 * CELL_SCALE;

pub const NONE: u32 = u32::MAX;

/// A spatial hash over points stored elsewhere: cell -> head of a chain through `next`.
#[derive(Default)]
struct PointHash {
    heads: FxHashMap<u64, u32>,
    next: Vec<u32>,
}

fn cell_key(ix: i64, iy: i64) -> u64 {
    ((ix as i32 as u32 as u64) << 32) | (iy as i32 as u32 as u64)
}

impl PointHash {
    fn insert(&mut self, xy: [f64; 2], id: u32) {
        let key = cell_key(
            (xy[0] * CELL_SCALE).floor() as i64,
            (xy[1] * CELL_SCALE).floor() as i64,
        );
        debug_assert_eq!(id as usize, self.next.len());
        let prev = self.heads.insert(key, id).unwrap_or(NONE);
        self.next.push(prev);
    }

    /// The first stored point within the merge distance of `xy` that `accept` takes.
    fn find(
        &self,
        xy: [f64; 2],
        pts: &[[f64; 2]],
        mut accept: impl FnMut(u32) -> bool,
    ) -> Option<u32> {
        let (fx, fy) = (xy[0] * CELL_SCALE, xy[1] * CELL_SCALE);
        let (ix, iy) = (fx.floor(), fy.floor());
        // only the neighbouring cell the point is close to can hold a match
        let span = |frac: f64| -> (i64, i64) {
            if frac < TOL_CELLS {
                (-1, 0)
            } else if frac > 1.0 - TOL_CELLS {
                (0, 1)
            } else {
                (0, 0)
            }
        };
        let (dx0, dx1) = span(fx - ix);
        let (dy0, dy1) = span(fy - iy);
        for dx in dx0..=dx1 {
            for dy in dy0..=dy1 {
                let Some(&head) = self.heads.get(&cell_key(ix as i64 + dx, iy as i64 + dy)) else {
                    continue;
                };
                let mut cur = head;
                while cur != NONE {
                    let p = pts[cur as usize];
                    let (ddx, ddy) = (p[0] - xy[0], p[1] - xy[1]);
                    if ddx * ddx + ddy * ddy < MERGE_DIST_SQ && accept(cur) {
                        return Some(cur);
                    }
                    cur = self.next[cur as usize];
                }
            }
        }
        None
    }
}

/// A merged node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BNode {
    pub pos: [f32; 3],
    /// Merged vertex ids of the triangle, [`NO_TRI`] ×3 without one.
    pub tri: [u32; 3],
    pub nav_type: u16,
    pub flags: u8,
}

/// A merged directed link, in the order the files were read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BLink {
    pub src: u32,
    pub dst: u32,
    /// Pass radius in [`aa_world::nav::format::RADIUS_UNIT`]s (0 = blocked).
    pub units: u8,
}

/// Counters of the merge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeStats {
    pub blocks: usize,
    pub blocks_failed: usize,
    pub vertices_read: usize,
    pub vertices_merged: usize,
    pub nodes_read: usize,
    /// navType other than triangle / waypoint (dummies at the origin of empty blocks).
    pub nodes_other: usize,
    pub nodes_merged: usize,
    /// Triangle nodes whose corner indices are missing, out of range or repeated.
    pub triangles_invalid: usize,
    pub links_read: usize,
    pub links_unresolved: usize,
    pub links_self: usize,
}

/// The merged graph under construction.
#[derive(Default)]
pub struct NavBuilder {
    pub vertices: Vec<[f32; 3]>,
    vxy: Vec<[f64; 2]>,
    vhash: PointHash,
    pub nodes: Vec<BNode>,
    nxy: Vec<[f64; 2]>,
    nhash: PointHash,
    pub links: Vec<BLink>,
    pub stats: MergeStats,
}

impl NavBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn add_vertex(&mut self, local: [f32; 3], offset: [f64; 3]) -> u32 {
        let xy = [
            f64::from(local[0]) + offset[0],
            f64::from(local[1]) + offset[1],
        ];
        self.stats.vertices_read += 1;
        if let Some(i) = self.vhash.find(xy, &self.vxy, |_| true) {
            self.stats.vertices_merged += 1;
            return i;
        }
        let i = self.vertices.len() as u32;
        self.vertices.push([
            xy[0] as f32,
            xy[1] as f32,
            (f64::from(local[2]) + offset[2]) as f32,
        ]);
        self.vxy.push(xy);
        self.vhash.insert(xy, i);
        i
    }

    /// Adds one block: its nodes, links and (for the triangle corners) vertices, shifted by
    /// `offset` (the block's origin in world metres).
    pub fn add_block(&mut self, net: &NetFile, verts: Option<&VertFile>, offset: [f64; 3]) {
        self.stats.blocks += 1;
        let vmap: Vec<u32> = verts
            .map(|v| {
                v.verts
                    .iter()
                    .map(|p| self.add_vertex(*p, offset))
                    .collect()
            })
            .unwrap_or_default();

        let mut nmap: Vec<u32> = Vec::with_capacity(net.nodes.len());
        let mut by_id: FxHashMap<i64, u32> =
            FxHashMap::with_capacity_and_hasher(net.nodes.len(), Default::default());
        for n in &net.nodes {
            self.stats.nodes_read += 1;
            if n.nav_type != NAV_TYPE_TRIANGLE && n.nav_type != NAV_TYPE_WAYPOINT {
                self.stats.nodes_other += 1;
                nmap.push(NONE);
                continue;
            }
            let tri = if n.nav_type == NAV_TYPE_TRIANGLE {
                let t = (|| {
                    let mut t = [0u32; 3];
                    for (k, &o) in n.tri.iter().enumerate() {
                        t[k] = *vmap.get(usize::try_from(o).ok()?)?;
                    }
                    (t[0] != t[1] && t[1] != t[2] && t[0] != t[2]).then_some(t)
                })();
                if t.is_none() {
                    self.stats.triangles_invalid += 1;
                }
                t.unwrap_or([NO_TRI; 3])
            } else {
                [NO_TRI; 3]
            };
            let xy = [
                f64::from(n.pos[0]) + offset[0],
                f64::from(n.pos[1]) + offset[1],
            ];
            let nav_type = n.nav_type;
            let nodes = &self.nodes;
            let id = match self
                .nhash
                .find(xy, &self.nxy, |i| nodes[i as usize].nav_type == nav_type)
            {
                Some(i) => {
                    self.stats.nodes_merged += 1;
                    let existing = &mut self.nodes[i as usize];
                    if existing.tri[0] == NO_TRI && tri[0] != NO_TRI {
                        existing.tri = tri;
                    }
                    i
                }
                None => {
                    let i = self.nodes.len() as u32;
                    self.nodes.push(BNode {
                        pos: [
                            xy[0] as f32,
                            xy[1] as f32,
                            (f64::from(n.pos[2]) + offset[2]) as f32,
                        ],
                        tri,
                        nav_type,
                        flags: n.flags,
                    });
                    self.nxy.push(xy);
                    self.nhash.insert(xy, i);
                    i
                }
            };
            nmap.push(id);
            // the first node of a repeated id wins, like the engine
            by_id.entry(i64::from(n.id)).or_insert(id);
        }

        for l in &net.links {
            self.stats.links_read += 1;
            let (Some(&s), Some(&d)) = (by_id.get(&i64::from(l.src)), by_id.get(&i64::from(l.dst)))
            else {
                self.stats.links_unresolved += 1;
                continue;
            };
            if s == d {
                self.stats.links_self += 1;
                continue;
            }
            self.links.push(BLink {
                src: s,
                dst: d,
                units: radius_to_units(l.radius),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::bai::testfiles::*;
    use super::super::bai::{parse_net, parse_verts};
    use super::*;

    /// A square [x0, x1] x [0, 10] (world metres) as two triangles in a block at `offset`.
    fn square(offset: f64, x0: f32, x1: f32, radius: f32) -> (NetFile, VertFile) {
        let (l0, l1) = (x0 - offset as f32, x1 - offset as f32);
        let corners = [
            [l0, 0.0, x0 / 10.0],
            [l1, 0.0, x1 / 10.0],
            [l1, 10.0, x1 / 10.0],
            [l0, 10.0, x0 / 10.0],
        ];
        let c = |a: usize, b: usize, d: usize| {
            [
                (corners[a][0] + corners[b][0] + corners[d][0]) / 3.0,
                (corners[a][1] + corners[b][1] + corners[d][1]) / 3.0,
                (corners[a][2] + corners[b][2] + corners[d][2]) / 3.0,
            ]
        };
        let net = net(
            55,
            &[
                TNode::tri(1, c(0, 1, 2), [0, 1, 2]),
                TNode::tri(2, c(0, 2, 3), [0, 2, 3]),
            ],
            &[TLink::new(1, 2, radius), TLink::new(2, 1, radius)],
        );
        (
            parse_net(&net).unwrap(),
            parse_verts(&verts(2, &corners)).unwrap(),
        )
    }

    #[test]
    fn border_copies_merge_by_position_and_type() {
        let mut b = NavBuilder::new();
        // the same square read from two blocks with different offsets
        let (n1, v1) = square(768.0, 1014.0, 1024.0, 1.0);
        b.add_block(&n1, Some(&v1), [768.0, 0.0, 0.0]);
        let (n2, v2) = square(1024.0, 1014.0, 1024.0, 2.0);
        b.add_block(&n2, Some(&v2), [1024.0, 0.0, 0.0]);
        assert_eq!(b.vertices.len(), 4);
        assert_eq!(b.nodes.len(), 2);
        assert_eq!(b.stats.nodes_merged, 2);
        assert_eq!(b.stats.vertices_merged, 4);
        // 4 links read, none merged yet: de-duplication happens when the graph is assembled
        assert_eq!(b.links.len(), 4);
        assert!(b.links.iter().all(|l| l.src < 2 && l.dst < 2));
        assert_eq!(
            b.links.iter().map(|l| l.units).collect::<Vec<_>>(),
            [20, 20, 40, 40]
        );
        assert_eq!(b.nodes[0].tri, [0, 1, 2]);
    }

    #[test]
    fn same_place_with_another_type_is_another_node() {
        let mut b = NavBuilder::new();
        let n = parse_net(&net(
            55,
            &[
                TNode::tri(1, [5.0, 5.0, 1.0], [-1; 3]),
                TNode::waypoint(2, [5.0, 5.0, 1.0]),
                TNode {
                    id: 3,
                    pos: [0.0, 0.0, 0.0],
                    tri: [-1; 3],
                    nav_type: 1,
                },
            ],
            &[
                TLink::new(1, 2, 1.0),
                TLink::new(2, 3, 1.0),
                TLink::new(1, 1, 1.0),
                TLink::new(1, 99, 1.0),
            ],
        ))
        .unwrap();
        b.add_block(&n, None, [0.0; 3]);
        assert_eq!(b.nodes.len(), 2);
        assert_eq!(b.stats.nodes_other, 1);
        assert_eq!(b.stats.triangles_invalid, 1);
        assert_eq!(
            b.links.len(),
            1,
            "the link to the dummy, the self link and the unknown id are dropped"
        );
        assert_eq!(b.stats.links_unresolved, 2);
        assert_eq!(b.stats.links_self, 1);
        // a triangle node without a usable triangle gets one from a later copy
        let (n2, v2) = square(0.0, 0.0, 10.0, 1.0);
        let mut b = NavBuilder::new();
        let mut broken = n2.clone();
        broken.nodes[0].tri = [-1; 3];
        b.add_block(&broken, Some(&v2), [0.0; 3]);
        assert_eq!(b.nodes[0].tri[0], NO_TRI);
        b.add_block(&n2, Some(&v2), [0.0; 3]);
        assert_ne!(b.nodes[0].tri[0], NO_TRI);
    }

    #[test]
    fn near_misses_and_far_points_stay_apart() {
        let mut b = NavBuilder::new();
        let pts = [
            [0.0f32, 0.0, 0.0],
            [0.00005, 0.0, 5.0],  // 5e-5 m: the same vertex
            [0.0002, 0.0, 0.0],   // 2e-4 m: another
            [0.009999, 0.0, 0.0], // another cell edge case
        ];
        let v = parse_verts(&verts(2, &pts)).unwrap();
        let n = parse_net(&net(55, &[], &[])).unwrap();
        b.add_block(&n, Some(&v), [0.0; 3]);
        assert_eq!(b.vertices.len(), 3);
        // matches across a hash cell border (cells are 1 cm)
        let mut b = NavBuilder::new();
        let v = parse_verts(&verts(2, &[[0.00999, 0.0, 0.0], [0.010_03, 0.0, 0.0]])).unwrap();
        b.add_block(&n, Some(&v), [0.0; 3]);
        assert_eq!(b.vertices.len(), 1);
        // negative coordinates
        let mut b = NavBuilder::new();
        let v = parse_verts(&verts(2, &[[-0.00001, -3.0, 0.0], [0.00001, -3.0, 0.0]])).unwrap();
        b.add_block(&n, Some(&v), [0.0; 3]);
        assert_eq!(b.vertices.len(), 1);
    }
}
