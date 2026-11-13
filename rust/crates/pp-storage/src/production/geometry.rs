use super::model::{MAX_JS_SAFE_INTEGER, is_ecmascript_whitespace, trim_ecmascript};
use anyhow::{Result, ensure};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshBounds {
    pub min_x: f64,
    pub min_y: f64,
    pub min_z: f64,
    pub max_x: f64,
    pub max_y: f64,
    pub max_z: f64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedMesh {
    pub triangles: usize,
    pub bounds: MeshBounds,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshDimensionUm(u64);

impl MeshDimensionUm {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshDimensions {
    pub width_um: MeshDimensionUm,
    pub depth_um: MeshDimensionUm,
    pub height_um: MeshDimensionUm,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GeometryLimits {
    pub max_bytes: usize,
    pub max_triangles: usize,
}
impl Default for GeometryLimits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_triangles: 1_000_000,
        }
    }
}

impl MeshBounds {
    pub(crate) fn from_vertices(vertices: &[[f64; 3]]) -> Result<Self> {
        ensure!(!vertices.is_empty(), "mesh has no vertices");
        let mut bounds = Self {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            min_z: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
            max_z: f64::NEG_INFINITY,
        };
        for [x, y, z] in vertices {
            ensure!(
                x.is_finite() && y.is_finite() && z.is_finite(),
                "mesh coordinate is not finite"
            );
            bounds.min_x = bounds.min_x.min(*x);
            bounds.min_y = bounds.min_y.min(*y);
            bounds.min_z = bounds.min_z.min(*z);
            bounds.max_x = bounds.max_x.max(*x);
            bounds.max_y = bounds.max_y.max(*y);
            bounds.max_z = bounds.max_z.max(*z);
        }
        Ok(bounds)
    }
}

fn number_syntax(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    let mut index = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        index += 1;
    }
    let before = index;
    while matches!(bytes.get(index), Some(b'0'..=b'9')) {
        index += 1;
    }
    let digits_before = index > before;
    let mut digits_after = false;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let decimal = index;
        while matches!(bytes.get(index), Some(b'0'..=b'9')) {
            index += 1;
        }
        digits_after = index > decimal;
    }
    ensure!(digits_before || digits_after, "invalid STL number");
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent = index;
        while matches!(bytes.get(index), Some(b'0'..=b'9')) {
            index += 1;
        }
        ensure!(index > exponent, "invalid STL exponent");
    }
    ensure!(index == bytes.len(), "invalid STL number");
    Ok(())
}

fn coordinate(value: &str) -> Result<f64> {
    number_syntax(value)?;
    let parsed: f64 = value.parse()?;
    ensure!(parsed.is_finite(), "mesh coordinate is not finite");
    Ok(parsed)
}

fn exact_binary_size(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 84 {
        return None;
    }
    let triangles = u32::from_le_bytes(bytes[80..84].try_into().ok()?) as usize;
    84usize
        .checked_add(triangles.checked_mul(50)?)?
        .eq(&bytes.len())
        .then_some(triangles)
}

fn binary_vertices(bytes: &[u8], limits: GeometryLimits) -> Result<Vec<[f64; 3]>> {
    let triangles =
        exact_binary_size(bytes).ok_or_else(|| anyhow::anyhow!("binary STL length is invalid"))?;
    ensure!(
        triangles > 0 && triangles <= limits.max_triangles,
        "STL triangle count is outside bounds"
    );
    let mut vertices = Vec::with_capacity(
        triangles
            .checked_mul(3)
            .ok_or_else(|| anyhow::anyhow!("triangle count overflows"))?,
    );
    let mut offset = 84;
    for _ in 0..triangles {
        offset += 12;
        for _ in 0..3 {
            let x = f32::from_le_bytes(bytes[offset..offset + 4].try_into()?);
            let y = f32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?);
            let z = f32::from_le_bytes(bytes[offset + 8..offset + 12].try_into()?);
            vertices.push([f64::from(x), f64::from(y), f64::from(z)]);
            offset += 12;
        }
        offset += 2;
    }
    Ok(vertices)
}

fn ascii_vertices(bytes: &[u8], limits: GeometryLimits) -> Result<Vec<[f64; 3]>> {
    let decoded = String::from_utf8_lossy(bytes);
    let text = trim_ecmascript(&decoded);
    let newline = text
        .find('\n')
        .ok_or_else(|| anyhow::anyhow!("ASCII STL header is missing"))?;
    let footer_start = text
        .rfind('\n')
        .ok_or_else(|| anyhow::anyhow!("ASCII STL footer is missing"))?;
    ensure!(footer_start > newline, "ASCII STL body is missing");
    let header = text[..newline]
        .strip_suffix('\r')
        .unwrap_or(&text[..newline]);
    ensure!(
        header.starts_with("solid") && !header.contains('\r'),
        "ASCII STL header is invalid"
    );
    let footer = &text[footer_start + 1..];
    ensure!(
        footer.starts_with("endsolid") && !footer.contains('\r'),
        "ASCII STL footer is invalid"
    );
    let mut vertices = Vec::new();
    let mut triangles = 0;
    for facet in text[newline + 1..footer_start].split_inclusive("endfacet") {
        let tokens = facet
            .split(is_ecmascript_whitespace)
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        if tokens.is_empty() {
            continue;
        }
        let mut index = 0;
        ensure!(
            tokens.get(index) == Some(&"facet") && tokens.get(index + 1) == Some(&"normal"),
            "ASCII STL facet is invalid"
        );
        for normal_index in 2..5 {
            number_syntax(
                tokens
                    .get(index + normal_index)
                    .ok_or_else(|| anyhow::anyhow!("truncated normal"))?,
            )?;
        }
        ensure!(
            tokens.get(index + 5) == Some(&"outer") && tokens.get(index + 6) == Some(&"loop"),
            "ASCII STL loop is invalid"
        );
        index += 7;
        for _ in 0..3 {
            ensure!(
                tokens.get(index) == Some(&"vertex"),
                "ASCII STL vertex is invalid"
            );
            vertices.push([
                coordinate(
                    tokens
                        .get(index + 1)
                        .ok_or_else(|| anyhow::anyhow!("truncated vertex"))?,
                )?,
                coordinate(
                    tokens
                        .get(index + 2)
                        .ok_or_else(|| anyhow::anyhow!("truncated vertex"))?,
                )?,
                coordinate(
                    tokens
                        .get(index + 3)
                        .ok_or_else(|| anyhow::anyhow!("truncated vertex"))?,
                )?,
            ]);
            index += 4;
        }
        ensure!(
            tokens.get(index) == Some(&"endloop") && tokens.get(index + 1) == Some(&"endfacet"),
            "ASCII STL facet is truncated"
        );
        triangles += 1;
        ensure!(
            triangles <= limits.max_triangles,
            "STL triangle count is outside bounds"
        );
    }
    ensure!(triangles > 0, "mesh has no triangles");
    Ok(vertices)
}

/// Every triangle's three vertices in file order, binary or ASCII, within
/// `limits`.
fn read_vertices(bytes: &[u8], limits: GeometryLimits) -> Result<Vec<[f64; 3]>> {
    ensure!(bytes.len() <= limits.max_bytes, "STL exceeds byte limit");
    if exact_binary_size(bytes).is_some() {
        binary_vertices(bytes, limits)
    } else {
        ascii_vertices(bytes, limits)
    }
}

pub fn parse_accepted_stl(bytes: &[u8], limits: GeometryLimits) -> Result<ParsedMesh> {
    let vertices = read_vertices(bytes, limits)?;
    Ok(ParsedMesh {
        triangles: vertices.len() / 3,
        bounds: MeshBounds::from_vertices(&vertices)?,
    })
}

/// The vertices [`parse_accepted_stl`] reads, three per triangle, with their
/// bounds. The thumbnail renderer draws from these.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn mesh(bytes: &[u8], limits: GeometryLimits) -> Result<(Vec<[f64; 3]>, MeshBounds)> {
    let vertices = read_vertices(bytes, limits)?;
    let bounds = MeshBounds::from_vertices(&vertices)?;
    Ok((vertices, bounds))
}

pub fn dimensions_um(mesh: &ParsedMesh) -> Result<MeshDimensions> {
    fn dimension(value: f64) -> Result<MeshDimensionUm> {
        ensure!(
            value.is_finite() && value > 0.0,
            "mesh dimension is not positive"
        );
        let rounded = (value * 1_000.0).round();
        ensure!(
            rounded.is_finite() && rounded > 0.0 && rounded <= MAX_JS_SAFE_INTEGER as f64,
            "mesh dimension is not a positive JavaScript-safe integer"
        );
        Ok(MeshDimensionUm(rounded as u64))
    }
    Ok(MeshDimensions {
        width_um: dimension(mesh.bounds.max_x - mesh.bounds.min_x)?,
        depth_um: dimension(mesh.bounds.max_y - mesh.bounds.min_y)?,
        height_um: dimension(mesh.bounds.max_z - mesh.bounds.min_z)?,
    })
}
