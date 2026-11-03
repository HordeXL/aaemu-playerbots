//! Helpers shared by the unit tests.

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A scratch directory that is removed when dropped. It lives below `$AA_TEST_TMP` when that is
/// set (a roomier volume), else in the OS temp dir.
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
    let base = match std::env::var_os("AA_TEST_TMP") {
        Some(root) => PathBuf::from(root).join("tmp-test"),
        None => std::env::temp_dir().join("playerbots-test"),
    };
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the test directory");
    TempDir(dir)
}
