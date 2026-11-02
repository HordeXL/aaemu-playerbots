//! From an A* corridor of nodes to a smoothed [`Path`]: funnel over runs of triangles, hard
//! points at waypoints.

use super::format::{flag, shared_edge};
use super::funnel::{string_pull, Portal};
use super::mesh::{Located, NavMesh, NodeRef};
use super::search::{Path, PathQuery, Scratch};
use crate::geom::Vec3;

/// Points closer than this (m, 3D) are one.
const MERGE_EPS: f32 = 0.02;

impl NavMesh {
    /// Builds the path along `sc.corridor`. With a `goal` it ends at the query's `to`, without
    /// one (a partial path) at the position of the corridor's last node.
    pub(crate) fn build_path(
        &self,
        q: &PathQuery,
        start: &Located,
        goal: Option<&Located>,
        sc: &mut Scratch,
    ) -> Path {
        let corridor = std::mem::take(&mut sc.corridor);
        let mut portals = std::mem::take(&mut sc.portals);
        let mut corners = std::mem::take(&mut sc.corners);
        portals.clear();
        corners.clear();

        // (point, from a waypoint): `pts` is the result, `corners` the funnel's scratch output
        let mut pts: Vec<(Vec3, bool)> = Vec::with_capacity(corridor.len().min(64) + 4);
        // a start on a waypoint (indoors) makes the way to the first node indoor too
        let start_wp = self.node_flags(start.node) & flag::WAYPOINT != 0;
        pts.push((q.from, start_wp));
        let mut apex = if start.inside {
            Vec3::new(q.from.x, q.from.y, start.point.z)
        } else {
            start.point
        };
        if !start.inside && apex.dist2d(q.from) > MERGE_EPS {
            pts.push((apex, start_wp));
        }
        let radius = q.agent_radius;

        // flush the portals gathered so far into a funnel run ending at `to`
        let run = |apex: &mut Vec3,
                   to: Vec3,
                   wp: bool,
                   portals: &mut Vec<Portal>,
                   corners: &mut Vec<Vec3>,
                   pts: &mut Vec<(Vec3, bool)>| {
            corners.clear();
            if !portals.is_empty() {
                let shrunk: Vec<Portal> = portals.iter().map(|p| p.shrunk(radius)).collect();
                if !string_pull(*apex, to, &shrunk, corners) {
                    // does not terminate on a sane corridor; walk through the portal middles
                    corners.clear();
                    for p in portals.iter() {
                        corners.push(Vec3::new(
                            (p.left.x + p.right.x) * 0.5,
                            (p.left.y + p.right.y) * 0.5,
                            (p.left.z + p.right.z) * 0.5,
                        ));
                    }
                    corners.push(to);
                }
            } else {
                corners.push(to);
            }
            portals.clear();
            let last = corners.len() - 1;
            for (k, c) in corners.iter().enumerate() {
                // the run's last point is the joint itself; the others are funnel corners
                pts.push((*c, k == last && wp));
            }
            *apex = to;
        };

        for (i, &n) in corridor.iter().enumerate() {
            let node = NodeRef(n);
            let tri = self.tri_indices(node);
            let mut joint = tri.is_none();
            if let (true, Some(tn)) = (i > 0, tri) {
                // a triangle entered from another triangle goes through their common edge; one
                // entered from a waypoint continues from the waypoint (already the apex)
                if let Some(tp) = self.tri_indices(NodeRef(corridor[i - 1])) {
                    match shared_edge(tp, tn) {
                        Some((ia, ib)) => {
                            let ic = 3 - ia - ib;
                            portals.push(Portal::across(
                                self.vertex(tp[ic]),
                                (self.vertex(tp[ia]), self.vertex_is_boundary(tp[ia])),
                                (self.vertex(tp[ib]), self.vertex_is_boundary(tp[ib])),
                            ));
                        }
                        // no common edge: the route passes the node's position
                        None => joint = true,
                    }
                }
            }
            if joint {
                let wp = self.node_flags(node) & flag::WAYPOINT != 0;
                let p = self.node_pos(node);
                if i == 0 && p.dist2d(apex) < MERGE_EPS {
                    continue;
                }
                run(&mut apex, p, wp, &mut portals, &mut corners, &mut pts);
            }
        }
        match goal {
            Some(g) => {
                let end = Vec3::new(q.to.x, q.to.y, g.point.z);
                if !g.inside && end.dist2d(g.point) > MERGE_EPS {
                    run(
                        &mut apex,
                        g.point,
                        false,
                        &mut portals,
                        &mut corners,
                        &mut pts,
                    );
                    pts.push((q.to, false));
                } else {
                    run(&mut apex, end, false, &mut portals, &mut corners, &mut pts);
                    // the last point is the caller's own goal
                    if let Some(last) = pts.last_mut() {
                        last.0 = q.to;
                    }
                }
            }
            None => {
                let last = corridor
                    .last()
                    .copied()
                    .map_or(apex, |n| self.node_pos(NodeRef(n)));
                run(&mut apex, last, false, &mut portals, &mut corners, &mut pts);
            }
        }

        // terrain heights for the corners on triangles; drop duplicates
        let last_index = pts.len() - 1;
        let mut points: Vec<Vec3> = Vec::with_capacity(pts.len());
        let mut wp_flags: Vec<bool> = Vec::with_capacity(pts.len());
        for (k, (p, wp)) in pts.iter().enumerate() {
            let mut p = *p;
            if k != 0 && !(k == last_index && goal.is_some()) && !wp {
                if let Some(h) = q.height.and_then(|f| f(p.x, p.y)) {
                    // 0 marks cells without terrain in the baked grid
                    if h > 0.0 && h.is_finite() {
                        p.z = h;
                    }
                }
            }
            if let Some(prev) = points.last() {
                if prev.dist(p) < MERGE_EPS {
                    if let Some(f) = wp_flags.last_mut() {
                        *f |= *wp;
                    }
                    // the final point stays the caller's
                    if k == last_index {
                        if let Some(l) = points.last_mut() {
                            *l = p;
                        }
                    }
                    continue;
                }
            }
            points.push(p);
            wp_flags.push(*wp);
        }
        let indoor: Vec<bool> = points
            .windows(2)
            .enumerate()
            .map(|(i, _)| wp_flags[i] || wp_flags[i + 1])
            .collect();
        let length = points.windows(2).map(|w| w[0].dist(w[1])).sum();
        let start_node = corridor.first().copied().unwrap_or(start.node.0);
        let end_node = corridor.last().copied().unwrap_or(start_node);
        let expansions = sc.expansions;

        sc.corridor = corridor;
        sc.portals = portals;
        sc.corners = corners;
        Path {
            points,
            indoor,
            length,
            expansions,
            start: NodeRef(start_node),
            end: NodeRef(end_node),
        }
    }
}
