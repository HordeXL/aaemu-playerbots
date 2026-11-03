//! The sea map boats sail on: a bit mask of 16 m cells over the baked terrain, a route planner
//! over it ([`route`]) and the finder of launch and landing spots at the shore ([`shore`]).
//!
//! A cell is *sea* when every terrain sample the cell touches lies below [`NAV_FLOOR_M`] (two
//! metres of water under a 100 m sea level) and holds real terrain (a baked sample of raw 0 with the
//! "cells without terrain hold raw 0" flag is a hole in the data, not deep water). The mask is
//! built once, lazily ([`crate::World::water`]), from the height grid in about a tenth of a second
//! and takes 0.7 MB for `main_world` (2239 x 2431 cells of 16 m). Everything else is computed on
//! demand from it, so hundreds of bots share one copy.
//!
//! Scope: the open sea only. Lakes and rivers lie above the sea level on the height grid (the water
//! surface is not in it), so they are not in the mask and boats are not planned over them.
//!
//! The planner and the shore finder take their terrain through a closure
//! (`&dyn Fn(x, y) -> Option<f32>`), so they work on synthetic grids in the tests.

use crate::terrain::Terrain;

pub mod route;
pub mod shore;

pub use route::{route, Disc, Route, RouteCfg, RouteError};
pub use shore::{find_shore, ShoreSpot};

/// Edge of a mask cell in metres.
pub const CELL_M: f32 = 16.0;
/// Terrain below this height (m) counts as navigable water: two metres under the 100 m sea level.
pub const NAV_FLOOR_M: f32 = 98.0;
/// The sea level of the 1.2 world in metres.
pub const SEA_LEVEL_M: f32 = 100.0;

/// Sea cells of the world on a [`CELL_M`] grid. See the module docs.
#[derive(Debug)]
pub struct WaterMap {
    origin_x: f32,
    origin_y: f32,
    cols: u32,
    rows: u32,
    words_per_row: u32,
    /// Row-major bit rows (bit `c % 64` of word `c / 64`), each row padded to whole words.
    bits: Box<[u64]>,
}

impl WaterMap {
    /// An all-land map of `cols` x `rows` cells whose cell (0, 0) starts at `(origin_x, origin_y)`.
    pub fn empty(origin_x: f32, origin_y: f32, cols: u32, rows: u32) -> WaterMap {
        let words_per_row = cols.div_ceil(64);
        WaterMap {
            origin_x,
            origin_y,
            cols,
            rows,
            words_per_row,
            bits: vec![0u64; words_per_row as usize * rows as usize].into_boxed_slice(),
        }
    }

    /// A map whose cell `(col, row)` is sea when `is_sea(col, row)` says so (synthetic maps).
    pub fn from_cells(
        origin: (f32, f32),
        cols: u32,
        rows: u32,
        is_sea: impl Fn(u32, u32) -> bool,
    ) -> WaterMap {
        let mut m = WaterMap::empty(origin.0, origin.1, cols, rows);
        for r in 0..rows {
            for c in 0..cols {
                if is_sea(c, r) {
                    m.set(c, r);
                }
            }
        }
        m
    }

    /// Builds the mask from a height grid: a cell is sea when every sample it touches (the 4 m grid
    /// has 5 x 5 of them per 16 m cell, the shared edge samples included) is real terrain below
    /// `floor_m`. Rows are scanned by up to four threads.
    pub fn from_terrain(t: &Terrain, floor_m: f32) -> WaterMap {
        let h = t.header;
        let per = ((CELL_M / h.step).round() as u32).max(1);
        let cols = (h.width - 1) / per;
        let rows = (h.height - 1) / per;
        let mut map = WaterMap::empty(h.origin_x, h.origin_y, cols, rows);
        if cols == 0 || rows == 0 {
            return map;
        }
        // raw samples below `limit` are under the floor (raw 0 is a hole when the grid says so)
        let limit = ((floor_m - h.offset) / h.scale).ceil().clamp(0.0, 65535.0) as u32;
        let hole_zero = h.flags & 1 != 0;
        let wpr = map.words_per_row as usize;
        let threads = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .clamp(1, 4);
        let rows_per_chunk = (rows as usize).div_ceil(threads).max(1);
        std::thread::scope(|s| {
            for (i, chunk) in map.bits.chunks_mut(rows_per_chunk * wpr).enumerate() {
                let first_row = (i * rows_per_chunk) as u32;
                s.spawn(move || {
                    fill_rows(t, chunk, wpr, first_row, cols, per, limit, hole_zero);
                });
            }
        });
        map
    }

    fn set(&mut self, c: u32, r: u32) {
        let i = r as usize * self.words_per_row as usize + (c / 64) as usize;
        self.bits[i] |= 1u64 << (c % 64);
    }

    /// Cells along x.
    pub fn cols(&self) -> u32 {
        self.cols
    }

    /// Cells along y.
    pub fn rows(&self) -> u32 {
        self.rows
    }

    /// Whether cell `(c, r)` is sea (cells outside the map are not).
    pub fn sea_cell(&self, c: i32, r: i32) -> bool {
        if c < 0 || r < 0 || c as u32 >= self.cols || r as u32 >= self.rows {
            return false;
        }
        let i = r as usize * self.words_per_row as usize + (c as u32 / 64) as usize;
        self.bits[i] >> (c as u32 % 64) & 1 == 1
    }

    /// The cell holding world point `(x, y)`; `None` outside the map.
    pub fn cell_of(&self, x: f32, y: f32) -> Option<(i32, i32)> {
        let fx = (x - self.origin_x) / CELL_M;
        let fy = (y - self.origin_y) / CELL_M;
        // written so that NaN fails the test
        if !(fx >= 0.0 && fy >= 0.0 && fx < self.cols as f32 && fy < self.rows as f32) {
            return None;
        }
        Some((fx as i32, fy as i32))
    }

    /// World coordinates of the centre of cell `(c, r)`.
    pub fn center_of(&self, c: i32, r: i32) -> (f32, f32) {
        (
            self.origin_x + (c as f32 + 0.5) * CELL_M,
            self.origin_y + (r as f32 + 0.5) * CELL_M,
        )
    }

    /// Whether the cell holding `(x, y)` is sea.
    pub fn sea_at(&self, x: f32, y: f32) -> bool {
        self.cell_of(x, y).is_some_and(|(c, r)| self.sea_cell(c, r))
    }

    /// Whether cell `(c, r)` and every cell within `clear` cells of it (a square) is sea: at least
    /// `clear * 16` m of water to the shore, give or take a cell.
    pub fn open_cell(&self, c: i32, r: i32, clear: i32) -> bool {
        for dr in -clear..=clear {
            for dc in -clear..=clear {
                if !self.sea_cell(c + dc, r + dr) {
                    return false;
                }
            }
        }
        true
    }

    /// [`WaterMap::open_cell`] of the cell holding `(x, y)`.
    pub fn open_at(&self, x: f32, y: f32, clear: i32) -> bool {
        self.cell_of(x, y)
            .is_some_and(|(c, r)| self.open_cell(c, r, clear))
    }

    /// Number of sea cells.
    pub fn sea_count(&self) -> u64 {
        self.bits.iter().map(|w| u64::from(w.count_ones())).sum()
    }

    /// Heap bytes held.
    pub fn bytes(&self) -> usize {
        self.bits.len() * 8
    }
}

/// Fills the mask rows of `chunk` (cell rows `first_row..`): see [`WaterMap::from_terrain`].
#[allow(clippy::too_many_arguments)]
fn fill_rows(
    t: &Terrain,
    chunk: &mut [u64],
    wpr: usize,
    first_row: u32,
    cols: u32,
    per: u32,
    limit: u32,
    hole_zero: bool,
) {
    let h = t.header;
    let rows_here = chunk.len() / wpr;
    let mut ok = vec![true; cols as usize];
    for k in 0..rows_here {
        let row = first_row + k as u32;
        ok.iter_mut().for_each(|v| *v = true);
        for sy in row * per..=row * per + per {
            for sx in 0..=(cols * per).min(h.width - 1) {
                let Some(raw) = t.raw(sx, sy) else {
                    continue;
                };
                let raw = u32::from(raw);
                if raw < limit && !(hole_zero && raw == 0) {
                    continue;
                }
                let c = sx / per;
                if c < cols {
                    ok[c as usize] = false;
                }
                // a sample on the shared edge belongs to the cell before as well
                if sx % per == 0 && c > 0 && c - 1 < cols {
                    ok[(c - 1) as usize] = false;
                }
            }
        }
        let words = &mut chunk[k * wpr..(k + 1) * wpr];
        for (c, good) in ok.iter().enumerate() {
            if *good {
                words[c / 64] |= 1u64 << (c % 64);
            }
        }
    }
}

/// The bot-convention yaw (0 = +Y, counter-clockwise positive) of the direction `(dx, dy)`.
pub fn yaw_of(dx: f32, dy: f32) -> f32 {
    (-dx).atan2(dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::{HeightGridHeader, Terrain};

    /// A 4 m grid of `w` x `h` samples where `f(x, y)` metres tall (x, y world metres).
    pub(crate) fn grid(w: u32, h: u32, flags: u32, f: impl Fn(f32, f32) -> f32) -> Terrain {
        let header = HeightGridHeader {
            width: w,
            height: h,
            origin_x: 0.0,
            origin_y: 0.0,
            step: 4.0,
            scale: 1.0 / 32.0,
            offset: 0.0,
            flags,
        };
        let mut samples = Vec::with_capacity((w * h) as usize);
        for row in 0..h {
            for col in 0..w {
                let m = f(col as f32 * 4.0, row as f32 * 4.0);
                samples.push((m * 32.0).round() as u16);
            }
        }
        Terrain::from_samples(header, samples).unwrap()
    }

    #[test]
    fn a_cell_is_sea_when_every_sample_it_touches_is_deep() {
        // 129 x 129 samples = 8 x 8 cells; land (120 m) in the strip x in [60, 76]
        let t = grid(129, 129, 0, |x, _| {
            if (60.0..=76.0).contains(&x) {
                120.0
            } else {
                50.0
            }
        });
        let m = WaterMap::from_terrain(&t, NAV_FLOOR_M);
        assert_eq!((m.cols(), m.rows()), (32, 32));
        // sample columns 15..=19 are land: they touch cells 3 (x 48..64) and 4 (64..80) and, with
        // the shared edge sample at x = 64, nothing else
        for r in 0..32 {
            for c in 0..32 {
                let expect = !(3..=4).contains(&c);
                assert_eq!(m.sea_cell(c, r), expect, "cell {c},{r}");
            }
        }
        assert!(m.sea_at(8.0, 8.0));
        assert!(!m.sea_at(70.0, 8.0));
        assert!(!m.sea_at(-1.0, 8.0), "outside the map");
        assert!(!m.sea_at(f32::NAN, 8.0));
        assert_eq!(m.cell_of(17.0, 33.0), Some((1, 2)));
        assert_eq!(m.center_of(1, 2), (24.0, 40.0));
    }

    #[test]
    fn holes_in_the_data_are_not_water() {
        // raw 0 is "no terrain" when flag bit 0 is set, plain 0 m deep sea otherwise
        let holes = grid(65, 65, 1, |x, _| if x < 32.0 { 0.0 } else { 50.0 });
        let m = WaterMap::from_terrain(&holes, NAV_FLOOR_M);
        assert!(!m.sea_at(8.0, 8.0));
        assert!(m.sea_at(40.0, 8.0));
        let deep = grid(65, 65, 0, |x, _| if x < 32.0 { 0.0 } else { 50.0 });
        let m = WaterMap::from_terrain(&deep, NAV_FLOOR_M);
        assert!(m.sea_at(8.0, 8.0));
    }

    #[test]
    fn the_floor_sets_how_shallow_a_cell_may_be() {
        let t = grid(65, 65, 0, |_, _| 97.0);
        assert!(WaterMap::from_terrain(&t, 98.0).sea_at(8.0, 8.0));
        assert!(!WaterMap::from_terrain(&t, 96.0).sea_at(8.0, 8.0));
    }

    #[test]
    fn openness_needs_water_all_around() {
        let m = WaterMap::from_cells((0.0, 0.0), 10, 10, |c, r| !(c == 5 && r == 5));
        assert!(m.open_cell(1, 1, 1));
        assert!(!m.open_cell(4, 4, 1), "next to the land cell");
        assert!(m.open_cell(4, 4, 0));
        assert!(!m.open_cell(0, 0, 1), "the map edge is not water");
        assert!(!m.open_at(88.0, 88.0, 0), "the land cell itself");
    }

    #[test]
    fn the_mask_is_small() {
        // the main world: 2239 x 2431 cells
        let m = WaterMap::empty(0.0, 0.0, 2239, 2431);
        assert_eq!(m.bytes(), 35 * 2431 * 8);
        assert!(m.bytes() < 700_000);
        assert_eq!(m.sea_count(), 0);
    }

    #[test]
    fn yaw_follows_the_bot_convention() {
        assert!((yaw_of(0.0, 1.0)).abs() < 1e-6);
        assert!((yaw_of(-1.0, 0.0) - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert!((yaw_of(1.0, 0.0) + std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }
}
