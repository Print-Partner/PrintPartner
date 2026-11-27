//! A sliced 3MF supplies object names for Checkoff, not new Source meshes.

use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
};
use zip::ZipArchive;

pub const MAX_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_OBJECTS: usize = 2_000;
pub const MAX_LABEL_BYTES: usize = 200;
const MODEL_RELATION: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
const MAX_RELATIONSHIPS_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Package,
    Limit,
    Model,
    Unsupported,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Object {
    pub id: u32,
    pub name: String,
    pub part_number: Option<String>,
}

pub fn inspect(bytes: &[u8]) -> Result<Vec<Object>, Refusal> {
    inspect_bounded(bytes, MAX_BYTES)
}

fn inspect_bounded(bytes: &[u8], max_bytes: usize) -> Result<Vec<Object>, Refusal> {
    if bytes.is_empty() {
        return Err(Refusal::Empty);
    }
    if bytes.len() > max_bytes {
        return Err(Refusal::Limit);
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|_| Refusal::Package)?;
    if archive.len() > 4096 {
        return Err(Refusal::Limit);
    }
    let index = match unique_member(&archive, |name| name.eq_ignore_ascii_case("_rels/.rels"))? {
        Some(index) => {
            let relations =
                read_member(&mut archive, index, max_bytes.min(MAX_RELATIONSHIPS_BYTES))?;
            let target =
                model_target(std::str::from_utf8(&relations).map_err(|_| Refusal::Package)?)?;
            unique_member(&archive, |name| name.eq_ignore_ascii_case(&target))?
                .ok_or(Refusal::Package)?
        }
        None => {
            // Older packages can omit the relationship. Only a single model
            // is unambiguous in that case.
            unique_member(&archive, |name| {
                name.to_ascii_lowercase().ends_with(".model")
            })?
            .ok_or(Refusal::Package)?
        }
    };
    let document = read_member(&mut archive, index, max_bytes)?;
    model(std::str::from_utf8(&document).map_err(|_| Refusal::Model)?)
}

fn unique_member(
    archive: &ZipArchive<Cursor<&[u8]>>,
    matches: impl Fn(&str) -> bool,
) -> Result<Option<usize>, Refusal> {
    let mut found = None;
    for (index, name) in archive.file_names().enumerate() {
        if matches(&name.map_err(|_| Refusal::Package)?) && found.replace(index).is_some() {
            return Err(Refusal::Package);
        }
    }
    Ok(found)
}

fn read_member(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    index: usize,
    max_bytes: usize,
) -> Result<Vec<u8>, Refusal> {
    let member = archive.by_index(index).map_err(|_| Refusal::Package)?;
    if member.size() > max_bytes as u64 {
        return Err(Refusal::Limit);
    }
    let mut document = Vec::new();
    member
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut document)
        .map_err(|_| Refusal::Package)?;
    if document.len() > max_bytes {
        return Err(Refusal::Limit);
    }
    Ok(document)
}

fn model_target(document: &str) -> Result<String, Refusal> {
    let mut reader = Reader::from_str(document);
    reader.config_mut().expand_empty_elements = true;
    let mut seen_root = false;
    let mut depth = 0usize;
    let mut target = None;
    loop {
        match reader.read_event().map_err(|_| Refusal::Package)? {
            Event::Start(element) => {
                match depth {
                    0 if !seen_root && element.local_name().as_ref() == "Relationships" => {
                        seen_root = true
                    }
                    0 => return Err(Refusal::Package),
                    1 if element.local_name().as_ref() == "Relationship" => {
                        if let Some(path) = relation_target(&element)?
                            && target.replace(path).is_some()
                        {
                            return Err(Refusal::Package);
                        }
                    }
                    _ => {}
                }
                depth += 1;
            }
            Event::End(_) => depth = depth.checked_sub(1).ok_or(Refusal::Package)?,
            Event::Eof if seen_root && depth == 0 => return target.ok_or(Refusal::Package),
            Event::DocType(_) | Event::Eof => return Err(Refusal::Package),
            _ => {}
        }
    }
}

fn relation_target(element: &BytesStart<'_>) -> Result<Option<String>, Refusal> {
    let (mut is_model, mut target, mut external) = (false, None, false);
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|_| Refusal::Package)?;
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|_| Refusal::Package)?;
        match attribute.key.as_ref() {
            "Type" => is_model = value == MODEL_RELATION,
            "Target" => target = Some(value.into_owned()),
            "TargetMode" => external = value != "Internal",
            _ => {}
        }
    }
    if !is_model {
        return Ok(None);
    }
    let target = target.ok_or(Refusal::Package)?;
    let path = target.strip_prefix('/').unwrap_or(&target);
    if external
        || path.contains([':', '?', '#', '\\', '\0'])
        || !path.to_ascii_lowercase().ends_with(".model")
    {
        return Err(Refusal::Package);
    }
    let mut segments = Vec::new();
    for part in path.split('/') {
        match part {
            "" => return Err(Refusal::Package),
            "." => {}
            ".." => {
                segments.pop().ok_or(Refusal::Package)?;
            }
            part => segments.push(part),
        }
    }
    Ok(Some(segments.join("/")))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tag {
    Model,
    Resources,
    Object,
    Mesh,
    Vertices,
    Vertex,
    Triangles,
    Triangle,
    Build,
    Item,
    Other,
}

impl Tag {
    fn from_name(name: &str) -> Self {
        match name {
            "model" => Self::Model,
            "resources" => Self::Resources,
            "object" => Self::Object,
            "mesh" => Self::Mesh,
            "vertices" => Self::Vertices,
            "vertex" => Self::Vertex,
            "triangles" => Self::Triangles,
            "triangle" => Self::Triangle,
            "build" => Self::Build,
            "item" => Self::Item,
            _ => Self::Other,
        }
    }
}

struct Resource {
    object: Object,
    direct: bool,
    vertices: u32,
    triangles: u32,
}

#[derive(Default)]
struct Model {
    current: Option<Resource>,
    objects: BTreeMap<u32, Option<Object>>,
    order: Vec<u32>,
    build: Option<Vec<(u32, Option<String>)>>,
    vertices: u32,
    triangles: u32,
}

fn model(xml: &str) -> Result<Vec<Object>, Refusal> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut tags = Vec::new();
    let mut seen_root = false;
    let mut model = Model::default();
    loop {
        match reader.read_event().map_err(|_| Refusal::Model)? {
            Event::Start(element) => {
                let tag = Tag::from_name(element.local_name().as_ref());
                if tags.is_empty() {
                    if seen_root || tag != Tag::Model {
                        return Err(Refusal::Model);
                    }
                    seen_root = true;
                }
                tags.push(tag);
                if tags.len() > 64 {
                    return Err(Refusal::Limit);
                }
                model.start(&tags, &element)?;
            }
            Event::End(_) => {
                if tags.as_slice() == [Tag::Model, Tag::Resources, Tag::Object] {
                    model.finish_object()?;
                }
                tags.pop().ok_or(Refusal::Model)?;
            }
            Event::Eof if seen_root && tags.is_empty() => break,
            Event::Eof | Event::DocType(_) => return Err(Refusal::Model),
            Event::Text(text)
                if tags.is_empty()
                    && !text.as_ref().bytes().all(|byte| byte.is_ascii_whitespace()) =>
            {
                return Err(Refusal::Model);
            }
            _ => {}
        }
    }
    model.finish()
}

impl Model {
    fn start(&mut self, tags: &[Tag], element: &BytesStart<'_>) -> Result<(), Refusal> {
        use Tag::*;
        match tags {
            [Model, Resources, Object] => {
                if self.objects.len() == MAX_OBJECTS {
                    return Err(Refusal::Limit);
                }
                self.current = Some(Resource {
                    object: object(element)?,
                    direct: false,
                    vertices: 0,
                    triangles: 0,
                });
            }
            [Model, Resources, Object, Mesh] => {
                let mesh = self.current.as_mut().ok_or(Refusal::Model)?;
                if mesh.direct {
                    return Err(Refusal::Model);
                }
                mesh.direct = true;
            }
            [Model, Resources, Object, Mesh, Vertices, Vertex] => {
                let mesh = self.current.as_mut().ok_or(Refusal::Model)?;
                geometry::<f64>(element, ["x", "y", "z"], f64::is_finite)?;
                mesh.vertices += 1;
                self.vertices += 1;
                if self.vertices > 5_000_000 {
                    return Err(Refusal::Limit);
                }
            }
            [Model, Resources, Object, Mesh, Triangles, Triangle] => {
                let mesh = self.current.as_mut().ok_or(Refusal::Model)?;
                geometry::<u32>(element, ["v1", "v2", "v3"], |index| index < mesh.vertices)?;
                mesh.triangles += 1;
                self.triangles += 1;
                if self.triangles > 10_000_000 {
                    return Err(Refusal::Limit);
                }
            }
            [Model, Build] => {
                if self.build.replace(Vec::new()).is_some() {
                    return Err(Refusal::Model);
                }
            }
            [Model, Build, Item] => {
                let items = self.build.as_mut().ok_or(Refusal::Model)?;
                if items.len() == MAX_OBJECTS {
                    return Err(Refusal::Limit);
                }
                let mut id = None;
                let mut part = None;
                let mut printable = true;
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|_| Refusal::Model)?;
                    match attribute.key.as_ref() {
                        "objectid" => id = Some(number(&attribute.value)?),
                        "printable" => {
                            printable = match attribute.value.as_ref() {
                                "true" | "1" => true,
                                "false" | "0" => false,
                                _ => return Err(Refusal::Model),
                            }
                        }
                        "partnumber" => {
                            part = label(
                                attribute
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .map_err(|_| Refusal::Model)?
                                    .as_ref(),
                            )?
                        }
                        _ if attribute.key.local_name().as_ref() == "path" => {
                            return Err(Refusal::Unsupported);
                        }
                        _ => {}
                    }
                }
                let id = id.ok_or(Refusal::Model)?;
                if printable {
                    items.push((id, part));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finish_object(&mut self) -> Result<(), Refusal> {
        let mesh = self.current.take().ok_or(Refusal::Model)?;
        let id = mesh.object.id;
        self.order.push(id);
        let object =
            (mesh.direct && mesh.vertices > 0 && mesh.triangles > 0).then_some(mesh.object);
        if self.objects.insert(id, object).is_some() {
            return Err(Refusal::Model);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<Object>, Refusal> {
        let objects: Vec<_> = match self.build {
            Some(items) => items
                .into_iter()
                .map(|(id, part)| {
                    let mut object = self
                        .objects
                        .get(&id)
                        .ok_or(Refusal::Model)?
                        .clone()
                        .ok_or(Refusal::Unsupported)?;
                    if let Some(part) = part {
                        if object
                            .part_number
                            .as_ref()
                            .is_some_and(|resource| resource != &part)
                        {
                            return Err(Refusal::Model);
                        }
                        object.part_number = Some(part);
                    }
                    Ok(object)
                })
                .collect::<Result<_, _>>()?,
            None => self
                .order
                .into_iter()
                .filter_map(|id| self.objects.remove(&id).flatten())
                .collect(),
        };
        if objects.is_empty() {
            return Err(Refusal::Empty);
        }
        Ok(objects)
    }
}

fn object(element: &BytesStart<'_>) -> Result<Object, Refusal> {
    let mut id = None;
    let mut name = None;
    let mut part_number = None;
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|_| Refusal::Model)?;
        match attribute.key.as_ref() {
            "id" => id = Some(number(&attribute.value)?),
            "name" => {
                name = label(
                    attribute
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_err(|_| Refusal::Model)?
                        .as_ref(),
                )?
            }
            "partnumber" => {
                part_number = label(
                    attribute
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_err(|_| Refusal::Model)?
                        .as_ref(),
                )?
            }
            _ => {}
        }
    }
    let id = id.ok_or(Refusal::Model)?;
    let name = name
        .or_else(|| part_number.clone())
        .unwrap_or_else(|| format!("object-{id}"));
    Ok(Object {
        id,
        name,
        part_number,
    })
}

fn label(text: &str) -> Result<Option<String>, Refusal> {
    let text = text.trim();
    if text.len() > MAX_LABEL_BYTES {
        return Err(Refusal::Limit);
    }
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

fn number<T: std::str::FromStr>(value: &str) -> Result<T, Refusal> {
    value.parse().map_err(|_| Refusal::Model)
}

fn geometry<T: std::str::FromStr>(
    element: &BytesStart<'_>,
    keys: [&str; 3],
    valid: impl Fn(T) -> bool,
) -> Result<(), Refusal> {
    let mut seen = 0;
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|_| Refusal::Model)?;
        if let Some(index) = keys.iter().position(|key| *key == attribute.key.as_ref()) {
            if !valid(number(&attribute.value)?) {
                return Err(Refusal::Model);
            }
            seen |= 1 << index;
        }
    }
    if seen == 0b111 {
        Ok(())
    } else {
        Err(Refusal::Model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::{ZipWriter, write::SimpleFileOptions};

    const GEOMETRY: &str = r#"<mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh>"#;

    #[test]
    fn reads_only_placed_meshes_and_preserves_item_order() {
        let xml = format!(
            r#"<model><resources><object id="1" name="Unused">{GEOMETRY}</object><object id="2" name="Arm &amp; base">{GEOMETRY}</object></resources><build><item objectid="1" printable="0"/><item objectid="2" partnumber="second"/><item objectid="2" partnumber="first"/></build></model>"#
        );
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("3D/3dmodel.model", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(xml.as_bytes()).unwrap();
        archive
            .start_file("metadata.xml", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"not even XML").unwrap();
        let objects = inspect(&archive.finish().unwrap().into_inner()).unwrap();
        assert_eq!(
            objects
                .iter()
                .map(|o| (o.name.as_str(), o.part_number.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("Arm & base", Some("second")),
                ("Arm & base", Some("first"))
            ]
        );
    }

    #[test]
    fn refuses_invalid_geometry_conflicting_tokens_and_unsupported_components() {
        let xml = format!(
            r#"<model><resources><object id="1" partnumber="one">{GEOMETRY}</object></resources><build><item objectid="1" partnumber="two"/></build></model>"#
        );
        assert_eq!(model(&xml), Err(Refusal::Model));
        let xml = xml.replace(r#"partnumber="two""#, r#"partnumber="one""#);
        assert_eq!(model(&xml).unwrap().len(), 1);
        assert_eq!(
            model(&xml.replace(r#"v3="2""#, r#"v3="3""#)),
            Err(Refusal::Model)
        );
        assert_eq!(
            model(&xml.replace(GEOMETRY, "<components/>")),
            Err(Refusal::Unsupported)
        );
        assert_eq!(model(&xml.replace("</model>", "")), Err(Refusal::Model));
    }

    #[test]
    fn bounds_the_package_and_its_decompressed_model() {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        archive.start_file("3D/3dmodel.model", options).unwrap();
        archive.write_all(&[b' '; 1024]).unwrap();
        let package = archive.finish().unwrap().into_inner();
        assert!(package.len() < 1024);
        assert_eq!(
            inspect_bounded(&package, package.len() - 1),
            Err(Refusal::Limit)
        );
        assert_eq!(
            inspect_bounded(&package, package.len()),
            Err(Refusal::Limit)
        );
        assert_eq!(inspect(b"not a zip"), Err(Refusal::Package));
    }

    #[test]
    fn inspects_the_declared_root_model_even_when_a_conventional_name_exists() {
        let package = |relationship: Option<&str>, duplicate: bool| {
            let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
            let models = [
                ("3D/3dmodel.model", "wrong"),
                ("selected.model", "right"),
                ("SELECTED.model", "wrong"),
            ];
            for (path, name) in models.into_iter().take(if duplicate { 3 } else { 2 }) {
                archive
                    .start_file(path, SimpleFileOptions::default())
                    .unwrap();
                write!(archive, r#"<model><resources><object id="1" name="{name}">{GEOMETRY}</object></resources></model>"#).unwrap();
            }
            if let Some(target) = relationship {
                archive
                    .start_file("_RELS/.RELS", SimpleFileOptions::default())
                    .unwrap();
                write!(archive, r#"<Relationships><Relationship Type="{MODEL_RELATION}" Target="{target}"/></Relationships>"#).unwrap();
            }
            archive.finish().unwrap().into_inner()
        };
        for target in [
            "/SELECTED.MODEL",
            "./selected.model",
            "3D/../selected.model",
        ] {
            assert_eq!(
                inspect(&package(Some(target), false)).unwrap()[0].name,
                "right"
            );
        }
        for relationship in [
            None,
            Some("/missing.model"),
            Some("../selected.model"),
            Some("//host/selected.model"),
        ] {
            assert_eq!(
                inspect(&package(relationship, false)),
                Err(Refusal::Package)
            );
        }
        assert_eq!(
            inspect(&package(Some("/selected.model"), true)),
            Err(Refusal::Package)
        );
        assert_eq!(model_target("</Relationships>"), Err(Refusal::Package));
    }
}
