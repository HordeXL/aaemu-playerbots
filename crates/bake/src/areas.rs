//! Per-zone level design files: `housing_area.xml` (housing polygons) and
//! `quest_sign_sphere.g` (quest sign spheres).
//!
//! Ported from the AAEmu Rust server (`aaemu-game/src/systems/npcs/subzone.rs` `parse_area_file_kind`,
//! `is_inside`, and `systems/npcs/sphere_quest.rs` `parse_quest_sign_sphere`; GPL-3.0-or-later).

use aa_world::baked::{HousingAreaPoly, QuestSphere};
use anyhow::{anyhow, Context};

/// `float.Parse(s, InvariantCulture)` with thousands separators allowed (as the server does).
fn parse_float(s: &str) -> Option<f32> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    t.replace(',', "").parse().ok()
}

/// `uint.Parse(s)`: surrounding white space and a leading `+`; `-0` counts as 0.
fn parse_uint(s: &str) -> Option<u32> {
    let t = s.trim();
    let t = t.strip_prefix('+').unwrap_or(t);
    if let Some(d) = t.strip_prefix('-') {
        return (!d.is_empty() && d.bytes().all(|b| b == b'0')).then_some(0);
    }
    t.parse().ok()
}

/// The housing polygons of one `housing_area.xml` for a zone whose origin cell is `origin`.
/// `value1` is `housing_areas.id` (`Id` belongs to the engine trigger and is only a fallback).
/// Points are local to the entity (`Pos` plus the entity's `cellX`/`cellY` offset from the zone
/// origin cell); the result is in world metres.
pub fn parse_housing_areas(
    xml: &str,
    zone_key: u32,
    origin: (i32, i32),
) -> anyhow::Result<Vec<HousingAreaPoly>> {
    parse_area_polys(xml, zone_key, origin, true)
}

/// The polygons of one `subzone_area.xml` (same file layout as the housing areas, but the area's
/// own `Id` is the `sub_zones.id`; the server's `parse_area_file`). Only the public farm sub zones
/// matter to the bots, see [`aa_world::farm::FARM_SUB_ZONES`].
pub fn parse_sub_zone_areas(
    xml: &str,
    zone_key: u32,
    origin: (i32, i32),
) -> anyhow::Result<Vec<HousingAreaPoly>> {
    parse_area_polys(xml, zone_key, origin, false)
}

fn parse_area_polys(
    xml: &str,
    zone_key: u32,
    origin: (i32, i32),
    housing: bool,
) -> anyhow::Result<Vec<HousingAreaPoly>> {
    let doc = roxmltree::Document::parse(xml).context("parse area xml")?;
    let root = doc.root_element();
    let mut out = Vec::new();
    if root.tag_name().name() != "Objects" {
        return Ok(out);
    }
    let named = |n: &roxmltree::Node, tag: &str| n.is_element() && n.tag_name().name() == tag;
    for block in root.children().filter(|n| named(n, "Entity")) {
        if block.attribute("Name").is_none() {
            continue;
        }
        let cell_x_offset = block
            .attribute("cellX")
            .and_then(|v| v.trim().parse::<i32>().ok())
            .unwrap_or(0);
        let cell_y_offset = block
            .attribute("cellY")
            .and_then(|v| v.trim().parse::<i32>().ok())
            .unwrap_or(0);
        for area in block.children().filter(|n| named(n, "Area")) {
            let id_attr = if housing {
                area.attribute("value1").or_else(|| area.attribute("Id"))
            } else {
                area.attribute("Id")
            };
            let id = match id_attr {
                Some(v) => parse_uint(v).ok_or_else(|| anyhow!("invalid area id {v:?}"))?,
                None => 0,
            };
            let mut start = [0f32; 2];
            if let Some(pos) = block.attribute("Pos") {
                let v: Vec<&str> = pos.split(',').collect();
                if v.len() != 3 {
                    continue;
                }
                if let (Some(x), Some(y)) = (parse_float(v[0]), parse_float(v[1])) {
                    start = [x, y];
                }
            }
            let cell_offset = (
                (origin.0 + cell_x_offset) as f32 * 1024.0,
                (origin.1 + cell_y_offset) as f32 * 1024.0,
            );
            let mut points = Vec::new();
            for pts in area.children().filter(|n| named(n, "Points")) {
                for point in pts.children().filter(|n| named(n, "Point")) {
                    let Some(pos) = point.attribute("Pos") else {
                        continue;
                    };
                    let v: Vec<&str> = pos.split(',').collect();
                    if v.len() != 3 {
                        continue;
                    }
                    if let (Some(x), Some(y)) = (parse_float(v[0]), parse_float(v[1])) {
                        points.push([x + cell_offset.0 + start[0], y + cell_offset.1 + start[1]]);
                    }
                }
            }
            out.push(HousingAreaPoly {
                id,
                zone_key,
                points,
            });
        }
    }
    Ok(out)
}

/// Polygon crossing test in x/y; points on an edge count as inside (the server's `Point.IsInside`).
pub fn polygon_contains(polygon: &[[f32; 2]], p: [f32; 2]) -> bool {
    if polygon.len() < 3 || !p[0].is_finite() || !p[1].is_finite() {
        return false;
    }
    let (x, y) = (f64::from(p[0]), f64::from(p[1]));
    let mut inside = false;
    for (a, b) in polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
    {
        let (ax, ay, bx, by) = (
            f64::from(a[0]),
            f64::from(a[1]),
            f64::from(b[0]),
            f64::from(b[1]),
        );
        if (x - ax) * (by - ay) == (y - ay) * (bx - ax)
            && x >= ax.min(bx)
            && x <= ax.max(bx)
            && y >= ay.min(by)
            && y <= ay.max(by)
        {
            return true;
        }
        if (ay > y) != (by > y) && x < ax + (y - ay) * (bx - ax) / (by - ay) {
            inside = !inside;
        }
    }
    inside
}

/// `string.Trim(' ').Trim('\t').Trim('\r')`.
fn trim_line(s: &str) -> &str {
    s.trim_matches(' ').trim_matches('\t').trim_matches('\r')
}

/// The spheres of one `quest_sign_sphere.g`: blocks of `area` / `qtype <quest>` / `ctype
/// <component>` / `pos ( x .., y .., z .. )` / `radius ..` lines, converted to world coordinates
/// with `origin` (the zone's origin cell; positions are local to it).
pub fn parse_quest_spheres(contents: &str, origin: (i32, i32)) -> anyhow::Result<Vec<QuestSphere>> {
    let lower = contents.to_lowercase();
    let lines: Vec<&str> = lower.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 4 < lines.len() {
        let (l0, l1, l2, l3, l4) = (
            trim_line(lines[i]),
            trim_line(lines[i + 1]),
            trim_line(lines[i + 2]),
            trim_line(lines[i + 3]),
            trim_line(lines[i + 4]),
        );
        if l0.starts_with("area")
            && l1.starts_with("qtype")
            && l2.starts_with("ctype")
            && l3.starts_with("pos")
            && l4.starts_with("radius")
        {
            let err = || anyhow!("bad quest sphere block at line {}", i + 1);
            let quest = l1.get(6..).and_then(parse_uint).ok_or_else(err)?;
            let component = l2.get(6..).and_then(parse_uint).ok_or_else(err)?;
            let pos_text: String = l3
                .get(4..)
                .ok_or_else(err)?
                .chars()
                .filter(|c| !matches!(c, '(' | ')' | 'x' | 'y' | 'z' | ' '))
                .collect();
            let pos: Vec<&str> = pos_text.split(',').collect();
            let mut xyz = [0f32; 3];
            if pos.len() == 3 {
                for (k, p) in pos.iter().enumerate() {
                    xyz[k] = parse_float_strict(p).ok_or_else(err)?;
                }
            }
            let radius = l4.get(7..).and_then(parse_float_strict).ok_or_else(err)?;
            out.push(QuestSphere {
                quest,
                component,
                x: origin.0 as f32 * 1024.0 + xyz[0],
                y: origin.1 as f32 * 1024.0 + xyz[1],
                z: xyz[2],
                radius,
            });
            i += 4;
        }
        i += 1;
    }
    Ok(out)
}

/// `float.Parse(s, NumberStyles.Float)`: no thousands separators.
fn parse_float_strict(s: &str) -> Option<f32> {
    let t = s.trim();
    if t.is_empty() || t.contains(',') {
        return None;
    }
    t.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUSING: &str = r#"<Objects>
 <Entity Name="shape_1" Pos="100,200,10" cellX="1" cellY="0">
  <Area Id="0" value1="52">
   <Points>
    <Point Pos="0,0,0"/>
    <Point Pos="50,0,0"/>
    <Point Pos="50,40,1"/>
    <Point Pos="0,40,0"/>
   </Points>
  </Area>
 </Entity>
 <Entity Name="shape_2" Pos="0,0,0">
  <Area Id="77"><Points><Point Pos="0,0,0"/><Point Pos="bad"/><Point Pos="1,1,1"/></Points></Area>
 </Entity>
 <Entity Pos="0,0,0"><Area value1="9"/></Entity>
</Objects>"#;

    #[test]
    fn housing_points_become_world_coordinates() {
        let a = parse_housing_areas(HOUSING, 179, (14, 13)).unwrap();
        assert_eq!(a.len(), 2, "entity without Name is skipped");
        assert_eq!(a[0].id, 52);
        assert_eq!(a[0].zone_key, 179);
        // origin (14,13) + cellX 1 -> x = 15*1024 + 100 + local, y = 13*1024 + 200 + local
        let (ox, oy) = (15.0 * 1024.0 + 100.0, 13.0 * 1024.0 + 200.0);
        assert_eq!(
            a[0].points,
            vec![
                [ox, oy],
                [ox + 50.0, oy],
                [ox + 50.0, oy + 40.0],
                [ox, oy + 40.0]
            ]
        );
        assert!(polygon_contains(&a[0].points, [ox + 10.0, oy + 10.0]));
        assert!(!polygon_contains(&a[0].points, [ox + 60.0, oy + 10.0]));
        assert!(
            polygon_contains(&a[0].points, [ox + 50.0, oy + 20.0]),
            "edge counts"
        );
        // value1 absent -> Id fallback; invalid points skipped
        assert_eq!(a[1].id, 77);
        assert_eq!(a[1].points.len(), 2);
    }

    const SUB_ZONES: &str = r#"<Objects>
 <Entity Name="farm" Pos="100,200,10" cellX="0" cellY="1">
  <Area Id="966" value1="52">
   <Points>
    <Point Pos="0,0,0"/>
    <Point Pos="60,0,0"/>
    <Point Pos="60,40,1"/>
    <Point Pos="0,40,0"/>
   </Points>
  </Area>
  <Area Id="12"><Points><Point Pos="0,0,0"/><Point Pos="9,0,0"/><Point Pos="9,9,0"/></Points></Area>
 </Entity>
</Objects>"#;

    #[test]
    fn sub_zone_areas_use_the_area_id_not_the_housing_value() {
        let a = parse_sub_zone_areas(SUB_ZONES, 182, (14, 13)).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].id, a[0].zone_key), (966, 182), "Id, not value1");
        assert_eq!(a[1].id, 12);
        let (ox, oy) = (14.0 * 1024.0 + 100.0, 14.0 * 1024.0 + 200.0);
        assert_eq!(a[0].points[0], [ox, oy]);
        assert_eq!(a[0].points[2], [ox + 60.0, oy + 40.0]);
        assert!(polygon_contains(&a[0].points, [ox + 30.0, oy + 20.0]));
        // the same file read as housing areas takes value1
        let h = parse_housing_areas(SUB_ZONES, 182, (14, 13)).unwrap();
        assert_eq!(h[0].id, 52);
        assert_eq!(h[1].id, 12, "no value1: Id is the fallback");
        // an area without an Id has id 0, which the bake drops
        let none = "<Objects><Entity Name=\"a\"><Area><Points/></Area></Entity></Objects>";
        assert_eq!(parse_sub_zone_areas(none, 1, (0, 0)).unwrap()[0].id, 0);
        assert!(parse_sub_zone_areas("<Objects", 1, (0, 0)).is_err());
    }

    #[test]
    fn housing_rejects_bad_xml_and_ids() {
        assert!(parse_housing_areas("<Objects", 1, (0, 0)).is_err());
        let bad = "<Objects><Entity Name=\"a\"><Area value1=\"x\"/></Entity></Objects>";
        assert!(parse_housing_areas(bad, 1, (0, 0)).is_err());
        assert!(parse_housing_areas("<Other/>", 1, (0, 0))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn quest_spheres_parse_like_the_server() {
        let text = "area\r\n    qtype 4295\r\n    ctype 18755\r\n    pos ( x 540.891, y 949.949, z 116.438 )\r\n    radius 40\r\n\
                    area\r\n    qtype 1\r\n    ctype 2\r\n    pos ( x 1.5, y -2.5, z 3 )\r\n    radius 12.5\r\n";
        let s = parse_quest_spheres(text, (14, 13)).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].quest, 4295);
        assert_eq!(s[0].component, 18755);
        assert!((s[0].x - (14.0 * 1024.0 + 540.891)).abs() < 1e-3);
        assert!((s[0].y - (13.0 * 1024.0 + 949.949)).abs() < 1e-3);
        assert_eq!(s[0].z, 116.438);
        assert_eq!(s[0].radius, 40.0);
        assert!((s[1].y - (13.0 * 1024.0 - 2.5)).abs() < 1e-3);
        assert_eq!(s[1].radius, 12.5);
    }

    #[test]
    fn quest_spheres_reject_bad_numbers_and_ignore_other_blocks() {
        let bad = "area\n qtype x\n ctype 2\n pos ( x 1, y 2, z 3 )\n radius 4\n";
        assert!(parse_quest_spheres(bad, (0, 0)).is_err());
        assert!(parse_quest_spheres("something\nelse\n", (0, 0))
            .unwrap()
            .is_empty());
        assert!(parse_quest_spheres("", (0, 0)).unwrap().is_empty());
    }
}
