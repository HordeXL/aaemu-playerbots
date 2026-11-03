//! Mirage Isle, the market island: a shared system instance (zone group 49, world
//! `arche_mall_world`) that every character may enter, whatever its level or faction.
//!
//! The way in is a portal doodad (template 4894) that stands next to the auctioneer in the towns;
//! its `DoodadFuncEnterSysInstance` names the island's zone, and the client casts the function's
//! skill on the doodad (`systems/npcs/doodad/funcs/world.rs: enter_sys_instance`, then
//! `systems/instances/indun.rs: request_system_instance`). The server saves the main-world position
//! first, so the exit doodad of the island (`DoodadFuncExitIndun`, 4895) brings the character back
//! to the portal it came through.
//!
//! The island is not a [`Dungeon`](super::dungeon::Dungeon): nobody clears it, and the dungeon goal
//! must not pick it. The parts that are also instance-world data (the arrival spawn, the exit
//! doodad, the mailbox) are read from the island's [`InstanceWorld`] when this is loaded.

use super::dungeon::{Entrance, MIRAGE_ZONE_GROUP};
use super::funcs::DoodadTravelFuncs;
use super::instance::InstanceWorlds;
use super::TravelInputs;
use crate::geom::Vec3;
use crate::spawns::Kind;
use crate::sql::{each_row, RowExt};

/// The mailbox doodad on the island (`doodads` template 320), where the market meets.
pub const MAILBOX_DOODAD: u32 = 320;

/// Where the island is entered, left and met at.
#[derive(Clone, Debug, PartialEq)]
pub struct Mirage {
    /// `zones.id` of the island.
    pub zone_id: u32,
    /// Zone key (what `SCLoadInstance` carries).
    pub zone_key: u16,
    /// The portal doodad in the main world: its template, the skill to cast and its spawns.
    pub portal: Entrance,
    /// Directory name of the island's instance world.
    pub world: Box<str>,
    /// Where a character arrives.
    pub arrival: Vec3,
    /// The exit doodad: template, position and the skill to cast on it.
    pub exit: Option<(u32, Vec3, u32)>,
    /// The mailbox, else the arrival point: where the market meets.
    pub meeting_point: Vec3,
}

impl Mirage {
    pub(crate) fn load(
        inp: &TravelInputs<'_>,
        funcs: &DoodadTravelFuncs,
        worlds: &InstanceWorlds,
    ) -> Option<Mirage> {
        let mut zones: Vec<(u32, u16, u32)> = Vec::new();
        each_row(inp.conn, "SELECT id, zone_key, group_id FROM zones", |r| {
            zones.push((r.u32(0), r.u16(1), r.u32(2)));
        })
        .ok()?;
        let (func, zone_key) = funcs.sys_enters.iter().find_map(|f| {
            let (_, key, group) = zones.iter().find(|z| z.0 == f.zone_id)?;
            (*group == MIRAGE_ZONE_GROUP).then_some((*f, *key))
        })?;
        let spawns = inp.doodad_spawns.of(func.doodad);
        if spawns.is_empty() {
            tracing::warn!(
                "travel: the Mirage Isle portal doodad {} has no spawns in the main world",
                func.doodad
            );
            return None;
        }
        let skill = if func.skill != 0 {
            func.skill
        } else {
            inp.doodads
                .templates
                .get(func.doodad)
                .map_or(0, |d| d.use_skill)
        };
        let Some(world) = worlds.by_zone_key(zone_key) else {
            tracing::warn!("travel: Mirage Isle (zone key {zone_key}) has no instance world");
            return None;
        };
        let arrival = world
            .spawn_for_zone(zone_key)
            .map_or(Vec3::new(0.0, 0.0, 0.0), |s| s.pos);
        let exit = world
            .exits()
            .iter()
            .min_by(|a, b| {
                a.pos
                    .dist2d_sq(arrival)
                    .total_cmp(&b.pos.dist2d_sq(arrival))
            })
            .map(|e| (e.template, e.pos, e.skill));
        // (a mailbox has no use skill, so the instance world does not list it as a usable doodad:
        // read the spawns of the template itself)
        let dir = inp.server_data.join("Worlds").join(&*world.name);
        let meeting_point =
            crate::spawns::load(&dir, Kind::Doodad, |t| t == MAILBOX_DOODAD, |_, _| None)
                .map(|(spawns, _)| spawns)
                .unwrap_or_default()
                .into_iter()
                .map(|(_, pos)| pos)
                .min_by(|a, b| a.dist2d_sq(arrival).total_cmp(&b.dist2d_sq(arrival)))
                .unwrap_or(arrival);
        Some(Mirage {
            zone_id: func.zone_id,
            zone_key,
            portal: Entrance {
                doodad: func.doodad,
                skill,
                spawns: spawns.into(),
            },
            world: world.name.clone(),
            arrival,
            exit,
            meeting_point,
        })
    }

    /// The portal spawn nearest to `from` (horizontal distance).
    pub fn nearest_portal(&self, from: Vec3) -> Option<Vec3> {
        self.portal
            .spawns
            .iter()
            .copied()
            .min_by(|a, b| a.dist2d_sq(from).total_cmp(&b.dist2d_sq(from)))
    }

    /// Approximate heap bytes held.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Mirage>()
            + std::mem::size_of_val(&*self.portal.spawns)
            + self.world.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirage(spawns: Vec<Vec3>) -> Mirage {
        Mirage {
            zone_id: 183,
            zone_key: 260,
            portal: Entrance {
                doodad: 4894,
                skill: 17829,
                spawns: spawns.into(),
            },
            world: "arche_mall_world".into(),
            arrival: Vec3::new(3680.5, 4572.2, 156.0),
            exit: Some((4895, Vec3::new(3683.9, 4545.5, 149.4), 17838)),
            meeting_point: Vec3::new(3675.0, 4560.1, 150.0),
        }
    }

    #[test]
    fn the_nearest_portal_is_the_closest_spawn() {
        let m = mirage(vec![
            Vec3::new(100.0, 100.0, 5.0),
            Vec3::new(900.0, 900.0, 5.0),
            Vec3::new(500.0, 100.0, 5.0),
        ]);
        let p = m.nearest_portal(Vec3::new(600.0, 120.0, 0.0)).unwrap();
        assert_eq!((p.x, p.y), (500.0, 100.0));
        assert!(mirage(Vec::new())
            .nearest_portal(Vec3::new(0.0, 0.0, 0.0))
            .is_none());
        assert!(m.bytes() > 0);
    }
}
