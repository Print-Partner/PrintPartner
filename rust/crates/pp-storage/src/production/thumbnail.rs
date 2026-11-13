//! A Source file's thumbnail (GRE-430 part 2a): STL bytes in, a 256 x 256
//! PNG out.
//!
//! The renderer reads the STL with the same reader and caps as Accept
//! ([`super::geometry`]). The camera is fixed: it looks at the part from the
//! front, right and above, with Z up as on a print bed, and fits the part's
//! outline to the image with a small margin. Every face is one neutral colour,
//! lit from above and in front, off the camera's axis so faces that point
//! different ways get different shades. Lighting is two-sided, so a face with
//! a reversed normal still shows. A depth buffer keeps the nearest face per
//! pixel. The background is transparent.
//!
//! The output depends only on the bytes and [`RENDERER_VERSION`], which the
//! cache key carries: change the version when the picture changes.

use super::geometry::{self, GeometryLimits};

/// Width and height in pixels.
pub const SIZE: u32 = 256;
/// Changes whenever the same STL would render differently.
pub const RENDERER_VERSION: u32 = 1;
/// Pixels left empty on each side.
const MARGIN: f64 = 8.0;
/// The face colour before lighting, a neutral blue-grey.
const BASE: [f64; 3] = [168.0, 180.0, 196.0];
const AMBIENT: f64 = 0.35;
/// Direction toward the light, before normalizing: mostly from above.
const LIGHT: [f64; 3] = [0.3, -0.6, 1.0];

#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    /// Not an STL the reader accepts: corrupt, empty, or over the caps.
    #[error("the STL cannot be read: {0}")]
    Unreadable(String),
    #[error("the PNG could not be encoded")]
    Encode,
}

/// The fixed view: unit vectors for the image's right and up directions and
/// toward the camera.
struct Camera {
    right: [f64; 3],
    up: [f64; 3],
    toward: [f64; 3],
}

impl Camera {
    fn fixed() -> Self {
        let toward = normalize([1.0, -1.0, 1.0]);
        let right = normalize(cross([0.0, 0.0, 1.0], toward));
        let up = cross(toward, right);
        Self { right, up, toward }
    }

    /// Image-plane x, y and depth (larger is nearer the camera).
    fn project(&self, point: [f64; 3]) -> [f64; 3] {
        [
            dot(point, self.right),
            dot(point, self.up),
            dot(point, self.toward),
        ]
    }
}

/// Renders `stl` to PNG bytes.
pub fn render(stl: &[u8], limits: GeometryLimits) -> Result<Vec<u8>, ThumbnailError> {
    let (vertices, _) = geometry::mesh(stl, limits)
        .map_err(|error| ThumbnailError::Unreadable(error.to_string()))?;
    encode(&rasterize(&vertices))
}

/// RGBA pixels, row by row from the top.
fn rasterize(vertices: &[[f64; 3]]) -> Vec<u8> {
    let camera = Camera::fixed();
    let projected: Vec<[f64; 3]> = vertices.iter().map(|v| camera.project(*v)).collect();
    let (mut low, mut high) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for [x, y, _] in &projected {
        low = [low[0].min(*x), low[1].min(*y)];
        high = [high[0].max(*x), high[1].max(*y)];
    }
    let extent = (high[0] - low[0]).max(high[1] - low[1]);
    let size = f64::from(SIZE);
    let scale = if extent > 0.0 {
        (size - 2.0 * MARGIN) / extent
    } else {
        1.0
    };
    let centre = [(low[0] + high[0]) / 2.0, (low[1] + high[1]) / 2.0];
    // Image x grows right and y grows down.
    let pixel = |[x, y, depth]: [f64; 3]| {
        [
            size / 2.0 + (x - centre[0]) * scale,
            size / 2.0 - (y - centre[1]) * scale,
            depth,
        ]
    };
    let light_from = normalize(LIGHT);
    let side = SIZE as usize;
    let mut depth = vec![f64::NEG_INFINITY; side * side];
    let mut rgba = vec![0u8; side * side * 4];
    for (triangle, corners) in vertices.chunks_exact(3).zip(projected.chunks_exact(3)) {
        let normal = cross(sub(triangle[1], triangle[0]), sub(triangle[2], triangle[0]));
        let length = dot(normal, normal).sqrt();
        if length == 0.0 {
            continue;
        }
        let light = AMBIENT + (1.0 - AMBIENT) * (dot(normal, light_from) / length).abs();
        let colour = BASE.map(|channel| (channel * light).round().clamp(0.0, 255.0) as u8);
        let [a, b, c] = [pixel(corners[0]), pixel(corners[1]), pixel(corners[2])];
        fill(&mut depth, &mut rgba, [a, b, c], colour);
    }
    rgba
}

/// Draws one triangle into the depth and colour buffers, sampling each pixel
/// at its centre.
fn fill(depth: &mut [f64], rgba: &mut [u8], [a, b, c]: [[f64; 3]; 3], colour: [u8; 3]) {
    let area = edge(a, b, c);
    if area == 0.0 {
        return;
    }
    let side = SIZE as usize;
    let span = |values: [f64; 3]| {
        let low = values
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min)
            .floor()
            .max(0.0);
        let high = values
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil();
        (low as usize, (high.max(0.0) as usize).min(side))
    };
    let (x0, x1) = span([a[0], b[0], c[0]]);
    let (y0, y1) = span([a[1], b[1], c[1]]);
    for y in y0..y1 {
        for x in x0..x1 {
            let point = [x as f64 + 0.5, y as f64 + 0.5, 0.0];
            let (wa, wb, wc) = (edge(b, c, point), edge(c, a, point), edge(a, b, point));
            let inside = if area > 0.0 {
                wa >= 0.0 && wb >= 0.0 && wc >= 0.0
            } else {
                wa <= 0.0 && wb <= 0.0 && wc <= 0.0
            };
            if !inside {
                continue;
            }
            let z = (wa * a[2] + wb * b[2] + wc * c[2]) / area;
            let index = y * side + x;
            if z > depth[index] {
                depth[index] = z;
                rgba[index * 4..index * 4 + 4]
                    .copy_from_slice(&[colour[0], colour[1], colour[2], 255]);
            }
        }
    }
}

fn encode(rgba: &[u8]) -> Result<Vec<u8>, ThumbnailError> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, SIZE, SIZE);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|_| ThumbnailError::Encode)?;
    writer
        .write_image_data(rgba)
        .map_err(|_| ThumbnailError::Encode)?;
    writer.finish().map_err(|_| ThumbnailError::Encode)?;
    Ok(bytes)
}

/// Twice the signed area of `a b p` in the image plane.
fn edge(a: [f64; 3], b: [f64; 3], p: [f64; 3]) -> f64 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = dot(v, v).sqrt();
    [v[0] / length, v[1] / length, v[2] / length]
}

#[cfg(test)]
mod tests;
