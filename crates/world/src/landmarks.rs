//! Places worth seeing in every zone, for the explore goal of the bots: the middle of the zone, its
//! highest walkable ground (the view from a hill), a stretch of its coast and its recall points.
//!
//! Built once, at the first [`World::landmarks`], from the baked zone grid (64 m sectors), the
//! terrain heights and the recall points (a scan of the grid, a few milliseconds, a few hundred
//! entries). Without the zone grid there are no landmarks; without the terrain only the zone
//! middles and the recall points.
//!
//! A zone as players know it ("Solzreed Peninsula") is often several zone keys of one zone group
//! (`w_solzreed_1`, `_2`, `_3`); every key gets its own landmarks, and [`Landmarks::region`] names
//! the zone a key belongs to, so "a zone the bot has not been to" means the whole group. Keys
//! without a zone group (test and cut-scene maps, unused areas) and keys the database does not
//! know get no landmarks.

use crate::baked::ZoneGrid;
use crate::world::World;

/// Ground below this height is water (sea level is 100 m; the bots swim from 99 m down).
pub const WATER_Z: f32 = 100.0;
/// A hill is only a landmark when it stands this much above the middle of its zone (metres).
pub const HILL_RISE_M: f32 = 25.0;
/// A coast is walkable ground with water this far away (metres) in one of four directions.
pub const COAST_REACH_M: f32 = 48.0;
/// Zones of fewer sectors than this have no hill and no coast of their own (a town, a camp).
pub const MIN_CELLS: u32 = 12;

/// What a landmark is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LandmarkKind {
    /// The walkable ground nearest to the middle of the zone.
    Centre,
    /// The highest walkable ground of the zone.
    Hill,
    /// Walkable ground at the water's edge, the one nearest to the middle of the zone.
    Coast,
    /// A recall point (the places a portal book and the respawns know by name).
    Recall,
}

impl LandmarkKind {
    /// Name for logs and the web UI.
    pub const fn name(self) -> &'static str {
        match self {
            LandmarkKind::Centre => "centre",
            LandmarkKind::Hill => "hill",
            LandmarkKind::Coast => "coast",
            LandmarkKind::Recall => "recall point",
        }
    }
}

/// One place worth seeing.
#[derive(Clone, Debug, PartialEq)]
pub struct Landmark {
    /// What it is.
    pub kind: LandmarkKind,
    /// The zone key it lies in.
    pub zone_key: u16,
    /// The zone as players know it (see [`Landmarks::region`]).
    pub region: u32,
    /// Where (z from the terrain, 0 without it; a recall point's own z).
    pub pos: [f32; 3],
    /// The recall point's name, empty for the others.
    pub name: Box<str>,
}

/// One zone of the grid.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneSpots {
    /// The zone key.
    pub zone_key: u16,
    /// Its sectors in the grid.
    pub cells: u32,
    /// The middle of its sectors (it may lie outside an oddly shaped zone).
    pub centroid: [f32; 2],
    first: u32,
    count: u32,
}

/// The landmarks of the world. See the module docs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Landmarks {
    /// By zone key, only zones with at least one landmark.
    zones: Vec<ZoneSpots>,
    /// The landmarks, grouped by zone in the order of `zones`.
    marks: Vec<Landmark>,
    /// `(zone key, region)` of every key in the grid, by key.
    regions: Vec<(u16, u32)>,
}

/// The region of a zone key of zone group `group` (0: none): the group, else the key itself
/// (offset so the two never meet).
pub const fn region_of(zone_key: u16, group: u16) -> u32 {
    if group != 0 {
        group as u32
    } else {
        0x1_0000 | zone_key as u32
    }
}

/// A recall point as [`Landmarks::build`] takes it.
#[derive(Clone, Copy, Debug)]
pub struct RecallSpot<'a> {
    pub zone_key: u16,
    pub pos: [f32; 3],
    pub name: &'a str,
}

#[derive(Clone, Copy, Default)]
struct Acc {
    cells: u32,
    sx: f64,
    sy: f64,
    /// (distance to the centroid, x, y, z)
    centre: Option<(f32, f32, f32, f32)>,
    hill: Option<(f32, f32, f32)>,
    /// (distance to the centroid, x, y, z)
    coast: Option<(f32, f32, f32, f32)>,
}

impl Landmarks {
    /// Builds the landmarks from the zone grid, the ground heights (`ground`, `None` everywhere
    /// without terrain), the zone group of each key (`group_of`: `None` leaves the key out,
    /// `Some(0)` makes it a region of its own) and the recall points.
    pub fn build(
        grid: Option<&ZoneGrid>,
        ground: &dyn Fn(f32, f32) -> Option<f32>,
        group_of: &dyn Fn(u16) -> Option<u16>,
        recalls: &[RecallSpot<'_>],
    ) -> Landmarks {
        let Some(grid) = grid else {
            return Landmarks::default();
        };
        let (w, h, sector) = (grid.width as usize, grid.height as usize, grid.sector);
        let at = |col: usize, row: usize| -> (f32, f32) {
            ((col as f32 + 0.5) * sector, (row as f32 + 0.5) * sector)
        };
        let mut acc: std::collections::BTreeMap<u16, Acc> = std::collections::BTreeMap::new();
        for row in 0..h {
            for col in 0..w {
                let key = grid.keys[row * w + col];
                if key == 0 {
                    continue;
                }
                let (x, y) = at(col, row);
                let a = acc.entry(key).or_default();
                a.cells += 1;
                a.sx += f64::from(x);
                a.sy += f64::from(y);
            }
        }
        let groups: std::collections::BTreeMap<u16, u16> = acc
            .keys()
            .filter_map(|&k| group_of(k).map(|g| (k, g)))
            .collect();
        acc.retain(|k, _| groups.contains_key(k));
        let centroid = |a: &Acc| -> [f32; 2] {
            let n = f64::from(a.cells.max(1));
            [(a.sx / n) as f32, (a.sy / n) as f32]
        };
        // walkable ground: above the water, or anything without terrain
        let terrain = ground(0.5 * sector * w as f32, 0.5 * sector * h as f32).is_some()
            || (0..h).step_by(16).any(|r| {
                (0..w)
                    .step_by(16)
                    .any(|c| ground(at(c, r).0, at(c, r).1).is_some())
            });
        let dry = |x: f32, y: f32| -> Option<f32> {
            match ground(x, y) {
                Some(z) if z >= WATER_Z => Some(z),
                Some(_) => None,
                None if terrain => None,
                None => Some(0.0),
            }
        };
        let wet = |x: f32, y: f32| ground(x, y).is_some_and(|z| z < WATER_Z - 1.0);
        for row in 0..h {
            for col in 0..w {
                let key = grid.keys[row * w + col];
                if key == 0 {
                    continue;
                }
                let (x, y) = at(col, row);
                let Some(a) = acc.get_mut(&key) else { continue };
                let c = centroid(a);
                let d = ((x - c[0]).powi(2) + (y - c[1]).powi(2)).sqrt();
                if a.centre.is_none_or(|b| d < b.0) {
                    if let Some(z) = dry(x, y) {
                        a.centre = Some((d, x, y, z));
                    }
                }
                if !terrain || (row + col) % 2 != 0 {
                    continue;
                }
                let Some(z) = dry(x, y) else { continue };
                if a.hill.is_none_or(|b| z > b.2) {
                    a.hill = Some((x, y, z));
                }
                let r = COAST_REACH_M;
                if a.coast.is_none_or(|b| d < b.0)
                    && [(r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)]
                        .iter()
                        .any(|(dx, dy)| wet(x + dx, y + dy))
                {
                    a.coast = Some((d, x, y, z));
                }
            }
        }
        let mut out = Landmarks {
            regions: groups.iter().map(|(&k, &g)| (k, region_of(k, g))).collect(),
            ..Landmarks::default()
        };
        for (&key, a) in &acc {
            let first = out.marks.len() as u32;
            let region = region_of(key, groups.get(&key).copied().unwrap_or(0));
            let mark = |kind, x: f32, y: f32, z: f32| Landmark {
                kind,
                zone_key: key,
                region,
                pos: [x, y, z],
                name: "".into(),
            };
            if let Some((_, x, y, z)) = a.centre {
                out.marks.push(mark(LandmarkKind::Centre, x, y, z));
            }
            if a.cells >= MIN_CELLS {
                let base = a.centre.map_or(WATER_Z, |c| c.3);
                if let Some((x, y, z)) = a.hill.filter(|h| h.2 >= base + HILL_RISE_M) {
                    out.marks.push(mark(LandmarkKind::Hill, x, y, z));
                }
                if let Some((_, x, y, z)) = a.coast {
                    out.marks.push(mark(LandmarkKind::Coast, x, y, z));
                }
            }
            for r in recalls.iter().filter(|r| r.zone_key == key) {
                out.marks.push(Landmark {
                    kind: LandmarkKind::Recall,
                    zone_key: key,
                    region,
                    pos: r.pos,
                    name: r.name.into(),
                });
            }
            let count = out.marks.len() as u32 - first;
            if count > 0 {
                out.zones.push(ZoneSpots {
                    zone_key: key,
                    cells: a.cells,
                    centroid: centroid(a),
                    first,
                    count,
                });
            }
        }
        out
    }

    /// The zones with landmarks, by zone key.
    pub fn zones(&self) -> &[ZoneSpots] {
        &self.zones
    }

    /// The zone `key`, if it has landmarks.
    pub fn zone(&self, key: u16) -> Option<&ZoneSpots> {
        self.zones
            .binary_search_by_key(&key, |z| z.zone_key)
            .ok()
            .map(|i| &self.zones[i])
    }

    /// The landmarks of zone `key` (the middle first).
    pub fn of_zone(&self, key: u16) -> &[Landmark] {
        self.zone(key).map_or(&[], |z| self.marks_of(z))
    }

    /// The landmarks of `zone`.
    pub fn marks_of(&self, zone: &ZoneSpots) -> &[Landmark] {
        let a = zone.first as usize;
        &self.marks[a..a + zone.count as usize]
    }

    /// Every landmark.
    pub fn all(&self) -> &[Landmark] {
        &self.marks
    }

    /// The zone as players know it that `zone_key` belongs to: its zone group, else the key
    /// itself (see [`region_of`]). Keys outside the grid are their own region.
    pub fn region(&self, zone_key: u16) -> u32 {
        match self.regions.binary_search_by_key(&zone_key, |r| r.0) {
            Ok(i) => self.regions[i].1,
            Err(_) => region_of(zone_key, 0),
        }
    }

    /// Bytes on the heap, roughly.
    pub fn bytes(&self) -> usize {
        self.zones.capacity() * std::mem::size_of::<ZoneSpots>()
            + self.marks.capacity() * std::mem::size_of::<Landmark>()
            + self.regions.capacity() * std::mem::size_of::<(u16, u32)>()
            + self.marks.iter().map(|m| m.name.len()).sum::<usize>()
    }
}

impl World {
    /// The landmarks of the world (built at the first call and shared). See [`Landmarks`].
    pub fn landmarks(&self) -> &Landmarks {
        self.landmarks.get_or_init(|| {
            let recalls: Vec<RecallSpot<'_>> = self
                .recall_points()
                .iter()
                .map(|r| RecallSpot {
                    zone_key: r.zone_key,
                    pos: [r.pos.x, r.pos.y, r.pos.z],
                    name: &r.name,
                })
                .collect();
            let marks = Landmarks::build(
                self.baked().zones.as_ref(),
                &|x, y| self.ground_z(x, y),
                &|k| self.zone_info(k).map(|i| i.group).filter(|&g| g != 0),
                &recalls,
            );
            tracing::debug!(
                zones = marks.zones().len(),
                landmarks = marks.all().len(),
                "landmarks built"
            );
            marks
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 20 x 10 grid of 64 m sectors: zone 1 on the left half, zone 2 on the right, a strip of
    /// zone 3 (two sectors) in the corner.
    fn grid() -> ZoneGrid {
        let (w, h) = (20u32, 10u32);
        let mut keys = vec![0u16; (w * h) as usize];
        for row in 0..h {
            for col in 0..w {
                keys[(row * w + col) as usize] = if col < 10 { 1 } else { 2 };
            }
        }
        keys[0] = 3;
        keys[1] = 3;
        ZoneGrid {
            width: w,
            height: h,
            sector: 64.0,
            keys,
        }
    }

    /// Flat land at 110 m with a hill around (150, 500) in zone 1 and the sea beyond x = 1150.
    fn ground(x: f32, y: f32) -> Option<f32> {
        if x > 1150.0 {
            return Some(80.0);
        }
        let d = ((x - 150.0).powi(2) + (y - 500.0).powi(2)).sqrt();
        Some(110.0 + (60.0 - d / 4.0).max(0.0))
    }

    #[test]
    fn every_zone_gets_its_middle_and_the_big_ones_a_hill_or_a_coast() {
        let recalls = [RecallSpot {
            zone_key: 2,
            pos: [1000.0, 300.0, 112.0],
            name: "Dewstone",
        }];
        // (zones 1 and 3 are one zone group)
        let group = |k: u16| Some(if k == 2 { 0 } else { 7 });
        let l = Landmarks::build(Some(&grid()), &ground, &group, &recalls);
        assert_eq!(
            l.zones().iter().map(|z| z.zone_key).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let kinds = |k: u16| l.of_zone(k).iter().map(|m| m.kind).collect::<Vec<_>>();
        assert_eq!(kinds(1), [LandmarkKind::Centre, LandmarkKind::Hill]);
        // zone 2 reaches into the sea: a middle on dry land, a coast and the recall point
        assert_eq!(
            kinds(2),
            [
                LandmarkKind::Centre,
                LandmarkKind::Coast,
                LandmarkKind::Recall
            ]
        );
        assert_eq!(kinds(3), [LandmarkKind::Centre], "too small for more");
        let hill = &l.of_zone(1)[1];
        assert!((hill.pos[0] - 150.0).abs() < 70.0 && (hill.pos[1] - 500.0).abs() < 70.0);
        assert!(hill.pos[2] > 150.0);
        let coast = &l.of_zone(2)[1];
        assert!(coast.pos[0] < 1150.0 && coast.pos[0] > 1150.0 - COAST_REACH_M - 64.0);
        assert!(ground(coast.pos[0], coast.pos[1]).unwrap() >= WATER_Z);
        let centre = &l.of_zone(2)[0];
        assert!(centre.pos[0] < 1150.0, "the middle is on dry land");
        assert_eq!(&*l.of_zone(2)[2].name, "Dewstone");
        assert!(l.of_zone(9).is_empty());
        assert_eq!(l.zone(1).unwrap().cells, 98, "two went to zone 3");
        assert!(l.bytes() > 0);
        assert_eq!(l.region(1), l.region(3));
        assert_eq!(l.region(1), 7);
        assert_ne!(l.region(2), l.region(1));
        assert_eq!(l.of_zone(3)[0].region, 7);
        assert_eq!(l.region(9), region_of(9, 0), "outside the grid");
    }

    #[test]
    fn without_terrain_only_the_middles_and_without_a_grid_nothing() {
        let l = Landmarks::build(Some(&grid()), &|_, _| None, &|_| Some(0), &[]);
        assert!(l.all().iter().all(|m| m.kind == LandmarkKind::Centre));
        assert_eq!(l.zones().len(), 3);
        assert!(Landmarks::build(None, &ground, &|_| Some(0), &[])
            .all()
            .is_empty());
        // a key without a zone group gets none
        let l = Landmarks::build(Some(&grid()), &ground, &|k| (k != 3).then_some(0), &[]);
        assert!(l.of_zone(3).is_empty() && !l.of_zone(1).is_empty());
        assert_eq!(LandmarkKind::Recall.name(), "recall point");
    }
}
