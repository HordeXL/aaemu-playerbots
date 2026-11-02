//! Read-only lookups in the server's `compact.sqlite3` used to validate and calibrate the bake.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use rusqlite::{Connection, OpenFlags};

/// A rectangle in world metres (`x`, `y` = minimum corner).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// An opened `compact.sqlite3` (read-only).
pub struct Compact {
    conn: Connection,
}

impl Compact {
    pub fn open(path: &Path) -> anyhow::Result<Compact> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open {}", path.display()))?;
        Ok(Compact { conn })
    }

    /// Ids of the `housing_areas` table.
    pub fn housing_area_ids(&self) -> anyhow::Result<HashSet<u32>> {
        let mut stmt = self.conn.prepare("SELECT id FROM housing_areas")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, u32>(0))?
            .collect::<Result<HashSet<_>, _>>()?;
        Ok(ids)
    }

    /// The `world_groups` rectangle of the world with this name (the whole-world map image).
    pub fn world_group_rect(&self, world: &str) -> anyhow::Result<Option<RectF>> {
        let mut stmt = self
            .conn
            .prepare("SELECT x, y, w, h FROM world_groups WHERE name = ?1 AND w > 0 AND h > 0")?;
        let mut rows = stmt.query([world])?;
        match rows.next()? {
            Some(r) => Ok(Some(RectF {
                x: r.get(0)?,
                y: r.get(1)?,
                w: r.get(2)?,
                h: r.get(3)?,
            })),
            None => Ok(None),
        }
    }

    /// `(name, rect)` of every zone group that has a map rectangle.
    pub fn zone_group_rects(&self) -> anyhow::Result<Vec<(String, RectF)>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, x, y, w, h FROM zone_groups WHERE w > 0 AND h > 0 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    RectF {
                        x: r.get(1)?,
                        y: r.get(2)?,
                        w: r.get(3)?,
                        h: r.get(4)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
