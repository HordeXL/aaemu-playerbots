//! `nav_<world>.bin`: the baked navigation mesh (see `docs/DATA-FORMATS.md`).
//!
//! CONTRACT (shared by the `aa-bake` writer and the `aa-world` reader). [`NavData`] is the
//! writer's in-memory form: flat arrays that map one to one onto the file sections. The reader
//! ([`super::NavMesh`]) never builds a `NavData`; it addresses the sections of the memory-mapped
//! file directly.
//!
//! The mesh was extracted from the client's `.bai` navigation data. The file layout, the merge of
//! neighbouring 256 m blocks and the idea of `shared_edge` follow the AAEmu Rust server's
//! `aaemu-game/src/systems/ai/geodata` (GPL-3.0-or-later), rewritten over flat arrays.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{ensure, Context};

pub const NAV_MAGIC: &[u8; 8] = b"PBNAV001";
pub const NAV_VERSION: u32 = 1;
pub const NAV_HEADER_LEN: usize = 64;
pub const NAV_SECTION_ENTRY_LEN: usize = 24;
/// Sections start at multiples of this (a cache line; any element type is aligned).
pub const NAV_SECTION_ALIGN: usize = 64;

/// `node_tri` of a node without a usable triangle (waypoints and nodes with bad geometry).
pub const NO_TRI: u32 = u32::MAX;

/// Link pass radius unit: the file stores `floor(radius / 0.05)` as a `u8` (up to 12.75 m).
pub const RADIUS_UNIT: f32 = 0.05;
/// Link cost unit: the file stores the walking length in 2 cm steps as a `u16` (up to 1310 m).
pub const COST_UNIT: f32 = 0.02;

/// Vertex flag bits (`vertex_flags`).
pub mod vflag {
    /// The vertex is on the border of the walkable mesh (a wall, a cliff, a hole): the path
    /// keeps the agent radius away from it. Interior vertices are not obstacles.
    pub const BOUNDARY: u8 = 1;
}

/// Node flag bits (`node_flags`).
pub mod flag {
    /// A human waypoint (navType 4): building interiors, stairs, bridges. No triangle.
    pub const WAYPOINT: u8 = 1;
    /// Near a road polyline: path finding may prefer edges between two such nodes.
    pub const ROAD: u8 = 2;
}

/// The sections of the file. The numbers are part of the format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Section {
    /// `[f32; 3]` per node: triangle centroid or waypoint position (world metres).
    NodePos = 1,
    /// `[u32; 3]` per node: vertex indices of the triangle, [`NO_TRI`] ×3 without one.
    NodeTri = 2,
    /// `u8` per node: [`flag`] bits.
    NodeFlags = 3,
    /// `u32` per node: strongly connected component id (0 = the largest).
    NodeComp = 4,
    /// `[f32; 3]` per vertex.
    Vertices = 5,
    /// `u32` × (nodes + 1): CSR offsets into the link arrays.
    AdjOff = 6,
    /// `u32` per link: target node.
    AdjTarget = 7,
    /// `u8` per link: widest agent radius that may pass, in [`RADIUS_UNIT`]s.
    AdjRadius = 8,
    /// `u16` per link: walking cost in [`COST_UNIT`]s.
    AdjCost = 9,
    /// `u32` per component: number of nodes.
    CompSize = 10,
    /// `u32` per component: weakly connected component id.
    CompWcc = 11,
    /// `u32` per component: completion index of the strongly connected component (a link never
    /// leads from a component to one with a higher index).
    CompOrder = 12,
    /// `u32` × (grid cells + 1): CSR offsets into [`Section::GridItems`], row-major.
    GridOff = 13,
    /// `u32` per entry: node ids whose triangle (or position) touches the cell.
    GridItems = 14,
    /// `u8` per vertex: [`vflag`] bits.
    VertFlags = 15,
}

impl Section {
    pub const ALL: [Section; 15] = [
        Section::NodePos,
        Section::NodeTri,
        Section::NodeFlags,
        Section::NodeComp,
        Section::Vertices,
        Section::AdjOff,
        Section::AdjTarget,
        Section::AdjRadius,
        Section::AdjCost,
        Section::CompSize,
        Section::CompWcc,
        Section::CompOrder,
        Section::GridOff,
        Section::GridItems,
        Section::VertFlags,
    ];

    pub fn from_id(id: u32) -> Option<Section> {
        Self::ALL.iter().copied().find(|s| *s as u32 == id)
    }

    /// Bytes per element.
    pub fn elem_size(self) -> usize {
        match self {
            Section::NodePos | Section::NodeTri | Section::Vertices => 12,
            Section::NodeFlags | Section::AdjRadius | Section::VertFlags => 1,
            Section::AdjCost => 2,
            _ => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Section::NodePos => "node positions",
            Section::NodeTri => "node triangles",
            Section::NodeFlags => "node flags",
            Section::NodeComp => "node components",
            Section::Vertices => "vertices",
            Section::AdjOff => "adjacency offsets",
            Section::AdjTarget => "link targets",
            Section::AdjRadius => "link radii",
            Section::AdjCost => "link costs",
            Section::CompSize => "component sizes",
            Section::CompWcc => "component wcc",
            Section::CompOrder => "component order",
            Section::GridOff => "grid offsets",
            Section::GridItems => "grid items",
            Section::VertFlags => "vertex flags",
        }
    }
}

/// Stored radius units for a pass radius in metres (`floor`, so a stored value never promises more
/// than the engine's radius). Negative / NaN radii (blocked links) give 0.
pub fn radius_to_units(r: f32) -> u8 {
    if r.is_nan() || r < 0.0 {
        return 0;
    }
    // +0.01 so that 0.35 (stored as 0.3499999) still maps to 7
    (r / RADIUS_UNIT + 0.01).floor().min(255.0) as u8
}

/// The smallest stored radius a link needs to admit an agent of `agent_radius` metres.
pub fn agent_radius_units(agent_radius: f32) -> u8 {
    if agent_radius.is_nan() || agent_radius <= 0.0 {
        return 0;
    }
    (agent_radius / RADIUS_UNIT - 0.01).ceil().clamp(0.0, 255.0) as u8
}

/// Stored cost units for a walking length in metres (rounded up, at least 1).
pub fn cost_to_units(len: f32) -> u16 {
    if !len.is_finite() || len <= 0.0 {
        return 1;
    }
    (len / COST_UNIT).ceil().clamp(1.0, 65535.0) as u16
}

/// The corner indices (into `a`) of the edge triangles `a` and `b` share, when they share exactly
/// two vertices; `None` otherwise (no common edge, or a degenerate triangle).
pub fn shared_edge(a: [u32; 3], b: [u32; 3]) -> Option<(usize, usize)> {
    let mut found = [usize::MAX; 2];
    let mut n = 0;
    for (i, va) in a.iter().enumerate() {
        if b.contains(va) {
            if n == 2 {
                return None;
            }
            found[n] = i;
            n += 1;
        }
    }
    (n == 2).then_some((found[0], found[1]))
}

/// The baked navigation mesh in memory (writer side).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NavData {
    /// `[min_x, min_y, max_x, max_y]` of all nodes and vertices.
    pub bounds: [f32; 4],
    /// Grid cell size in metres.
    pub bucket: f32,
    pub grid_w: u32,
    pub grid_h: u32,
    /// The pass radius the component tables were computed for, in [`RADIUS_UNIT`]s.
    pub comp_radius: u8,
    pub node_pos: Vec<[f32; 3]>,
    pub node_tri: Vec<[u32; 3]>,
    pub node_flags: Vec<u8>,
    pub node_comp: Vec<u32>,
    pub vertices: Vec<[f32; 3]>,
    pub vertex_flags: Vec<u8>,
    pub adj_off: Vec<u32>,
    pub adj_target: Vec<u32>,
    pub adj_radius: Vec<u8>,
    pub adj_cost: Vec<u16>,
    pub comp_size: Vec<u32>,
    pub comp_wcc: Vec<u32>,
    pub comp_order: Vec<u32>,
    pub grid_off: Vec<u32>,
    pub grid_items: Vec<u32>,
}

/// Where the sections of a file lie (computed by [`NavData::layout`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionSpan {
    pub section: Section,
    pub offset: u64,
    pub count: u64,
}

impl NavData {
    pub fn node_count(&self) -> usize {
        self.node_pos.len()
    }

    pub fn link_count(&self) -> usize {
        self.adj_target.len()
    }

    /// Sets `bounds` from the node positions and vertices.
    pub fn compute_bounds(&mut self) {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in self.node_pos.iter().chain(self.vertices.iter()) {
            b[0] = b[0].min(p[0]);
            b[1] = b[1].min(p[1]);
            b[2] = b[2].max(p[0]);
            b[3] = b[3].max(p[1]);
        }
        if b[0] > b[2] {
            b = [0.0; 4];
        }
        self.bounds = b;
    }

    /// The triangle of a node (`None` for waypoints / bad geometry / bad indices).
    pub fn triangle(&self, node: usize) -> Option<[usize; 3]> {
        let t = *self.node_tri.get(node)?;
        if t[0] == NO_TRI {
            return None;
        }
        let v = self.vertices.len();
        let t = t.map(|i| i as usize);
        t.iter().all(|&i| i < v).then_some(t)
    }

    /// Sets `vertex_flags`: a vertex is a [`vflag::BOUNDARY`] vertex when it is an end of a
    /// triangle edge that no link of at least `min_units` radius crosses (to a triangle sharing
    /// that edge).
    pub fn compute_vertex_flags(&mut self, min_units: u8) {
        let n = self.node_count();
        let mut flags = vec![0u8; self.vertices.len()];
        // per triangle: which of its edges (edge k joins corners k and k+1) are crossed
        let mut crossed = vec![0u8; n];
        for (a, mask) in crossed.iter_mut().enumerate() {
            let ta = self.node_tri[a];
            if ta[0] == NO_TRI {
                continue;
            }
            for l in self.adj_off[a] as usize..self.adj_off[a + 1] as usize {
                if self.adj_radius[l] < min_units {
                    continue;
                }
                let tb = self.node_tri[self.adj_target[l] as usize];
                if tb[0] == NO_TRI {
                    continue;
                }
                if let Some((i, j)) = shared_edge(ta, tb) {
                    // corners i < j: the edge is k = i when j = i + 1, else (0, 2) is edge 2
                    let k = if j == i + 1 { i } else { 2 };
                    *mask |= 1 << k;
                }
            }
        }
        for (ta, mask) in self.node_tri.iter().zip(&crossed) {
            if ta[0] == NO_TRI {
                continue;
            }
            for k in 0..3 {
                if mask & (1 << k) == 0 {
                    for c in [ta[k], ta[(k + 1) % 3]] {
                        if let Some(f) = flags.get_mut(c as usize) {
                            *f |= vflag::BOUNDARY;
                        }
                    }
                }
            }
        }
        self.vertex_flags = flags;
    }

    /// Fills `adj_cost` from the geometry: through the middle of the shared edge for two
    /// triangles (2D), the straight 3D distance between the node positions otherwise.
    pub fn compute_costs(&mut self) {
        let mut cost = Vec::with_capacity(self.adj_target.len());
        for a in 0..self.node_count() {
            let (lo, hi) = (self.adj_off[a] as usize, self.adj_off[a + 1] as usize);
            let pa = self.node_pos[a];
            let ta = self.node_tri[a];
            for l in lo..hi {
                let b = self.adj_target[l] as usize;
                let pb = self.node_pos[b];
                let tb = self.node_tri[b];
                let shared = (ta[0] != NO_TRI && tb[0] != NO_TRI)
                    .then(|| shared_edge(ta, tb))
                    .flatten();
                let len = match shared {
                    Some((i, j)) => {
                        let (v1, v2) =
                            (self.vertices[ta[i] as usize], self.vertices[ta[j] as usize]);
                        let (mx, my) = ((v1[0] + v2[0]) * 0.5, (v1[1] + v2[1]) * 0.5);
                        let d1 = ((pa[0] - mx).powi(2) + (pa[1] - my).powi(2)).sqrt();
                        let d2 = ((pb[0] - mx).powi(2) + (pb[1] - my).powi(2)).sqrt();
                        d1 + d2
                    }
                    None => ((pa[0] - pb[0]).powi(2)
                        + (pa[1] - pb[1]).powi(2)
                        + (pa[2] - pb[2]).powi(2))
                    .sqrt(),
                };
                cost.push(cost_to_units(len));
            }
        }
        self.adj_cost = cost;
    }

    /// Builds the point location grid (`grid_*`, `bucket`) with cells of `bucket` metres over
    /// `bounds` (call [`NavData::compute_bounds`] first). Every triangle goes into each cell it
    /// touches, every other node into the cell of its position.
    pub fn build_grid(&mut self, bucket: f32) -> anyhow::Result<()> {
        ensure!(
            bucket.is_finite() && bucket >= 1.0,
            "bad grid cell size {bucket}"
        );
        let [min_x, min_y, max_x, max_y] = self.bounds;
        let gw = (((max_x - min_x) / bucket).floor() as u64 + 1).max(1);
        let gh = (((max_y - min_y) / bucket).floor() as u64 + 1).max(1);
        ensure!(
            gw * gh <= 1 << 26,
            "grid of {gw}x{gh} cells is too large for {bucket} m cells"
        );
        self.bucket = bucket;
        self.grid_w = gw as u32;
        self.grid_h = gh as u32;
        let cells = (gw * gh) as usize;
        let geo = GridGeo {
            min_x: f64::from(min_x),
            min_y: f64::from(min_y),
            bucket: f64::from(bucket),
            gw: gw as usize,
            gh: gh as usize,
        };
        let mut off = vec![0u32; cells + 1];
        // pass 1: count, pass 2: fill
        for pass in 0..2 {
            let mut fill = if pass == 1 {
                let mut total = 0u32;
                for o in off.iter_mut() {
                    let c = *o;
                    *o = total;
                    total = total
                        .checked_add(c)
                        .context("point location grid has more than 2^32 entries")?;
                }
                vec![0u32; total as usize]
            } else {
                Vec::new()
            };
            let mut cursor = if pass == 1 { off.clone() } else { Vec::new() };
            for node in 0..self.node_count() {
                let mut emit = |cell: usize| {
                    if pass == 0 {
                        off[cell] += 1;
                    } else {
                        fill[cursor[cell] as usize] = node as u32;
                        cursor[cell] += 1;
                    }
                };
                match self.triangle(node) {
                    Some(t) => {
                        let p = t.map(|i| {
                            let v = self.vertices[i];
                            (f64::from(v[0]), f64::from(v[1]))
                        });
                        geo.cover_triangle(p, &mut emit);
                    }
                    None => {
                        let p = self.node_pos[node];
                        emit(geo.cell_of(f64::from(p[0]), f64::from(p[1])));
                    }
                }
            }
            if pass == 1 {
                self.grid_items = fill;
            }
        }
        self.grid_off = off;
        Ok(())
    }

    /// Checks that the arrays agree with each other (sizes, offsets, indices). The reader's deep
    /// check ([`super::NavMesh::verify`]) repeats this on the mapped file.
    pub fn validate(&self) -> anyhow::Result<()> {
        let n = self.node_count();
        let l = self.link_count();
        let c = self.comp_size.len();
        ensure!(n > 0, "empty navigation mesh");
        ensure!(
            n < u32::MAX as usize / 2 && l < u32::MAX as usize,
            "mesh too large"
        );
        ensure!(
            self.node_tri.len() == n && self.node_flags.len() == n && self.node_comp.len() == n
        );
        ensure!(self.adj_off.len() == n + 1, "adjacency offsets");
        ensure!(
            self.adj_radius.len() == l && self.adj_cost.len() == l,
            "link arrays"
        );
        ensure!(
            self.comp_wcc.len() == c && self.comp_order.len() == c,
            "component tables"
        );
        ensure!(
            self.grid_off.len() == self.grid_w as usize * self.grid_h as usize + 1,
            "grid offsets"
        );
        ensure!(
            self.adj_off[0] == 0 && self.adj_off[n] as usize == l,
            "adjacency range"
        );
        ensure!(
            self.adj_off.windows(2).all(|w| w[0] <= w[1]),
            "adjacency offsets not monotone"
        );
        ensure!(
            self.adj_target.iter().all(|&t| (t as usize) < n),
            "link target out of range"
        );
        ensure!(
            self.node_comp.iter().all(|&k| (k as usize) < c),
            "node component out of range"
        );
        let v = self.vertices.len();
        ensure!(self.vertex_flags.len() == v, "vertex flags");
        ensure!(
            self.node_tri
                .iter()
                .all(|t| t[0] == NO_TRI || t.iter().all(|&i| (i as usize) < v)),
            "triangle vertex out of range"
        );
        ensure!(
            self.grid_off.windows(2).all(|w| w[0] <= w[1])
                && self.grid_off.last().copied().unwrap_or(0) as usize == self.grid_items.len(),
            "grid offsets"
        );
        ensure!(
            self.grid_items.iter().all(|&i| (i as usize) < n),
            "grid item out of range"
        );
        ensure!(
            self.bucket.is_finite() && self.bucket > 0.0,
            "grid cell size"
        );
        Ok(())
    }

    fn counts(&self, s: Section) -> u64 {
        (match s {
            Section::NodePos | Section::NodeTri | Section::NodeFlags | Section::NodeComp => {
                self.node_pos.len()
            }
            Section::Vertices | Section::VertFlags => self.vertices.len(),
            Section::AdjOff => self.adj_off.len(),
            Section::AdjTarget | Section::AdjRadius | Section::AdjCost => self.adj_target.len(),
            Section::CompSize | Section::CompWcc | Section::CompOrder => self.comp_size.len(),
            Section::GridOff => self.grid_off.len(),
            Section::GridItems => self.grid_items.len(),
        }) as u64
    }

    /// Offsets of the sections and the total file size.
    pub fn layout(&self) -> (Vec<SectionSpan>, u64) {
        let table_end = NAV_HEADER_LEN + Section::ALL.len() * NAV_SECTION_ENTRY_LEN;
        let mut pos = table_end.next_multiple_of(NAV_SECTION_ALIGN) as u64;
        let mut spans = Vec::with_capacity(Section::ALL.len());
        for s in Section::ALL {
            let count = self.counts(s);
            spans.push(SectionSpan {
                section: s,
                offset: pos,
                count,
            });
            pos += count * s.elem_size() as u64;
            pos = pos.next_multiple_of(NAV_SECTION_ALIGN as u64);
        }
        (spans, pos)
    }

    /// Writes the file (header, section table, sections). Validates first.
    pub fn write_to(&self, out: &mut impl Write) -> anyhow::Result<()> {
        self.validate()?;
        let (spans, total) = self.layout();
        let mut head = [0u8; NAV_HEADER_LEN];
        head[0..8].copy_from_slice(NAV_MAGIC);
        let put_u32 =
            |h: &mut [u8], o: usize, v: u32| h[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let put_f32 =
            |h: &mut [u8], o: usize, v: f32| h[o..o + 4].copy_from_slice(&v.to_le_bytes());
        put_u32(&mut head, 8, NAV_VERSION);
        put_u32(&mut head, 12, self.node_count() as u32);
        put_u32(&mut head, 16, self.vertices.len() as u32);
        put_u32(&mut head, 20, self.link_count() as u32);
        put_u32(&mut head, 24, self.comp_size.len() as u32);
        put_u32(&mut head, 28, spans.len() as u32);
        for (i, b) in self.bounds.iter().enumerate() {
            put_f32(&mut head, 32 + 4 * i, *b);
        }
        put_f32(&mut head, 48, self.bucket);
        put_u32(&mut head, 52, self.grid_w);
        put_u32(&mut head, 56, self.grid_h);
        head[60] = self.comp_radius;
        out.write_all(&head)?;
        for sp in &spans {
            let mut e = [0u8; NAV_SECTION_ENTRY_LEN];
            e[0..4].copy_from_slice(&(sp.section as u32).to_le_bytes());
            e[4..8].copy_from_slice(&(sp.section.elem_size() as u32).to_le_bytes());
            e[8..16].copy_from_slice(&sp.offset.to_le_bytes());
            e[16..24].copy_from_slice(&sp.count.to_le_bytes());
            out.write_all(&e)?;
        }
        let mut written = (NAV_HEADER_LEN + spans.len() * NAV_SECTION_ENTRY_LEN) as u64;
        let zeros = [0u8; NAV_SECTION_ALIGN];
        for sp in &spans {
            let pad = (sp.offset - written) as usize;
            out.write_all(&zeros[..pad])?;
            written += pad as u64;
            written += self.write_section(out, sp.section)?;
        }
        let pad = (total - written) as usize;
        ensure!(
            pad < NAV_SECTION_ALIGN,
            "internal error: trailing padding {pad}"
        );
        out.write_all(&zeros[..pad])?;
        Ok(())
    }

    fn write_section(&self, out: &mut impl Write, s: Section) -> io::Result<u64> {
        fn put<T: Copy>(
            out: &mut impl Write,
            data: &[T],
            conv: impl Fn(T, &mut Vec<u8>),
        ) -> io::Result<u64> {
            let mut buf = Vec::with_capacity(1 << 16);
            let mut n = 0u64;
            for chunk in data.chunks(1 << 13) {
                buf.clear();
                for v in chunk {
                    conv(*v, &mut buf);
                }
                n += buf.len() as u64;
                out.write_all(&buf)?;
            }
            Ok(n)
        }
        fn f3(v: [f32; 3], b: &mut Vec<u8>) {
            for c in v {
                b.extend_from_slice(&c.to_le_bytes());
            }
        }
        fn u3(v: [u32; 3], b: &mut Vec<u8>) {
            for c in v {
                b.extend_from_slice(&c.to_le_bytes());
            }
        }
        fn u1(v: u32, b: &mut Vec<u8>) {
            b.extend_from_slice(&v.to_le_bytes());
        }
        match s {
            Section::NodePos => put(out, &self.node_pos, f3),
            Section::NodeTri => put(out, &self.node_tri, u3),
            Section::NodeFlags => {
                out.write_all(&self.node_flags)?;
                Ok(self.node_flags.len() as u64)
            }
            Section::NodeComp => put(out, &self.node_comp, u1),
            Section::Vertices => put(out, &self.vertices, f3),
            Section::VertFlags => {
                out.write_all(&self.vertex_flags)?;
                Ok(self.vertex_flags.len() as u64)
            }
            Section::AdjOff => put(out, &self.adj_off, u1),
            Section::AdjTarget => put(out, &self.adj_target, u1),
            Section::AdjRadius => {
                out.write_all(&self.adj_radius)?;
                Ok(self.adj_radius.len() as u64)
            }
            Section::AdjCost => put(out, &self.adj_cost, |v, b| {
                b.extend_from_slice(&v.to_le_bytes())
            }),
            Section::CompSize => put(out, &self.comp_size, u1),
            Section::CompWcc => put(out, &self.comp_wcc, u1),
            Section::CompOrder => put(out, &self.comp_order, u1),
            Section::GridOff => put(out, &self.grid_off, u1),
            Section::GridItems => put(out, &self.grid_items, u1),
        }
    }

    /// The whole file in memory (tests, small meshes).
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let mut v = Vec::with_capacity(self.layout().1 as usize);
        self.write_to(&mut v)?;
        Ok(v)
    }

    /// Writes the file to `path`.
    pub fn write_file(&self, path: &Path) -> anyhow::Result<()> {
        let file =
            std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut out = io::BufWriter::with_capacity(1 << 20, file);
        self.write_to(&mut out)
            .with_context(|| format!("write {}", path.display()))?;
        out.flush()?;
        Ok(())
    }
}

/// Grid geometry for [`NavData::build_grid`].
struct GridGeo {
    min_x: f64,
    min_y: f64,
    bucket: f64,
    gw: usize,
    gh: usize,
}

impl GridGeo {
    fn col(&self, x: f64) -> usize {
        (((x - self.min_x) / self.bucket).floor().max(0.0) as usize).min(self.gw - 1)
    }

    fn row(&self, y: f64) -> usize {
        (((y - self.min_y) / self.bucket).floor().max(0.0) as usize).min(self.gh - 1)
    }

    fn cell_of(&self, x: f64, y: f64) -> usize {
        self.row(y) * self.gw + self.col(x)
    }

    /// Calls `emit(cell)` for every cell the triangle touches (a slightly padded, conservative
    /// cover: per row of cells, the x extent of the triangle inside the row's slab).
    fn cover_triangle(&self, p: [(f64, f64); 3], emit: &mut impl FnMut(usize)) {
        const PAD: f64 = 1e-3;
        let min_y = p.iter().map(|v| v.1).fold(f64::MAX, f64::min) - PAD;
        let max_y = p.iter().map(|v| v.1).fold(f64::MIN, f64::max) + PAD;
        let (r0, r1) = (self.row(min_y), self.row(max_y));
        for r in r0..=r1 {
            let lo = (self.min_y + r as f64 * self.bucket - PAD).max(min_y);
            let hi = (self.min_y + (r + 1) as f64 * self.bucket + PAD).min(max_y);
            let mut xmin = f64::MAX;
            let mut xmax = f64::MIN;
            for k in 0..3 {
                let (a, b) = (p[k], p[(k + 1) % 3]);
                let dy = b.1 - a.1;
                let (t0, t1) = if dy.abs() < 1e-12 {
                    if a.1 < lo || a.1 > hi {
                        continue;
                    }
                    (0.0, 1.0)
                } else {
                    let (ta, tb) = ((lo - a.1) / dy, (hi - a.1) / dy);
                    (ta.min(tb).max(0.0), ta.max(tb).min(1.0))
                };
                if t0 > t1 {
                    continue;
                }
                for t in [t0, t1] {
                    let x = a.0 + (b.0 - a.0) * t;
                    xmin = xmin.min(x);
                    xmax = xmax.max(x);
                }
            }
            if xmin > xmax {
                continue;
            }
            let (c0, c1) = (self.col(xmin - PAD), self.col(xmax + PAD));
            for c in c0..=c1 {
                emit(r * self.gw + c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_units_round_trip_conservatively() {
        assert_eq!(radius_to_units(-1.0), 0);
        assert_eq!(radius_to_units(f32::NAN), 0);
        assert_eq!(radius_to_units(0.0), 0);
        assert_eq!(radius_to_units(0.35), 7);
        assert_eq!(radius_to_units(0.4), 8);
        assert_eq!(radius_to_units(0.5), 10);
        assert_eq!(radius_to_units(0.54), 10);
        assert_eq!(radius_to_units(1000.0), 255);
        assert_eq!(agent_radius_units(0.5), 10);
        assert_eq!(agent_radius_units(0.25), 5);
        assert_eq!(agent_radius_units(0.51), 11);
        assert_eq!(agent_radius_units(0.0), 0);
        // an agent of exactly the link radius passes
        for r in [0.4f32, 0.5, 0.75, 1.0, 1.25] {
            assert!(radius_to_units(r) >= agent_radius_units(r), "{r}");
        }
    }

    #[test]
    fn shared_edge_needs_exactly_two() {
        assert_eq!(shared_edge([1, 2, 3], [3, 2, 9]), Some((1, 2)));
        assert_eq!(shared_edge([1, 2, 3], [1, 5, 6]), None);
        assert_eq!(shared_edge([1, 2, 3], [4, 5, 6]), None);
        assert_eq!(shared_edge([1, 2, 3], [3, 1, 2]), None, "same triangle");
    }

    #[test]
    fn cost_units() {
        assert_eq!(cost_to_units(0.0), 1);
        assert_eq!(cost_to_units(1.0), 50);
        assert_eq!(cost_to_units(1.0001), 51);
        assert_eq!(cost_to_units(1e9), 65535);
        assert_eq!(cost_to_units(f32::NAN), 1);
    }
}
