//! Terrain height grid (`heights_<world>.bin`, see `docs/DATA-FORMATS.md`), memory-mapped so only
//! the pages around bots become resident.
//!
//! CONTRACT (shared by the `aa-bake` writer and the `aa-world` reader). The types and function
//! signatures in this file are fixed; implementations may change.

use std::path::Path;

pub const HEIGHTS_MAGIC: &[u8; 8] = b"PBHGT001";
pub const HEIGHTS_HEADER_LEN: usize = 64;

/// Header of a height grid file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeightGridHeader {
    pub width: u32,
    pub height: u32,
    pub origin_x: f32,
    pub origin_y: f32,
    pub step: f32,
    pub scale: f32,
    pub offset: f32,
    pub flags: u32,
}

/// Writes a height grid file (header + row-major u16 samples).
pub fn write_height_grid(_path: &Path, _header: &HeightGridHeader, _samples: &[u16]) -> anyhow::Result<()> {
    anyhow::bail!("height grid writer not implemented yet")
}

/// Read-only, memory-mapped height grid.
pub struct Terrain {
    pub header: HeightGridHeader,
}

impl Terrain {
    pub fn open(_path: &Path) -> anyhow::Result<Terrain> {
        anyhow::bail!("height grid reader not implemented yet")
    }

    /// Bilinear terrain height at world (x, y); `None` outside the grid.
    pub fn height(&self, _x: f32, _y: f32) -> Option<f32> {
        None
    }

    /// (min_x, min_y, max_x, max_y) covered by the grid.
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let h = &self.header;
        (
            h.origin_x,
            h.origin_y,
            h.origin_x + h.step * (h.width.saturating_sub(1)) as f32,
            h.origin_y + h.step * (h.height.saturating_sub(1)) as f32,
        )
    }
}
