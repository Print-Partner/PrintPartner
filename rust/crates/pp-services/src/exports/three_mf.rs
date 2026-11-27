//! Standalone 3MF models and managed Plate archives.

use serde::Serialize;
use std::{
    collections::HashMap,
    io::{self, Cursor, Seek, SeekFrom, Write},
};
use zip::{CompressionMethod, ZipWriter, result::ZipError, write::SimpleFileOptions};

const MODEL_PATH: &str = "3D/3dmodel.model";
pub const MAX_OBJECTS: usize = 10_000;
const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>"#;
const RELATIONSHIPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>"#;
const MODEL_HEADER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
<resources>
"#;

#[derive(Debug)]
pub enum Refusal {
    OutputLimit,
    NoTriangles,
    Encode,
}

#[derive(Default)]
pub struct DirectLayout {
    x: u64,
    y: u64,
    row_depth: u64,
}

#[derive(Debug)]
pub struct PackingConflict;

impl DirectLayout {
    pub fn place(&mut self, [width, depth, height]: [u64; 3]) -> Result<[u32; 2], PackingConflict> {
        const GAP: u64 = 10_000;
        const ROW_WIDTH: u64 = 250_000;
        const MAX_COORDINATE: u64 = i32::MAX as u64;
        if [width, depth, height]
            .into_iter()
            .any(|d| d > MAX_COORDINATE)
        {
            return Err(PackingConflict);
        }
        if self.x > 0 && self.x + width > ROW_WIDTH {
            self.x = 0;
            self.y += self.row_depth + GAP;
            self.row_depth = 0;
        }
        if self.y + depth > MAX_COORDINATE {
            return Err(PackingConflict);
        }
        let position = [self.x as u32, self.y as u32];
        self.x += width + GAP;
        self.row_depth = self.row_depth.max(depth);
        Ok(position)
    }
}

impl From<ZipError> for Refusal {
    fn from(error: ZipError) -> Self {
        match error {
            ZipError::Io(error) => error.into(),
            _ => Self::Encode,
        }
    }
}

impl From<io::Error> for Refusal {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::FileTooLarge {
            Self::OutputLimit
        } else {
            Self::Encode
        }
    }
}

pub struct Bundle {
    zip: ZipWriter<BoundedBuffer>,
    xml_remaining: usize,
    max_bytes: usize,
}

impl Bundle {
    /// Independently caps archive bytes and the combined uncompressed model XML.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            zip: ZipWriter::new(BoundedBuffer::new(max_bytes)),
            xml_remaining: max_bytes,
            max_bytes,
        }
    }

    pub fn model(&self) -> Result<Model, Refusal> {
        Model::new(self.xml_remaining)
    }

    pub fn plate(&mut self, ordinal: u32, model: Model) -> Result<String, Refusal> {
        let xml = model.finish()?;
        self.xml_remaining = self
            .xml_remaining
            .checked_sub(xml.len())
            .ok_or(Refusal::OutputLimit)?;
        let bytes = package(&xml, self.max_bytes)?;
        let entry = format!("plates/{ordinal:04}.3mf");
        member(&mut self.zip, &entry, &bytes, CompressionMethod::Stored)?;
        Ok(entry)
    }

    pub fn finish(mut self, manifest: &impl Serialize) -> Result<Vec<u8>, Refusal> {
        self.zip
            .start_file("manifest.json", SimpleFileOptions::default())?;
        serde_json::to_writer(&mut self.zip, manifest).map_err(|error| {
            match error.io_error_kind() {
                Some(io::ErrorKind::FileTooLarge) => Refusal::OutputLimit,
                _ => Refusal::Encode,
            }
        })?;
        Ok(self.zip.finish()?.into_bytes())
    }
}

fn package(xml: &[u8], max_bytes: usize) -> Result<Vec<u8>, Refusal> {
    let mut package = ZipWriter::new(BoundedBuffer::new(max_bytes));
    for (name, bytes) in [
        ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
        ("_rels/.rels", RELATIONSHIPS.as_bytes()),
        (MODEL_PATH, xml),
    ] {
        member(&mut package, name, bytes, CompressionMethod::Deflated)?;
    }
    Ok(package.finish()?.into_bytes())
}

pub struct Model {
    xml: BoundedBuffer,
    items: Vec<String>,
}

pub struct Object<'a> {
    pub token: &'a str,
    pub name: &'a str,
    pub color: Option<&'a str>,
    pub position_um: [u32; 2],
    pub minimum: [f64; 3],
    pub vertices: &'a [[f64; 3]],
}

impl Model {
    pub fn new(max_bytes: usize) -> Result<Self, Refusal> {
        let mut xml = BoundedBuffer::new(max_bytes);
        xml.write_all(MODEL_HEADER.as_bytes())?;
        Ok(Self {
            xml,
            items: Vec::new(),
        })
    }

    pub fn package(self, max_bytes: usize) -> Result<Vec<u8>, Refusal> {
        package(&self.finish()?, max_bytes)
    }

    pub fn object(&mut self, object: Object<'_>) -> Result<usize, Refusal> {
        let id = self.items.len() + 1;
        if id > MAX_OBJECTS {
            return Err(Refusal::OutputLimit);
        }
        let token = attribute(object.token);
        let name = attribute(object.name);
        let property = object
            .color
            .map(|color| (MAX_OBJECTS + id, attribute(color)));
        if let Some((material, color)) = &property {
            writeln!(
                self.xml,
                r#"<basematerials id="{material}"><base name="{color}" displaycolor="{color}"/></basematerials>"#
            )?;
        }
        write!(
            self.xml,
            r#"<object id="{id}" name="{name}" partnumber="{token}" type="model""#
        )?;
        if let Some((material, _)) = property {
            write!(self.xml, r#" pid="{material}" pindex="0""#)?;
        }
        writeln!(self.xml, "><mesh><vertices>")?;
        let [x, y] = object.position_um.map(|um| f64::from(um) / 1_000.0);
        let shift = [x, y, 0.0];
        let mut indices = HashMap::new();
        let mut faces = Vec::new();
        for face in object.vertices.as_chunks::<3>().0 {
            let points = [0, 1, 2].map(|i| {
                [0, 1, 2].map(|axis| {
                    let value = face[i][axis] - object.minimum[axis] + shift[axis];
                    if value == 0.0 { 0.0 } else { value }
                })
            });
            if points[0] == points[1] || points[1] == points[2] || points[0] == points[2] {
                continue;
            }
            let mut face = [0; 3];
            for (slot, point) in face.iter_mut().zip(points) {
                let next = indices.len();
                let index = indices.entry(point.map(f64::to_bits)).or_insert(next);
                *slot = *index;
                if *index == next {
                    let [x, y, z] = point;
                    writeln!(self.xml, r#"<vertex x="{x}" y="{y}" z="{z}"/>"#)?;
                }
            }
            faces.push(face);
        }
        if faces.is_empty() {
            return Err(Refusal::NoTriangles);
        }
        writeln!(self.xml, "</vertices><triangles>")?;
        for [a, b, c] in faces {
            writeln!(self.xml, r#"<triangle v1="{a}" v2="{b}" v3="{c}"/>"#)?;
        }
        writeln!(self.xml, "</triangles></mesh></object>")?;
        self.items.push(token);
        Ok(id)
    }

    fn finish(mut self) -> Result<Vec<u8>, Refusal> {
        writeln!(self.xml, "</resources><build>")?;
        for (index, token) in self.items.iter().enumerate() {
            let id = index + 1;
            writeln!(self.xml, r#"<item objectid="{id}" partnumber="{token}"/>"#)?;
        }
        writeln!(self.xml, "</build></model>")?;
        Ok(self.xml.into_bytes())
    }
}

fn attribute(text: &str) -> String {
    let text = text.replace(
        |c: char| {
            (c < ' ' && !matches!(c, '\t' | '\n' | '\r')) || matches!(c, '\u{fffe}' | '\u{ffff}')
        },
        "\u{fffd}",
    );
    super::escape(&text)
}

fn member(
    zip: &mut ZipWriter<BoundedBuffer>,
    name: &str,
    bytes: &[u8],
    compression: CompressionMethod,
) -> Result<(), Refusal> {
    zip.start_file(
        name,
        SimpleFileOptions::default().compression_method(compression),
    )?;
    zip.write_all(bytes)?;
    Ok(())
}

struct BoundedBuffer {
    cursor: Cursor<Vec<u8>>,
    limit: usize,
}

impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            cursor: Cursor::default(),
            limit,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.cursor.into_inner()
    }
}

impl Seek for BoundedBuffer {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.cursor.seek(position)
    }
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cursor.position().saturating_add(bytes.len() as u64) > self.limit as u64 {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.cursor.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object() -> Object<'static> {
        Object {
            token: "unit-1",
            name: "A & \"B\"",
            color: Some("#12AB34"),
            position_um: [1500, 2500],
            minimum: [10.0, 20.0, 30.0],
            vertices: &[[10.0, 20.0, 30.0], [11.0, 20.0, 30.0], [10.0, 21.0, 31.0]],
        }
    }

    #[test]
    fn ticket_t_58_three_mf_writes_object_color_and_escapes_names() {
        let mut model = Bundle::new(8_192).model().unwrap();
        assert_eq!(model.object(object()).unwrap(), 1);
        let xml = String::from_utf8(model.finish().unwrap()).unwrap();
        assert!(xml.contains(r##"<base name="#12AB34" displaycolor="#12AB34"/>"##));
        assert!(xml.contains(r#"name="A &amp; &quot;B&quot;" partnumber="unit-1" type="model" pid="10001" pindex="0""#));
        assert!(xml.contains(r#"<vertex x="1.5" y="3.5" z="1"/>"#));
    }

    #[test]
    fn ticket_t_58_three_mf_limits_xml_and_rejects_collapsed_meshes() {
        let mut model = Bundle::new(MODEL_HEADER.len() + 8).model().unwrap();
        assert!(matches!(model.object(object()), Err(Refusal::OutputLimit)));

        let mut model = Bundle::new(8_192).model().unwrap();
        let collapsed = Object {
            vertices: &[[0.0; 3]; 3],
            ..object()
        };
        assert!(matches!(model.object(collapsed), Err(Refusal::NoTriangles)));
    }

    #[test]
    fn ticket_t_58_three_mf_direct_packs_without_overlap_and_bounds_coordinates() {
        let mut layout = DirectLayout::default();
        assert_eq!(layout.place([150_000, 50_000, 1]).unwrap(), [0, 0]);
        assert_eq!(layout.place([90_000, 20_000, 1]).unwrap(), [160_000, 0]);
        assert_eq!(layout.place([300_000, 80_000, 1]).unwrap(), [0, 60_000]);
        assert_eq!(layout.place([10_000, 10_000, 1]).unwrap(), [0, 150_000]);
        assert!(layout.place([i32::MAX as u64 + 1, 1, 1]).is_err());
        assert!(layout.place([250_000, i32::MAX as u64, 1]).is_err());
    }

    #[test]
    fn ticket_t_58_three_mf_direct_limits_standalone_package() {
        let mut model = Model::new(8_192).unwrap();
        model.object(object()).unwrap();
        assert!(matches!(model.package(128), Err(Refusal::OutputLimit)));
    }
}
