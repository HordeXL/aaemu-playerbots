//! Synthetic meshes for the tests of the `nav` module (not compiled outside tests).

use std::collections::HashMap;

use super::format::{flag, radius_to_units, NavData, NO_TRI};
use super::NavMesh;

pub(crate) struct TestMesh {
    verts: Vec<[f32; 3]>,
    vert_index: HashMap<[u32; 3], u32>,
    nodes: Vec<([f32; 3], [u32; 3], u8)>,
    links: Vec<(usize, usize, u8)>,
    pub bucket: f32,
    pub comp_radius_m: f32,
    /// Skip the brute-force component search (meshes of thousands of nodes): one component.
    pub single_component: bool,
}

impl TestMesh {
    pub fn new() -> Self {
        TestMesh {
            verts: Vec::new(),
            vert_index: HashMap::new(),
            nodes: Vec::new(),
            links: Vec::new(),
            bucket: 8.0,
            comp_radius_m: 0.5,
            single_component: false,
        }
    }

    fn vertex(&mut self, p: [f32; 3]) -> u32 {
        let key = p.map(f32::to_bits);
        if let Some(&i) = self.vert_index.get(&key) {
            return i;
        }
        let i = self.verts.len() as u32;
        self.verts.push(p);
        self.vert_index.insert(key, i);
        i
    }

    /// A triangle node; vertices equal to earlier ones are shared.
    pub fn tri(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> usize {
        let t = [self.vertex(a), self.vertex(b), self.vertex(c)];
        let pos = [
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ];
        self.nodes.push((pos, t, 0));
        self.nodes.len() - 1
    }

    /// A waypoint node (no triangle).
    pub fn waypoint(&mut self, p: [f32; 3]) -> usize {
        self.nodes.push((p, [NO_TRI; 3], flag::WAYPOINT));
        self.nodes.len() - 1
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn node_pos(&self, node: usize) -> [f32; 3] {
        self.nodes[node].0
    }

    pub fn flag(&mut self, node: usize, f: u8) {
        self.nodes[node].2 |= f;
    }

    /// A one-way link with the given pass radius (metres; negative = blocked).
    pub fn link(&mut self, a: usize, b: usize, radius: f32) {
        self.links.push((a, b, radius_to_units(radius)));
    }

    pub fn link2(&mut self, a: usize, b: usize, radius: f32) {
        self.link(a, b, radius);
        self.link(b, a, radius);
    }

    /// Links every pair of triangles that share an edge, both ways.
    pub fn link_neighbours(&mut self, radius: f32) {
        let mut edges: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        for (i, (_, t, _)) in self.nodes.iter().enumerate() {
            if t[0] == NO_TRI {
                continue;
            }
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                edges.entry((a.min(b), a.max(b))).or_default().push(i);
            }
        }
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        for nodes in edges.values() {
            for (i, &a) in nodes.iter().enumerate() {
                for &b in &nodes[i + 1..] {
                    pairs.push((a.min(b), a.max(b)));
                }
            }
        }
        pairs.sort_unstable();
        pairs.dedup();
        for (a, b) in pairs {
            self.link2(a, b, radius);
        }
    }

    /// A `w` x `h` grid of square cells of `cell` metres from (x0, y0), two triangles per cell
    /// (z from `z`), skipping the cells for which `skip(cx, cy)`. Returns the node ids by cell.
    pub fn grid(
        &mut self,
        (x0, y0): (f32, f32),
        (w, h): (usize, usize),
        cell: f32,
        z: impl Fn(f32, f32) -> f32,
        skip: impl Fn(usize, usize) -> bool,
    ) -> HashMap<(usize, usize), (usize, usize)> {
        let mut ids = HashMap::new();
        for cy in 0..h {
            for cx in 0..w {
                if skip(cx, cy) {
                    continue;
                }
                let (xa, ya) = (x0 + cx as f32 * cell, y0 + cy as f32 * cell);
                let (xb, yb) = (xa + cell, ya + cell);
                let p = |x: f32, y: f32| [x, y, z(x, y)];
                let t1 = self.tri(p(xa, ya), p(xb, ya), p(xb, yb));
                let t2 = self.tri(p(xa, ya), p(xb, yb), p(xa, yb));
                ids.insert((cx, cy), (t1, t2));
            }
        }
        ids
    }

    /// The writer-side data: CSR links, costs, grid, and the component tables computed by brute
    /// force (small meshes only).
    pub fn data(self) -> NavData {
        let n = self.nodes.len();
        let mut d = NavData {
            comp_radius: radius_to_units(self.comp_radius_m),
            ..NavData::default()
        };
        d.vertices = self.verts;
        for (pos, tri, fl) in &self.nodes {
            d.node_pos.push(*pos);
            d.node_tri.push(*tri);
            d.node_flags.push(*fl);
        }
        let mut links = self.links;
        links.sort_by_key(|l| (l.0, l.1));
        let mut off = vec![0u32; n + 1];
        for l in &links {
            off[l.0 + 1] += 1;
        }
        for i in 0..n {
            off[i + 1] += off[i];
        }
        d.adj_off = off;
        d.adj_target = links.iter().map(|l| l.1 as u32).collect();
        d.adj_radius = links.iter().map(|l| l.2).collect();

        if self.single_component {
            d.node_comp = vec![0; n];
            d.comp_size = vec![n as u32];
            d.comp_wcc = vec![0];
            d.comp_order = vec![0];
            d.compute_bounds();
            d.compute_costs();
            d.compute_vertex_flags(super::format::radius_to_units(0.4));
            d.build_grid(self.bucket).unwrap();
            d.validate().unwrap();
            return d;
        }
        // components over the links an agent of comp_radius may use
        let ok = |l: &(usize, usize, u8)| l.2 >= d.comp_radius;
        let mut reach = vec![vec![false; n]; n];
        for (i, r) in reach.iter_mut().enumerate() {
            r[i] = true;
            let mut stack = vec![i];
            while let Some(a) = stack.pop() {
                for l in links.iter().filter(|l| l.0 == a && ok(l)) {
                    if !r[l.1] {
                        r[l.1] = true;
                        stack.push(l.1);
                    }
                }
            }
        }
        let mut comp = vec![usize::MAX; n];
        let mut comps: Vec<Vec<usize>> = Vec::new();
        for i in 0..n {
            if comp[i] != usize::MAX {
                continue;
            }
            let members: Vec<usize> = (0..n).filter(|&j| reach[i][j] && reach[j][i]).collect();
            for &m in &members {
                comp[m] = comps.len();
            }
            comps.push(members);
        }
        // weak components
        let mut wcc: Vec<usize> = (0..comps.len()).collect();
        fn find(p: &mut [usize], mut a: usize) -> usize {
            while p[a] != a {
                p[a] = p[p[a]];
                a = p[a];
            }
            a
        }
        for l in links.iter().filter(|l| ok(l)) {
            let (a, b) = (find(&mut wcc, comp[l.0]), find(&mut wcc, comp[l.1]));
            if a != b {
                wcc[a] = b;
            }
        }
        d.node_comp = comp.iter().map(|&c| c as u32).collect();
        d.comp_size = comps.iter().map(|m| m.len() as u32).collect();
        d.comp_wcc = (0..comps.len()).map(|c| find(&mut wcc, c) as u32).collect();
        // order: the number of components reachable from it, so links never lead to a higher one
        d.comp_order = comps
            .iter()
            .map(|m| {
                let r = &reach[m[0]];
                let mut seen: Vec<usize> = (0..n).filter(|&j| r[j]).map(|j| comp[j]).collect();
                seen.sort_unstable();
                seen.dedup();
                seen.len() as u32
            })
            .collect();
        d.compute_bounds();
        d.compute_costs();
        d.compute_vertex_flags(super::format::radius_to_units(0.4));
        let bucket = self.bucket;
        d.build_grid(bucket).unwrap();
        d.validate().unwrap();
        d
    }

    pub fn build(self) -> NavMesh {
        NavMesh::from_bytes(self.data().to_bytes().unwrap()).unwrap()
    }
}
