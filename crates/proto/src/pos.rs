//! Packed world positions and fixed-point coordinates.
// Vendored from AAEmu-1.2-Rust rust/crates/aaemu-commons/src/helpers.rs (GPL-3.0-or-later).

fn long_to_coord(v: i64) -> f32 {
    (v >> 32) as f32 / 4096.0
}

fn coord_to_long(v: f32) -> i64 {
    ((v * 4096.0) as i64) << 32
}

/// Decodes the 9-byte packed world position (x/y: 24-bit fixed point with sign bits in byte 8,
/// z: 22-bit fixed point).
pub fn decode_position(v: &[u8; 9]) -> (f32, f32, f32) {
    let raw = |a: u8, b: u8, c: u8| 8 * (i64::from(a) | i64::from(b) << 8 | i64::from(c) << 16);
    let mut x = raw(v[0], v[1], v[2]) << 32;
    let mut y = raw(v[3], v[4], v[5]) << 32;
    if v[8] & 0x80 != 0 {
        x = x.wrapping_neg();
    }
    if v[8] & 0x40 != 0 {
        y = y.wrapping_neg();
    }
    let z_raw = u64::from(v[6]) | u64::from(v[7]) << 8 | u64::from(v[8] & 0x3F) << 16;
    let z = z_raw as f64 * 0.000_000_238_418_58 * 4196.0 - 100.0;
    let z = ((z * 10_000.0).round_ties_even() / 10_000.0) as f32;
    (long_to_coord(x), long_to_coord(y), z)
}

pub fn encode_position(x: f32, y: f32, z: f32) -> [u8; 9] {
    let pack = |l: i64| {
        let pre = l >> 31;
        (pre ^ l.wrapping_add(pre).wrapping_add(i64::from(pre < 0))) >> 3
    };
    let rx = pack(coord_to_long(x));
    let ry = pack(coord_to_long(y));
    let rz = (f64::from((z + 100.0) / 4196.0 * 4_194_304.0) + 0.5).floor() as i64;

    let sign = (u8::from(y < 0.0) + 2 * u8::from(x < 0.0)) << 6;
    [
        (rx >> 32) as u8,
        (rx >> 40) as u8,
        (rx >> 48) as u8,
        (ry >> 32) as u8,
        (ry >> 40) as u8,
        (ry >> 48) as u8,
        rz as u8,
        (rz >> 8) as u8,
        (((rz >> 16) & 0x3F) as u8).wrapping_add(sign),
    ]
}

/// Fixed point (`<< 32`, 1/4096 units) to float, as used by `fx64` coordinates.
pub fn convert_long_to_coord(v: i64) -> f32 {
    long_to_coord(v)
}

/// Float to fixed point (`<< 32`, 1/4096 units), as used by `fx64` coordinates.
pub fn convert_coord_to_long(v: f32) -> i64 {
    coord_to_long(v)
}
