//! Readers for the client's `.bai` navigation files: `netmission*.bai` (the node / link graph,
//! "CGraph" v54 / v55) and `vertsmission*.bai` (the triangle corner vertices, v1 / v2).
//!
//! Layouts (little endian) follow the AAEmu Rust server's `aaemu-game/src/systems/ai/geodata/bai.rs`
//! (GPL-3.0-or-later), which was checked against the 10.x engine's readers; they are re-read here
//! over plain byte slices with every size validated up front, so damaged data is an error and
//! never a panic:
//!
//! ```text
//! net:   i32 version | f32[3] bbox min | f32[3] bbox max | i32 nodeCount
//!        | nodeCount x 60-byte node | i32 linkCount | linkCount x 44-byte link
//! node:  +0 i32 id | +4 f32[3] dir | +16 f32[3] up | +28 f32[3] position (centroid, file local)
//!        | +40 i32 index | +44 i32[3] triangle corner indices into the verts file (-1 = none)
//!        | +56 u16 navType (2 triangle, 4 waypoint, 1 dummy) | +58 u8 flags | +59 u8
//! link:  +0 u32 source id | +4 u32 target id | +8 f32[3] edge middle | +20 f32 max pass radius
//!        (-1 = blocked) | +24 f32 exposure | +28 f32[3] unread | +40 u8 edge corner a
//!        | +41 u8 edge corner b | +42 u8 flags | +43 u8
//! verts: i32 version | i32 count | count x 32-byte vertex (f32[3] position, f32[3] dir,
//!        f32 radius, 4 bytes)
//! ```

use anyhow::{bail, ensure};

pub const NODE_SIZE: usize = 60;
pub const LINK_SIZE: usize = 44;
pub const VERT_SIZE: usize = 32;

pub const NAV_TYPE_TRIANGLE: u16 = 2;
pub const NAV_TYPE_WAYPOINT: u16 = 4;

/// A node as stored in the file (positions are file local).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawNode {
    pub id: i32,
    pub pos: [f32; 3],
    /// Corner indices into the verts file, -1 = none.
    pub tri: [i32; 3],
    pub nav_type: u16,
    pub flags: u8,
}

/// A directed link as stored in the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawLink {
    /// Node ids (not indices).
    pub src: u32,
    pub dst: u32,
    /// The widest agent radius that may pass; -1 = blocked.
    pub radius: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetFile {
    pub version: i32,
    pub nodes: Vec<RawNode>,
    pub links: Vec<RawLink>,
    /// Bytes after the last link (0 = the layout fits).
    pub trailing: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VertFile {
    pub version: i32,
    pub verts: Vec<[f32; 3]>,
}

fn i32_at(d: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn f32_at(d: &[u8], o: usize) -> f32 {
    f32::from_bits(u32_at(d, o))
}

/// Reads a `netmission*.bai`. The 1.2 pak has v55 files and, in 2 299 stale sectors, v54 files
/// with the same node / link layout.
pub fn parse_net(d: &[u8]) -> anyhow::Result<NetFile> {
    ensure!(d.len() >= 36, "net file too short ({} bytes)", d.len());
    let version = i32_at(d, 0);
    if version != 54 && version != 55 {
        bail!("unsupported net file version {version} (expected 54 or 55)");
    }
    let count = i32_at(d, 28);
    ensure!(count >= 0, "negative node count {count}");
    let count = count as usize;
    let nodes_end = count
        .checked_mul(NODE_SIZE)
        .and_then(|b| b.checked_add(32))
        .filter(|&e| e + 4 <= d.len());
    let Some(nodes_end) = nodes_end else {
        bail!("{count} nodes do not fit in {} bytes", d.len());
    };
    let mut nodes = Vec::with_capacity(count);
    for i in 0..count {
        let o = 32 + i * NODE_SIZE;
        nodes.push(RawNode {
            id: i32_at(d, o),
            pos: [f32_at(d, o + 28), f32_at(d, o + 32), f32_at(d, o + 36)],
            tri: [i32_at(d, o + 44), i32_at(d, o + 48), i32_at(d, o + 52)],
            nav_type: u16::from_le_bytes([d[o + 56], d[o + 57]]),
            flags: d[o + 58],
        });
    }
    let lcount = i32_at(d, nodes_end);
    ensure!(lcount >= 0, "negative link count {lcount}");
    let lcount = lcount as usize;
    let links_end = lcount
        .checked_mul(LINK_SIZE)
        .and_then(|b| b.checked_add(nodes_end + 4))
        .filter(|&e| e <= d.len());
    let Some(links_end) = links_end else {
        bail!(
            "{lcount} links do not fit in the {} bytes after the nodes",
            d.len() - nodes_end - 4
        );
    };
    let mut links = Vec::with_capacity(lcount);
    for i in 0..lcount {
        let o = nodes_end + 4 + i * LINK_SIZE;
        links.push(RawLink {
            src: u32_at(d, o),
            dst: u32_at(d, o + 4),
            radius: f32_at(d, o + 20),
        });
    }
    Ok(NetFile {
        version,
        nodes,
        links,
        trailing: d.len() - links_end,
    })
}

/// Reads a `vertsmission*.bai` (v2; the stale 1.2 sectors have v1 with the same records).
pub fn parse_verts(d: &[u8]) -> anyhow::Result<VertFile> {
    ensure!(d.len() >= 8, "verts file too short ({} bytes)", d.len());
    let version = i32_at(d, 0);
    if version != 1 && version != 2 {
        bail!("unsupported verts file version {version} (expected 1 or 2)");
    }
    let count = i32_at(d, 4);
    ensure!(count >= 0, "negative vertex count {count}");
    let count = count as usize;
    let fits = count
        .checked_mul(VERT_SIZE)
        .and_then(|b| b.checked_add(8))
        .is_some_and(|e| e <= d.len());
    if !fits {
        bail!("{count} vertices do not fit in {} bytes", d.len());
    }
    let verts = (0..count)
        .map(|i| {
            let o = 8 + i * VERT_SIZE;
            [f32_at(d, o), f32_at(d, o + 4), f32_at(d, o + 8)]
        })
        .collect();
    Ok(VertFile { version, verts })
}

/// Synthetic `.bai` bytes for tests.
#[cfg(test)]
pub(crate) mod testfiles {
    #[derive(Default)]
    pub struct W(pub Vec<u8>);

    impl W {
        pub fn i32(&mut self, v: i32) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        pub fn u32(&mut self, v: u32) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        pub fn f32(&mut self, v: f32) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        pub fn u8(&mut self, v: u8) -> &mut Self {
            self.0.push(v);
            self
        }
        pub fn v3(&mut self, v: [f32; 3]) -> &mut Self {
            self.f32(v[0]).f32(v[1]).f32(v[2])
        }
    }

    pub struct TNode {
        pub id: i32,
        pub pos: [f32; 3],
        pub tri: [i32; 3],
        pub nav_type: u16,
    }

    impl TNode {
        pub fn tri(id: i32, pos: [f32; 3], tri: [i32; 3]) -> Self {
            TNode {
                id,
                pos,
                tri,
                nav_type: 2,
            }
        }

        pub fn waypoint(id: i32, pos: [f32; 3]) -> Self {
            TNode {
                id,
                pos,
                tri: [-1; 3],
                nav_type: 4,
            }
        }
    }

    pub struct TLink {
        pub src: u32,
        pub dst: u32,
        pub radius: f32,
    }

    impl TLink {
        pub fn new(src: u32, dst: u32, radius: f32) -> Self {
            TLink { src, dst, radius }
        }
    }

    pub fn net(version: i32, nodes: &[TNode], links: &[TLink]) -> Vec<u8> {
        let mut w = W::default();
        w.i32(version).v3([0.0; 3]).v3([8192.0, 8192.0, 0.0]);
        w.i32(nodes.len() as i32);
        for n in nodes {
            w.i32(n.id)
                .v3([0.0; 3])
                .v3([0.0; 3])
                .v3(n.pos)
                .i32(0)
                .i32(n.tri[0])
                .i32(n.tri[1])
                .i32(n.tri[2])
                .u8(n.nav_type as u8)
                .u8((n.nav_type >> 8) as u8)
                .u8(0)
                .u8(0);
        }
        w.i32(links.len() as i32);
        for l in links {
            w.u32(l.src)
                .u32(l.dst)
                .v3([0.0; 3])
                .f32(l.radius)
                .f32(0.5)
                .f32(0.0)
                .f32(0.0)
                .f32(327.66)
                .u8(0)
                .u8(1)
                .u8(9)
                .u8(49);
        }
        w.0
    }

    pub fn verts(version: i32, points: &[[f32; 3]]) -> Vec<u8> {
        let mut w = W::default();
        w.i32(version).i32(points.len() as i32);
        for p in points {
            w.v3(*p).v3([0.0; 3]).f32(0.0).u8(1).u8(0).u8(0).u8(0);
        }
        w.0
    }
}

#[cfg(test)]
mod tests {
    use super::testfiles::*;
    use super::*;

    #[test]
    fn reads_nodes_links_and_verts() {
        let bytes = net(
            55,
            &[
                TNode::tri(7, [1.0, 2.0, 3.0], [0, 1, 2]),
                TNode::waypoint(9, [4.0, 5.0, 6.0]),
            ],
            &[TLink::new(7, 9, 0.75), TLink::new(9, 7, -1.0)],
        );
        let n = parse_net(&bytes).unwrap();
        assert_eq!(n.version, 55);
        assert_eq!(n.trailing, 0);
        assert_eq!(n.nodes.len(), 2);
        assert_eq!(n.nodes[0].id, 7);
        assert_eq!(n.nodes[0].pos, [1.0, 2.0, 3.0]);
        assert_eq!(n.nodes[0].tri, [0, 1, 2]);
        assert_eq!(n.nodes[0].nav_type, NAV_TYPE_TRIANGLE);
        assert_eq!(n.nodes[1].nav_type, NAV_TYPE_WAYPOINT);
        assert_eq!(n.links.len(), 2);
        assert_eq!(
            (n.links[0].src, n.links[0].dst, n.links[0].radius),
            (7, 9, 0.75)
        );
        assert_eq!(n.links[1].radius, -1.0);
        // v54 is accepted, other versions are not
        assert_eq!(parse_net(&net(54, &[], &[])).unwrap().version, 54);
        assert!(parse_net(&net(53, &[], &[])).is_err());

        let v = parse_verts(&verts(2, &[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]])).unwrap();
        assert_eq!(v.verts, vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        assert_eq!(parse_verts(&verts(1, &[])).unwrap().verts.len(), 0);
        assert!(parse_verts(&verts(3, &[])).is_err());
    }

    #[test]
    fn damaged_files_are_errors_not_panics() {
        let bytes = net(
            55,
            &[
                TNode::tri(1, [0.0; 3], [0, 1, 2]),
                TNode::tri(2, [1.0; 3], [0, 1, 2]),
            ],
            &[TLink::new(1, 2, 1.0)],
        );
        for cut in 0..bytes.len() {
            assert!(parse_net(&bytes[..cut]).is_err(), "cut {cut} parsed");
        }
        // absurd counts
        let mut b = bytes.clone();
        b[28..32].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(parse_net(&b).is_err());
        b[28..32].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(parse_net(&b).is_err());
        let mut b = bytes.clone();
        let lc = 32 + 2 * NODE_SIZE;
        b[lc..lc + 4].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(parse_net(&b).is_err());
        let vb = verts(2, &[[0.0; 3]]);
        for cut in 0..vb.len() {
            assert!(parse_verts(&vb[..cut]).is_err(), "verts cut {cut} parsed");
        }
        let mut b = vb.clone();
        b[4..8].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(parse_verts(&b).is_err());
        // trailing bytes are tolerated and counted
        let mut b = bytes.clone();
        b.extend_from_slice(&[0; 5]);
        assert_eq!(parse_net(&b).unwrap().trailing, 5);
        // random garbage never panics
        let mut seed = 99u64;
        let mut rnd = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 33
        };
        for _ in 0..200 {
            let len = rnd() as usize % 300;
            let junk: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
            let _ = parse_net(&junk);
            let _ = parse_verts(&junk);
        }
    }
}
