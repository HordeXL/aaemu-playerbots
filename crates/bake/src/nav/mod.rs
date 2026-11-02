//! Bake step `nav` / `nav_instances`: the client's `.bai` navigation meshes into
//! `nav_<world>.bin` (format: `docs/DATA-FORMATS.md`, reader: `aa_world::nav::NavMesh`).
//!
//! A world's navigation data is either a grid of 256 m "path" blocks
//! (`game/worlds/<w>/paths/<bx>_<by>/netmission0.bai`, positions relative to the block origin
//! `block * 256`) or, for instance worlds, one folder per zone
//! (`game/worlds/<w>/zone/<zone key>/netmission0.bai`, positions relative to the zone origin cell
//! `origin * 1024` from `world.xml`). Like the server, a world with zone folders uses those and
//! nothing else. `hidemission*.bai` and `areasmission*.bai` are not used.

pub mod bai;
pub mod graph;
pub mod merge;
pub mod roads;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context};

use crate::pak::Pak;

pub use graph::{assemble, AssembleOptions, AssembleStats};
pub use merge::{MergeStats, NavBuilder};

/// Islands smaller than this are dropped from the main world (a tree-ringed ledge or a roof).
pub const MIN_COMPONENT_MAIN: usize = 50;
/// Instance worlds are fragmented into rooms: keep nearly everything.
pub const MIN_COMPONENT_INSTANCE: usize = 4;

/// How a world lays out its navigation files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavLayout {
    /// `paths/<bx>_<by>/`, 256 m blocks.
    Paths,
    /// `zone/<key>/`, positions relative to the zone's origin cell.
    Zones,
}

impl NavLayout {
    pub fn name(self) -> &'static str {
        match self {
            NavLayout::Paths => "paths",
            NavLayout::Zones => "zones",
        }
    }
}

/// One folder with navigation files.
#[derive(Debug, Clone, PartialEq)]
pub struct NavFolder {
    /// Folder name (`038_058` or a zone key).
    pub name: String,
    /// Mission suffixes (`netmission<suffix>.bai`, normally `0`).
    pub suffixes: Vec<String>,
    /// World position of the folder's local origin.
    pub offset: [f64; 3],
}

/// What baking one world's mesh produced.
#[derive(Debug, Clone)]
pub struct NavReport {
    pub world: String,
    pub layout: NavLayout,
    pub folders: usize,
    pub files_failed: usize,
    pub merge: MergeStats,
    pub assemble: AssembleStats,
    pub file: String,
    pub bytes: u64,
    /// The `ROAD` flags (main world only).
    pub roads: Option<roads::RoadReport>,
    /// Seconds: reading + merging the files, assembling, writing.
    pub secs: (f32, f32, f32),
    pub warnings: Vec<String>,
}

impl std::fmt::Display for NavReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (m, a) = (&self.merge, &self.assemble);
        writeln!(
            f,
            "nav {}: {} folders ({}), {} file(s) failed; read {} nodes, {} merged -> {} ({} triangles, {} waypoints; {} other-type, {} invalid triangles)",
            self.world,
            self.folders,
            self.layout.name(),
            self.files_failed,
            m.nodes_read,
            m.nodes_merged,
            a.nodes_merged,
            a.triangles,
            a.waypoints,
            m.nodes_other,
            m.triangles_invalid
        )?;
        writeln!(
            f,
            "  links: {} read, {} unresolved, {} self; merged {} (-{} duplicates, {} with differing radius); {} blocked, {} narrow dropped, {} kept",
            m.links_read,
            m.links_unresolved,
            m.links_self,
            a.links_merged,
            a.links_duplicate,
            a.links_duplicate_conflict,
            a.links_blocked,
            a.links_narrow,
            a.links_kept
        )?;
        writeln!(
            f,
            "  islands: {} weak components, {} kept; {} nodes dropped, {} kept ({} triangles, {} waypoints), {} vertices ({} on the border), {} links",
            a.wcc_total,
            a.wcc_kept,
            a.nodes_dropped,
            a.nodes_kept,
            a.triangles_kept,
            a.waypoints_kept,
            a.vertices_kept,
            a.boundary_vertices,
            a.links_final
        )?;
        writeln!(
            f,
            "  strong components (0.5 m): {} (largest {}, {} singletons), {} nodes in components >= 50, {} >= 1000; {} one-way links, {} triangle links without a common edge",
            a.scc_count,
            a.scc_largest,
            a.scc_singletons,
            a.nodes_in_scc_50,
            a.nodes_in_scc_1000,
            a.one_way_links,
            a.links_no_portal
        )?;
        writeln!(
            f,
            "  grid {} m: {} cells, {} entries; {} -> {:.1} MB; read {:.1} s, assemble {:.1} s, write {:.1} s",
            a.bucket,
            a.grid_cells,
            a.grid_items,
            self.file,
            self.bytes as f64 / 1e6,
            self.secs.0,
            self.secs.1,
            self.secs.2
        )?;
        if let Some(r) = &self.roads {
            write!(f, "{r}")?;
        }
        for w in &self.warnings {
            writeln!(f, "  warning: {w}")?;
        }
        Ok(())
    }
}

/// `zone key -> origin cell` from a `world.xml` (all `<Zone id originX originY>` elements).
pub fn parse_zone_origins(xml: &str) -> HashMap<u32, (i32, i32)> {
    let Ok(doc) = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    ) else {
        return HashMap::new();
    };
    let attr =
        |n: roxmltree::Node, a: &str| n.attribute(a).and_then(|v| v.trim().parse::<i64>().ok());
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "Zone")
        .filter_map(|n| {
            let id = u32::try_from(attr(n, "id")?).ok()?;
            Some((
                id,
                (
                    i32::try_from(attr(n, "originX").unwrap_or(0)).ok()?,
                    i32::try_from(attr(n, "originY").unwrap_or(0)).ok()?,
                ),
            ))
        })
        .collect()
}

/// Splits `game/worlds/<w>/<zone|paths>/<folder>/netmission<suffix>.bai`.
fn split_net_name(name: &str) -> Option<(&str, NavLayout, &str, &str)> {
    let rest = name.strip_prefix("game/worlds/")?;
    let (world, rest) = rest.split_once('/')?;
    let (kind, rest) = rest.split_once('/')?;
    let layout = match kind {
        "zone" => NavLayout::Zones,
        "paths" => NavLayout::Paths,
        _ => return None,
    };
    let (folder, file) = rest.split_once('/')?;
    let suffix = file.strip_prefix("netmission")?.strip_suffix(".bai")?;
    Some((world, layout, folder, suffix))
}

/// The worlds of the pak that have navigation data, with the layout each uses. A world with
/// zone folders is a `Zones` world.
pub fn nav_worlds_in_pak(pak: &Pak) -> BTreeMap<String, NavLayout> {
    let mut out: BTreeMap<String, NavLayout> = BTreeMap::new();
    for name in pak.names_with_prefix("game/worlds/") {
        if let Some((world, layout, _, _)) = split_net_name(name) {
            let e = out.entry(world.to_string()).or_insert(layout);
            if layout == NavLayout::Zones {
                *e = NavLayout::Zones;
            }
        }
    }
    out
}

/// The folders of `world` with navigation files and where their local origins lie.
pub fn nav_folders(
    pak: &Pak,
    world: &str,
    zone_origins: &HashMap<u32, (i32, i32)>,
) -> (NavLayout, Vec<NavFolder>) {
    let prefix = format!("game/worlds/{world}/");
    let mut zones: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in pak.names_with_prefix(&prefix) {
        if let Some((w, layout, folder, suffix)) = split_net_name(name) {
            if w != world {
                continue;
            }
            let map = if layout == NavLayout::Zones {
                &mut zones
            } else {
                &mut paths
            };
            map.entry(folder.to_string())
                .or_default()
                .insert(suffix.to_string());
        }
    }
    let mut folders = Vec::new();
    if !zones.is_empty() {
        for (name, suffixes) in zones {
            let Ok(key) = name.parse::<u32>() else {
                continue;
            };
            let (ox, oy) = zone_origins.get(&key).copied().unwrap_or((0, 0));
            folders.push(NavFolder {
                name,
                suffixes: suffixes.into_iter().collect(),
                offset: [f64::from(ox) * 1024.0, f64::from(oy) * 1024.0, 0.0],
            });
        }
        return (NavLayout::Zones, folders);
    }
    for (name, suffixes) in paths {
        let Some((bx, by)) = name.split_once('_') else {
            continue;
        };
        let (Ok(bx), Ok(by)) = (bx.parse::<u32>(), by.parse::<u32>()) else {
            continue;
        };
        folders.push(NavFolder {
            name,
            suffixes: suffixes.into_iter().collect(),
            offset: [f64::from(bx) * 256.0, f64::from(by) * 256.0, 0.0],
        });
    }
    (NavLayout::Paths, folders)
}

/// Reads, merges and assembles the navigation data of `world` and writes
/// `<out_dir>/nav_<world>.bin`.
pub fn bake_nav_world(
    pak: &mut Pak,
    world: &str,
    zone_origins: &HashMap<u32, (i32, i32)>,
    out_dir: &Path,
    min_component_nodes: usize,
) -> anyhow::Result<NavReport> {
    let t0 = Instant::now();
    let (layout, folders) = nav_folders(pak, world, zone_origins);
    if folders.is_empty() {
        bail!(
            "no navigation files (netmission*.bai) for world {world} in {}",
            pak.path().display()
        );
    }
    let mut builder = NavBuilder::new();
    let mut warnings = Vec::new();
    let mut files_failed = 0usize;
    let warn = |w: &mut Vec<String>, msg: String| {
        if w.len() < 20 {
            w.push(msg);
        }
    };
    for folder in &folders {
        for suffix in &folder.suffixes {
            let base = format!(
                "game/worlds/{world}/{}/{}/",
                if layout == NavLayout::Zones {
                    "zone"
                } else {
                    "paths"
                },
                folder.name
            );
            let net_name = format!("{base}netmission{suffix}.bai");
            let net = match pak
                .read(&net_name)
                .and_then(|b| b.context("missing"))
                .and_then(|b| bai::parse_net(&b))
            {
                Ok(n) => n,
                Err(e) => {
                    files_failed += 1;
                    builder.stats.blocks_failed += 1;
                    warn(&mut warnings, format!("{net_name}: {e:#}"));
                    continue;
                }
            };
            if net.nodes.is_empty() {
                builder.stats.blocks += 1;
                continue;
            }
            let verts_name = format!("{base}vertsmission{suffix}.bai");
            let verts = match pak.read(&verts_name) {
                Ok(Some(b)) => match bai::parse_verts(&b) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        files_failed += 1;
                        warn(&mut warnings, format!("{verts_name}: {e:#}"));
                        None
                    }
                },
                Ok(None) => None,
                Err(e) => {
                    files_failed += 1;
                    warn(&mut warnings, format!("{verts_name}: {e:#}"));
                    None
                }
            };
            builder.add_block(&net, verts.as_ref(), folder.offset);
        }
    }
    let merge = builder.stats;
    let read_secs = t0.elapsed().as_secs_f32();

    let t1 = Instant::now();
    let (mut data, assemble) = assemble(
        builder,
        &AssembleOptions {
            min_component_nodes,
            bucket: None,
        },
    )
    .with_context(|| format!("navigation mesh of {world}"))?;
    let roads = (layout == NavLayout::Paths)
        .then(|| roads::flag_roads(pak, world, zone_origins, &mut data));
    let assemble_secs = t1.elapsed().as_secs_f32();

    let t2 = Instant::now();
    let file = format!("nav_{world}.bin");
    let final_path = out_dir.join(&file);
    let tmp_path = out_dir.join(format!("{file}.tmp"));
    data.write_file(&tmp_path)?;
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {} to {}", tmp_path.display(), final_path.display()))?;
    let bytes = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
    let write_secs = t2.elapsed().as_secs_f32();

    Ok(NavReport {
        world: world.to_string(),
        layout,
        folders: folders.len(),
        files_failed,
        merge,
        assemble,
        file,
        bytes,
        roads,
        secs: (read_secs, assemble_secs, write_secs),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_net_names() {
        assert_eq!(
            split_net_name("game/worlds/main_world/paths/038_058/netmission0.bai"),
            Some(("main_world", NavLayout::Paths, "038_058", "0"))
        );
        assert_eq!(
            split_net_name("game/worlds/instance_library/zone/298/netmission1.bai"),
            Some(("instance_library", NavLayout::Zones, "298", "1"))
        );
        assert_eq!(
            split_net_name("game/worlds/w/paths/038_058/hidemission0.bai"),
            None
        );
        assert_eq!(
            split_net_name("game/worlds/w/paths/038_058/vertsmission0.bai"),
            None
        );
        assert_eq!(
            split_net_name("game/worlds/w/terrain/netmission0.bai"),
            None
        );
        assert_eq!(
            split_net_name("game/other/w/paths/1_1/netmission0.bai"),
            None
        );
    }

    #[test]
    fn zone_origins_from_world_xml() {
        let xml = r#"<?xml version="1.0"?>
<World name="instance_x" cellXCount="2" cellYCount="2">
  <ZoneList>
    <Zone name="a" id="296" originX="1" originY="0"><cellList/></Zone>
    <Zone name="b" id="297" originX="0" originY="1"/>
    <Zone name="c" id="x"/>
  </ZoneList>
</World>"#;
        let o = parse_zone_origins(xml);
        assert_eq!(o.get(&296), Some(&(1, 0)));
        assert_eq!(o.get(&297), Some(&(0, 1)));
        assert_eq!(o.len(), 2);
        assert!(parse_zone_origins("not xml").is_empty());
    }
}
