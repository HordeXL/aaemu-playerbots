//! `game/worlds/<world>/world.xml`: world size, ocean level and the zone key of every 64 m sector.
//!
//! Ported from the AAEmu Rust server (`aaemu-game/src/data/world_templates.rs`,
//! `WorldTemplate::read_world_node` / `read_zone_node` / `zone_id_at`; GPL-3.0-or-later).

use anyhow::{bail, Context};

/// Sectors per cell side (a sector is 64 m, a cell 1024 m).
pub const SECTORS_PER_CELL: i32 = 16;
/// Metres per sector.
pub const SECTOR_SIZE: f32 = 64.0;
/// Metres per cell.
pub const CELL_SIZE: f32 = 1024.0;

/// One `<Zone>` of the world.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneDef {
    pub key: u32,
    pub name: String,
    /// `originX` / `originY`: the cell that local zone coordinates are relative to.
    pub origin: (i32, i32),
}

/// Parsed `world.xml`.
#[derive(Debug, Clone)]
pub struct WorldDef {
    pub name: String,
    /// World size in cells.
    pub cell_x: u32,
    pub cell_y: u32,
    pub unit_size: u32,
    pub ocean_level: f32,
    pub max_height: f32,
    pub zones: Vec<ZoneDef>,
    /// Zone key per 64 m sector, row-major (`sector_y * sectors_x + sector_x`), 0 = none.
    pub sector_zone: Vec<u16>,
    /// Sector columns (`cell_x * 16`).
    pub sectors_x: u32,
    /// Sector rows (`cell_y * 16`).
    pub sectors_y: u32,
}

impl WorldDef {
    /// Origin cell of a zone, (0, 0) when unknown (the server's `GetZoneOriginCell`).
    pub fn zone_origin(&self, zone_key: u32) -> (i32, i32) {
        self.zones
            .iter()
            .find(|z| z.key == zone_key)
            .map_or((0, 0), |z| z.origin)
    }

    /// Zone key at world (x, y) (the server's `zone_id_at`, but flooring negative positions
    /// instead of truncating them); 0 when outside the world.
    pub fn zone_at(&self, x: f32, y: f32) -> u32 {
        let sx = (x / SECTOR_SIZE).floor() as i32;
        let sy = (y / SECTOR_SIZE).floor() as i32;
        if sx < 0 || sy < 0 || sx >= self.sectors_x as i32 || sy >= self.sectors_y as i32 {
            return 0;
        }
        u32::from(self.sector_zone[sy as usize * self.sectors_x as usize + sx as usize])
    }
}

fn attr<T: std::str::FromStr>(node: roxmltree::Node, name: &str, default: T) -> T {
    node.attribute(name)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn children<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    tag: &'static str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    node.children()
        .filter(move |n| n.is_element() && n.tag_name().name() == tag)
}

/// Parses a `world.xml` document.
pub fn parse_world_xml(xml: &str) -> anyhow::Result<WorldDef> {
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .context("parse world.xml")?;
    let root = doc.root_element();
    if root.tag_name().name() != "World" {
        bail!(
            "world.xml root is <{}>, expected <World>",
            root.tag_name().name()
        );
    }
    let cell_x: u32 = attr(root, "cellXCount", 0);
    let cell_y: u32 = attr(root, "cellYCount", 0);
    if cell_x == 0 || cell_y == 0 || cell_x > 4096 || cell_y > 4096 {
        bail!("bad world size {cell_x}x{cell_y} cells");
    }
    let sectors_x = cell_x * SECTORS_PER_CELL as u32;
    let sectors_y = cell_y * SECTORS_PER_CELL as u32;
    let mut def = WorldDef {
        name: attr(root, "name", String::new()),
        cell_x,
        cell_y,
        unit_size: attr(root, "unitSize", 2),
        ocean_level: attr(root, "oceanLevel", 100.0),
        max_height: attr(root, "maxTerrainHeight", 4096.0),
        zones: Vec::new(),
        sector_zone: vec![0; sectors_x as usize * sectors_y as usize],
        sectors_x,
        sectors_y,
    };
    for zone_list in children(root, "ZoneList") {
        for zone_node in children(zone_list, "Zone") {
            let key: u32 = attr(zone_node, "id", 0);
            def.zones.push(ZoneDef {
                key,
                name: attr(zone_node, "name", String::new()),
                origin: (attr(zone_node, "originX", 0), attr(zone_node, "originY", 0)),
            });
            // Zone keys are stored as u16 in the baked grid.
            let key16 = u16::try_from(key).unwrap_or(0);
            for cell_list in children(zone_node, "cellList") {
                for cell in children(cell_list, "cell") {
                    let cx: i32 = attr(cell, "x", 0);
                    let cy: i32 = attr(cell, "y", 0);
                    for sector_list in children(cell, "sectorList") {
                        for sector in children(sector_list, "sector") {
                            let sx = attr::<i32>(sector, "x", 0) + cx * SECTORS_PER_CELL;
                            let sy = attr::<i32>(sector, "y", 0) + cy * SECTORS_PER_CELL;
                            if sx < 0 || sy < 0 || sx >= sectors_x as i32 || sy >= sectors_y as i32
                            {
                                bail!(
                                    "zone {key}: sector ({sx}, {sy}) outside the {sectors_x}x{sectors_y} world"
                                );
                            }
                            def.sector_zone[sy as usize * sectors_x as usize + sx as usize] = key16;
                        }
                    }
                }
            }
        }
    }
    Ok(def)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<World name="main_world" resolution="1024" unitSize="2" cellXCount="2" cellYCount="1" oceanLevel="99.5" maxTerrainHeight="2048">
  <ZoneList>
    <Zone name="zone_a" id="129" originX="0" originY="0">
      <cellList><cell x="0" y="0"><sectorList><sector x="0" y="0"/><sector x="15" y="15"/></sectorList></cell></cellList>
    </Zone>
    <Zone name="zone_b" id="130" originX="1" originY="0">
      <cellList><cell x="1" y="0"><sectorList><sector x="0" y="1"/></sectorList></cell></cellList>
    </Zone>
  </ZoneList>
</World>"#;

    #[test]
    fn parses_zones_and_sectors() {
        let w = parse_world_xml(XML).unwrap();
        assert_eq!(w.name, "main_world");
        assert_eq!((w.cell_x, w.cell_y), (2, 1));
        assert_eq!((w.sectors_x, w.sectors_y), (32, 16));
        assert_eq!(w.ocean_level, 99.5);
        assert_eq!(w.max_height, 2048.0);
        assert_eq!(w.zones.len(), 2);
        assert_eq!(w.zone_origin(130), (1, 0));
        assert_eq!(w.zone_origin(999), (0, 0));
        assert_eq!(w.zone_at(1.0, 1.0), 129);
        assert_eq!(w.zone_at(15.0 * 64.0 + 1.0, 15.0 * 64.0 + 1.0), 129);
        assert_eq!(w.zone_at(1024.0 + 1.0, 64.0 + 1.0), 130);
        assert_eq!(w.zone_at(100.0, 100.0), 0, "no zone there");
        assert_eq!(w.zone_at(-1.0, 0.0), 0);
        assert_eq!(w.zone_at(2048.0, 0.0), 0);
    }

    #[test]
    fn rejects_bad_documents() {
        assert!(parse_world_xml("<Other/>").is_err());
        assert!(parse_world_xml("<World cellXCount=\"0\" cellYCount=\"0\"/>").is_err());
        assert!(parse_world_xml("not xml").is_err());
        let outside = XML.replace("<sector x=\"15\" y=\"15\"/>", "<sector x=\"15\" y=\"16\"/>");
        assert!(parse_world_xml(&outside).is_err());
    }
}
