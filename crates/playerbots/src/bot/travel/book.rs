//! The teleport book as the bot knows it: the private entries (recorded with Memory Ink), the
//! visited recall districts and the bound recall point.
//!
//! The server sends a private entry as an offset from the origin cell of its zone
//! ([`PortalEntry::off_x`]); [`Entry::world`] is the world position once the zone's cell is known
//! (see [`aa_world::travel::portal::ZoneOrigins`]). District entries are the recall points of
//! `recalls.json` with the same id, so their world position comes from the world data.

use aa_proto::sc_travel::PortalEntry;
use aa_world::travel::portal::ZoneOrigins;

/// The most private entries the recording duty keeps in the book.
pub const MAX_PRIVATE: usize = 20;
/// The most private entries remembered at all (a book that came with more is cut).
const KEEP_PRIVATE: usize = 64;
/// The most district entries remembered.
const KEEP_DISTRICTS: usize = 160;
/// Longest name of an entry the bot records, in characters.
pub const MAX_NAME_CHARS: usize = 32;
/// Tolerance for learning a zone cell from a recall point (the data and the wire are exact).
pub const EXACT_TOLERANCE_M: f32 = 1.5;
/// Tolerance for learning a zone cell from the spot the bot stood on when it recorded (the server's
/// idea of the position may lag).
pub const STOOD_TOLERANCE_M: f32 = 6.0;

/// A private book entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Book entry id.
    pub id: u32,
    /// Zone key of the saved spot (0 = the offsets are world coordinates).
    pub zone: u16,
    /// The name it was given.
    pub name: Box<str>,
    /// Wire offsets from the zone's origin cell.
    pub off: [f32; 2],
    /// World `(x, y)`, known once the zone's origin cell is.
    pub world: Option<[f32; 2]>,
}

/// A visited recall district (`id` = the recall point's id in `recalls.json`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct District {
    /// Recall point id.
    pub id: u32,
    /// Zone key.
    pub zone: u16,
    /// Wire offsets from the zone's origin cell.
    pub off: [f32; 2],
}

/// Where a gate can be opened to, as the planner sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    /// [`aa_proto::cs::PORTAL_KIND_DISTRICT`] or [`aa_proto::cs::PORTAL_KIND_PRIVATE`].
    pub kind: u8,
    /// Book entry id.
    pub id: u32,
    /// Zone key of the entry.
    pub zone: u16,
    /// World `(x, y)`.
    pub at: [f32; 2],
}

/// The teleport book. See the module docs.
#[derive(Debug, Default)]
pub struct Book {
    /// Private entries, in id order.
    pub private: Vec<Entry>,
    /// Visited recall districts.
    pub districts: Vec<District>,
    /// The bound recall point (a recall point id, 0 = none).
    pub bound: u32,
    /// Something changed that [`Book::resolve`] has not looked at.
    dirty: bool,
}

impl Book {
    /// Merges private entries (by id; the server sends the whole book at login and a single new
    /// entry afterwards). Returns the ids that were not in the book before.
    pub fn merge(&mut self, list: &[PortalEntry]) -> Vec<u32> {
        let mut added = Vec::new();
        for e in list {
            let zone = u16::try_from(e.zone_id).unwrap_or(0);
            let entry = Entry {
                id: e.id,
                zone,
                name: e.name.clone(),
                off: [e.off_x, e.off_y],
                world: None,
            };
            let at_cap = self.private.len() >= KEEP_PRIVATE;
            match self.private.iter_mut().find(|p| p.id == e.id) {
                Some(p) => {
                    // (a rename keeps what is known of the position)
                    let world = p.world;
                    *p = entry;
                    p.world = world.filter(|_| p.zone == zone);
                }
                None if !at_cap => {
                    self.private.push(entry);
                    added.push(e.id);
                }
                None => {}
            }
        }
        self.private.sort_by_key(|p| p.id);
        self.dirty = true;
        added
    }

    /// Replaces the district list (the server always sends all of it) and the bound point.
    pub fn set_districts(&mut self, list: &[PortalEntry], bound: u32) {
        self.districts = list
            .iter()
            .take(KEEP_DISTRICTS)
            .map(|e| District {
                id: e.id,
                zone: u16::try_from(e.zone_id).unwrap_or(0),
                off: [e.off_x, e.off_y],
            })
            .collect();
        self.bound = bound;
        self.dirty = true;
    }

    /// Forgets an entry (`kind` 1 = district, else private).
    pub fn remove(&mut self, kind: u8, id: i32) {
        let Ok(id) = u32::try_from(id) else { return };
        if kind == aa_proto::cs::PORTAL_KIND_DISTRICT {
            self.districts.retain(|d| d.id != id);
        } else {
            self.private.retain(|p| p.id != id);
        }
    }

    /// The private entry `id`.
    pub fn private_by_id(&self, id: u32) -> Option<&Entry> {
        self.private.iter().find(|p| p.id == id)
    }

    /// Notes the world position of private entry `id` (the bot knows where it stood).
    pub fn set_world(&mut self, id: u32, world: [f32; 2]) {
        if let Some(p) = self.private.iter_mut().find(|p| p.id == id) {
            p.world = Some(world);
        }
    }

    /// Whether [`Book::resolve`] has anything to look at.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Learns zone origin cells from the districts (`recall_at` maps a recall point id to its zone
    /// key and world `(x, y)`) and gives every private entry whose zone is known its world
    /// position. Returns whether `origins` learned something.
    pub fn resolve(
        &mut self,
        origins: &mut ZoneOrigins,
        recall_at: impl Fn(u32) -> Option<(u16, [f32; 2])>,
    ) -> bool {
        self.dirty = false;
        let mut learned = false;
        for d in &self.districts {
            if d.zone == 0 || origins.get(d.zone).is_some() {
                continue;
            }
            if let Some((zone, at)) = recall_at(d.id) {
                if zone == d.zone {
                    learned |= origins
                        .learn_from(
                            zone,
                            (at[0], at[1]),
                            (d.off[0], d.off[1]),
                            EXACT_TOLERANCE_M,
                        )
                        .unwrap_or(false);
                }
            }
        }
        for p in &mut self.private {
            if p.world.is_none() {
                p.world = origins
                    .world_xy(p.zone, (p.off[0], p.off[1]))
                    .map(|(x, y)| [x, y]);
            }
        }
        learned
    }

    /// Every entry a gate can be opened to whose world position is known: the private entries and
    /// the districts (`recall_at` gives their positions).
    pub fn targets<'a>(
        &'a self,
        recall_at: impl Fn(u32) -> Option<(u16, [f32; 2])> + 'a,
    ) -> impl Iterator<Item = Target> + 'a {
        let private = self.private.iter().filter_map(|p| {
            Some(Target {
                kind: aa_proto::cs::PORTAL_KIND_PRIVATE,
                id: p.id,
                zone: p.zone,
                at: p.world?,
            })
        });
        let districts = self.districts.iter().filter_map(move |d| {
            let (zone, at) = recall_at(d.id)?;
            Some(Target {
                kind: aa_proto::cs::PORTAL_KIND_DISTRICT,
                id: d.id,
                zone,
                at,
            })
        });
        private.chain(districts)
    }

    /// The distance to the nearest private entry with a known position, and its id.
    pub fn nearest_private(&self, at: [f32; 2]) -> Option<(f32, u32)> {
        self.private
            .iter()
            .filter_map(|p| Some((dist(p.world?, at), p.id)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
    }

    /// The private entry the bot can do without: one whose position it does not know, else the
    /// one that has a neighbour closest by (it is the most redundant), the older id first.
    pub fn least_useful(&self) -> Option<u32> {
        if let Some(p) = self.private.iter().find(|p| p.world.is_none()) {
            return Some(p.id);
        }
        self.private
            .iter()
            .map(|p| {
                let near = self
                    .private
                    .iter()
                    .filter(|q| q.id != p.id)
                    .filter_map(|q| Some(dist(p.world?, q.world?)))
                    .fold(f32::INFINITY, f32::min);
                (near, p.id)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
            .map(|(_, id)| id)
    }
}

/// Horizontal distance between two world points.
pub fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn wire(id: u32, zone: u32, off: (f32, f32)) -> PortalEntry {
        PortalEntry {
            id,
            name: format!("spot {id}").into(),
            zone_id: zone,
            off_x: off.0,
            off_y: off.1,
            z: 100.0,
            z_rot: 0.0,
        }
    }

    #[test]
    fn merge_adds_by_id_and_a_rename_keeps_the_position() {
        let mut b = Book::default();
        assert_eq!(b.merge(&[wire(4100, 179, (10.0, 20.0))]), vec![4100]);
        b.set_world(4100, [1.0, 2.0]);
        // the same id again: not new, name changes, the known position stays
        let mut renamed = wire(4100, 179, (10.0, 20.0));
        renamed.name = "home".into();
        assert!(b.merge(&[renamed]).is_empty());
        assert_eq!(&*b.private[0].name, "home");
        assert_eq!(b.private[0].world, Some([1.0, 2.0]));
        // a second entry sorts after; ids stay unique
        assert_eq!(b.merge(&[wire(4099, 179, (0.0, 0.0))]), vec![4099]);
        assert_eq!(
            b.private.iter().map(|p| p.id).collect::<Vec<_>>(),
            [4099, 4100]
        );
        // the book is bounded
        let many: Vec<_> = (0..200).map(|i| wire(5000 + i, 1, (0.0, 0.0))).collect();
        b.merge(&many);
        assert_eq!(b.private.len(), KEEP_PRIVATE);
    }

    #[test]
    fn resolve_learns_cells_from_districts_and_places_private_entries() {
        let mut b = Book::default();
        // recall point 243 is at world (15578.04, 15382.12) in zone 179; the wire sent the offsets
        b.set_districts(&[wire(243, 179, (218.04, 22.12))], 243);
        b.merge(&[wire(4100, 179, (100.0, 200.0)), wire(4101, 77, (5.0, 5.0))]);
        assert!(b.is_dirty());
        let mut origins = ZoneOrigins::default();
        let recall = |id: u32| (id == 243).then_some((179, [15578.04, 15382.12]));
        assert!(b.resolve(&mut origins, recall));
        assert!(!b.is_dirty());
        assert_eq!(origins.get(179), Some((15, 15)));
        assert_eq!(b.private[0].world, Some([100.0 + 15360.0, 200.0 + 15360.0]));
        assert_eq!(b.private[1].world, None, "zone 77 is unknown");
        // nothing new the second time
        assert!(!b.resolve(&mut origins, recall));
        // a recall point in another zone than the entry says is not trusted
        let mut b2 = Book::default();
        b2.set_districts(&[wire(243, 180, (218.04, 22.12))], 0);
        let mut o2 = ZoneOrigins::default();
        assert!(!b2.resolve(&mut o2, recall));
        assert!(o2.is_empty());
    }

    #[test]
    fn targets_cover_private_entries_and_districts_with_known_positions() {
        let mut b = Book::default();
        b.merge(&[wire(4100, 0, (100.0, 200.0)), wire(4101, 77, (5.0, 5.0))]);
        b.set_districts(&[wire(243, 179, (0.0, 0.0)), wire(9, 179, (0.0, 0.0))], 0);
        let mut o = ZoneOrigins::default();
        b.resolve(&mut o, |_| None);
        let recall = |id: u32| (id == 243).then_some((179, [15578.0, 15382.0]));
        let t: Vec<_> = b.targets(recall).collect();
        assert_eq!(t.len(), 2, "zone 77 and recall 9 have no position: {t:?}");
        assert_eq!((t[0].kind, t[0].id, t[0].at), (2, 4100, [100.0, 200.0]));
        assert_eq!((t[1].kind, t[1].id), (1, 243));
        assert_eq!(b.nearest_private([90.0, 190.0]).map(|n| n.1), Some(4100));
    }

    #[test]
    fn remove_follows_the_kind() {
        let mut b = Book::default();
        b.merge(&[wire(4100, 0, (1.0, 1.0))]);
        b.set_districts(&[wire(243, 179, (0.0, 0.0))], 243);
        b.remove(1, 4100);
        assert_eq!(
            b.private.len(),
            1,
            "a district delete never touches a private entry"
        );
        b.remove(2, 243);
        assert_eq!(b.districts.len(), 1);
        b.remove(1, 243);
        assert!(b.districts.is_empty());
        b.remove(2, 4100);
        assert!(b.private.is_empty());
        b.remove(2, -5);
    }

    #[test]
    fn the_least_useful_entry_has_no_position_or_the_closest_neighbour() {
        let mut b = Book::default();
        b.merge(&[
            wire(4100, 0, (0.0, 0.0)),
            wire(4101, 0, (5000.0, 0.0)),
            wire(4102, 0, (300.0, 0.0)),
            wire(4103, 5, (0.0, 0.0)),
        ]);
        let mut o = ZoneOrigins::default();
        b.resolve(&mut o, |_| None);
        assert_eq!(b.least_useful(), Some(4103), "unknown position goes first");
        b.remove(2, 4103);
        // 4100 and 4102 are 300 m apart, 4101 is far from both: the older of the pair goes
        assert_eq!(b.least_useful(), Some(4100));
        assert_eq!(Book::default().least_useful(), None);
    }
}
