//! Helpers shared by the unit tests.

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A scratch directory that is removed when dropped. C: is nearly full on the dev machine, so the
/// data volume is preferred when present.
pub struct TempDir(PathBuf);

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh, empty scratch directory named after `name` and the process.
pub fn temp_dir(name: &str) -> TempDir {
    let d_root = Path::new("D:/aaemu-playerbots-data");
    let base = if d_root.is_dir() {
        d_root.join("tmp-test")
    } else {
        std::env::temp_dir().join("playerbots-test")
    };
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the test directory");
    TempDir(dir)
}
