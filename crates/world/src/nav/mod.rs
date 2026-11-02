//! Navigation mesh path finding for bots.
//!
//! `aa-bake` converts the client's `.bai` navmesh of a world into one compact, memory-mappable
//! `nav_<world>.bin` (format: `docs/DATA-FORMATS.md`, writer side: [`format::NavData`]).
//! [`NavMesh`] maps it read-only and answers, from any thread without locks:
//!
//! * [`NavMesh::locate`]: the triangle (or waypoint) under a position, multi-floor aware;
//! * [`NavMesh::find_path`]: a weighted A* over the triangle graph smoothed with the funnel
//!   algorithm, reusing per-thread scratch memory ([`Scratch`]);
//! * [`NavMesh::component_of`] / [`NavMesh::may_reach`]: connectivity answered without a search.
//!
//! [`NavSet`] (in [`crate::baked::Baked`]) holds the main world's mesh and opens instance worlds'
//! meshes on first use.

pub mod format;
mod funnel;
mod mesh;
mod path;
mod search;
mod set;

#[cfg(test)]
pub(crate) mod testmesh;
#[cfg(test)]
mod tests;

pub use format::NavData;
pub use mesh::{Located, NavMesh, NodeRef};
pub use search::{
    thread_scratch_bytes, Path, PathQuery, PathResult, Scratch, DEFAULT_AGENT_RADIUS,
    DEFAULT_MAX_EXPANSIONS, DEFAULT_ROAD_BIAS, DEFAULT_SNAP_DIST, DEFAULT_WEIGHT,
    MAX_EXPANSIONS_LIMIT,
};
pub use set::NavSet;
