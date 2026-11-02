use super::format::{flag, NavData, Section, NAV_HEADER_LEN, NAV_SECTION_ENTRY_LEN};
use super::mesh::MAX_FLOOR_GAP;
use super::testmesh::TestMesh;
use super::*;
use crate::geom::Vec3;

fn v(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

/// A flat `w` x `h` field of `cell` m cells at z = 10.
fn field(w: usize, h: usize, cell: f32) -> NavMesh {
    let mut m = TestMesh::new();
    m.grid((0.0, 0.0), (w, h), cell, |_, _| 10.0, |_, _| false);
    m.link_neighbours(2.0);
    m.build()
}

/// Every sample along the path lies inside a triangle (the string never leaves the mesh).
fn assert_on_mesh(mesh: &NavMesh, path: &Path) {
    for w in path.points.windows(2) {
        let len = w[0].dist2d(w[1]);
        let steps = (len / 0.1).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let p = v(
                w[0].x + (w[1].x - w[0].x) * t,
                w[0].y + (w[1].y - w[0].y) * t,
                10.0,
            );
            let l = mesh.locate_detailed(p, 0.0);
            assert!(
                l.is_some_and(|l| l.inside),
                "{p:?} (segment {:?} -> {:?}) is off the mesh",
                w[0],
                w[1]
            );
        }
    }
}

fn found(r: PathResult) -> Path {
    match r {
        PathResult::Found(p) => p,
        other => panic!("expected Found, got {other:?}"),
    }
}

// ---- locate --------------------------------------------------------------------------------

#[test]
fn locate_inside_and_near() {
    let m = field(10, 10, 2.0);
    let l = m.locate_detailed(v(5.3, 7.7, 10.2), 1.0).unwrap();
    assert!(l.inside);
    assert!((l.point.z - 10.0).abs() < 1e-4 && (l.point.x - 5.3).abs() < 1e-6);
    assert!((l.dist - 0.2).abs() < 1e-4, "{}", l.dist);
    // off the mesh: only within max_dist, snapped to the nearest point
    assert_eq!(m.locate(v(-1.0, 5.0, 10.0), 0.5), None);
    let l = m.locate_detailed(v(-1.0, 5.0, 10.0), 1.5).unwrap();
    assert!(!l.inside);
    assert!((l.dist - 1.0).abs() < 1e-4, "{}", l.dist);
    assert!(l.point.x.abs() < 1e-4);
    assert_eq!(m.locate(v(500.0, 500.0, 10.0), 8.0), None);
    assert_eq!(m.locate(v(f32::NAN, 1.0, 1.0), 8.0), None);
    assert_eq!(m.locate(v(1.0, 1.0, f32::INFINITY), 8.0), None);
}

#[test]
fn locate_picks_the_floor_closest_in_z() {
    let mut t = TestMesh::new();
    t.grid((0.0, 0.0), (5, 5), 2.0, |_, _| 0.0, |_, _| false);
    t.grid((0.0, 0.0), (5, 5), 2.0, |_, _| 5.0, |_, _| false);
    let m = t.build();
    let z_of = |z| m.locate_detailed(v(5.0, 5.0, z), 1.0).unwrap().point.z;
    assert_eq!(z_of(4.8), 5.0);
    assert_eq!(z_of(0.3), 0.0);
    assert_eq!(z_of(2.4), 0.0);
    assert_eq!(z_of(2.6), 5.0);
    // a triangle that contains the position counts up to MAX_FLOOR_GAP above or below it; a
    // floor further away (a junk plane, another storey of the map) is no answer
    assert_eq!(z_of(5.0 + MAX_FLOOR_GAP - 1.0), 5.0);
    assert!(m
        .locate_detailed(v(5.0, 5.0, 5.0 + MAX_FLOOR_GAP + 1.0), 1.0)
        .is_none());
    assert!(m.locate_detailed(v(5.0, 5.0, 300.0), 1.0).is_none());
}

#[test]
fn locate_prefers_a_close_waypoint_over_a_far_floor() {
    let mut t = TestMesh::new();
    t.grid((0.0, 0.0), (5, 5), 2.0, |_, _| 0.0, |_, _| false);
    let wp = t.waypoint([5.0, 5.0, 6.0]);
    let m = t.build();
    // standing on the first floor of a building over the terrain
    let l = m.locate_detailed(v(5.2, 5.1, 6.1), 4.0).unwrap();
    assert_eq!(l.node.0 as usize, wp);
    assert!(m.is_waypoint(l.node));
    // outside the waypoint's reach: the terrain triangle
    let l = m.locate_detailed(v(5.2, 5.1, 0.2), 4.0).unwrap();
    assert!(!m.is_waypoint(l.node));
    // a waypoint can also be the only thing near
    let l = m.locate_detailed(v(5.0, 9.0, 6.0), 4.0).unwrap();
    assert!(l.inside, "terrain covers y = 9");
}

#[test]
fn locate_finds_triangles_spanning_many_cells() {
    // one 200 m triangle over 8 m grid cells
    let mut t = TestMesh::new();
    t.tri([0.0, 0.0, 1.0], [200.0, 0.0, 1.0], [0.0, 200.0, 1.0]);
    let m = t.build();
    for p in [
        (1.0, 1.0),
        (150.0, 10.0),
        (10.0, 150.0),
        (60.0, 60.0),
        (99.0, 99.0),
    ] {
        assert!(
            m.locate_detailed(v(p.0, p.1, 1.0), 0.0).unwrap().inside,
            "{p:?}"
        );
    }
    assert_eq!(m.locate(v(150.0, 150.0, 1.0), 0.0), None);
}

// ---- find_path ----------------------------------------------------------------------------

#[test]
fn straight_path_across_an_open_field() {
    let m = field(10, 10, 2.0);
    let q = PathQuery::new(v(1.0, 1.0, 10.0), v(19.0, 3.0, 10.0));
    let p = found(m.find_path(&q));
    // interior vertices are no obstacles, so the string is as straight as the triangle
    // corridor the search chose allows: at most a hair longer than the straight line
    assert_eq!(p.points[0], v(1.0, 1.0, 10.0));
    assert_eq!(*p.points.last().unwrap(), v(19.0, 3.0, 10.0));
    let straight = (18.0f32 * 18.0 + 4.0).sqrt();
    assert!(
        p.length >= straight - 1e-3 && p.length < straight * 1.01,
        "{}",
        p.length
    );
    assert!(p.points.len() <= 4, "{:?}", p.points);
    assert_eq!(p.indoor, vec![false; p.points.len() - 1]);
    assert_on_mesh(&m, &p);
    assert!(p.expansions > 0);
    // same triangle: no search needed beyond the start
    let p = found(m.find_path(&PathQuery::new(v(1.0, 0.5, 10.0), v(1.5, 0.2, 10.0))));
    assert_eq!(p.points.len(), 2);
    assert_eq!(p.start, p.end);
}

#[test]
fn detours_around_a_hole_and_stays_on_the_mesh() {
    let mut t = TestMesh::new();
    // 20 x 20 m, a wall of cells x = 10..11, y = 0..15 is missing
    t.grid(
        (0.0, 0.0),
        (20, 20),
        1.0,
        |_, _| 10.0,
        |cx, cy| cx == 10 && cy < 15,
    );
    t.link_neighbours(2.0);
    let m = t.build();
    let q = PathQuery::new(v(2.0, 2.0, 10.0), v(18.0, 2.0, 10.0));
    let p = found(m.find_path(&q));
    assert!(
        p.points.len() >= 4,
        "bends around both wall corners: {:?}",
        p.points
    );
    assert!(p.length > 30.0 && p.length < 40.0, "length {}", p.length);
    assert_on_mesh(&m, &p);
    // the corners keep the agent radius away from the wall end (10..11, 15)
    for c in &p.points[1..p.points.len() - 1] {
        let near_end = (c.x - 10.0).abs() < 0.2 && (c.y - 15.0).abs() < 0.2
            || (c.x - 11.0).abs() < 0.2 && (c.y - 15.0).abs() < 0.2;
        assert!(!near_end, "corner {c:?} hugs the wall end");
    }
    // the optimal weight finds a path of about the same length
    let mut exact = q;
    exact.weight = 1.0;
    let e = found(m.find_path(&exact));
    assert!(
        (e.length - p.length).abs() < 1.5,
        "{} vs {}",
        e.length,
        p.length
    );
    assert!(e.expansions >= p.expansions);
}

#[test]
fn unreachable_is_answered_from_the_components() {
    let mut t = TestMesh::new();
    t.grid((0.0, 0.0), (4, 4), 2.0, |_, _| 10.0, |_, _| false);
    t.grid((100.0, 0.0), (4, 4), 2.0, |_, _| 10.0, |_, _| false);
    t.link_neighbours(2.0);
    let m = t.build();
    assert_eq!(m.component_count(), 2);
    let a = m.locate(v(1.0, 1.0, 10.0), 1.0).unwrap();
    let b = m.locate(v(101.0, 1.0, 10.0), 1.0).unwrap();
    assert!(!m.same_component(a, b));
    assert_eq!(m.may_reach(a, b), Some(false));
    assert_eq!(m.may_reach(a, a), Some(true));
    assert_eq!(
        m.find_path(&PathQuery::new(v(1.0, 1.0, 10.0), v(101.0, 1.0, 10.0))),
        PathResult::Unreachable
    );
    assert_eq!(m.component_size(m.component_of(a)), 32);
    // off the mesh at either end
    assert_eq!(
        m.find_path(&PathQuery::new(v(1.0, 1.0, 10.0), v(50.0, 1.0, 10.0))),
        PathResult::NoMesh
    );
    assert_eq!(
        m.find_path(&PathQuery::new(v(-50.0, 1.0, 10.0), v(1.0, 1.0, 10.0))),
        PathResult::NoMesh
    );
}

#[test]
fn one_way_links_are_directional() {
    let mut t = TestMesh::new();
    let g1 = t.grid((0.0, 0.0), (4, 4), 2.0, |_, _| 10.0, |_, _| false);
    let g2 = t.grid((20.0, 0.0), (4, 4), 2.0, |_, _| 10.0, |_, _| false);
    t.link_neighbours(2.0);
    // a ledge from the first grid down to the second, no way back
    t.link(g1[&(3, 0)].0, g2[&(0, 0)].1, 2.0);
    let m = t.build();
    let a = m.locate(v(1.0, 1.0, 10.0), 1.0).unwrap();
    let b = m.locate(v(21.0, 1.0, 10.0), 1.0).unwrap();
    assert_eq!(m.may_reach(a, b), None, "a search has to decide");
    assert_eq!(m.may_reach(b, a), Some(false));
    let p = found(m.find_path(&PathQuery::new(v(1.0, 1.0, 10.0), v(21.0, 1.0, 10.0))));
    assert!(p.length > 20.0 && p.length < 30.0, "{}", p.length);
    assert_eq!(
        m.find_path(&PathQuery::new(v(21.0, 1.0, 10.0), v(1.0, 1.0, 10.0))),
        PathResult::Unreachable
    );
}

#[test]
fn partial_path_when_the_cap_hits_and_the_replan_finishes() {
    let mut t = TestMesh::new();
    t.grid((0.0, 0.0), (200, 2), 1.0, |_, _| 10.0, |_, _| false);
    t.link_neighbours(2.0);
    let m = t.build();
    let mut q = PathQuery::new(v(0.5, 0.5, 10.0), v(199.5, 0.5, 10.0));
    q.weight = 1.0;
    q.max_expansions = 50;
    let PathResult::Partial(p) = m.find_path(&q) else {
        panic!("expected a partial path");
    };
    assert_eq!(p.expansions, 50);
    let end = *p.points.last().unwrap();
    assert!(end.x > 5.0 && end.x < 100.0, "progress to {end:?}");
    assert_on_mesh(&m, &p);
    // walk it and ask again from where it ended
    q.from = end;
    q.max_expansions = 100_000;
    let rest = found(m.find_path(&q));
    assert_eq!(*rest.points.last().unwrap(), q.to);
}

#[test]
fn penalties_steer_a_replan_to_the_other_lane() {
    // a loop: lanes at y 0..2 and 5..7 around a hole, joined at both ends
    let mut t = TestMesh::new();
    t.grid(
        (0.0, 0.0),
        (12, 7),
        1.0,
        |_, _| 10.0,
        |cx, cy| (2..10).contains(&cx) && (2..5).contains(&cy),
    );
    t.link_neighbours(2.0);
    let m = t.build();
    let mut q = PathQuery::new(v(0.5, 3.5, 10.0), v(11.5, 3.5, 10.0));
    let top_lane = |m: &NavMesh| -> Vec<(NodeRef, f32)> {
        (0..m.node_count() as u32)
            .map(NodeRef)
            .filter(|n| m.node_pos(*n).y > 3.5)
            .map(|n| (n, 100.0))
            .collect()
    };
    let bottom_lane = |m: &NavMesh| -> Vec<(NodeRef, f32)> {
        (0..m.node_count() as u32)
            .map(NodeRef)
            .filter(|n| m.node_pos(*n).y < 3.5)
            .map(|n| (n, 100.0))
            .collect()
    };
    let pen_top = top_lane(&m);
    q.penalties = &pen_top;
    let p = found(m.find_path(&q));
    assert!(p.points.iter().any(|c| c.y < 2.5), "{:?}", p.points);
    assert!(p.points.iter().all(|c| c.y < 3.6), "{:?}", p.points);
    let pen_bottom = bottom_lane(&m);
    q.penalties = &pen_bottom;
    let p = found(m.find_path(&q));
    assert!(p.points.iter().any(|c| c.y > 4.5), "{:?}", p.points);
}

#[test]
fn road_nodes_are_preferred() {
    let mut t = TestMesh::new();
    t.grid(
        (0.0, 0.0),
        (12, 7),
        1.0,
        |_, _| 10.0,
        |cx, cy| (2..10).contains(&cx) && (2..5).contains(&cy),
    );
    t.link_neighbours(2.0);
    // make the top lane a road
    let top: Vec<usize> = (0..t.node_count())
        .filter(|&i| t.node_pos(i)[1] > 3.5)
        .collect();
    for i in top {
        t.flag(i, flag::ROAD);
    }
    let m = t.build();
    let mut q = PathQuery::new(v(0.5, 3.5, 10.0), v(11.5, 3.5, 10.0));
    let p = found(m.find_path(&q));
    assert!(
        p.points.iter().any(|c| c.y > 4.5),
        "on the road: {:?}",
        p.points
    );
    q.road_bias = 1.0;
    // without the bias both lanes tie; the path must still exist
    found(m.find_path(&q));
}

#[test]
fn narrow_links_need_a_narrow_agent() {
    let mut t = TestMesh::new();
    let g1 = t.grid((0.0, 0.0), (4, 4), 2.0, |_, _| 10.0, |_, _| false);
    let g2 = t.grid((20.0, 0.0), (4, 4), 2.0, |_, _| 10.0, |_, _| false);
    t.link_neighbours(2.0);
    t.link2(g1[&(3, 0)].0, g2[&(0, 0)].1, 0.45);
    let m = t.build();
    // the component tables are exact for 0.5 m and do not contain the 0.45 m link
    assert_eq!(m.component_count(), 2);
    let mut q = PathQuery::new(v(1.0, 1.0, 10.0), v(21.0, 1.0, 10.0));
    assert_eq!(m.find_path(&q), PathResult::Unreachable);
    q.agent_radius = 0.4;
    assert!(
        matches!(m.find_path(&q), PathResult::Found(_)),
        "a 0.4 m agent fits"
    );
}

#[test]
fn waypoints_are_hard_points_and_mark_segments_indoor() {
    let mut t = TestMesh::new();
    let west = t.grid((0.0, 0.0), (5, 5), 2.0, |_, _| 10.0, |_, _| false);
    let east = t.grid((30.0, 0.0), (5, 5), 2.0, |_, _| 10.0, |_, _| false);
    t.link_neighbours(2.0);
    // a stair up and over: three waypoints between the grids
    let w1 = t.waypoint([14.0, 5.0, 12.0]);
    let w2 = t.waypoint([20.0, 5.0, 18.0]);
    let w3 = t.waypoint([26.0, 5.0, 12.0]);
    t.link2(west[&(4, 2)].0, w1, 1.0);
    t.link2(w1, w2, 1.0);
    t.link2(w2, w3, 1.0);
    t.link2(w3, east[&(0, 2)].1, 1.0);
    let m = t.build();
    let mut q = PathQuery::new(v(1.0, 5.0, 10.0), v(39.0, 5.0, 10.0));
    let height = |_x: f32, _y: f32| Some(77.0);
    q.height = Some(&height);
    let p = found(m.find_path(&q));
    for w in [[14.0, 5.0, 12.0], [20.0, 5.0, 18.0], [26.0, 5.0, 12.0]] {
        assert!(
            p.points.iter().any(|c| *c == v(w[0], w[1], w[2])),
            "passes waypoint {w:?}: {:?}",
            p.points
        );
    }
    assert_eq!(p.indoor.len(), p.points.len() - 1);
    assert!(p.indoor.iter().any(|&i| i), "{:?}", p.indoor);
    assert!(!p.indoor[0] || p.points.len() == 2 || p.points[1].x > 10.0);
    // the caller's own z at both ends, the terrain's elsewhere on triangles, waypoints keep theirs
    assert_eq!(p.points[0].z, 10.0);
    assert_eq!(p.points.last().unwrap().z, 10.0);
    for (c, i) in p.points.iter().zip(1..) {
        if i < p.points.len() && c.x > 13.0 && c.x < 27.0 {
            assert!(c.z <= 18.0, "waypoint z kept: {c:?}");
        }
    }
}

#[test]
fn height_callback_sets_corner_z() {
    let mut t = TestMesh::new();
    t.grid(
        (0.0, 0.0),
        (20, 20),
        1.0,
        |_, _| 10.0,
        |cx, cy| cx == 10 && cy < 15,
    );
    t.link_neighbours(2.0);
    let m = t.build();
    let mut q = PathQuery::new(v(2.0, 2.0, 10.0), v(18.0, 2.0, 10.0));
    let p = found(m.find_path(&q));
    assert!(p.points[1..p.points.len() - 1].iter().all(|c| c.z == 10.0));
    let h = |x: f32, _y: f32| Some(55.0 + x * 0.0);
    q.height = Some(&h);
    let p = found(m.find_path(&q));
    assert!(p.points.len() >= 4);
    assert!(p.points[1..p.points.len() - 1].iter().all(|c| c.z == 55.0));
    assert_eq!(p.points[0].z, 10.0);
    // a grid hole (0) or no terrain leaves the mesh z
    let none = |_x: f32, _y: f32| Some(0.0);
    q.height = Some(&none);
    let p = found(m.find_path(&q));
    assert!(p.points[1..p.points.len() - 1].iter().all(|c| c.z == 10.0));
}

#[test]
fn start_and_goal_off_the_mesh_are_snapped() {
    let m = field(10, 10, 2.0);
    // 3 m west of the field, 2 m north of it
    let q = PathQuery::new(v(-3.0, 5.0, 10.0), v(5.0, 22.0, 10.0));
    let p = found(m.find_path(&q));
    assert_eq!(p.points[0], q.from);
    assert_eq!(*p.points.last().unwrap(), q.to);
    assert!(p.points.len() >= 4, "{:?}", p.points);
    // beyond the snap distance
    let mut far = q;
    far.snap_dist = 1.0;
    assert_eq!(m.find_path(&far), PathResult::NoMesh);
}

#[test]
fn scratch_memory_stays_bounded() {
    let mut t = TestMesh::new();
    t.single_component = true;
    t.grid((0.0, 0.0), (120, 120), 1.0, |_, _| 10.0, |_, _| false);
    t.link_neighbours(2.0);
    let m = t.build();
    let mut q = PathQuery::new(v(0.5, 0.5, 10.0), v(119.5, 119.5, 10.0));
    q.weight = 1.0;
    q.max_expansions = 30_000;
    let r = m.find_path(&q);
    assert!(matches!(r, PathResult::Found(_) | PathResult::Partial(_)));
    assert!(
        thread_scratch_bytes() < 8 << 20,
        "scratch kept {} bytes",
        thread_scratch_bytes()
    );
    // a tiny cap fails early without growing anything
    q.max_expansions = 10;
    assert!(matches!(m.find_path(&q), PathResult::Partial(_)));
}

#[test]
fn explicit_scratch_gives_the_same_answer() {
    let m = field(10, 10, 2.0);
    let q = PathQuery::new(v(1.0, 1.0, 10.0), v(19.0, 19.0, 10.0));
    let mut sc = Scratch::new();
    let a = m.find_path_with(&q, &mut sc);
    let b = m.find_path_with(&q, &mut sc);
    assert_eq!(a, b);
    assert_eq!(a, m.find_path(&q));
    assert!(sc.memory_bytes() > 0);
}

// ---- file format ---------------------------------------------------------------------------

fn sample_data() -> NavData {
    let mut t = TestMesh::new();
    t.grid(
        (0.0, 0.0),
        (6, 6),
        2.0,
        |x, y| 10.0 + x * 0.1 + y * 0.05,
        |cx, cy| cx == 2 && cy < 4,
    );
    t.link_neighbours(2.0);
    let w = t.waypoint([5.0, 5.0, 14.0]);
    t.link2(w, 3, 1.0);
    t.data()
}

#[test]
fn file_round_trip_preserves_the_mesh() {
    let d = sample_data();
    let bytes = d.to_bytes().unwrap();
    assert_eq!(bytes.len() as u64, d.layout().1);
    assert_eq!(bytes.len() % 64, 0);
    let m = NavMesh::from_bytes(bytes.clone()).unwrap();
    m.verify().unwrap();
    assert_eq!(m.node_count(), d.node_count());
    assert_eq!(m.vertex_count(), d.vertices.len());
    assert_eq!(m.link_count(), d.link_count());
    assert_eq!(m.component_count(), d.comp_size.len());
    assert_eq!(m.bounds(), d.bounds);
    for i in 0..d.node_count() {
        let n = NodeRef(i as u32);
        let p = d.node_pos[i];
        assert_eq!(m.node_pos(n), v(p[0], p[1], p[2]));
        assert_eq!(m.node_flags(n), d.node_flags[i]);
        assert_eq!(m.component_of(n), d.node_comp[i]);
        assert_eq!(m.degree(n), (d.adj_off[i + 1] - d.adj_off[i]) as usize);
        match d.triangle(i) {
            Some(t) => {
                let c = m.triangle(n).unwrap();
                for k in 0..3 {
                    let w = d.vertices[t[k]];
                    assert_eq!(c[k], v(w[0], w[1], w[2]));
                }
            }
            None => assert!(m.triangle(n).is_none()),
        }
    }
    // the link arrays
    let n0 = NodeRef(0);
    let nb = m.neighbours(n0, 0.5);
    let lo = d.adj_off[0] as usize;
    let hi = d.adj_off[1] as usize;
    let want: Vec<NodeRef> = (lo..hi).map(|l| NodeRef(d.adj_target[l])).collect();
    assert_eq!(nb, want);
    // and through a real file
    let dir = crate::terrain::test_dir("nav-roundtrip");
    let path = dir.join("nav_test.bin");
    d.write_file(&path).unwrap();
    let mapped = NavMesh::open(&path).unwrap();
    assert_eq!(mapped.file_bytes(), bytes.len());
    mapped.verify().unwrap();
    assert_eq!(
        mapped.find_path(&PathQuery::new(v(1.0, 1.0, 10.0), v(11.0, 11.0, 12.0))),
        m.find_path(&PathQuery::new(v(1.0, 1.0, 10.0), v(11.0, 11.0, 12.0)))
    );
    drop(mapped);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn section_entry(bytes: &[u8], s: Section) -> usize {
    let count = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
    (0..count)
        .map(|i| NAV_HEADER_LEN + i * NAV_SECTION_ENTRY_LEN)
        .find(|&e| u32::from_le_bytes(bytes[e..e + 4].try_into().unwrap()) == s as u32)
        .unwrap()
}

#[test]
fn damaged_headers_and_tables_are_rejected_at_open() {
    let good = sample_data().to_bytes().unwrap();
    assert!(NavMesh::from_bytes(good.clone()).is_ok());

    // truncated everywhere that matters
    for cut in [
        0,
        7,
        8,
        30,
        63,
        64,
        100,
        500,
        good.len() / 2,
        good.len() - 100,
    ] {
        assert!(
            NavMesh::from_bytes(good[..cut].to_vec()).is_err(),
            "cut at {cut} accepted"
        );
    }
    let patch = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut b = good.clone();
        f(&mut b);
        NavMesh::from_bytes(b).is_err()
    };
    assert!(patch(&|b| b[0] = b'X'), "magic");
    assert!(patch(&|b| b[8] = 9), "version");
    assert!(
        patch(&|b| b[12..16].copy_from_slice(&0u32.to_le_bytes())),
        "no nodes"
    );
    assert!(
        patch(&|b| b[12] += 1),
        "node count disagrees with the sections"
    );
    assert!(
        patch(&|b| b[20] += 1),
        "link count disagrees with the sections"
    );
    assert!(patch(&|b| b[28] = 0), "no sections");
    assert!(patch(&|b| b[28] = 200), "section table past the file");
    assert!(
        patch(&|b| b[48..52].copy_from_slice(&f32::NAN.to_bits().to_le_bytes())),
        "bucket"
    );
    assert!(
        patch(&|b| b[52..56].copy_from_slice(&0u32.to_le_bytes())),
        "grid width"
    );
    assert!(
        patch(&|b| b[32..36].copy_from_slice(&f32::NAN.to_bits().to_le_bytes())),
        "bounds"
    );
    for s in Section::ALL {
        let e = section_entry(&good, s);
        assert!(patch(&|b| b[e + 4] ^= 1), "{} element size", s.name());
        assert!(
            patch(&|b| b[e + 8..e + 16].copy_from_slice(&(good.len() as u64).to_le_bytes())),
            "{} offset past the end",
            s.name()
        );
        assert!(
            patch(&|b| b[e + 8..e + 16].copy_from_slice(&u64::MAX.to_le_bytes())),
            "{} offset overflow",
            s.name()
        );
        assert!(patch(&|b| b[e + 8] |= 1), "{} unaligned", s.name());
        if s != Section::GridItems {
            assert!(
                patch(&|b| b[e + 16..e + 24].copy_from_slice(&u64::MAX.to_le_bytes())),
                "{} count overflow",
                s.name()
            );
        }
        // a section listed twice
        assert!(
            patch(&|b| {
                let other = section_entry(
                    b,
                    if s == Section::NodePos {
                        Section::NodeTri
                    } else {
                        Section::NodePos
                    },
                );
                let id = b[e..e + 4].to_vec();
                b[other..other + 4].copy_from_slice(&id);
            }),
            "{} listed twice",
            s.name()
        );
    }
    // the adjacency offsets must span the links, the grid offsets the grid items
    for s in [Section::AdjOff, Section::GridOff] {
        let e = section_entry(&good, s);
        let off = u64::from_le_bytes(good[e + 8..e + 16].try_into().unwrap()) as usize;
        let count = u64::from_le_bytes(good[e + 16..e + 24].try_into().unwrap()) as usize;
        assert!(patch(&|b| b[off] = 1), "{} first", s.name());
        assert!(
            patch(&|b| b[off + 4 * (count - 1)] ^= 1),
            "{} last",
            s.name()
        );
    }
}

#[test]
fn bad_index_values_are_caught_by_verify_and_never_panic_in_queries() {
    let good = sample_data().to_bytes().unwrap();
    let e = section_entry(&good, Section::AdjTarget);
    let off = u64::from_le_bytes(good[e + 8..e + 16].try_into().unwrap()) as usize;
    let mut b = good.clone();
    b[off..off + 4].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    let m = NavMesh::from_bytes(b).unwrap();
    assert!(m.verify().is_err());
    for _ in 0..3 {
        let _ = m.find_path(&PathQuery::new(v(1.0, 1.0, 10.0), v(11.0, 11.0, 12.0)));
    }
    assert!(m.neighbours(NodeRef(0), 0.5).iter().all(|n| m.contains(*n)));
    // tri vertex index out of range: treated as no triangle
    let e = section_entry(&good, Section::NodeTri);
    let off = u64::from_le_bytes(good[e + 8..e + 16].try_into().unwrap()) as usize;
    let mut b = good.clone();
    b[off..off + 4].copy_from_slice(&0xFFFF_0000u32.to_le_bytes());
    let m = NavMesh::from_bytes(b).unwrap();
    assert!(m.verify().is_err());
    assert!(m.triangle(NodeRef(0)).is_none());
}

#[test]
fn random_corruption_never_panics() {
    let good = sample_data().to_bytes().unwrap();
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut opened = 0;
    for _ in 0..400 {
        let mut b = good.clone();
        for _ in 0..1 + next() % 6 {
            let i = (next() % b.len() as u64) as usize;
            b[i] = next() as u8;
        }
        let Ok(m) = NavMesh::from_bytes(b) else {
            continue;
        };
        opened += 1;
        let _ = m.verify();
        for (a, bb) in [
            ((1.0, 1.0), (11.0, 11.0)),
            ((3.0, 9.0), (9.0, 3.0)),
            ((5.0, 5.0), (5.5, 5.0)),
        ] {
            let mut q = PathQuery::new(v(a.0, a.1, 10.0), v(bb.0, bb.1, 11.0));
            q.max_expansions = 500;
            let _ = m.find_path(&q);
            let _ = m.locate(q.from, 4.0);
        }
        for i in 0..m.node_count().min(40) {
            let n = NodeRef(i as u32);
            let _ = (
                m.neighbours(n, 0.5),
                m.triangle(n),
                m.component_of(n),
                m.node_pos(n),
            );
            let _ = m.may_reach(n, NodeRef(0));
        }
    }
    assert!(
        opened > 50,
        "only {opened} corrupted files opened; the fuzz is too destructive"
    );
}
