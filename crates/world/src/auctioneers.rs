//! Auctioneers: where the NPCs of the auction house stand.
//!
//! Every NPC template with the auctioneer role (`npcs.auctioneer`, [`NpcRoles::AUCTIONEER`]) that
//! spawns in the world, as a flat list built once at load (139 spawns in the 1.2 main world: one
//! per Mirage portal and one or more in every capital). The auction packets only work within
//! 5 m of such an NPC, so a bot that wants the auction house walks to the nearest one it can
//! reach: [`World::nearest_auctioneer`].

use crate::geom::{IdMap, Vec3};
use crate::npc::{NpcRoles, NpcTemplate};
use crate::spatial::SpawnIndex;
use crate::World;

/// How far from the spawn point an auctioneer may be found by a [`World::nearest_auctioneer`]
/// candidate search (the list is sorted by distance and cut here).
const SEARCH_CANDIDATES: usize = 12;
/// A position is snapped onto the navigation mesh from at most this far (metres).
const SNAP_M: f32 = 24.0;

/// One auctioneer spawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Auctioneer {
    /// NPC template.
    pub template: u32,
    /// Spawn position.
    pub pos: Vec3,
}

/// Every auctioneer spawn of the world.
#[derive(Debug, Default)]
pub struct Auctioneers {
    list: Vec<Auctioneer>,
}

impl Auctioneers {
    /// The spawns of every NPC template that has the auctioneer role.
    pub fn build(npcs: &IdMap<NpcTemplate>, spawns: &SpawnIndex) -> Auctioneers {
        let mut list = Vec::new();
        for t in npcs
            .values()
            .iter()
            .filter(|t| t.roles.has(NpcRoles::AUCTIONEER))
        {
            list.extend(spawns.of(t.id).iter().map(|&pos| Auctioneer {
                template: t.id,
                pos,
            }));
        }
        Auctioneers { list }
    }

    /// All spawns, by template.
    pub fn all(&self) -> &[Auctioneer] {
        &self.list
    }

    /// Number of spawns.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether the world has no auctioneer.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The spawns within `max_m` metres (horizontal) of `from`, nearest first, at most `limit`.
    pub fn nearest(&self, from: Vec3, max_m: f32, limit: usize) -> Vec<(f32, Auctioneer)> {
        let mut near: Vec<(f32, Auctioneer)> = self
            .list
            .iter()
            .map(|a| (a.pos.dist2d(from), *a))
            .filter(|(d, _)| *d <= max_m)
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.truncate(limit);
        near
    }

    /// Heap bytes held.
    pub fn bytes(&self) -> usize {
        self.list.capacity() * std::mem::size_of::<Auctioneer>()
    }
}

impl World {
    /// The auctioneer spawns of the world.
    pub fn auctioneers(&self) -> &Auctioneers {
        &self.auctioneers
    }

    /// The nearest auctioneer within `max_m` metres (straight line) of `from` that a character
    /// standing at `from` can walk to and that `skip` does not reject. When the navigation mesh is
    /// loaded, candidates whose mesh component cannot reach `from` (another island, a closed
    /// courtyard) are passed over; without a mesh the nearest wins.
    pub fn nearest_auctioneer(
        &self,
        from: Vec3,
        max_m: f32,
        skip: impl Fn(&Auctioneer) -> bool,
    ) -> Option<Auctioneer> {
        let candidates = self.auctioneers.nearest(from, max_m, usize::MAX);
        let here = self.nav().and_then(|nav| nav.locate(from, SNAP_M));
        candidates
            .into_iter()
            .filter(|(_, a)| !skip(a))
            .take(SEARCH_CANDIDATES)
            .find(|(_, a)| match (self.nav(), here) {
                (Some(nav), Some(from_node)) => nav
                    .locate(a.pos, SNAP_M)
                    .is_none_or(|to| nav.may_reach(from_node, to) != Some(false)),
                _ => true,
            })
            .map(|(_, a)| a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn npc(id: u32, roles: NpcRoles) -> (u32, NpcTemplate) {
        (
            id,
            NpcTemplate {
                id,
                name: format!("npc {id}").into(),
                level: 1,
                faction_id: 0,
                kind: 1,
                grade: 1,
                aggressive: false,
                roles,
                is_quest_giver: false,
            },
        )
    }

    fn sample() -> Auctioneers {
        let npcs = IdMap::build(vec![
            npc(872, NpcRoles::AUCTIONEER),
            npc(10857, NpcRoles::AUCTIONEER.with(NpcRoles::BANKER)),
            npc(5, NpcRoles::MERCHANT),
        ]);
        let spawns = SpawnIndex::build(vec![
            (872, Vec3::new(11240.0, 12110.0, 145.0)),
            (872, Vec3::new(11249.0, 12117.0, 145.0)),
            (10857, Vec3::new(10493.0, 15551.0, 100.0)),
            (5, Vec3::new(11240.0, 12111.0, 145.0)),
            // (no such template: never listed)
            (9999, Vec3::new(1.0, 1.0, 1.0)),
        ]);
        Auctioneers::build(&npcs, &spawns)
    }

    #[test]
    fn only_auctioneer_templates_are_listed() {
        let a = sample();
        assert_eq!(a.len(), 3);
        assert!(!a.is_empty());
        assert!(a
            .all()
            .iter()
            .all(|s| s.template == 872 || s.template == 10857));
        assert!(a.bytes() >= a.len() * std::mem::size_of::<Auctioneer>());
        assert!(Auctioneers::default().is_empty());
    }

    #[test]
    fn nearest_is_sorted_and_cut() {
        let a = sample();
        let from = Vec3::new(11245.0, 12112.0, 145.0);
        let near = a.nearest(from, 100.0, 8);
        assert_eq!(near.len(), 2);
        assert!(near[0].0 <= near[1].0);
        assert_eq!(near[0].1.pos, Vec3::new(11240.0, 12110.0, 145.0));
        // the Gweonid one is 3.4 km away
        assert_eq!(a.nearest(from, 4000.0, 8).len(), 3);
        assert_eq!(a.nearest(from, 4000.0, 1).len(), 1);
        assert!(a.nearest(from, 1.0, 8).is_empty());
    }
}
