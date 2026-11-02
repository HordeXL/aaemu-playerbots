//! [`NavMesh`]: the memory-mapped navigation mesh file and point location on it.

use std::fs::File;
use std::path::Path;

use anyhow::{bail, ensure, Context};
use memmap2::Mmap;

use super::format::{
    flag, Section, NAV_HEADER_LEN, NAV_MAGIC, NAV_SECTION_ENTRY_LEN, NAV_VERSION, NO_TRI,
};
use crate::geom::Vec3;

/// A node of the mesh (index into the node arrays).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeRef(pub u32);

/// Where a world position lies on the mesh.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Located {
    pub node: NodeRef,
    /// The point of the node nearest to the position: the position's x/y with the triangle
    /// plane's z when it is inside a triangle, the waypoint itself, or the nearest point of the
    /// triangle.
    pub point: Vec3,
    /// The position is inside the node's triangle (x/y).
    pub inside: bool,
    /// Distance from the position to `point` (3D); the vertical gap when `inside`.
    pub dist: f32,
}

enum Backing {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

/// A section of the file: byte offset and byte length.
#[derive(Clone, Copy, Default)]
struct Span {
    off: usize,
    len: usize,
}

/// A read-only navigation mesh over a memory-mapped `nav_<world>.bin`.
///
/// Opening validates the header and the section table (sizes, offsets, element counts); the
/// section *contents* are not scanned, so only the pages a query touches become resident. Every
/// value read from the mapping that acts as an index is checked where it is used, so a damaged
/// file can produce wrong answers but never a panic or an out-of-bounds access; run
/// [`NavMesh::verify`] for a full scan.
pub struct NavMesh {
    data: Backing,
    nodes: usize,
    verts: usize,
    links: usize,
    comps: usize,
    pub(crate) bounds: [f32; 4],
    pub(crate) bucket: f32,
    grid_w: usize,
    grid_h: usize,
    comp_radius: u8,
    pos: Span,
    tri: Span,
    flags: Span,
    comp: Span,
    vert: Span,
    vert_flags: Span,
    adj_off: Span,
    adj_target: Span,
    adj_radius: Span,
    adj_cost: Span,
    comp_size: Span,
    comp_wcc: Span,
    comp_order: Span,
    grid_off: Span,
    grid_items: Span,
}

fn rd_u32(h: &[u8], o: usize) -> u32 {
    h.get(o..o + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn rd_u64(h: &[u8], o: usize) -> u64 {
    h.get(o..o + 8).map_or(0, |b| {
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    })
}

fn rd_f32(h: &[u8], o: usize) -> f32 {
    f32::from_bits(rd_u32(h, o))
}

impl NavMesh {
    /// Maps `path` read-only and validates the header and section table.
    pub fn open(path: &Path) -> anyhow::Result<NavMesh> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        // SAFETY: read-only map of a private data file; truncating it while mapped would fault,
        // which is accepted for baked data (same as the height grid).
        let map =
            unsafe { Mmap::map(&file) }.with_context(|| format!("mmap {}", path.display()))?;
        Self::new(Backing::Mapped(map)).with_context(|| format!("{}", path.display()))
    }

    /// A mesh over an in-memory copy of the file (tests, tools).
    pub fn from_bytes(bytes: Vec<u8>) -> anyhow::Result<NavMesh> {
        Self::new(Backing::Owned(bytes))
    }

    fn new(data: Backing) -> anyhow::Result<NavMesh> {
        let b: &[u8] = match &data {
            Backing::Mapped(m) => m,
            Backing::Owned(v) => v,
        };
        ensure!(
            b.len() >= NAV_HEADER_LEN,
            "navigation mesh shorter than its {NAV_HEADER_LEN}-byte header"
        );
        ensure!(&b[0..8] == NAV_MAGIC, "bad navigation mesh magic");
        let version = rd_u32(b, 8);
        ensure!(
            version == NAV_VERSION,
            "unsupported navigation mesh version {version}"
        );
        let nodes = rd_u32(b, 12) as usize;
        let verts = rd_u32(b, 16) as usize;
        let links = rd_u32(b, 20) as usize;
        let comps = rd_u32(b, 24) as usize;
        let section_count = rd_u32(b, 28) as usize;
        let bounds = [rd_f32(b, 32), rd_f32(b, 36), rd_f32(b, 40), rd_f32(b, 44)];
        let bucket = rd_f32(b, 48);
        let grid_w = rd_u32(b, 52) as usize;
        let grid_h = rd_u32(b, 56) as usize;
        let comp_radius = b[60];
        ensure!(nodes > 0, "navigation mesh without nodes");
        ensure!(
            bounds.iter().all(|v| v.is_finite())
                && bounds[0] <= bounds[2]
                && bounds[1] <= bounds[3],
            "bad navigation mesh bounds {bounds:?}"
        );
        ensure!(
            bucket.is_finite() && bucket >= 1.0 && grid_w > 0 && grid_h > 0,
            "bad grid {grid_w}x{grid_h} of {bucket} m cells"
        );
        let cells = grid_w
            .checked_mul(grid_h)
            .filter(|&c| c <= 1 << 26)
            .context("grid too large")?;
        ensure!(
            (1..=64).contains(&section_count)
                && b.len() >= NAV_HEADER_LEN + section_count * NAV_SECTION_ENTRY_LEN,
            "bad section table ({section_count} sections)"
        );
        let mut spans: [Option<Span>; 16] = [None; 16];
        for i in 0..section_count {
            let e = NAV_HEADER_LEN + i * NAV_SECTION_ENTRY_LEN;
            let id = rd_u32(b, e);
            let elem = rd_u32(b, e + 4) as usize;
            let off = rd_u64(b, e + 8);
            let count = rd_u64(b, e + 16);
            // sections this version does not know are skipped
            let Some(section) = Section::from_id(id) else {
                continue;
            };
            ensure!(
                elem == section.elem_size(),
                "{}: element size {elem}, expected {}",
                section.name(),
                section.elem_size()
            );
            let expect = match section {
                Section::NodePos | Section::NodeTri | Section::NodeFlags | Section::NodeComp => {
                    Some(nodes)
                }
                Section::Vertices | Section::VertFlags => Some(verts),
                Section::AdjOff => Some(nodes + 1),
                Section::AdjTarget | Section::AdjRadius | Section::AdjCost => Some(links),
                Section::CompSize | Section::CompWcc | Section::CompOrder => Some(comps),
                Section::GridOff => Some(cells + 1),
                Section::GridItems => None,
            };
            if let Some(n) = expect {
                ensure!(
                    count == n as u64,
                    "{}: {count} entries, the header says {n}",
                    section.name()
                );
            }
            let bytes = count
                .checked_mul(elem as u64)
                .and_then(|l| l.checked_add(off))
                .filter(|&end| end <= b.len() as u64)
                .with_context(|| {
                    format!(
                        "{}: {count} × {elem} bytes at {off} reach past the end of the file ({} bytes)",
                        section.name(),
                        b.len()
                    )
                })?;
            let _ = bytes;
            ensure!(
                off.is_multiple_of(4),
                "{}: unaligned offset {off}",
                section.name()
            );
            ensure!(
                spans[section as usize].is_none(),
                "{}: listed twice",
                section.name()
            );
            spans[section as usize] = Some(Span {
                off: off as usize,
                len: count as usize * elem,
            });
        }
        let get = |s: Section| -> anyhow::Result<Span> {
            match spans[s as usize] {
                Some(sp) => Ok(sp),
                None => bail!("section {} missing", s.name()),
            }
        };
        let m = NavMesh {
            nodes,
            verts,
            links,
            comps,
            bounds,
            bucket,
            grid_w,
            grid_h,
            comp_radius,
            pos: get(Section::NodePos)?,
            tri: get(Section::NodeTri)?,
            flags: get(Section::NodeFlags)?,
            comp: get(Section::NodeComp)?,
            vert: get(Section::Vertices)?,
            vert_flags: get(Section::VertFlags)?,
            adj_off: get(Section::AdjOff)?,
            adj_target: get(Section::AdjTarget)?,
            adj_radius: get(Section::AdjRadius)?,
            adj_cost: get(Section::AdjCost)?,
            comp_size: get(Section::CompSize)?,
            comp_wcc: get(Section::CompWcc)?,
            comp_order: get(Section::CompOrder)?,
            grid_off: get(Section::GridOff)?,
            grid_items: get(Section::GridItems)?,
            data,
        };
        // O(1) spot checks of the index arrays
        ensure!(
            m.u32_at(m.adj_off, 0) == 0 && m.u32_at(m.adj_off, nodes) as usize == links,
            "adjacency offsets do not span the links"
        );
        ensure!(
            m.u32_at(m.grid_off, 0) == 0
                && m.u32_at(m.grid_off, cells) as usize == m.grid_items.len / 4,
            "grid offsets do not span the grid items"
        );
        Ok(m)
    }

    #[inline]
    fn bytes(&self) -> &[u8] {
        match &self.data {
            Backing::Mapped(m) => m,
            Backing::Owned(v) => v,
        }
    }

    /// The `i`-th little-endian `u32` of a section. A bad index (only possible with a damaged
    /// file: callers check ranges) reads as 0, so a query can be wrong but never panics;
    /// `open` checked that every section lies inside the file.
    #[inline]
    fn u32_at(&self, s: Span, i: usize) -> u32 {
        let o = i * 4;
        if o + 4 > s.len {
            return 0;
        }
        self.bytes()
            .get(s.off + o..s.off + o + 4)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    #[inline]
    fn f32_at(&self, s: Span, i: usize) -> f32 {
        f32::from_bits(self.u32_at(s, i))
    }

    #[inline]
    fn u16_at(&self, s: Span, i: usize) -> u16 {
        let o = i * 2;
        if o + 2 > s.len {
            return 0;
        }
        self.bytes()
            .get(s.off + o..s.off + o + 2)
            .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
    }

    #[inline]
    fn u8_at(&self, s: Span, i: usize) -> u8 {
        if i >= s.len {
            return 0;
        }
        self.bytes().get(s.off + i).copied().unwrap_or(0)
    }

    // ---- counts and header values ----------------------------------------------------------

    pub fn node_count(&self) -> usize {
        self.nodes
    }

    pub fn vertex_count(&self) -> usize {
        self.verts
    }

    pub fn link_count(&self) -> usize {
        self.links
    }

    pub fn component_count(&self) -> usize {
        self.comps
    }

    /// `[min_x, min_y, max_x, max_y]` covered by the mesh.
    pub fn bounds(&self) -> [f32; 4] {
        self.bounds
    }

    /// The pass radius (metres) the component tables are exact for.
    pub fn component_radius(&self) -> f32 {
        f32::from(self.comp_radius) * super::format::RADIUS_UNIT
    }

    /// Size of the file in bytes (the mapped size).
    pub fn file_bytes(&self) -> usize {
        self.bytes().len()
    }

    // ---- nodes ------------------------------------------------------------------------------

    /// Whether `n` names a node of this mesh.
    #[inline]
    pub fn contains(&self, n: NodeRef) -> bool {
        (n.0 as usize) < self.nodes
    }

    /// Position of a node (triangle centroid or waypoint).
    #[inline]
    pub fn node_pos(&self, n: NodeRef) -> Vec3 {
        let i = n.0 as usize;
        Vec3::new(
            self.f32_at(self.pos, i * 3),
            self.f32_at(self.pos, i * 3 + 1),
            self.f32_at(self.pos, i * 3 + 2),
        )
    }

    /// The `flag` bits of a node.
    #[inline]
    pub fn node_flags(&self, n: NodeRef) -> u8 {
        self.u8_at(self.flags, n.0 as usize)
    }

    /// A waypoint (building interior, stairs, bridge).
    pub fn is_waypoint(&self, n: NodeRef) -> bool {
        self.node_flags(n) & flag::WAYPOINT != 0
    }

    #[inline]
    pub(crate) fn vertex(&self, i: u32) -> Vec3 {
        let i = i as usize;
        Vec3::new(
            self.f32_at(self.vert, i * 3),
            self.f32_at(self.vert, i * 3 + 1),
            self.f32_at(self.vert, i * 3 + 2),
        )
    }

    /// Whether a vertex is on the border of the walkable mesh (an obstacle corner).
    #[inline]
    pub(crate) fn vertex_is_boundary(&self, i: u32) -> bool {
        self.u8_at(self.vert_flags, i as usize) & super::format::vflag::BOUNDARY != 0
    }

    /// The vertex indices of a node's triangle; `None` for waypoints and bad indices.
    #[inline]
    pub(crate) fn tri_indices(&self, n: NodeRef) -> Option<[u32; 3]> {
        let i = n.0 as usize;
        if i >= self.nodes {
            return None;
        }
        let t = [
            self.u32_at(self.tri, i * 3),
            self.u32_at(self.tri, i * 3 + 1),
            self.u32_at(self.tri, i * 3 + 2),
        ];
        if t[0] == NO_TRI || t.iter().any(|&v| v as usize >= self.verts) {
            return None;
        }
        Some(t)
    }

    /// The three corners of a node's triangle.
    pub fn triangle(&self, n: NodeRef) -> Option<[Vec3; 3]> {
        let t = self.tri_indices(n)?;
        Some(t.map(|i| self.vertex(i)))
    }

    /// Strongly connected component of a node (0 = the largest). Mutually reachable nodes share
    /// it; see [`NavMesh::may_reach`].
    pub fn component_of(&self, n: NodeRef) -> u32 {
        self.u32_at(self.comp, n.0 as usize)
    }

    /// Number of nodes of a component.
    pub fn component_size(&self, comp: u32) -> u32 {
        self.u32_at(self.comp_size, comp as usize)
    }

    /// Two nodes can reach each other (same strongly connected component) for agents of at least
    /// [`NavMesh::component_radius`].
    pub fn same_component(&self, a: NodeRef, b: NodeRef) -> bool {
        self.contains(a) && self.contains(b) && self.component_of(a) == self.component_of(b)
    }

    /// Whether an agent can get from `from` to `to`: `Some(true)` within one component,
    /// `Some(false)` when the component tables rule it out (different weakly connected piece, or
    /// only a one-way route in the other direction), `None` when a search is needed.
    pub fn may_reach(&self, from: NodeRef, to: NodeRef) -> Option<bool> {
        if !self.contains(from) || !self.contains(to) {
            return Some(false);
        }
        let (cf, ct) = (self.component_of(from), self.component_of(to));
        if cf == ct {
            return Some(true);
        }
        if self.u32_at(self.comp_wcc, cf as usize) != self.u32_at(self.comp_wcc, ct as usize) {
            return Some(false);
        }
        // links only lead to components with a lower or equal completion index
        if self.u32_at(self.comp_order, cf as usize) < self.u32_at(self.comp_order, ct as usize) {
            return Some(false);
        }
        None
    }

    // ---- links ------------------------------------------------------------------------------

    /// The range of link indices leaving a node.
    #[inline]
    pub(crate) fn link_range(&self, n: u32) -> (usize, usize) {
        let i = n as usize;
        let lo = self.u32_at(self.adj_off, i) as usize;
        let hi = self.u32_at(self.adj_off, i + 1) as usize;
        if lo <= hi && hi <= self.links {
            (lo, hi)
        } else {
            (0, 0)
        }
    }

    #[inline]
    pub(crate) fn link_target(&self, l: usize) -> u32 {
        self.u32_at(self.adj_target, l)
    }

    #[inline]
    pub(crate) fn link_radius(&self, l: usize) -> u8 {
        self.u8_at(self.adj_radius, l)
    }

    #[inline]
    pub(crate) fn link_cost(&self, l: usize) -> f32 {
        f32::from(self.u16_at(self.adj_cost, l)) * super::format::COST_UNIT
    }

    /// Number of links leaving a node.
    pub fn degree(&self, n: NodeRef) -> usize {
        let (lo, hi) = self.link_range(n.0);
        hi - lo
    }

    /// The nodes reachable in one step by an agent of `agent_radius`.
    pub fn neighbours(&self, n: NodeRef, agent_radius: f32) -> Vec<NodeRef> {
        let need = super::format::agent_radius_units(agent_radius);
        let (lo, hi) = self.link_range(n.0);
        (lo..hi)
            .filter(|&l| self.link_radius(l) >= need)
            .map(|l| NodeRef(self.link_target(l)))
            .filter(|t| self.contains(*t))
            .collect()
    }

    // ---- point location ---------------------------------------------------------------------

    fn cell_of(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        let c = ((x - self.bounds[0]) / self.bucket).floor();
        let r = ((y - self.bounds[1]) / self.bucket).floor();
        // NaN fails both tests
        if !(c >= 0.0 && r >= 0.0 && c < self.grid_w as f32 && r < self.grid_h as f32) {
            return None;
        }
        Some((c as usize, r as usize))
    }

    /// The node ids of one grid cell.
    fn cell_items(&self, col: usize, row: usize) -> impl Iterator<Item = u32> + '_ {
        let cell = row * self.grid_w + col;
        let lo = self.u32_at(self.grid_off, cell) as usize;
        let hi = self.u32_at(self.grid_off, cell + 1) as usize;
        let (lo, hi) = if lo <= hi && hi <= self.grid_items.len / 4 {
            (lo, hi)
        } else {
            (0, 0)
        };
        (lo..hi).map(move |i| self.u32_at(self.grid_items, i))
    }

    /// Finds the node at a world position (multi-floor aware).
    ///
    /// Among the triangles containing the position's x/y the one whose plane is vertically
    /// closest to `pos.z` wins; a waypoint within `max_dist` (3D) beats it only if it is closer
    /// than that vertical gap. When no triangle contains the position, the nearest triangle or
    /// waypoint within `max_dist` is used.
    pub fn locate(&self, pos: Vec3, max_dist: f32) -> Option<NodeRef> {
        self.locate_detailed(pos, max_dist).map(|l| l.node)
    }

    /// [`NavMesh::locate`] with the snapped point and the distance.
    pub fn locate_detailed(&self, pos: Vec3, max_dist: f32) -> Option<Located> {
        if !(pos.x.is_finite() && pos.y.is_finite() && pos.z.is_finite()) {
            return None;
        }
        let max_dist = if max_dist.is_finite() {
            max_dist.clamp(0.0, 512.0)
        } else {
            0.0
        };
        let margin = max_dist.max(1e-3);
        let b = self.bounds;
        if pos.x < b[0] - margin
            || pos.x > b[2] + margin
            || pos.y < b[1] - margin
            || pos.y > b[3] + margin
        {
            return None;
        }
        let (home_c, home_r) = self.cell_of(pos.x.clamp(b[0], b[2]), pos.y.clamp(b[1], b[3]))?;
        let mut best: Option<Located> = None;
        // pass 1: the home cell, triangles that contain the position and nearby waypoints
        for id in self.cell_items(home_c, home_r) {
            self.consider(NodeRef(id), pos, max_dist, true, &mut best);
        }
        if let Some(l) = best {
            if l.inside {
                return Some(l);
            }
        }
        // pass 2: everything within max_dist, triangles by their nearest point
        let reach = ((max_dist / self.bucket).ceil() as usize).clamp(1, 16);
        let c0 = home_c.saturating_sub(reach);
        let c1 = (home_c + reach).min(self.grid_w - 1);
        let r0 = home_r.saturating_sub(reach);
        let r1 = (home_r + reach).min(self.grid_h - 1);
        for r in r0..=r1 {
            for c in c0..=c1 {
                for id in self.cell_items(c, r) {
                    self.consider(NodeRef(id), pos, max_dist, false, &mut best);
                }
            }
        }
        best
    }

    /// Scores one node as a candidate for `locate_detailed`.
    fn consider(
        &self,
        n: NodeRef,
        pos: Vec3,
        max_dist: f32,
        inside_only: bool,
        best: &mut Option<Located>,
    ) {
        if !self.contains(n) {
            return;
        }
        // an "inside" candidate scores by its vertical gap (a huge triangle's plane z is not
        // reliable, so there is no distance limit), everything else by 3D distance
        let cand = match self.tri_indices(n) {
            Some(t) => {
                let [a, b, c] = t.map(|i| self.vertex(i));
                let Some((p, inside)) = tri_nearest(pos, a, b, c) else {
                    return;
                };
                if inside {
                    Located {
                        node: n,
                        point: p,
                        inside: true,
                        dist: (p.z - pos.z).abs(),
                    }
                } else {
                    let d = p.dist(pos);
                    if inside_only || d > max_dist {
                        return;
                    }
                    Located {
                        node: n,
                        point: p,
                        inside: false,
                        dist: d,
                    }
                }
            }
            None => {
                let p = self.node_pos(n);
                let d = p.dist(pos);
                if d > max_dist {
                    return;
                }
                Located {
                    node: n,
                    point: p,
                    inside: false,
                    dist: d,
                }
            }
        };
        let better = match best {
            None => true,
            Some(cur) => {
                cand.dist < cur.dist || (cand.dist == cur.dist && cand.inside && !cur.inside)
            }
        };
        if better {
            *best = Some(cand);
        }
    }

    // ---- deep check -------------------------------------------------------------------------

    /// Scans every index array of the file (O(file size), pages everything in): link targets,
    /// triangle vertices, components and grid items must be in range, offsets monotone.
    pub fn verify(&self) -> anyhow::Result<()> {
        let n = self.nodes;
        let mut prev = 0usize;
        for i in 0..=n {
            let o = self.u32_at(self.adj_off, i) as usize;
            ensure!(
                o >= prev && o <= self.links,
                "adjacency offset {i} out of order"
            );
            prev = o;
        }
        ensure!(
            prev == self.links,
            "adjacency offsets do not end at the link count"
        );
        for l in 0..self.links {
            ensure!(
                (self.link_target(l) as usize) < n,
                "link {l}: target out of range"
            );
        }
        for i in 0..n {
            let c = self.u32_at(self.comp, i) as usize;
            ensure!(c < self.comps, "node {i}: component out of range");
            let t = [
                self.u32_at(self.tri, i * 3),
                self.u32_at(self.tri, i * 3 + 1),
                self.u32_at(self.tri, i * 3 + 2),
            ];
            if t[0] != NO_TRI {
                ensure!(
                    t.iter().all(|&v| (v as usize) < self.verts),
                    "node {i}: triangle vertex out of range"
                );
            }
        }
        for c in 0..self.comps {
            ensure!(
                (self.u32_at(self.comp_size, c) as usize) <= n,
                "component {c}: size out of range"
            );
        }
        let cells = self.grid_w * self.grid_h;
        let mut prev = 0usize;
        for i in 0..=cells {
            let o = self.u32_at(self.grid_off, i) as usize;
            ensure!(
                o >= prev && o <= self.grid_items.len / 4,
                "grid offset {i} out of order"
            );
            prev = o;
        }
        for i in 0..self.grid_items.len / 4 {
            ensure!(
                (self.u32_at(self.grid_items, i) as usize) < n,
                "grid item {i} out of range"
            );
        }
        Ok(())
    }
}

/// The point of triangle `abc` (x/y) nearest to `pos` and whether `pos` is inside it. The point's
/// z is the plane's z there. `None` for a degenerate (zero area in x/y) triangle.
pub(crate) fn tri_nearest(pos: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<(Vec3, bool)> {
    let (px, py) = (f64::from(pos.x), f64::from(pos.y));
    let (ax, ay) = (f64::from(a.x), f64::from(a.y));
    let (bx, by) = (f64::from(b.x), f64::from(b.y));
    let (cx, cy) = (f64::from(c.x), f64::from(c.y));
    let det = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy);
    if det.abs() < 1e-9 {
        return None;
    }
    let bary = |x: f64, y: f64| {
        let l0 = ((by - cy) * (x - cx) + (cx - bx) * (y - cy)) / det;
        let l1 = ((cy - ay) * (x - cx) + (ax - cx) * (y - cy)) / det;
        (l0, l1, 1.0 - l0 - l1)
    };
    let z_of = |(l0, l1, l2): (f64, f64, f64)| {
        (l0 * f64::from(a.z) + l1 * f64::from(b.z) + l2 * f64::from(c.z)) as f32
    };
    const EPS: f64 = -1e-7;
    let w = bary(px, py);
    if w.0 >= EPS && w.1 >= EPS && w.2 >= EPS {
        return Some((Vec3::new(pos.x, pos.y, z_of(w)), true));
    }
    // outside: the nearest point of the three edges
    let mut best = (f64::MAX, 0.0f64, 0.0f64);
    for ((sx, sy), (ex, ey)) in [
        ((ax, ay), (bx, by)),
        ((bx, by), (cx, cy)),
        ((cx, cy), (ax, ay)),
    ] {
        let (dx, dy) = (ex - sx, ey - sy);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((px - sx) * dx + (py - sy) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (qx, qy) = (sx + dx * t, sy + dy * t);
        let d = (px - qx).powi(2) + (py - qy).powi(2);
        if d < best.0 {
            best = (d, qx, qy);
        }
    }
    let w = bary(best.1, best.2);
    Some((Vec3::new(best.1 as f32, best.2 as f32, z_of(w)), false))
}
