//! Baked extras written by `aa-bake` (see `docs/DATA-FORMATS.md`): map calibration, housing area
//! polygons, quest sign spheres and the zone sector grid.
//!
//! CONTRACT (shared by the `aa-bake` writer and the `aa-world` reader). The types and function
//! signatures in this file are fixed; implementations may change.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::terrain::Terrain;

/// World rectangle covered by the map image (metres).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorldRect {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

/// `map_<world>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapCalibration {
    /// File name of the image, relative to the data dir.
    pub image: String,
    pub width: u32,
    pub height: u32,
    /// `"painted"` or `"relief"`.
    pub source: String,
    pub world: WorldRect,
}

/// One entry of `housing_areas.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HousingAreaPoly {
    /// `housing_areas.id` in compact.sqlite3.
    pub id: u32,
    pub zone_key: u32,
    /// World x/y, polygon in order (not closed: last != first).
    pub points: Vec<[f32; 2]>,
}

/// One entry of `quest_spheres.json`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct QuestSphere {
    pub quest: u32,
    pub component: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub radius: f32,
}

pub const ZONES_MAGIC: &[u8; 8] = b"PBZON001";

/// `zones_<world>.bin`: zone key per 64 m sector.
pub struct ZoneGrid {
    pub width: u32,
    pub height: u32,
    pub sector: f32,
    /// Row-major zone keys (`row * width + col`), 0 = none.
    pub keys: Vec<u16>,
}

impl ZoneGrid {
    pub fn open(_path: &Path) -> anyhow::Result<ZoneGrid> {
        anyhow::bail!("zone grid reader not implemented yet")
    }

    pub fn write(&self, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("zone grid writer not implemented yet")
    }

    /// Zone key at world (x, y), `None` outside the grid or for key 0.
    pub fn zone_key_at(&self, _x: f32, _y: f32) -> Option<u16> {
        None
    }
}

/// Everything baked for one world. Missing files load as `None`/empty (with a warning), so bots
/// run without baked data (flat terrain fallback, no map image).
#[derive(Default)]
pub struct Baked {
    pub terrain: Option<Terrain>,
    pub map: Option<MapCalibration>,
    pub housing_areas: Vec<HousingAreaPoly>,
    pub quest_spheres: Vec<QuestSphere>,
    pub zones: Option<ZoneGrid>,
}

impl Baked {
    /// Loads `<dir>/heights_<world>.bin`, `map_<world>.json`, `housing_areas.json`,
    /// `quest_spheres.json` and `zones_<world>.bin`, each optional.
    pub fn load(_dir: &Path, _world: &str) -> Baked {
        Baked::default()
    }
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let text = serde_json::to_string(value)?;
    std::fs::write(path, text)?;
    Ok(())
}
