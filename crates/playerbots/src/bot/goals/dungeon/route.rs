//! The route through an instance dungeon: where the hostile NPCs of the world's spawn list stand,
//! grouped into stops and ordered nearest-next with the bosses last.
//!
//! The static spawn list ([`aa_world::travel::InstanceWorld::npcs`]) is what the bot knows
//! before it enters; once inside, the units the server shows are the truth. The route only says
//! where to go next, so the party walks through the dungeon instead of waiting for monsters to
//! come. Pure functions, no world lookups: the caller says which templates are hostile.

use aa_world::travel::InstanceNpc;

/// Spawns closer than this (metres, horizontally) on about the same floor make one stop.
pub const CLUSTER_M: f32 = 18.0;
/// Spawns further apart than this in height are on different floors.
pub const FLOOR_M: f32 = 8.0;
/// Most stops of a route.
pub const MAX_STOPS: usize = 48;

/// A place on the route.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    /// Centre (x).
    pub x: f32,
    /// Centre (y).
    pub y: f32,
    /// Mean height of the spawns.
    pub z: f32,
    /// Hostile spawns in the stop.
    pub mobs: u8,
    /// Template of a boss that spawns here (0 = none).
    pub boss: u32,
}

impl Stop {
    /// Whether a boss spawns here.
    pub fn is_boss(&self) -> bool {
        self.boss != 0
    }

    /// Horizontal distance from `p` to the stop.
    pub fn dist2d(&self, p: [f32; 3]) -> f32 {
        ((self.x - p[0]).powi(2) + (self.y - p[1]).powi(2)).sqrt()
    }
}

/// The bosses of the instance worlds the bots know. (The spawn data has no boss flag, and an
/// NPC's grade says nothing about it, so bosses are named by template.)
pub fn bosses_of(world: &str) -> &'static [u32] {
    match world {
        // Right Hand of Nerta Vera, Nerta (twice: the fight), Stonebrother Colossus, Okape the
        // Slayer
        "instance_cuttingwind_deadmine" => &[12150, 11362, 11363, 12188],
        _ => &[],
    }
}

fn root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Plans the route: the hostile spawns (`hostile(template)`) grouped into stops, those without a
/// boss in order of nearest-next from `start`, then the boss stops the same way.
pub fn plan(
    npcs: &[InstanceNpc],
    start: [f32; 3],
    bosses: &[u32],
    hostile: impl Fn(u32) -> bool,
) -> Vec<Stop> {
    let spawns: Vec<&InstanceNpc> = npcs.iter().filter(|n| hostile(n.template)).collect();
    // single-linkage clusters (union-find over the spawns)
    let mut parent: Vec<usize> = (0..spawns.len()).collect();
    for i in 0..spawns.len() {
        for j in i + 1..spawns.len() {
            let (a, b) = (spawns[i].pos, spawns[j].pos);
            let flat = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
            if flat <= CLUSTER_M && (a.z - b.z).abs() <= FLOOR_M {
                let (ra, rb) = (root(&mut parent, i), root(&mut parent, j));
                if ra != rb {
                    parent[rb] = ra;
                }
            }
        }
    }
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    for i in 0..spawns.len() {
        let r = root(&mut parent, i);
        match groups.iter_mut().find(|g| g.0 == r) {
            Some(g) => g.1.push(i),
            None => groups.push((r, vec![i])),
        }
    }
    let stops: Vec<Stop> = groups
        .iter()
        .map(|(_, members)| {
            let n = members.len() as f32;
            let (mut x, mut y, mut z) = (0.0, 0.0, 0.0);
            let mut boss = 0;
            for &m in members {
                let p = spawns[m].pos;
                x += p.x;
                y += p.y;
                z += p.z;
                if boss == 0 && bosses.contains(&spawns[m].template) {
                    boss = spawns[m].template;
                }
            }
            Stop {
                x: x / n,
                y: y / n,
                z: z / n,
                mobs: members.len().min(255) as u8,
                boss,
            }
        })
        .collect();
    let (mut plain, mut last): (Vec<Stop>, Vec<Stop>) =
        stops.into_iter().partition(|s| !s.is_boss());
    let mut at = start;
    let mut route = Vec::with_capacity(plain.len() + last.len());
    for list in [&mut plain, &mut last] {
        while let Some((i, _)) = list
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.dist2d(at)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
        {
            let s = list.remove(i);
            at = [s.x, s.y, s.z];
            route.push(s);
        }
    }
    if route.len() > MAX_STOPS {
        // keep the bosses: drop plain stops from the middle of the plain part
        let plain_n = route.iter().filter(|s| !s.is_boss()).count();
        let cut = (route.len() - MAX_STOPS).min(plain_n);
        let from = (plain_n - cut) / 2;
        route.drain(from..from + cut);
    }
    route
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_world::Vec3;

    fn npc(template: u32, x: f32, y: f32, z: f32) -> InstanceNpc {
        InstanceNpc {
            template,
            pos: Vec3::new(x, y, z),
            level: 20,
            grade: 6,
            aggressive: true,
        }
    }

    fn all(_: u32) -> bool {
        true
    }

    #[test]
    fn spawns_close_together_make_one_stop() {
        let npcs = [
            npc(1, 100.0, 100.0, 10.0),
            npc(1, 110.0, 100.0, 10.0),
            npc(1, 120.0, 105.0, 10.0),
            npc(1, 400.0, 100.0, 10.0),
        ];
        let r = plan(&npcs, [0.0, 0.0, 10.0], &[], all);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].mobs, 3);
        assert!((r[0].x - 110.0).abs() < 0.1 && (r[0].y - 101.67).abs() < 0.1);
        assert_eq!(r[1].mobs, 1);
    }

    #[test]
    fn floors_are_separate_stops() {
        let npcs = [npc(1, 100.0, 100.0, 10.0), npc(1, 102.0, 100.0, 40.0)];
        assert_eq!(plan(&npcs, [0.0; 3], &[], all).len(), 2);
    }

    #[test]
    fn the_route_goes_nearest_next() {
        let npcs = [
            npc(1, 300.0, 0.0, 0.0),
            npc(1, 100.0, 0.0, 0.0),
            npc(1, 200.0, 0.0, 0.0),
        ];
        let r = plan(&npcs, [0.0; 3], &[], all);
        let xs: Vec<f32> = r.iter().map(|s| s.x).collect();
        assert_eq!(xs, [100.0, 200.0, 300.0]);
        // from the far end it is the other way round
        let r = plan(&npcs, [400.0, 0.0, 0.0], &[], all);
        let xs: Vec<f32> = r.iter().map(|s| s.x).collect();
        assert_eq!(xs, [300.0, 200.0, 100.0]);
    }

    #[test]
    fn bosses_come_last() {
        let npcs = [
            npc(7, 100.0, 0.0, 0.0), // a boss, nearest to the start
            npc(1, 200.0, 0.0, 0.0),
            npc(1, 300.0, 0.0, 0.0),
            npc(8, 500.0, 0.0, 0.0), // another boss
        ];
        let r = plan(&npcs, [0.0; 3], &[7, 8], all);
        let xs: Vec<f32> = r.iter().map(|s| s.x).collect();
        assert_eq!(xs, [200.0, 300.0, 100.0, 500.0]);
        assert_eq!(r[2].boss, 7);
        assert!(r[2].is_boss() && !r[0].is_boss());
    }

    #[test]
    fn friendly_npcs_are_left_out() {
        let npcs = [npc(502, 10.0, 0.0, 0.0), npc(11106, 200.0, 0.0, 0.0)];
        let r = plan(&npcs, [0.0; 3], &[], |t| t != 502);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].x, 200.0);
        assert!(plan(&[], [0.0; 3], &[], all).is_empty());
    }

    #[test]
    fn long_routes_are_cut_but_keep_the_bosses() {
        let npcs: Vec<InstanceNpc> = (0..100)
            .map(|i| npc(if i == 99 { 9 } else { 1 }, i as f32 * 50.0, 0.0, 0.0))
            .collect();
        let r = plan(&npcs, [0.0; 3], &[9], all);
        assert_eq!(r.len(), MAX_STOPS);
        assert!(r.last().unwrap().is_boss());
    }

    #[test]
    fn the_cutting_wind_bosses_are_known() {
        assert!(bosses_of("instance_cuttingwind_deadmine").contains(&12188));
        assert!(bosses_of("instance_sal_temple").is_empty());
    }
}
