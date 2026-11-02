//! Weighted A* over the navmesh links and the per-thread scratch memory it runs in.
//!
//! Nodes are triangle centroids and waypoints; the step cost is baked per link (through the
//! middle of the shared edge for two triangles). The search state lives in a generation-stamped
//! open-addressing table plus a binary heap that are reused between queries: nothing is
//! proportional to the node count of the mesh, only to the number of nodes a query touches.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

use super::format::{agent_radius_units, flag};
use super::funnel::Portal;
use super::mesh::{Located, NavMesh, NodeRef};
use crate::geom::Vec3;

/// Default agent radius (metres).
pub const DEFAULT_AGENT_RADIUS: f32 = 0.5;
/// Default heuristic weight: ~+3% path length for ~4x fewer expansions than plain A*.
pub const DEFAULT_WEIGHT: f32 = 1.5;
/// Default expansion cap of one query.
pub const DEFAULT_MAX_EXPANSIONS: u32 = 60_000;
/// Hard upper limit of `max_expansions` (bounds the scratch memory).
pub const MAX_EXPANSIONS_LIMIT: u32 = 2_000_000;
/// Default distance within which a start / goal off the mesh is snapped onto it (metres).
pub const DEFAULT_SNAP_DIST: f32 = 8.0;
/// Default cost multiplier of a link between two `ROAD` nodes.
pub const DEFAULT_ROAD_BIAS: f32 = 0.75;

/// A path request.
#[derive(Clone, Copy)]
pub struct PathQuery<'a> {
    pub from: Vec3,
    pub to: Vec3,
    /// The agent's radius: narrower links are not used and the string keeps this distance from
    /// triangle corners.
    pub agent_radius: f32,
    /// Heuristic weight (1.0 = exact A*, larger = faster and slightly longer paths).
    pub weight: f32,
    /// Most nodes one query may expand (clamped to [`MAX_EXPANSIONS_LIMIT`]); when it is hit the
    /// best progress so far is returned as [`PathResult::Partial`].
    pub max_expansions: u32,
    /// Largest distance a start / goal off the mesh is snapped from.
    pub snap_dist: f32,
    /// Cost multiplier (< 1 prefers) of links between two `ROAD` nodes; 1.0 = off.
    pub road_bias: f32,
    /// Extra cost (metres) of entering a node, e.g. the nodes a bot got stuck on.
    pub penalties: &'a [(NodeRef, f32)],
    /// Terrain height at (x, y), used for the z of path corners on triangles; `Some(h)` with
    /// `h > 0` replaces the mesh's z. Waypoint corners (indoors) keep the mesh z.
    pub height: Option<&'a dyn Fn(f32, f32) -> Option<f32>>,
}

impl<'a> PathQuery<'a> {
    /// A query with the default settings.
    pub fn new(from: Vec3, to: Vec3) -> Self {
        PathQuery {
            from,
            to,
            agent_radius: DEFAULT_AGENT_RADIUS,
            weight: DEFAULT_WEIGHT,
            max_expansions: DEFAULT_MAX_EXPANSIONS,
            snap_dist: DEFAULT_SNAP_DIST,
            road_bias: DEFAULT_ROAD_BIAS,
            penalties: &[],
            height: None,
        }
    }
}

/// A smoothed route.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    /// The way points: the start, the corners the string bends around, the goal (for a
    /// partial path: the node it got to).
    pub points: Vec<Vec3>,
    /// Per segment (`points.len() - 1` entries): the segment touches a waypoint (building
    /// interior, stairs, bridge); its z comes from the mesh, not the terrain.
    pub indoor: Vec<bool>,
    /// Total length in metres (3D).
    pub length: f32,
    /// Nodes the search expanded.
    pub expansions: u32,
    /// The mesh nodes the start and the end of the search are on.
    pub start: NodeRef,
    pub end: NodeRef,
}

/// The answer of [`NavMesh::find_path`].
#[derive(Clone, Debug, PartialEq)]
pub enum PathResult {
    /// A route to the goal.
    Found(Path),
    /// The expansion cap was hit: the route to the node nearest the goal found so far. Walk it
    /// and ask again.
    Partial(Path),
    /// There is no route: the goal lies in another component, or the search ran dry.
    Unreachable,
    /// The start or the goal is not on the mesh (further than `snap_dist` from it).
    NoMesh,
}

impl PathResult {
    /// The path of a `Found` or `Partial` result.
    pub fn path(&self) -> Option<&Path> {
        match self {
            PathResult::Found(p) | PathResult::Partial(p) => Some(p),
            _ => None,
        }
    }
}

const EMPTY: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Slot {
    node: u32,
    gen: u32,
    g: f32,
    parent: u32,
}

const VACANT: Slot = Slot {
    node: EMPTY,
    gen: 0,
    g: 0.0,
    parent: EMPTY,
};

#[derive(Clone, Copy)]
struct HeapEntry {
    f: f32,
    g: f32,
    node: u32,
}

impl PartialEq for HeapEntry {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for HeapEntry {
    // reversed: BinaryHeap pops the greatest, we want the lowest f (then the lowest node id so
    // equal-cost ties are deterministic)
    fn cmp(&self, o: &Self) -> Ordering {
        o.f.total_cmp(&self.f).then_with(|| o.node.cmp(&self.node))
    }
}

/// Initial table size (slots). Grows on demand up to the query's limit.
const INITIAL_SLOTS: usize = 1 << 12;
/// A table larger than this many slots is dropped after the query that grew it.
const RETAIN_SLOTS: usize = 1 << 16;

/// Reusable search memory of one thread: a generation-stamped open-addressing table of the
/// nodes touched by the current query, the open list, and buffers for the corridor and the
/// funnel. Clearing is O(1) (the generation advances).
pub struct Scratch {
    slots: Vec<Slot>,
    shift: u32,
    gen: u32,
    used: usize,
    heap: BinaryHeap<HeapEntry>,
    pub(crate) corridor: Vec<u32>,
    pub(crate) portals: Vec<Portal>,
    pub(crate) corners: Vec<Vec3>,
    /// Nodes expanded by the last search.
    pub(crate) expansions: u32,
}

impl Default for Scratch {
    fn default() -> Self {
        Scratch::new()
    }
}

impl Scratch {
    pub fn new() -> Scratch {
        Scratch {
            slots: vec![VACANT; INITIAL_SLOTS],
            shift: 32 - INITIAL_SLOTS.trailing_zeros(),
            gen: 0,
            used: 0,
            heap: BinaryHeap::new(),
            corridor: Vec::new(),
            portals: Vec::new(),
            corners: Vec::new(),
            expansions: 0,
        }
    }

    /// Heap bytes currently held.
    pub fn memory_bytes(&self) -> usize {
        self.slots.capacity() * std::mem::size_of::<Slot>()
            + self.heap.capacity() * std::mem::size_of::<HeapEntry>()
            + self.corridor.capacity() * 4
            + self.portals.capacity() * std::mem::size_of::<Portal>()
            + self.corners.capacity() * std::mem::size_of::<Vec3>()
    }

    fn begin(&mut self) {
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            // the generation wrapped: stale stamps could match, wipe the table
            self.slots.fill(VACANT);
            self.gen = 1;
        }
        self.used = 0;
        self.heap.clear();
    }

    fn end(&mut self) {
        if self.slots.len() > RETAIN_SLOTS {
            self.slots = vec![VACANT; INITIAL_SLOTS];
            self.shift = 32 - INITIAL_SLOTS.trailing_zeros();
            self.used = 0;
        }
        if self.heap.capacity() > RETAIN_SLOTS {
            self.heap = BinaryHeap::new();
        }
        if self.corridor.capacity() > RETAIN_SLOTS {
            self.corridor = Vec::new();
        }
    }

    #[inline]
    fn home(&self, node: u32) -> usize {
        (node.wrapping_mul(0x9E37_79B1) >> self.shift) as usize
    }

    /// Index of the slot holding `node`, or of the free slot where it would go.
    #[inline]
    fn probe(&self, node: u32) -> (usize, bool) {
        let mask = self.slots.len() - 1;
        let mut i = self.home(node);
        loop {
            let s = &self.slots[i];
            if s.gen != self.gen {
                return (i, false);
            }
            if s.node == node {
                return (i, true);
            }
            i = (i + 1) & mask;
        }
    }

    /// Doubles the table (rehashing the live entries); `false` beyond `max_slots`.
    fn grow(&mut self, max_slots: usize) -> bool {
        let new_len = self.slots.len() * 2;
        if new_len > max_slots {
            return false;
        }
        let old = std::mem::replace(&mut self.slots, vec![VACANT; new_len]);
        self.shift = 32 - new_len.trailing_zeros();
        for s in old {
            if s.gen == self.gen {
                let (i, _) = self.probe(s.node);
                self.slots[i] = s;
            }
        }
        true
    }

    fn get(&self, node: u32) -> Option<Slot> {
        let (i, found) = self.probe(node);
        found.then(|| self.slots[i])
    }
}

thread_local! {
    static THREAD_SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::new());
}

/// Heap bytes of the calling thread's scratch (what `find_path` keeps per thread).
pub fn thread_scratch_bytes() -> usize {
    THREAD_SCRATCH.with(|s| s.borrow().memory_bytes())
}

enum Outcome {
    /// The corridor to the goal is in `Scratch::corridor`.
    Found,
    /// Capped: the corridor to the node nearest the goal is in `Scratch::corridor`.
    Capped,
    /// The open list ran dry.
    Exhausted,
}

fn dist2d(a: Vec3, b: Vec3) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

impl NavMesh {
    /// Finds a smoothed route from `q.from` to `q.to`, using the calling thread's scratch.
    pub fn find_path(&self, q: &PathQuery) -> PathResult {
        THREAD_SCRATCH.with(|s| match s.try_borrow_mut() {
            Ok(mut sc) => self.find_path_with(q, &mut sc),
            // called from inside a height callback: use a throwaway scratch
            Err(_) => self.find_path_with(q, &mut Scratch::new()),
        })
    }

    /// [`NavMesh::find_path`] in an explicit scratch.
    pub fn find_path_with(&self, q: &PathQuery, sc: &mut Scratch) -> PathResult {
        let snap = q.snap_dist;
        let Some(start) = self.locate_detailed(q.from, snap) else {
            return PathResult::NoMesh;
        };
        let Some(goal) = self.locate_detailed(q.to, snap) else {
            return PathResult::NoMesh;
        };
        // agents narrower than the component tables' radius may use links the tables ignore
        let exact = agent_radius_units(q.agent_radius) >= self.comp_radius_units();
        if exact && self.may_reach(start.node, goal.node) == Some(false) {
            return PathResult::Unreachable;
        }
        let r = self.search(q, &start, &goal, sc);
        let res = match r {
            Outcome::Found => {
                let p = self.build_path(q, &start, Some(&goal), sc);
                PathResult::Found(p)
            }
            Outcome::Capped => {
                let p = self.build_path(q, &start, None, sc);
                PathResult::Partial(p)
            }
            Outcome::Exhausted => PathResult::Unreachable,
        };
        sc.end();
        res
    }

    pub(crate) fn comp_radius_units(&self) -> u8 {
        (self.component_radius() / super::format::RADIUS_UNIT).round() as u8
    }

    /// The A* proper. On `Found` / `Capped` the node chain is left in `sc.corridor`.
    fn search(&self, q: &PathQuery, start: &Located, goal: &Located, sc: &mut Scratch) -> Outcome {
        sc.begin();
        sc.corridor.clear();
        let w = if q.weight.is_finite() {
            q.weight.max(1.0)
        } else {
            DEFAULT_WEIGHT
        };
        let max_exp = q.max_expansions.clamp(1, MAX_EXPANSIONS_LIMIT);
        // the table may grow to ~8 slots per allowed expansion before the search gives up
        let max_slots = ((max_exp as usize) * 8)
            .next_power_of_two()
            .max(INITIAL_SLOTS);
        let need = agent_radius_units(q.agent_radius);
        let bias = if q.road_bias.is_finite() && q.road_bias > 0.0 && q.road_bias < 1.0 {
            q.road_bias
        } else {
            1.0
        };
        let goal_node = goal.node.0;
        let goal_pos = goal.point;
        let start_node = start.node.0;

        let h_start = dist2d(self.node_pos(start.node), goal_pos);
        let (i, _) = sc.probe(start_node);
        sc.slots[i] = Slot {
            node: start_node,
            gen: sc.gen,
            g: 0.0,
            parent: EMPTY,
        };
        sc.used = 1;
        sc.heap.push(HeapEntry {
            f: w * h_start,
            g: 0.0,
            node: start_node,
        });
        let mut best = (h_start, start_node);
        let mut expansions = 0u32;
        let mut capped = false;
        let mut found = false;

        'search: while let Some(e) = sc.heap.pop() {
            let Some(cur) = sc.get(e.node) else { continue };
            if e.g > cur.g {
                continue; // stale entry of a node reached more cheaply since
            }
            if e.node == goal_node {
                found = true;
                break;
            }
            if expansions >= max_exp {
                capped = true;
                break;
            }
            expansions += 1;
            let pos = self.node_pos(NodeRef(e.node));
            let h = dist2d(pos, goal_pos);
            if h < best.0 {
                best = (h, e.node);
            }
            let from_road = bias < 1.0 && self.node_flags(NodeRef(e.node)) & flag::ROAD != 0;
            let (lo, hi) = self.link_range(e.node);
            for l in lo..hi {
                if self.link_radius(l) < need {
                    continue;
                }
                let t = self.link_target(l);
                if (t as usize) >= self.node_count() {
                    continue;
                }
                let tref = NodeRef(t);
                let mut cost = self.link_cost(l);
                if from_road && self.node_flags(tref) & flag::ROAD != 0 {
                    cost *= bias;
                }
                if !q.penalties.is_empty() {
                    for (n, extra) in q.penalties {
                        if n.0 == t {
                            cost += extra.max(0.0);
                        }
                    }
                }
                let ng = e.g + cost;
                let (si, present) = sc.probe(t);
                if present {
                    if ng >= sc.slots[si].g {
                        continue;
                    }
                    sc.slots[si].g = ng;
                    sc.slots[si].parent = e.node;
                } else {
                    // keep the load factor below 1/2
                    let mut si = si;
                    if (sc.used + 1) * 2 > sc.slots.len() {
                        if !sc.grow(max_slots) {
                            capped = true;
                            break 'search;
                        }
                        si = sc.probe(t).0;
                    }
                    sc.slots[si] = Slot {
                        node: t,
                        gen: sc.gen,
                        g: ng,
                        parent: e.node,
                    };
                    sc.used += 1;
                }
                let ht = dist2d(self.node_pos(tref), goal_pos);
                sc.heap.push(HeapEntry {
                    f: ng + w * ht,
                    g: ng,
                    node: t,
                });
            }
        }

        sc.expansions = expansions;
        if found {
            self.trace(goal_node, sc);
            Outcome::Found
        } else if capped {
            // not reached: the route to the node nearest the goal
            self.trace(best.1, sc);
            Outcome::Capped
        } else {
            Outcome::Exhausted
        }
    }

    /// Fills `sc.corridor` with the chain start .. `node` from the parent links.
    fn trace(&self, node: u32, sc: &mut Scratch) {
        sc.corridor.clear();
        let mut cur = node;
        // a corrupt table cannot loop forever: the chain is at most as long as the table
        for _ in 0..=sc.used {
            sc.corridor.push(cur);
            match sc.get(cur) {
                Some(s) if s.parent != EMPTY => cur = s.parent,
                _ => break,
            }
        }
        sc.corridor.reverse();
    }
}
