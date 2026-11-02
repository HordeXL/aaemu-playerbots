//! Small helpers around `rusqlite` for reading the server's `compact.sqlite3`.
//!
//! The database stores booleans as the text `'t'` / `'f'` (columns declared `NUM`), integers as
//! integers and sometimes `NULL`; the readers here are lenient so every loader can use plain
//! positional columns (`SELECT a, b, c ...`) without caring about the storage class.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Row};

/// Opens a SQLite file strictly read-only. Temp tables and sorts stay in memory so nothing is
/// ever written next to the database (or to the system drive).
pub fn open_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open {} read-only", path.display()))?;
    conn.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA query_only = ON;")
        .context("set read-only pragmas")?;
    Ok(conn)
}

/// A cell as an integer: integers as is, reals truncated, `'t'`/`'true'` as 1, `'f'`/`'false'`
/// as 0, other text parsed, everything else (NULL, blobs) as 0.
pub fn cell_i64(v: ValueRef<'_>) -> i64 {
    match v {
        ValueRef::Integer(i) => i,
        ValueRef::Real(f) => f as i64,
        ValueRef::Text(t) => match t {
            b"t" | b"T" | b"true" | b"True" => 1,
            b"f" | b"F" | b"false" | b"False" => 0,
            _ => std::str::from_utf8(t)
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .unwrap_or(0),
        },
        ValueRef::Null | ValueRef::Blob(_) => 0,
    }
}

/// Typed positional accessors over a result row; never fail.
pub trait RowExt {
    /// Column `i` as a signed integer (see [`cell_i64`]).
    fn int(&self, i: usize) -> i64;
    /// Column `i` as `u32`, negative values read as 0.
    fn u32(&self, i: usize) -> u32;
    /// Column `i` as `u8`, clamped to `0..=255`.
    fn u8(&self, i: usize) -> u8;
    /// Column `i` as `u16`, clamped to `0..=65535`.
    fn u16(&self, i: usize) -> u16;
    /// Column `i` as a boolean (`'t'`, `'1'`, any non-zero number).
    fn flag(&self, i: usize) -> bool;
    /// Column `i` as text (empty for NULL / non-text).
    fn text(&self, i: usize) -> &str;
}

impl RowExt for Row<'_> {
    fn int(&self, i: usize) -> i64 {
        cell_i64(self.get_ref_unwrap(i))
    }

    fn u32(&self, i: usize) -> u32 {
        self.int(i).clamp(0, i64::from(u32::MAX)) as u32
    }

    fn u8(&self, i: usize) -> u8 {
        self.int(i).clamp(0, i64::from(u8::MAX)) as u8
    }

    fn u16(&self, i: usize) -> u16 {
        self.int(i).clamp(0, i64::from(u16::MAX)) as u16
    }

    fn flag(&self, i: usize) -> bool {
        match self.get_ref_unwrap(i) {
            ValueRef::Text(t) => matches!(t, b"t" | b"T" | b"true" | b"True" | b"1"),
            other => cell_i64(other) != 0,
        }
    }

    fn text(&self, i: usize) -> &str {
        match self.get_ref_unwrap(i) {
            ValueRef::Text(t) => std::str::from_utf8(t).unwrap_or(""),
            _ => "",
        }
    }
}

/// Runs `sql` and calls `f` for every row.
pub fn each_row(conn: &Connection, sql: &str, mut f: impl FnMut(&Row<'_>)) -> Result<()> {
    let mut stmt = conn
        .prepare(sql)
        .with_context(|| format!("prepare `{sql}`"))?;
    let mut rows = stmt.query([]).with_context(|| format!("run `{sql}`"))?;
    while let Some(row) = rows.next().with_context(|| format!("read `{sql}`"))? {
        f(row);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_cells() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t (a, b, c, d, e);
             INSERT INTO t VALUES ('t', 'f', 7, NULL, 'abc');
             INSERT INTO t VALUES (1, 0, '12', 2.9, 'x');",
        )
        .unwrap();
        let mut seen = Vec::new();
        each_row(&conn, "SELECT a, b, c, d, e FROM t ORDER BY rowid", |r| {
            seen.push((
                r.flag(0),
                r.flag(1),
                r.int(2),
                r.int(3),
                r.text(4).to_owned(),
            ));
        })
        .unwrap();
        assert_eq!(seen[0], (true, false, 7, 0, "abc".into()));
        assert_eq!(seen[1], (true, false, 12, 2, "x".into()));
    }
}
