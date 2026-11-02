//! The navigation meshes of a baked data directory: the main world's, plus instance worlds'
//! opened on first use.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rustc_hash::FxHashMap;

use super::NavMesh;
use crate::World;

/// File name of a world's mesh inside the baked directory.
pub fn nav_file_name(world: &str) -> String {
    format!("nav_{world}.bin")
}

/// World names become part of a file name: only plain identifiers are accepted.
fn plain_name(world: &str) -> bool {
    !world.is_empty()
        && world.len() <= 96
        && world
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The main world's mesh and a lazily filled cache of the other worlds'.
#[derive(Default)]
pub struct NavSet {
    dir: Option<PathBuf>,
    main_world: String,
    main: Option<Arc<NavMesh>>,
    /// World name -> mesh (`None`: the file is missing or unreadable; warned once).
    others: RwLock<FxHashMap<String, Option<Arc<NavMesh>>>>,
}

fn open_mesh(path: &Path, what: &str) -> Option<Arc<NavMesh>> {
    if !path.is_file() {
        tracing::warn!("baked {what} not found: {}", path.display());
        return None;
    }
    match NavMesh::open(path) {
        Ok(m) => Some(Arc::new(m)),
        Err(e) => {
            tracing::warn!("skipping malformed baked {what} {}: {e:#}", path.display());
            None
        }
    }
}

impl NavSet {
    /// Opens `<dir>/nav_<world>.bin` (missing or malformed: a warning and no mesh).
    pub fn load(dir: &Path, world: &str) -> NavSet {
        let main = plain_name(world)
            .then(|| open_mesh(&dir.join(nav_file_name(world)), "navigation mesh"))
            .flatten();
        NavSet {
            dir: Some(dir.to_path_buf()),
            main_world: world.to_string(),
            main,
            others: RwLock::new(FxHashMap::default()),
        }
    }

    /// The main world's mesh.
    pub fn main(&self) -> Option<&NavMesh> {
        self.main.as_deref()
    }

    /// The mesh of `world`: the main world's, or an instance world's opened on the first call
    /// (the file is read outside the lock; afterwards a lookup is a shared read lock and an
    /// `Arc` clone, so callers may also keep the `Arc`).
    pub fn get(&self, world: &str) -> Option<Arc<NavMesh>> {
        if world == self.main_world {
            return self.main.clone();
        }
        if let Some(hit) = self.read().get(world) {
            return hit.clone();
        }
        let dir = self.dir.as_ref()?;
        if !plain_name(world) {
            return None;
        }
        let opened = open_mesh(&dir.join(nav_file_name(world)), "instance navigation mesh");
        // two threads may both have opened it: keep the first
        let mut map = self.others.write().unwrap_or_else(|e| e.into_inner());
        map.entry(world.to_string()).or_insert(opened).clone()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, FxHashMap<String, Option<Arc<NavMesh>>>> {
        self.others.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Number of instance meshes opened so far.
    pub fn opened_instances(&self) -> usize {
        self.read().values().filter(|m| m.is_some()).count()
    }
}

impl World {
    /// The navigation mesh of the main world (`None` without `nav_<world>.bin` in the baked
    /// data).
    pub fn nav(&self) -> Option<&NavMesh> {
        self.baked().navs.main()
    }

    /// The navigation mesh of any world by name: the main world's, or an instance world's,
    /// opened from the baked directory on the first request.
    pub fn nav_of(&self, world_name: &str) -> Option<Arc<NavMesh>> {
        self.baked().navs.get(world_name)
    }
}
