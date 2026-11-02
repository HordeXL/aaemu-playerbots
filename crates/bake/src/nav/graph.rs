//! From the merged graph to the baked mesh: de-duplicate and filter the links, drop small
//! islands, compute the connectivity tables, lay the nodes out in space-filling-curve order and
//! build the point location grid.
//!
//! * links: a second link between the same two nodes overwrites the first (last read wins, as in
//!   the engine); blocked links (radius -1) and links narrower than [`MIN_LINK_UNITS`] are dropped;
//! * islands: weakly connected components (over the kept links, in both directions) with fewer
//!   than `min_component_nodes` nodes are dropped;
//! * components: strongly connected components over the links an agent of [`COMP_UNITS`] may use
//!   (iterative Tarjan: the graph has millions of nodes); the SCC completion order and the weakly
//!   connected component of every SCC are stored so that path finding can rule out unreachable
//!   goals without searching, see `aa_world::nav::NavMesh::may_reach`.

use aa_world::nav::format::{flag, NavData};

use super::bai::NAV_TYPE_WAYPOINT;
use super::merge::{BLink, NavBuilder, NONE};

/// Links with a stored pass radius below this are dropped (0.4 m).
pub const MIN_LINK_UNITS: u8 = 8;
/// The agent radius the component tables are computed for (0.5 m, the default bot radius).
pub const COMP_UNITS: u8 = 10;

/// Settings of [`assemble`].
#[derive(Debug, Clone, Copy)]
pub struct AssembleOptions {
    /// Weakly connected components with fewer nodes than this are dropped.
    pub min_component_nodes: usize,
    /// Grid cell size in metres; `None` picks one from the extent of the mesh.
    pub bucket: Option<f32>,
}

/// Counters of the assembly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssembleStats {
    pub nodes_merged: usize,
    pub triangles: usize,
    pub waypoints: usize,
    pub links_merged: usize,
    pub links_duplicate: usize,
    /// Duplicate links whose copies disagree on the pass radius class.
    pub links_duplicate_conflict: usize,
    pub links_blocked: usize,
    pub links_narrow: usize,
    pub links_kept: usize,
    pub wcc_total: usize,
    pub wcc_kept: usize,
    pub nodes_dropped: usize,
    pub nodes_kept: usize,
    pub triangles_kept: usize,
    pub waypoints_kept: usize,
    pub vertices_kept: usize,
    /// Vertices on the border of the walkable mesh (the path keeps the agent radius away).
    pub boundary_vertices: usize,
    pub links_final: usize,
    pub scc_count: usize,
    pub scc_largest: usize,
    pub scc_singletons: usize,
    pub nodes_in_scc_50: usize,
    pub nodes_in_scc_1000: usize,
    pub one_way_links: usize,
    /// Triangle-to-triangle links without a common edge (the route passes the node position).
    pub links_no_portal: usize,
    pub bucket: f32,
    pub grid_cells: usize,
    pub grid_items: usize,
}

fn find(parent: &mut [u32], mut a: u32) -> u32 {
    while parent[a as usize] != a {
        let p = parent[a as usize];
        parent[a as usize] = parent[p as usize];
        a = parent[a as usize];
    }
    a
}

/// Strongly connected components over the links with `units >= min_units` (iterative Tarjan).
/// Returns the component of every node, numbered in completion order, and their count. A link
/// never leads from a component to one with a higher number.
pub fn tarjan_scc(off: &[u32], tgt: &[u32], units: &[u8], min_units: u8) -> (Vec<u32>, usize) {
    let n = off.len() - 1;
    const UNVISITED: u32 = u32::MAX;
    let mut index = vec![UNVISITED; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut comp = vec![UNVISITED; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut call: Vec<(u32, u32)> = Vec::new();
    let mut counter = 0u32;
    let mut count = 0u32;
    for root in 0..n as u32 {
        if index[root as usize] != UNVISITED {
            continue;
        }
        index[root as usize] = counter;
        low[root as usize] = counter;
        counter += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        call.push((root, off[root as usize]));
        while let Some(&mut (v, ref mut e)) = call.last_mut() {
            if *e < off[v as usize + 1] {
                let l = *e as usize;
                *e += 1;
                if units[l] < min_units {
                    continue;
                }
                let w = tgt[l];
                if index[w as usize] == UNVISITED {
                    index[w as usize] = counter;
                    low[w as usize] = counter;
                    counter += 1;
                    stack.push(w);
                    on_stack[w as usize] = true;
                    call.push((w, off[w as usize]));
                } else if on_stack[w as usize] {
                    low[v as usize] = low[v as usize].min(index[w as usize]);
                }
            } else {
                if low[v as usize] == index[v as usize] {
                    while let Some(x) = stack.pop() {
                        on_stack[x as usize] = false;
                        comp[x as usize] = count;
                        if x == v {
                            break;
                        }
                    }
                    count += 1;
                }
                call.pop();
                if let Some(&(p, _)) = call.last() {
                    low[p as usize] = low[p as usize].min(low[v as usize]);
                }
            }
        }
    }
    (comp, count as usize)
}

/// Interleaves the low 16 bits of two numbers (Z-order curve).
fn morton(x: u32, y: u32) -> u32 {
    fn spread(mut v: u32) -> u32 {
        v &= 0xFFFF;
        v = (v | (v << 8)) & 0x00FF_00FF;
        v = (v | (v << 4)) & 0x0F0F_0F0F;
        v = (v | (v << 2)) & 0x3333_3333;
        v = (v | (v << 1)) & 0x5555_5555;
        v
    }
    spread(x) | (spread(y) << 1)
}

/// Picks a grid cell size: the smallest of 8 / 16 / 32 / 64 m that keeps the grid at or below
/// 3 million cells.
fn pick_bucket(bounds: [f32; 4]) -> f32 {
    let (w, h) = (
        f64::from(bounds[2] - bounds[0]),
        f64::from(bounds[3] - bounds[1]),
    );
    for b in [8.0f32, 16.0, 32.0] {
        if (w / f64::from(b) + 1.0) * (h / f64::from(b) + 1.0) <= 3.0e6 {
            return b;
        }
    }
    64.0
}

/// Turns the merged graph into the file's in-memory form.
pub fn assemble(b: NavBuilder, opts: &AssembleOptions) -> anyhow::Result<(NavData, AssembleStats)> {
    let NavBuilder {
        vertices,
        nodes,
        mut links,
        ..
    } = b;
    let n = nodes.len();
    anyhow::ensure!(n > 0, "no navigation nodes");
    anyhow::ensure!(n < (u32::MAX / 2) as usize, "too many navigation nodes");
    let mut st = AssembleStats {
        nodes_merged: n,
        links_merged: links.len(),
        ..Default::default()
    };
    for nd in &nodes {
        if nd.nav_type == NAV_TYPE_WAYPOINT {
            st.waypoints += 1;
        } else {
            st.triangles += 1;
        }
    }

    // ---- links: de-duplicate (last wins), filter ------------------------------------------
    links.sort_by_key(|l| (l.src, l.dst)); // stable: equal keys stay in read order
    let mut kept: Vec<BLink> = Vec::with_capacity(links.len());
    for i in 0..links.len() {
        let l = links[i];
        if i + 1 < links.len() && links[i + 1].src == l.src && links[i + 1].dst == l.dst {
            st.links_duplicate += 1;
            if links[i + 1].units != l.units {
                st.links_duplicate_conflict += 1;
            }
            continue;
        }
        if l.units == 0 {
            st.links_blocked += 1;
        } else if l.units < MIN_LINK_UNITS {
            st.links_narrow += 1;
        } else {
            kept.push(l);
        }
    }
    drop(links);
    st.links_kept = kept.len();

    // ---- islands: weakly connected components ---------------------------------------------
    let mut parent: Vec<u32> = (0..n as u32).collect();
    for l in &kept {
        let (a, c) = (find(&mut parent, l.src), find(&mut parent, l.dst));
        if a != c {
            parent[a as usize] = c;
        }
    }
    let mut wcc_size = vec![0u32; n];
    for i in 0..n as u32 {
        let r = find(&mut parent, i);
        wcc_size[r as usize] += 1;
    }
    st.wcc_total = wcc_size.iter().filter(|&&s| s > 0).count();
    let mut new_id = vec![NONE; n];
    let mut kept_nodes = 0u32;
    for (i, id) in new_id.iter_mut().enumerate() {
        let r = find(&mut parent, i as u32) as usize;
        if wcc_size[r] as usize >= opts.min_component_nodes {
            *id = kept_nodes;
            kept_nodes += 1;
        }
    }
    st.wcc_kept = wcc_size
        .iter()
        .filter(|&&s| s > 0 && s as usize >= opts.min_component_nodes)
        .count();
    let m = kept_nodes as usize;
    st.nodes_kept = m;
    st.nodes_dropped = n - m;
    anyhow::ensure!(
        m > 0,
        "every navigation island is smaller than {} nodes",
        opts.min_component_nodes
    );

    // compact node list (old order), CSR over the kept links (still sorted by source)
    let mut old_of = vec![0u32; m];
    for (i, &id) in new_id.iter().enumerate() {
        if id != NONE {
            old_of[id as usize] = i as u32;
        }
    }
    let mut off = vec![0u32; m + 1];
    let mut tgt: Vec<u32> = Vec::with_capacity(kept.len());
    let mut units: Vec<u8> = Vec::with_capacity(kept.len());
    for l in &kept {
        let (s, d) = (new_id[l.src as usize], new_id[l.dst as usize]);
        if s == NONE || d == NONE {
            continue; // inside a dropped island (a link never joins a kept and a dropped node)
        }
        off[s as usize + 1] += 1;
        tgt.push(d);
        units.push(l.units);
    }
    drop(kept);
    for i in 0..m {
        off[i + 1] += off[i];
    }

    // ---- strongly connected components ----------------------------------------------------
    let (scc, scc_count) = tarjan_scc(&off, &tgt, &units, COMP_UNITS);
    let mut scc_size = vec![0u32; scc_count];
    for &c in &scc {
        scc_size[c as usize] += 1;
    }
    // one-way links: the reverse is missing or blocked for agents of COMP_UNITS
    {
        let has = |a: u32, b: u32| -> bool {
            let (lo, hi) = (off[a as usize] as usize, off[a as usize + 1] as usize);
            tgt[lo..hi]
                .iter()
                .zip(&units[lo..hi])
                .any(|(&t, &u)| t == b && u >= COMP_UNITS)
        };
        for a in 0..m as u32 {
            for l in off[a as usize] as usize..off[a as usize + 1] as usize {
                if units[l] >= COMP_UNITS && !has(tgt[l], a) {
                    st.one_way_links += 1;
                }
            }
        }
    }
    // numbering by size, largest first (ties keep completion order)
    let mut by_size: Vec<u32> = (0..scc_count as u32).collect();
    by_size.sort_by_key(|&c| (std::cmp::Reverse(scc_size[c as usize]), c));
    let mut rank = vec![0u32; scc_count];
    for (r, &c) in by_size.iter().enumerate() {
        rank[c as usize] = r as u32;
    }
    st.scc_count = scc_count;
    st.scc_largest = scc_size.iter().copied().max().unwrap_or(0) as usize;
    st.scc_singletons = scc_size.iter().filter(|&&s| s == 1).count();
    st.nodes_in_scc_50 = scc_size
        .iter()
        .filter(|&&s| s >= 50)
        .map(|&s| s as usize)
        .sum();
    st.nodes_in_scc_1000 = scc_size
        .iter()
        .filter(|&&s| s >= 1000)
        .map(|&s| s as usize)
        .sum();
    // weakly connected component of every SCC, as a dense id
    let mut wcc_dense = vec![NONE; n];
    let mut next_wcc = 0u32;
    let mut comp_wcc = vec![0u32; scc_count];
    for node in 0..m {
        let c = scc[node];
        let r = find(&mut parent, old_of[node]) as usize;
        if wcc_dense[r] == NONE {
            wcc_dense[r] = next_wcc;
            next_wcc += 1;
        }
        comp_wcc[rank[c as usize] as usize] = wcc_dense[r];
    }
    let mut comp_order = vec![0u32; scc_count];
    let mut comp_size = vec![0u32; scc_count];
    for c in 0..scc_count {
        comp_order[rank[c] as usize] = c as u32;
        comp_size[rank[c] as usize] = scc_size[c];
    }

    // ---- layout: Morton order of the nodes, vertices in order of first use -----------------
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    for &o in &old_of {
        let p = nodes[o as usize].pos;
        min_x = min_x.min(p[0]);
        min_y = min_y.min(p[1]);
    }
    let mut order: Vec<(u32, u32)> = (0..m as u32)
        .map(|i| {
            let p = nodes[old_of[i as usize] as usize].pos;
            let ix = (((p[0] - min_x) / 8.0) as u32).min(0xFFFF);
            let iy = (((p[1] - min_y) / 8.0) as u32).min(0xFFFF);
            (morton(ix, iy), i)
        })
        .collect();
    order.sort_unstable();
    let mut pos_of = vec![0u32; m]; // compact id -> final id
    for (final_id, &(_, c)) in order.iter().enumerate() {
        pos_of[c as usize] = final_id as u32;
    }

    let mut d = NavData {
        comp_radius: COMP_UNITS,
        ..NavData::default()
    };
    d.node_pos.reserve(m);
    d.node_tri.reserve(m);
    d.node_flags.reserve(m);
    d.node_comp.reserve(m);
    d.adj_off.reserve(m + 1);
    d.adj_target.reserve(tgt.len());
    d.adj_radius.reserve(tgt.len());
    let mut vmap = vec![NONE; vertices.len()];
    d.adj_off.push(0);
    for &(_, c) in &order {
        let nd = &nodes[old_of[c as usize] as usize];
        d.node_pos.push(nd.pos);
        let tri = if nd.tri[0] == aa_world::nav::format::NO_TRI {
            nd.tri
        } else {
            nd.tri.map(|v| {
                let slot = &mut vmap[v as usize];
                if *slot == NONE {
                    *slot = d.vertices.len() as u32;
                    d.vertices.push(vertices[v as usize]);
                }
                *slot
            })
        };
        d.node_tri.push(tri);
        d.node_flags.push(if nd.nav_type == NAV_TYPE_WAYPOINT {
            flag::WAYPOINT
        } else {
            0
        });
        d.node_comp.push(rank[scc[c as usize] as usize]);
        for l in off[c as usize] as usize..off[c as usize + 1] as usize {
            d.adj_target.push(pos_of[tgt[l] as usize]);
            d.adj_radius.push(units[l]);
        }
        d.adj_off.push(d.adj_target.len() as u32);
    }
    d.comp_size = comp_size;
    d.comp_wcc = comp_wcc;
    d.comp_order = comp_order;
    st.links_final = d.adj_target.len();
    st.vertices_kept = d.vertices.len();
    st.waypoints_kept = d
        .node_flags
        .iter()
        .filter(|&&f| f & flag::WAYPOINT != 0)
        .count();
    st.triangles_kept = m - st.waypoints_kept;
    // triangle pairs that are linked without a common edge
    for a in 0..m {
        let ta = d.node_tri[a];
        if ta[0] == aa_world::nav::format::NO_TRI {
            continue;
        }
        for l in d.adj_off[a] as usize..d.adj_off[a + 1] as usize {
            let tb = d.node_tri[d.adj_target[l] as usize];
            if tb[0] != aa_world::nav::format::NO_TRI
                && aa_world::nav::format::shared_edge(ta, tb).is_none()
            {
                st.links_no_portal += 1;
            }
        }
    }

    d.compute_bounds();
    d.compute_costs();
    d.compute_vertex_flags(MIN_LINK_UNITS);
    st.boundary_vertices = d.vertex_flags.iter().filter(|&&f| f != 0).count();
    let bucket = opts.bucket.unwrap_or_else(|| pick_bucket(d.bounds));
    d.build_grid(bucket)?;
    st.bucket = bucket;
    st.grid_cells = d.grid_off.len() - 1;
    st.grid_items = d.grid_items.len();
    d.validate()?;
    Ok((d, st))
}

#[cfg(test)]
mod tests {
    use super::super::bai::testfiles::*;
    use super::super::bai::{parse_net, parse_verts};
    use super::*;

    #[test]
    fn scc_handles_cycles_chains_and_one_way_links() {
        // 0 <-> 1 -> 2 <-> 3, 4 alone, 5 -> 5 skipped (no self links), 6 -> 0 narrow
        let edges: &[(u32, u32, u8)] = &[
            (0, 1, 20),
            (1, 0, 20),
            (1, 2, 20),
            (2, 3, 20),
            (3, 2, 20),
            (6, 0, 4),
            (0, 6, 20),
        ];
        let n = 7;
        let mut off = vec![0u32; n + 1];
        for e in edges {
            off[e.0 as usize + 1] += 1;
        }
        for i in 0..n {
            off[i + 1] += off[i];
        }
        let mut sorted = edges.to_vec();
        sorted.sort();
        let tgt: Vec<u32> = sorted.iter().map(|e| e.1).collect();
        let units: Vec<u8> = sorted.iter().map(|e| e.2).collect();
        let (comp, count) = tarjan_scc(&off, &tgt, &units, 10);
        // components: {0,1}, {2,3}, {4}, {5}, {6}
        assert_eq!(count, 5);
        assert_eq!(comp[0], comp[1]);
        assert_eq!(comp[2], comp[3]);
        assert_ne!(comp[0], comp[2]);
        assert_ne!(
            comp[6], comp[0],
            "6 -> 0 is too narrow, 0 -> 6 alone is one-way"
        );
        // a link never leads to a higher number
        for (s, t, u) in &sorted {
            if *u >= 10 {
                assert!(comp[*s as usize] >= comp[*t as usize], "{s} -> {t}");
            }
        }
        // with the narrow link allowed 6 joins {0, 1}
        let (comp, count) = tarjan_scc(&off, &tgt, &units, 4);
        assert_eq!(count, 4);
        assert_eq!(comp[6], comp[0]);
    }

    #[test]
    fn scc_does_not_recurse() {
        // a 300 000 node ring and a long chain: a recursive implementation would overflow
        let n = 300_000u32;
        let mut off = vec![0u32; n as usize + 1];
        let mut tgt = Vec::new();
        for i in 0..n {
            tgt.push((i + 1) % n);
            off[i as usize + 1] = i + 1;
        }
        let units = vec![20u8; tgt.len()];
        let (comp, count) = tarjan_scc(&off, &tgt, &units, 10);
        assert_eq!(count, 1);
        assert!(comp.iter().all(|&c| c == 0));
        let mut off = vec![0u32; n as usize + 1];
        let mut tgt = Vec::new();
        for i in 0..n {
            if i + 1 < n {
                tgt.push(i + 1);
            }
            off[i as usize + 1] = tgt.len() as u32;
        }
        let units = vec![20u8; tgt.len()];
        let (comp, count) = tarjan_scc(&off, &tgt, &units, 10);
        assert_eq!(count, n as usize);
        assert!(comp[0] > comp[n as usize - 1], "sinks complete first");
    }

    fn two_squares_builder() -> NavBuilder {
        // a 2x1 strip of squares (4 triangles, strongly connected), a separate lonely triangle
        // pair and a waypoint link
        let verts = verts(
            2,
            &[
                [0.0, 0.0, 1.0],
                [10.0, 0.0, 1.0],
                [20.0, 0.0, 1.0],
                [0.0, 10.0, 1.0],
                [10.0, 10.0, 1.0],
                [20.0, 10.0, 1.0],
                [100.0, 0.0, 1.0],
                [110.0, 0.0, 1.0],
                [100.0, 10.0, 1.0],
            ],
        );
        let c = |a: [f32; 3], b: [f32; 3], d: [f32; 3]| {
            [(a[0] + b[0] + d[0]) / 3.0, (a[1] + b[1] + d[1]) / 3.0, 1.0]
        };
        let v = |i: usize| -> [f32; 3] {
            let p = [
                [0.0, 0.0],
                [10.0, 0.0],
                [20.0, 0.0],
                [0.0, 10.0],
                [10.0, 10.0],
                [20.0, 10.0],
                [100.0, 0.0],
                [110.0, 0.0],
                [100.0, 10.0],
            ][i];
            [p[0], p[1], 1.0]
        };
        let nodes = [
            TNode::tri(1, c(v(0), v(1), v(4)), [0, 1, 4]),
            TNode::tri(2, c(v(0), v(4), v(3)), [0, 4, 3]),
            TNode::tri(3, c(v(1), v(2), v(5)), [1, 2, 5]),
            TNode::tri(4, c(v(1), v(5), v(4)), [1, 5, 4]),
            TNode::tri(5, c(v(6), v(7), v(8)), [6, 7, 8]),
            TNode::waypoint(6, [50.0, 5.0, 8.0]),
        ];
        let links = [
            TLink::new(1, 2, 2.0),
            TLink::new(2, 1, 2.0),
            TLink::new(1, 4, 2.0),
            TLink::new(4, 1, 2.0),
            TLink::new(4, 3, 2.0),
            TLink::new(3, 4, 0.45), // narrow back: still kept (>= 0.4) but not for 0.5 m agents
            TLink::new(2, 3, -1.0), // blocked
            TLink::new(3, 1, 0.1),  // too narrow, dropped
        ];
        let net = parse_net(&net(55, &nodes, &links)).unwrap();
        let verts = parse_verts(&verts).unwrap();
        let mut b = NavBuilder::new();
        b.add_block(&net, Some(&verts), [0.0; 3]);
        b
    }

    #[test]
    fn assembles_filters_and_builds_the_tables() {
        let (d, st) = assemble(
            two_squares_builder(),
            &AssembleOptions {
                min_component_nodes: 3,
                bucket: Some(8.0),
            },
        )
        .unwrap();
        assert_eq!(st.nodes_merged, 6);
        assert_eq!((st.triangles, st.waypoints), (5, 1));
        assert_eq!(st.links_blocked, 1);
        assert_eq!(st.links_narrow, 1);
        assert_eq!(st.links_kept, 6);
        // the strip of 4 triangles survives, the lone triangle and the waypoint are islands
        assert_eq!(st.nodes_kept, 4);
        assert_eq!(st.nodes_dropped, 2);
        assert_eq!(st.wcc_total, 3);
        assert_eq!(st.wcc_kept, 1);
        assert_eq!(d.node_count(), 4);
        assert_eq!(d.vertices.len(), 6, "only the vertices of kept triangles");
        assert_eq!(d.link_count(), 6);
        // SCCs at 0.5 m: node 3 (id 3) has a way in (4 -> 3) but only a narrow way out
        assert_eq!(st.scc_count, 2);
        assert_eq!(st.scc_largest, 3);
        assert_eq!(d.comp_size, vec![3, 1]);
        assert_eq!(d.comp_wcc[0], d.comp_wcc[1]);
        assert_eq!(st.one_way_links, 1);
        // the singleton is a sink: the big component can reach it, not the other way round
        assert!(d.comp_order[0] > d.comp_order[1]);
        // the file validates and opens
        let mesh = aa_world::nav::NavMesh::from_bytes(d.to_bytes().unwrap()).unwrap();
        mesh.verify().unwrap();
        assert_eq!(mesh.component_count(), 2);
        assert_eq!(st.links_no_portal, 0);
    }

    #[test]
    fn small_island_limit_and_empty_input() {
        let (d, st) = assemble(
            two_squares_builder(),
            &AssembleOptions {
                min_component_nodes: 1,
                bucket: None,
            },
        )
        .unwrap();
        assert_eq!(d.node_count(), 6);
        assert_eq!(st.wcc_kept, 3);
        assert_eq!(st.waypoints_kept, 1);
        assert!(assemble(
            NavBuilder::new(),
            &AssembleOptions {
                min_component_nodes: 1,
                bucket: None
            }
        )
        .is_err());
        assert!(assemble(
            two_squares_builder(),
            &AssembleOptions {
                min_component_nodes: 1000,
                bucket: None
            }
        )
        .is_err());
    }

    #[test]
    fn duplicate_links_last_read_wins() {
        let mut b = NavBuilder::new();
        let nodes = [
            TNode::tri(1, [1.0, 1.0, 0.0], [-1; 3]),
            TNode::tri(2, [2.0, 2.0, 0.0], [-1; 3]),
        ];
        // two copies of the same link in two blocks, the second blocked
        let first = parse_net(&net(
            55,
            &nodes,
            &[TLink::new(1, 2, 1.0), TLink::new(2, 1, 1.0)],
        ))
        .unwrap();
        let second = parse_net(&net(
            55,
            &nodes,
            &[TLink::new(1, 2, -1.0), TLink::new(2, 1, 0.8)],
        ))
        .unwrap();
        b.add_block(&first, None, [0.0; 3]);
        b.add_block(&second, None, [0.0; 3]);
        let (d, st) = assemble(
            b,
            &AssembleOptions {
                min_component_nodes: 1,
                bucket: None,
            },
        )
        .unwrap();
        assert_eq!(st.links_duplicate, 2);
        assert_eq!(st.links_duplicate_conflict, 2);
        assert_eq!(
            st.links_blocked, 1,
            "the later copy of 1 -> 2 wins and is blocked"
        );
        assert_eq!(d.link_count(), 1);
        assert_eq!(d.adj_radius, vec![16]);
    }

    #[test]
    fn morton_orders_neighbours_together() {
        assert_eq!(morton(0, 0), 0);
        assert_eq!(morton(1, 0), 1);
        assert_eq!(morton(0, 1), 2);
        assert_eq!(morton(1, 1), 3);
        assert_eq!(morton(2, 0), 4);
        assert_eq!(morton(0xFFFF, 0xFFFF), u32::MAX);
    }
}
