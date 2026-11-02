//! Assembles the per-cell `heightmap.dat` files into one world height grid.

use std::time::Instant;

use aa_world::terrain::HeightGridHeader;
use anyhow::{bail, ensure};

use crate::hmap::{decode_cell, CELL_SAMPLES};
use crate::pak::Pak;
use crate::worldxml::{WorldDef, CELL_SIZE};

/// Metres per raw unit of the baked grid: 1/32 m, so the u16 range reaches 2047.97 m (the raw
/// main_world maximum is ~1695 m; the server's own 1/64 m grid clamps at 1023.98 m).
pub const HEIGHT_SCALE: f32 = 1.0 / 32.0;
/// `flags` bit 0: cells without terrain hold raw 0.
pub const FLAG_VOID_IS_ZERO: u32 = 1;

/// The assembled height grid (kept in memory for the relief map and the sanity checks).
pub struct HeightGrid {
    pub header: HeightGridHeader,
    pub samples: Vec<u16>,
}

impl HeightGrid {
    /// Bilinear height at world (x, y), `None` outside; same maths as `aa_world::terrain`.
    pub fn height(&self, x: f32, y: f32) -> Option<f32> {
        let h = &self.header;
        let fx = (x - h.origin_x) / h.step;
        let fy = (y - h.origin_y) / h.step;
        if !(fx >= 0.0 && fy >= 0.0 && fx <= (h.width - 1) as f32 && fy <= (h.height - 1) as f32) {
            return None;
        }
        let c0 = (fx.floor() as usize).min(h.width as usize - 1);
        let r0 = (fy.floor() as usize).min(h.height as usize - 1);
        let c1 = (c0 + 1).min(h.width as usize - 1);
        let r1 = (r0 + 1).min(h.height as usize - 1);
        let (tx, ty) = (fx - c0 as f32, fy - r0 as f32);
        let at =
            |c: usize, r: usize| self.samples[r * h.width as usize + c] as f32 * h.scale + h.offset;
        let top = at(c0, r0) + (at(c1, r0) - at(c0, r0)) * tx;
        let bottom = at(c0, r1) + (at(c1, r1) - at(c0, r1)) * tx;
        Some(top + (bottom - top) * ty)
    }

    /// Raw sample, `None` outside.
    pub fn raw(&self, col: usize, row: usize) -> Option<u16> {
        (col < self.header.width as usize && row < self.header.height as usize)
            .then(|| self.samples[row * self.header.width as usize + col])
    }
}

/// What the height bake did.
#[derive(Debug, Clone, Default)]
pub struct HeightsReport {
    pub cells_in_world: usize,
    /// Cells with a `heightmap.dat` in the pak.
    pub cells_with_terrain: usize,
    /// Cells whose `heightmap.dat` failed to decode (left as raw 0), with the reason.
    pub failed_cells: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub step: f32,
    pub min_height: f32,
    pub max_height: f32,
    pub bytes_read: u64,
    /// Samples above the 16-bit range (65535 / 32 = 2047.97 m) that were
    /// clamped.
    pub samples_clamped: u64,
    pub seconds: f32,
}

/// `step` must be a multiple of the native 2 m grid and divide the 1024 m cell.
pub fn validate_step(step: f32) -> anyhow::Result<()> {
    ensure!(
        step.is_finite()
            && step >= 2.0
            && (step / 2.0).fract() == 0.0
            && (CELL_SIZE / step).fract() == 0.0,
        "step {step} must be a multiple of 2 m that divides 1024 m (2, 4, 8, 16, ...)"
    );
    Ok(())
}

/// Reads every cell's heightmap and assembles the grid at `step` metres per sample, processing
/// one cell at a time. Cells without terrain stay raw 0.
pub fn bake_heights(
    pak: &mut Pak,
    world: &WorldDef,
    step: f32,
) -> anyhow::Result<(HeightGrid, HeightsReport)> {
    validate_step(step)?;
    let started = Instant::now();
    let per_cell = (CELL_SIZE / step) as usize;
    let skip = (step / 2.0) as usize; // 2 m samples per grid step
    let width = world.cell_x as usize * per_cell;
    let height = world.cell_y as usize * per_cell;
    let total = width
        .checked_mul(height)
        .filter(|&t| t <= 1 << 31)
        .ok_or_else(|| anyhow::anyhow!("height grid {width}x{height} is too large"))?;
    let mut samples = vec![0u16; total];
    let mut report = HeightsReport {
        cells_in_world: world.cell_x as usize * world.cell_y as usize,
        width: width as u32,
        height: height as u32,
        step,
        min_height: f32::MAX,
        max_height: f32::MIN,
        ..Default::default()
    };

    // Visit the cells in pak order: sequential-ish disk reads.
    let mut cells: Vec<(u64, u32, u32, String)> = Vec::new();
    for cy in 0..world.cell_y {
        for cx in 0..world.cell_x {
            let name = format!(
                "game/worlds/{}/cells/{cx:03}_{cy:03}/client/terrain/heightmap.dat",
                world.name
            );
            if let Some(e) = pak.entry(&name) {
                cells.push((e.offset, cx, cy, name));
            }
        }
    }
    cells.sort_unstable();
    report.cells_with_terrain = cells.len();
    if cells.is_empty() {
        bail!("no heightmap.dat found for world {}", world.name);
    }

    for (_, cx, cy, name) in &cells {
        let Some(bytes) = pak.read(name)? else {
            continue;
        };
        report.bytes_read += bytes.len() as u64;
        let heights = match decode_cell(&bytes) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("{name}: {e:#}; cell left empty");
                report.failed_cells.push(format!("{name}: {e:#}"));
                continue;
            }
        };
        let (base_col, base_row) = (*cx as usize * per_cell, *cy as usize * per_cell);
        for gy in 0..per_cell {
            let row_start = (base_row + gy) * width + base_col;
            for gx in 0..per_cell {
                let h = heights[gx * skip * CELL_SAMPLES + gy * skip];
                report.min_height = report.min_height.min(h);
                report.max_height = report.max_height.max(h);
                let raw = (h / HEIGHT_SCALE).round();
                if raw > 65535.0 {
                    report.samples_clamped += 1;
                }
                samples[row_start + gx] = raw.clamp(0.0, 65535.0) as u16;
            }
        }
    }
    if report.min_height > report.max_height {
        report.min_height = 0.0;
        report.max_height = 0.0;
    }
    report.seconds = started.elapsed().as_secs_f32();
    let header = HeightGridHeader {
        width: width as u32,
        height: height as u32,
        origin_x: 0.0,
        origin_y: 0.0,
        step,
        scale: HEIGHT_SCALE,
        offset: 0.0,
        flags: FLAG_VOID_IS_ZERO,
    };
    Ok((HeightGrid { header, samples }, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_validation() {
        for ok in [2.0, 4.0, 8.0, 16.0, 64.0] {
            validate_step(ok).unwrap();
        }
        for bad in [0.0, 1.0, 3.0, 6.0, 5.0, f32::NAN, -4.0] {
            assert!(validate_step(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn grid_lookup_matches_the_runtime_reader() {
        let header = HeightGridHeader {
            width: 3,
            height: 2,
            origin_x: 0.0,
            origin_y: 0.0,
            step: 4.0,
            scale: HEIGHT_SCALE,
            offset: 0.0,
            flags: 1,
        };
        let samples = vec![0u16, 64, 128, 64, 128, 640];
        let g = HeightGrid {
            header,
            samples: samples.clone(),
        };
        let t = aa_world::terrain::Terrain::from_samples(header, samples).unwrap();
        for (x, y) in [(0.0, 0.0), (2.0, 1.0), (6.0, 2.0), (8.0, 4.0), (3.3, 3.9)] {
            assert_eq!(g.height(x, y), t.height(x, y), "({x}, {y})");
        }
        assert_eq!(g.height(8.1, 0.0), None);
        assert_eq!(g.raw(2, 1), Some(640));
        assert_eq!(g.raw(3, 1), None);
    }
}
