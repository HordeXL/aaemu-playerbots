//! Spawn positions per template plus a uniform grid for "what is near this point" queries.
//!
//! Layout is flat and compact (positions are `f32`, ids `u32`): one `Vec<Vec3>` grouped by
//! template with a sorted template list for binary search, and a CSR-style grid (`starts` +
//! `entries`) over the bounding box of all points. All distances are horizontal (x/y): spawn
//! heights are missing or approximate, and caves/buildings overlap in 2D anyway.

use crate::geom::Vec3;

/// Edge length of a grid cell in metres.
pub const CELL: f32 = 256.0;

#[derive(Clone, Copy, Debug)]
struct Entry {
    template: u32,
    pos: Vec3,
}

/// Static spawn positions of one kind of object (NPCs or doodads).
#[derive(Debug, Default)]
pub struct SpawnIndex {
    /// Sorted distinct template ids.
    templates: Vec<u32>,
    /// `(start, len)` into `points` for each entry of `templates`.
    ranges: Vec<(u32, u32)>,
    /// Positions grouped by template, in input order within a template.
    points: Vec<Vec3>,
    // Grid.
    min_x: f32,
    min_y: f32,
    w: u32,
    h: u32,
    /// `w * h + 1` offsets into `entries`.
    starts: Vec<u32>,
    entries: Vec<Entry>,
}

impl SpawnIndex {
    /// Builds the index from `(template, position)` pairs. Non-finite positions are dropped.
    pub fn build(mut spawns: Vec<(u32, Vec3)>) -> Self {
        spawns.retain(|(_, p)| p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
        spawns.sort_by_key(|s| s.0); // stable: file order within a template is kept

        let mut templates = Vec::new();
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        let mut points = Vec::with_capacity(spawns.len());
        for (i, (t, p)) in spawns.iter().enumerate() {
            if templates.last() != Some(t) {
                templates.push(*t);
                ranges.push((i as u32, 0));
            }
            ranges.last_mut().expect("range pushed above").1 += 1;
            points.push(*p);
        }

        let mut idx = SpawnIndex {
            templates,
            ranges,
            points,
            ..SpawnIndex::default()
        };
        idx.build_grid(&spawns);
        idx
    }

    fn build_grid(&mut self, spawns: &[(u32, Vec3)]) {
        if spawns.is_empty() {
            return;
        }
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (_, p) in spawns {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
        self.min_x = min_x;
        self.min_y = min_y;
        self.w = ((max_x - min_x) / CELL) as u32 + 1;
        self.h = ((max_y - min_y) / CELL) as u32 + 1;
        let cells = self.w as usize * self.h as usize;

        // Counting sort of the entries into cells.
        let mut starts = vec![0u32; cells + 1];
        for (_, p) in spawns {
            starts[self.cell_of(p.x, p.y) + 1] += 1;
        }
        for i in 0..cells {
            starts[i + 1] += starts[i];
        }
        let mut fill = starts.clone();
        let mut entries = vec![
            Entry {
                template: 0,
                pos: Vec3::default()
            };
            spawns.len()
        ];
        for (t, p) in spawns {
            let c = self.cell_of(p.x, p.y);
            entries[fill[c] as usize] = Entry {
                template: *t,
                pos: *p,
            };
            fill[c] += 1;
        }
        self.starts = starts;
        self.entries = entries;
    }

    fn cell_of(&self, x: f32, y: f32) -> usize {
        let cx = (((x - self.min_x) / CELL) as u32).min(self.w - 1);
        let cy = (((y - self.min_y) / CELL) as u32).min(self.h - 1);
        cy as usize * self.w as usize + cx as usize
    }

    /// Spawn positions of `template` (empty if it never spawns).
    pub fn of(&self, template: u32) -> &[Vec3] {
        match self.templates.binary_search(&template) {
            Ok(i) => {
                let (start, len) = self.ranges[i];
                &self.points[start as usize..(start + len) as usize]
            }
            Err(_) => &[],
        }
    }

    /// The spawn of `template` closest (horizontally) to `from`.
    pub fn nearest(&self, template: u32, from: Vec3) -> Option<Vec3> {
        self.of(template)
            .iter()
            .copied()
            .min_by(|a, b| a.dist2d_sq(from).total_cmp(&b.dist2d_sq(from)))
    }

    /// Calls `f(template, position)` for every spawn within `radius` metres (horizontal) of
    /// `center`.
    pub fn within(&self, center: Vec3, radius: f32, mut f: impl FnMut(u32, Vec3)) {
        if self.entries.is_empty() || radius < 0.0 || !radius.is_finite() {
            return;
        }
        let r2 = radius * radius;
        let lo_x = ((center.x - radius - self.min_x) / CELL).floor();
        let hi_x = ((center.x + radius - self.min_x) / CELL).floor();
        let lo_y = ((center.y - radius - self.min_y) / CELL).floor();
        let hi_y = ((center.y + radius - self.min_y) / CELL).floor();
        if hi_x < 0.0 || hi_y < 0.0 || lo_x >= self.w as f32 || lo_y >= self.h as f32 {
            return;
        }
        let (x0, x1) = (lo_x.max(0.0) as u32, (hi_x as u32).min(self.w - 1));
        let (y0, y1) = (lo_y.max(0.0) as u32, (hi_y as u32).min(self.h - 1));
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                let c = cy as usize * self.w as usize + cx as usize;
                let range = self.starts[c] as usize..self.starts[c + 1] as usize;
                for e in &self.entries[range] {
                    if e.pos.dist2d_sq(center) <= r2 {
                        f(e.template, e.pos);
                    }
                }
            }
        }
    }

    /// Total number of spawns.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether there are no spawns.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Number of distinct templates that spawn.
    pub fn template_count(&self) -> usize {
        self.templates.len()
    }

    /// Whether `template` has at least one spawn.
    pub fn has(&self, template: u32) -> bool {
        self.templates.binary_search(&template).is_ok()
    }

    /// Heap bytes held.
    pub fn bytes(&self) -> usize {
        self.templates.capacity() * 4
            + self.ranges.capacity() * 8
            + self.points.capacity() * std::mem::size_of::<Vec3>()
            + self.starts.capacity() * 4
            + self.entries.capacity() * std::mem::size_of::<Entry>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<(u32, Vec3)> {
        // Pseudo-random but deterministic scatter over a 3 km x 2 km area, 5 templates.
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f32 / (1u64 << 53) as f32
        };
        (0..4000)
            .map(|i| {
                (
                    (i % 5) as u32 + 100,
                    Vec3::new(
                        -500.0 + rnd() * 3000.0,
                        200.0 + rnd() * 2000.0,
                        rnd() * 50.0,
                    ),
                )
            })
            .collect()
    }

    #[test]
    fn per_template_lookup_and_nearest() {
        let idx = SpawnIndex::build(vec![
            (7, Vec3::new(10.0, 10.0, 0.0)),
            (7, Vec3::new(100.0, 10.0, 0.0)),
            (3, Vec3::new(0.0, 0.0, 0.0)),
        ]);
        assert_eq!(idx.of(7).len(), 2);
        assert_eq!(idx.of(3).len(), 1);
        assert!(idx.of(99).is_empty());
        assert_eq!(idx.template_count(), 2);
        assert_eq!(
            idx.nearest(7, Vec3::new(90.0, 0.0, 0.0)),
            Some(Vec3::new(100.0, 10.0, 0.0))
        );
        assert_eq!(idx.nearest(99, Vec3::default()), None);
    }

    #[test]
    fn grid_query_matches_brute_force() {
        let pts = sample();
        let idx = SpawnIndex::build(pts.clone());
        for (center, radius) in [
            (Vec3::new(500.0, 800.0, 0.0), 300.0),
            (Vec3::new(-500.0, 200.0, 0.0), 700.0), // box corner
            (Vec3::new(2500.0, 2200.0, 0.0), 50.0),
            (Vec3::new(-10_000.0, 0.0, 0.0), 100.0), // far outside
            (Vec3::new(1000.0, 1000.0, 0.0), 0.0),
            (Vec3::new(1000.0, 1000.0, 0.0), 5000.0), // everything
        ] {
            let mut got = Vec::new();
            idx.within(center, radius, |t, p| {
                got.push((t, p.x.to_bits(), p.y.to_bits()))
            });
            let mut want: Vec<_> = pts
                .iter()
                .filter(|(_, p)| p.dist2d_sq(center) <= radius * radius)
                .map(|(t, p)| (*t, p.x.to_bits(), p.y.to_bits()))
                .collect();
            got.sort_unstable();
            want.sort_unstable();
            assert_eq!(got, want, "center {center:?} radius {radius}");
        }
    }

    #[test]
    fn empty_index_is_safe() {
        let idx = SpawnIndex::build(Vec::new());
        assert!(idx.is_empty());
        let mut n = 0;
        idx.within(Vec3::default(), 1000.0, |_, _| n += 1);
        assert_eq!(n, 0);
        assert!(idx.of(1).is_empty());
    }

    #[test]
    fn drops_non_finite_positions() {
        let idx = SpawnIndex::build(vec![
            (1, Vec3::new(f32::NAN, 0.0, 0.0)),
            (1, Vec3::new(1.0, 2.0, 3.0)),
        ]);
        assert_eq!(idx.len(), 1);
    }
}
