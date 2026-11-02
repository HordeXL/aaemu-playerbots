//! World map images: the client's painted map (DDS) with its world rectangle, a hillshaded relief
//! rendered from the height grid, and the debug overlay used to verify the calibration.

use std::io::BufWriter;
use std::path::Path;

use aa_world::baked::WorldRect;
use anyhow::Context;

use crate::dds::Rgba;
use crate::heights::HeightGrid;

/// An RGB8 canvas, row-major, top row first.
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32, fill: [u8; 3]) -> Canvas {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for _ in 0..width as usize * height as usize {
            rgb.extend_from_slice(&fill);
        }
        Canvas { width, height, rgb }
    }

    /// Opaque copy of a decoded image (alpha composited over `bg`).
    pub fn from_rgba(img: &Rgba, bg: [u8; 3]) -> Canvas {
        let mut rgb = Vec::with_capacity(img.data.len() / 4 * 3);
        for i in 0..img.data.len() / 4 {
            let px = &img.data[i * 4..i * 4 + 4];
            let a = px[3] as u32;
            for c in 0..3 {
                rgb.push(((px[c] as u32 * a + bg[c] as u32 * (255 - a)) / 255) as u8);
            }
        }
        Canvas {
            width: img.width,
            height: img.height,
            rgb,
        }
    }

    pub fn put(&mut self, x: i64, y: i64, color: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height {
            let o = (y as usize * self.width as usize + x as usize) * 3;
            self.rgb[o..o + 3].copy_from_slice(&color);
        }
    }

    pub fn get(&self, x: u32, y: u32) -> [u8; 3] {
        let o = (y as usize * self.width as usize + x as usize) * 3;
        [self.rgb[o], self.rgb[o + 1], self.rgb[o + 2]]
    }

    /// Filled circle with a one pixel outline.
    pub fn marker(&mut self, cx: f64, cy: f64, radius: f64, fill: [u8; 3], outline: [u8; 3]) {
        let r = radius.ceil() as i64 + 1;
        for dy in -r..=r {
            for dx in -r..=r {
                let d = ((dx * dx + dy * dy) as f64).sqrt();
                if d <= radius {
                    self.put(cx.round() as i64 + dx, cy.round() as i64 + dy, fill);
                } else if d <= radius + 1.5 {
                    self.put(cx.round() as i64 + dx, cy.round() as i64 + dy, outline);
                }
            }
        }
    }

    /// Axis-aligned rectangle outline (inclusive pixel coordinates).
    pub fn rect_outline(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, color: [u8; 3]) {
        for x in x0..=x1 {
            self.put(x, y0, color);
            self.put(x, y1, color);
        }
        for y in y0..=y1 {
            self.put(x0, y, color);
            self.put(x1, y, color);
        }
    }

    pub fn write_png(&self, path: &Path) -> anyhow::Result<()> {
        let file =
            std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut enc = png::Encoder::new(BufWriter::new(file), self.width, self.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&self.rgb)?;
        Ok(())
    }
}

/// Writes a decoded RGBA image as PNG.
pub fn write_rgba_png(img: &Rgba, path: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&img.data)?;
    Ok(())
}

/// Pixel position of world (x, y) in an image covering `rect` (pixel (0,0) = top-left corner =
/// world (min_x, max_y)).
pub fn world_to_pixel(rect: &WorldRect, width: u32, height: u32, x: f32, y: f32) -> (f64, f64) {
    let u = f64::from(x - rect.min_x) / f64::from(rect.max_x - rect.min_x);
    let v = 1.0 - f64::from(y - rect.min_y) / f64::from(rect.max_y - rect.min_y);
    (u * f64::from(width), v * f64::from(height))
}

/// A hypsometric colour ramp for land.
fn land_color(h: f32, ocean: f32) -> [f32; 3] {
    const STOPS: [(f32, [f32; 3]); 7] = [
        (0.0, [78.0, 134.0, 74.0]),
        (40.0, [116.0, 158.0, 78.0]),
        (150.0, [190.0, 180.0, 112.0]),
        (350.0, [166.0, 134.0, 92.0]),
        (700.0, [138.0, 112.0, 98.0]),
        (1000.0, [200.0, 196.0, 196.0]),
        (1200.0, [250.0, 250.0, 250.0]),
    ];
    let t = h - ocean;
    if t <= STOPS[0].0 {
        return STOPS[0].1;
    }
    for w in STOPS.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.0 {
            let f = (t - a.0) / (b.0 - a.0);
            return [
                a.1[0] + (b.1[0] - a.1[0]) * f,
                a.1[1] + (b.1[1] - a.1[1]) * f,
                a.1[2] + (b.1[2] - a.1[2]) * f,
            ];
        }
    }
    STOPS[STOPS.len() - 1].1
}

/// A hillshaded relief of the height grid: water (below `ocean`) in blue, about
/// `target_width` pixels wide. Returns the image and the exact world rectangle it covers.
pub fn render_relief(grid: &HeightGrid, ocean: f32, target_width: u32) -> (Canvas, WorldRect) {
    let hdr = &grid.header;
    let (gw, gh) = (hdr.width as usize, hdr.height as usize);
    let factor = ((gw as f32 / target_width as f32).round() as usize).max(1);
    let (w, h) = (gw / factor, gh / factor);
    // Block averages (y up); void samples (raw 0) are excluded, NaN marks an all-void block.
    let mut elev = vec![f32::NAN; w * h];
    for by in 0..h {
        for bx in 0..w {
            let (mut sum, mut n) = (0f32, 0u32);
            for sy in 0..factor {
                let row = (by * factor + sy) * gw + bx * factor;
                for &raw in &grid.samples[row..row + factor] {
                    if raw != 0 {
                        sum += raw as f32;
                        n += 1;
                    }
                }
            }
            if n > 0 {
                elev[by * w + bx] = sum / n as f32 * hdr.scale + hdr.offset;
            }
        }
    }
    let cell = factor as f32 * hdr.step;
    let at = |x: i64, y: i64, fallback: f32| -> f32 {
        let x = x.clamp(0, w as i64 - 1) as usize;
        let y = y.clamp(0, h as i64 - 1) as usize;
        let v = elev[y * w + x];
        if v.is_nan() {
            fallback
        } else {
            v
        }
    };
    // Light from the north-west, 45 degrees up.
    let az = 315f32.to_radians();
    let alt = 45f32.to_radians();
    let light = [az.sin() * alt.cos(), az.cos() * alt.cos(), alt.sin()];
    let z_factor = 2.0;
    let mut canvas = Canvas::new(w as u32, h as u32, [0, 0, 0]);
    for py in 0..h {
        let by = h - 1 - py; // image top = north
        for px in 0..w {
            let e = elev[by * w + px];
            let color = if e.is_nan() {
                [18.0, 22.0, 38.0]
            } else {
                let (x, y) = (px as i64, by as i64);
                let dzdx = (at(x + 1, y, e) - at(x - 1, y, e)) / (2.0 * cell) * z_factor;
                let dzdy = (at(x, y + 1, e) - at(x, y - 1, e)) / (2.0 * cell) * z_factor;
                let n = [-dzdx, -dzdy, 1.0];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                let shade = ((n[0] * light[0] + n[1] * light[1] + n[2] * light[2]) / len).max(0.0);
                if e < ocean {
                    // water: deeper is darker; the sea floor relief shows faintly
                    let depth = ((ocean - e) / 200.0).clamp(0.0, 1.0);
                    let base = [
                        70.0 - 55.0 * depth,
                        140.0 - 85.0 * depth,
                        200.0 - 80.0 * depth,
                    ];
                    let k = 0.85 + 0.3 * shade;
                    [base[0] * k, base[1] * k, base[2] * k]
                } else {
                    let base = land_color(e, ocean);
                    let k = 0.45 + 0.95 * shade;
                    [base[0] * k, base[1] * k, base[2] * k]
                }
            };
            canvas.put(
                px as i64,
                py as i64,
                [
                    color[0].clamp(0.0, 255.0) as u8,
                    color[1].clamp(0.0, 255.0) as u8,
                    color[2].clamp(0.0, 255.0) as u8,
                ],
            );
        }
    }
    let rect = WorldRect {
        min_x: hdr.origin_x,
        min_y: hdr.origin_y,
        max_x: hdr.origin_x + (w * factor) as f32 * hdr.step,
        max_y: hdr.origin_y + (h * factor) as f32 * hdr.step,
    };
    (canvas, rect)
}

/// A marker to draw on the debug overlay.
pub struct DebugMarker {
    pub x: f32,
    pub y: f32,
    pub color: [u8; 3],
}

/// The painted map with the land/sea boundary of the height grid (red, at `ocean`), optional zone
/// group rectangles (yellow) and the markers, drawn using `rect` as the calibration under test.
pub fn debug_overlay(
    painted: &Rgba,
    rect: &WorldRect,
    grid: &HeightGrid,
    ocean: f32,
    zone_rects: &[WorldRect],
    markers: &[DebugMarker],
) -> Canvas {
    let mut canvas = Canvas::from_rgba(painted, [255, 255, 255]);
    let (w, h) = (canvas.width as usize, canvas.height as usize);
    // land mask in image space
    let mut land = vec![false; w * h];
    for py in 0..h {
        for px in 0..w {
            let x = rect.min_x + (px as f32 + 0.5) / w as f32 * (rect.max_x - rect.min_x);
            let y = rect.max_y - (py as f32 + 0.5) / h as f32 * (rect.max_y - rect.min_y);
            land[py * w + px] =
                grid.height(x, y).is_some_and(|z| z > ocean + 2.0) && nonvoid(grid, x, y);
        }
    }
    for py in 0..h {
        for px in 0..w {
            if !land[py * w + px] {
                continue;
            }
            let edge = px == 0
                || py == 0
                || px + 1 == w
                || py + 1 == h
                || !land[py * w + px - 1]
                || !land[py * w + px + 1]
                || !land[(py - 1) * w + px]
                || !land[(py + 1) * w + px];
            if edge {
                canvas.put(px as i64, py as i64, [220, 20, 20]);
            }
        }
    }
    for z in zone_rects {
        let (x0, y1) = world_to_pixel(rect, canvas.width, canvas.height, z.min_x, z.min_y);
        let (x1, y0) = world_to_pixel(rect, canvas.width, canvas.height, z.max_x, z.max_y);
        canvas.rect_outline(x0 as i64, y0 as i64, x1 as i64, y1 as i64, [230, 200, 0]);
    }
    for m in markers {
        let (px, py) = world_to_pixel(rect, canvas.width, canvas.height, m.x, m.y);
        canvas.marker(px, py, 6.0, m.color, [255, 255, 255]);
    }
    canvas
}

/// Whether the sample nearest to (x, y) holds terrain (raw > 0).
fn nonvoid(grid: &HeightGrid, x: f32, y: f32) -> bool {
    let h = &grid.header;
    let col = ((x - h.origin_x) / h.step).round();
    let row = ((y - h.origin_y) / h.step).round();
    if col < 0.0 || row < 0.0 {
        return false;
    }
    grid.raw(col as usize, row as usize).is_some_and(|r| r != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_world::terrain::HeightGridHeader;

    fn grid(w: u32, h: u32, f: impl Fn(u32, u32) -> f32) -> HeightGrid {
        let mut samples = Vec::new();
        for r in 0..h {
            for c in 0..w {
                samples.push((f(c, r) * 64.0) as u16);
            }
        }
        HeightGrid {
            header: HeightGridHeader {
                width: w,
                height: h,
                origin_x: 0.0,
                origin_y: 0.0,
                step: 4.0,
                scale: 1.0 / 64.0,
                offset: 0.0,
                flags: 1,
            },
            samples,
        }
    }

    #[test]
    fn world_to_pixel_corners() {
        let rect = WorldRect {
            min_x: -100.0,
            min_y: 0.0,
            max_x: 300.0,
            max_y: 200.0,
        };
        assert_eq!(world_to_pixel(&rect, 400, 200, -100.0, 200.0), (0.0, 0.0));
        assert_eq!(world_to_pixel(&rect, 400, 200, 300.0, 0.0), (400.0, 200.0));
        assert_eq!(
            world_to_pixel(&rect, 400, 200, 100.0, 100.0),
            (200.0, 100.0)
        );
    }

    #[test]
    fn relief_is_north_up_with_water_and_land() {
        // 8x8 grid at 4 m: west half sea (50 m), east half land (300 m); a void column at x = 7
        let g = grid(8, 8, |c, _| match c {
            7 => 0.0,
            0..=3 => 50.0,
            _ => 300.0,
        });
        let (img, rect) = render_relief(&g, 100.0, 4);
        assert_eq!((img.width, img.height), (4, 4));
        assert_eq!(
            rect,
            WorldRect {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 32.0,
                max_y: 32.0
            }
        );
        let west = img.get(0, 1);
        let east = img.get(2, 1);
        assert!(west[2] > west[0], "water is blue: {west:?}");
        assert!(
            east[1] > east[2] || east[0] > east[2],
            "land is not blue: {east:?}"
        );
        // north up: row 0 is the highest y; a left-to-right gradient must be identical per row
        assert_eq!(img.get(0, 0), img.get(0, 3));
    }

    #[test]
    fn overlay_marks_the_coast_and_markers() {
        let g = grid(40, 40, |c, _| if c < 20 { 50.0 } else { 200.0 });
        let painted = Rgba {
            width: 40,
            height: 40,
            data: vec![255; 40 * 40 * 4],
        };
        let rect = WorldRect {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 160.0,
            max_y: 160.0,
        };
        let img = debug_overlay(
            &painted,
            &rect,
            &g,
            100.0,
            &[],
            &[DebugMarker {
                x: 40.0,
                y: 120.0,
                color: [0, 0, 255],
            }],
        );
        // the bilinear blend puts the 100 m crossing just before x = 80 m: pixel 19 is the first
        // land column and carries the red coast line
        assert_eq!(img.get(19, 10), [220, 20, 20]);
        assert_eq!(
            img.get(25, 10),
            [255, 255, 255],
            "land interior is not drawn"
        );
        assert_eq!(img.get(5, 30), [255, 255, 255], "open sea is not drawn");
        // marker at world (40, 120) -> pixel (10, 10)
        assert_eq!(img.get(10, 10), [0, 0, 255]);
    }
}
