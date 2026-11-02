//! Minimal DDS decoder: mip 0 of DXT1 / DXT3 / DXT5 compressed and 32-bit uncompressed images
//! to RGBA8. The client's map images are DXT1 with a mipmap chain.

use anyhow::{bail, ensure};

/// A decoded image, RGBA8 row-major, top row first.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

const FOURCC_DXT1: u32 = u32::from_le_bytes(*b"DXT1");
const FOURCC_DXT3: u32 = u32::from_le_bytes(*b"DXT3");
const FOURCC_DXT5: u32 = u32::from_le_bytes(*b"DXT5");
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDS_HEADER_LEN: usize = 128;

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// RGB565 -> RGB888 (bit replication).
fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 0x1F) as u32;
    let g = ((c >> 5) & 0x3F) as u32;
    let b = (c & 0x1F) as u32;
    [
        ((r << 3) | (r >> 2)) as u8,
        ((g << 2) | (g >> 4)) as u8,
        ((b << 3) | (b >> 2)) as u8,
    ]
}

/// Decodes the colour half of a DXT block (8 bytes) into 16 RGBA pixels. `opaque_mode` forces the
/// four-colour mode (DXT3/DXT5), otherwise `c0 <= c1` selects the three-colour + transparent mode.
fn decode_color_block(b: &[u8], opaque_mode: bool) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let p0 = rgb565(c0);
    let p1 = rgb565(c1);
    let mut pal = [[0u8; 4]; 4];
    pal[0] = [p0[0], p0[1], p0[2], 255];
    pal[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 || opaque_mode {
        for i in 0..3 {
            pal[2][i] = ((2 * p0[i] as u32 + p1[i] as u32) / 3) as u8;
            pal[3][i] = ((p0[i] as u32 + 2 * p1[i] as u32) / 3) as u8;
        }
        pal[2][3] = 255;
        pal[3][3] = 255;
    } else {
        for i in 0..3 {
            pal[2][i] = ((p0[i] as u32 + p1[i] as u32) / 2) as u8;
        }
        pal[2][3] = 255;
        pal[3] = [0, 0, 0, 0];
    }
    let bits = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let mut out = [[0u8; 4]; 16];
    for (i, px) in out.iter_mut().enumerate() {
        *px = pal[((bits >> (2 * i)) & 3) as usize];
    }
    out
}

/// DXT5 alpha block (8 bytes) -> 16 alpha values.
fn decode_alpha_block(b: &[u8]) -> [u8; 16] {
    let a0 = b[0] as u32;
    let a1 = b[1] as u32;
    let mut pal = [0u8; 8];
    pal[0] = a0 as u8;
    pal[1] = a1 as u8;
    if a0 > a1 {
        for i in 1..7u32 {
            pal[(i + 1) as usize] = (((7 - i) * a0 + i * a1) / 7) as u8;
        }
    } else {
        for i in 1..5u32 {
            pal[(i + 1) as usize] = (((5 - i) * a0 + i * a1) / 5) as u8;
        }
        pal[6] = 0;
        pal[7] = 255;
    }
    let mut bits = 0u64;
    for (i, byte) in b[2..8].iter().enumerate() {
        bits |= (*byte as u64) << (8 * i);
    }
    let mut out = [0u8; 16];
    for (i, a) in out.iter_mut().enumerate() {
        *a = pal[((bits >> (3 * i)) & 7) as usize];
    }
    out
}

/// Decodes mip level 0 of a DDS file.
pub fn decode_dds(data: &[u8]) -> anyhow::Result<Rgba> {
    ensure!(data.len() >= DDS_HEADER_LEN, "DDS too short");
    ensure!(&data[0..4] == b"DDS ", "not a DDS file");
    let height = u32_at(data, 12);
    let width = u32_at(data, 16);
    ensure!(
        width > 0 && height > 0 && width <= 16384 && height <= 16384,
        "bad DDS size {width}x{height}"
    );
    let pf_flags = u32_at(data, 80);
    let fourcc = u32_at(data, 84);
    let body = &data[DDS_HEADER_LEN..];
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];

    if pf_flags & DDPF_FOURCC != 0 {
        let (block_bytes, kind) = match fourcc {
            FOURCC_DXT1 => (8usize, 1),
            FOURCC_DXT3 => (16, 3),
            FOURCC_DXT5 => (16, 5),
            other => bail!("unsupported DDS fourcc {:?}", other.to_le_bytes()),
        };
        let bw = w.div_ceil(4);
        let bh = h.div_ceil(4);
        ensure!(
            body.len() >= bw * bh * block_bytes,
            "DDS mip 0 truncated ({} bytes, need {})",
            body.len(),
            bw * bh * block_bytes
        );
        for by in 0..bh {
            for bx in 0..bw {
                let b = &body[(by * bw + bx) * block_bytes..][..block_bytes];
                let (color_block, alpha): (&[u8], [u8; 16]) = match kind {
                    1 => (b, [255; 16]),
                    3 => {
                        let mut a = [0u8; 16];
                        for i in 0..16 {
                            let nibble = (b[i / 2] >> (4 * (i % 2))) & 0xF;
                            a[i] = nibble * 17;
                        }
                        (&b[8..], a)
                    }
                    _ => (&b[8..], decode_alpha_block(&b[..8])),
                };
                let px = decode_color_block(color_block, kind != 1);
                for (i, p) in px.iter().enumerate() {
                    let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                    if x < w && y < h {
                        let o = (y * w + x) * 4;
                        out[o..o + 3].copy_from_slice(&p[..3]);
                        out[o + 3] = if kind == 1 { p[3] } else { alpha[i] };
                    }
                }
            }
        }
    } else if pf_flags & DDPF_RGB != 0 && u32_at(data, 88) == 32 {
        ensure!(body.len() >= w * h * 4, "DDS mip 0 truncated");
        let masks = [
            u32_at(data, 92),
            u32_at(data, 96),
            u32_at(data, 100),
            u32_at(data, 104),
        ];
        let shifts: Vec<Option<u32>> = masks
            .iter()
            .map(|m| (*m == 0xFF << m.trailing_zeros().min(24)).then(|| m.trailing_zeros()))
            .collect();
        for i in 0..w * h {
            let v = u32_at(body, i * 4);
            for c in 0..4 {
                out[i * 4 + c] = match shifts[c] {
                    Some(s) => (v >> s) as u8,
                    None if c == 3 => 255,
                    None => 0,
                };
            }
        }
    } else {
        bail!("unsupported DDS pixel format (flags {pf_flags:#x})");
    }
    Ok(Rgba {
        width,
        height,
        data: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dds(width: u32, height: u32, fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; DDS_HEADER_LEN];
        d[0..4].copy_from_slice(b"DDS ");
        d[4..8].copy_from_slice(&124u32.to_le_bytes());
        d[12..16].copy_from_slice(&height.to_le_bytes());
        d[16..20].copy_from_slice(&width.to_le_bytes());
        d[76..80].copy_from_slice(&32u32.to_le_bytes());
        d[80..84].copy_from_slice(&DDPF_FOURCC.to_le_bytes());
        d[84..88].copy_from_slice(fourcc);
        d.extend_from_slice(body);
        d
    }

    #[test]
    fn rgb565_expands_to_full_range() {
        assert_eq!(rgb565(0xFFFF), [255, 255, 255]);
        assert_eq!(rgb565(0x0000), [0, 0, 0]);
        assert_eq!(rgb565(0xF800), [255, 0, 0]);
        assert_eq!(rgb565(0x001F), [0, 0, 255]);
    }

    #[test]
    fn dxt1_four_color_block() {
        // c0 = red, c1 = blue (c0 > c1), indices: pixel i uses index i % 4
        let mut body = vec![0x00, 0xF8, 0x1F, 0x00];
        let bits: u32 = (0..16).fold(0, |acc, i| acc | (((i % 4) as u32) << (2 * i)));
        body.extend_from_slice(&bits.to_le_bytes());
        let img = decode_dds(&dds(4, 4, b"DXT1", &body)).unwrap();
        assert_eq!((img.width, img.height), (4, 4));
        let px = |x: usize, y: usize| &img.data[(y * 4 + x) * 4..][..4];
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
        assert_eq!(px(1, 0), [0, 0, 255, 255]);
        assert_eq!(px(2, 0), [170, 0, 85, 255]);
        assert_eq!(px(3, 0), [85, 0, 170, 255]);
        assert_eq!(px(0, 3), [255, 0, 0, 255]);
    }

    #[test]
    fn dxt1_three_color_block_has_transparent_index() {
        // c0 = blue < c1 = red -> 3-colour mode, index 3 transparent
        let mut body = vec![0x1F, 0x00, 0x00, 0xF8];
        body.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let img = decode_dds(&dds(4, 4, b"DXT1", &body)).unwrap();
        assert_eq!(&img.data[0..4], [0, 0, 0, 0]);
        let mut body = vec![0x1F, 0x00, 0x00, 0xF8];
        body.extend_from_slice(&0xAAAA_AAAAu32.to_le_bytes()); // index 2 = midpoint
        let img = decode_dds(&dds(4, 4, b"DXT1", &body)).unwrap();
        assert_eq!(&img.data[0..4], [127, 0, 127, 255]);
    }

    #[test]
    fn dxt5_alpha_and_cropping() {
        // 6x2 image -> 2x1 blocks; both blocks: colour white, alpha a0=255 a1=0, all index 1 (=a1 → 0)
        let mut block = vec![255u8, 0];
        let alpha_bits: u64 = (0..16).fold(0u64, |acc, i| acc | (1u64 << (3 * i)));
        block.extend_from_slice(&alpha_bits.to_le_bytes()[..6]);
        block.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0]);
        let mut body = block.clone();
        body.extend_from_slice(&block);
        let img = decode_dds(&dds(6, 2, b"DXT5", &body)).unwrap();
        assert_eq!(img.data.len(), 6 * 2 * 4);
        assert_eq!(&img.data[0..4], [255, 255, 255, 0]);
        assert_eq!(&img.data[5 * 4..6 * 4], [255, 255, 255, 0]);
    }

    #[test]
    fn dxt5_interpolated_alpha() {
        // a0 = 255 > a1 = 0: index 2 = (6*255 + 0)/7 = 218; index 7 = (1*255)/7 = 36
        let a = decode_alpha_block(&[255, 0, 0b0001_0010, 0, 0, 0, 0, 0]); // pixels 0 and 1 use index 2
        assert_eq!(a[0], 218);
        assert_eq!(a[1], 218);
        let mut b = [0u8; 8];
        b[0] = 255;
        b[1] = 0;
        b[2] = 0b0011_1111; // pixels 0 and 1 use index 7
        let a = decode_alpha_block(&b);
        assert_eq!(a[0], 36);
        assert_eq!(a[1], 36);
        // a0 <= a1: 6-value mode with 0 and 255 at the end
        let a = decode_alpha_block(&[0, 255, 0b0011_1000, 0b0000_0000, 0, 0, 0, 0]);
        assert_eq!(a[0], 0);
        assert_eq!(a[1], 255); // index 7
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_dds(b"nope").is_err());
        assert!(decode_dds(&dds(4, 4, b"ATI2", &[0; 16])).is_err());
        assert!(decode_dds(&dds(8, 8, b"DXT1", &[0; 8])).is_err());
    }
}
