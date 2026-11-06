//! Lossless JSON bodies for accepted reads.
//!
//! The bodies match the derived `Serialize` output byte for byte, except that
//! Part manifest metadata is written from its UTF-16 carrier, so text that is
//! not valid Unicode keeps its unpaired surrogates as `\uXXXX` escapes.

use super::{AcceptedRead, Batch, BuildRead, graph::Part, graph::Snapshot};
use crate::manifest_text::DraftPart;
use anyhow::{Result, bail, ensure};
use serde::Serialize;
use serde_json::Value;

pub struct AcceptedReadJsonBody(Vec<u8>);
pub struct AcceptedSnapshotJsonBody(Vec<u8>);
pub struct AcceptedBatchJsonBody(Vec<u8>);

impl AcceptedReadJsonBody {
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}
impl AcceptedSnapshotJsonBody {
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}
impl AcceptedBatchJsonBody {
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// Writes a JSON object field by field, so a nested body is written once.
#[derive(Default)]
struct Object(Vec<u8>);

impl Object {
    fn key(&mut self, key: &'static str) {
        self.0.push(if self.0.is_empty() { b'{' } else { b',' });
        self.0.push(b'"');
        self.0.extend_from_slice(key.as_bytes());
        self.0.extend_from_slice(b"\":");
    }

    fn raw(&mut self, key: &'static str, raw: &[u8]) {
        self.key(key);
        self.0.extend_from_slice(raw);
    }

    fn value(&mut self, key: &'static str, value: &impl Serialize) -> Result<()> {
        self.key(key);
        serde_json::to_writer(&mut self.0, value)?;
        Ok(())
    }

    fn finish(mut self) -> Vec<u8> {
        if self.0.is_empty() {
            self.0.push(b'{');
        }
        self.0.push(b'}');
        self.0
    }
}

fn array(items: impl IntoIterator<Item = Result<Vec<u8>>>) -> Result<Vec<u8>> {
    let mut output = vec![b'['];
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            output.push(b',');
        }
        output.extend_from_slice(&item?);
    }
    output.push(b']');
    Ok(output)
}

impl Part {
    fn json_bytes(&self) -> Result<Vec<u8>> {
        let Value::Object(mut scalar) = serde_json::to_value(self)? else {
            bail!("Accepted Part JSON is not an object");
        };
        let fields = scalar.keys().cloned().collect::<Vec<_>>();
        for field in ["requirement", "optionGroupId", "manifestSource"] {
            ensure!(
                scalar.remove(field).is_some(),
                "Accepted Part JSON has no {field} field"
            );
        }
        let fields = fields.iter().map(String::as_str).collect::<Vec<_>>();
        Ok(
            DraftPart::new(Value::Object(scalar), self.manifest.clone())
                .json_fields(&fields, false),
        )
    }
}

impl Snapshot {
    fn json_bytes(&self) -> Result<Vec<u8>> {
        // Exhaustive, so a new field fails to compile here. The serde parity
        // test pins the names and order.
        let Self {
            format,
            profile,
            plan_version,
            revision_id,
            revision_number,
            revision_digest,
            accepted_at,
            provenance,
            required_unit_mapping_digest,
            parts,
        } = self;
        let mut object = Object::default();
        object.value("format", format)?;
        object.value("profile", profile)?;
        object.value("planVersion", plan_version)?;
        object.value("revisionId", revision_id)?;
        object.value("revisionNumber", revision_number)?;
        object.value("revisionDigest", revision_digest)?;
        object.value("acceptedAt", accepted_at)?;
        object.value("provenance", provenance)?;
        object.value("requiredUnitMappingDigest", required_unit_mapping_digest)?;
        object.raw("parts", &array(parts.iter().map(Part::json_bytes))?);
        Ok(object.finish())
    }

    pub fn into_json_body(self) -> Result<AcceptedSnapshotJsonBody> {
        self.json_bytes().map(AcceptedSnapshotJsonBody)
    }
}

impl AcceptedRead {
    fn json_bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Ready { snapshot } => {
                let mut object = Object::default();
                object.value("kind", &"ready")?;
                object.raw("snapshot", &snapshot.json_bytes()?);
                Ok(object.finish())
            }
            _ => Ok(serde_json::to_vec(self)?),
        }
    }

    pub fn into_json_body(self) -> Result<AcceptedReadJsonBody> {
        self.json_bytes().map(AcceptedReadJsonBody)
    }
}

impl BuildRead {
    fn json_bytes(&self) -> Result<Vec<u8>> {
        let Self {
            profile_id,
            accepted,
            context,
            context_error,
        } = self;
        let mut object = Object::default();
        object.value("profileId", profile_id)?;
        object.raw("accepted", &accepted.json_bytes()?);
        object.value("context", context)?;
        if let Some(error) = context_error {
            object.value("contextError", error)?;
        }
        Ok(object.finish())
    }
}

impl Batch {
    pub fn into_json_body(self) -> Result<AcceptedBatchJsonBody> {
        let Self { builds, .. } = self;
        let mut object = Object::default();
        object.raw("builds", &array(builds.iter().map(BuildRead::json_bytes))?);
        Ok(AcceptedBatchJsonBody(object.finish()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::graph;
    use super::*;
    use crate::manifest_text::{JsText, ManifestText, OptionGroupId};

    fn accepted() -> AcceptedRead {
        let root = std::env::temp_dir().join(format!("pp-json-body-{}", rand::random::<u64>()));
        std::fs::create_dir_all(root.join("repos/1/revisions/accepted")).unwrap();
        let database = root.join("fixture.db");
        std::fs::write(
            &database,
            include_bytes!("../../tests/fixtures/accepted-plate-node.db"),
        )
        .unwrap();
        let mut connection = rusqlite::Connection::open(database).unwrap();
        let tx = connection.transaction().unwrap();
        let accepted = graph::read(
            &tx,
            "default",
            1,
            &root.join("repos"),
            &mut graph::Budget::default(),
        )
        .unwrap();
        drop(tx);
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
        accepted
    }

    fn first_part(accepted: &mut AcceptedRead) -> &mut Part {
        let AcceptedRead::Ready { snapshot } = accepted else {
            panic!("accepted fixture is not ready")
        };
        &mut snapshot.parts[0]
    }

    fn batch(accepted: AcceptedRead) -> Batch {
        Batch {
            builds: vec![
                BuildRead {
                    profile_id: 1,
                    accepted,
                    context: None,
                    context_error: Some("context \"failed\"".into()),
                },
                BuildRead {
                    profile_id: 2,
                    accepted: AcceptedRead::Missing,
                    context: None,
                    context_error: None,
                },
            ],
            stock: None,
            plates: None,
        }
    }

    #[test]
    fn scalar_bodies_match_serde_byte_for_byte() {
        let mut accepted = accepted();
        let text = "Needs \"glue\" \u{1}\t✓ 🧩";
        let part = first_part(&mut accepted);
        part.requirement = Some(text.into());
        part.option_group_id = Some("group/α".into());
        part.manifest_source = Some("manifest.yaml".into());
        part.manifest.requirement = Some(ManifestText(JsText::scalar(text)));
        part.manifest.option_group_id = Some(OptionGroupId(JsText::scalar("group/α")));
        part.manifest.manifest_source = Some("manifest.yaml".into());

        let AcceptedRead::Ready { snapshot } = &accepted else {
            unreachable!()
        };
        assert_eq!(
            snapshot.clone().into_json_body().unwrap().into_bytes(),
            serde_json::to_vec(snapshot).unwrap()
        );
        assert_eq!(
            accepted.clone().into_json_body().unwrap().into_bytes(),
            serde_json::to_vec(&accepted).unwrap()
        );
        let batch = batch(accepted);
        let expected = serde_json::to_vec(&batch).unwrap();
        assert_eq!(batch.into_json_body().unwrap().into_bytes(), expected);
    }

    #[test]
    fn non_scalar_manifest_text_keeps_its_surrogates() {
        let mut accepted = accepted();
        let part = first_part(&mut accepted);
        part.requirement = None;
        part.option_group_id = None;
        part.manifest.requirement = Some(ManifestText(JsText::from_units(vec![0x61, 0xd800])));
        part.manifest.option_group_id = Some(OptionGroupId(JsText::from_units(vec![0xdc00])));

        let body =
            String::from_utf8(batch(accepted).into_json_body().unwrap().into_bytes()).unwrap();
        let requirement = body.find(r#""requirement":"a\ud800""#).unwrap();
        let option_group = body.find(r#""optionGroupId":"\udc00""#).unwrap();
        let source = body.find(r#""manifestSource":"#).unwrap();
        let artifact = body.find(r#""artifact":"#).unwrap();
        assert!(requirement < option_group && option_group < source && source < artifact);
        assert!(body.starts_with(
            r#"{"builds":[{"profileId":1,"accepted":{"kind":"ready","snapshot":{"format":"#
        ));
    }

    #[test]
    fn non_ready_reads_use_the_serde_shape() {
        for accepted in [
            AcceptedRead::Empty {
                profile: graph::Profile {
                    id: 1,
                    name: "Build".into(),
                    order_number: None,
                    special_request: None,
                    archived_at: None,
                },
            },
            AcceptedRead::Missing,
            AcceptedRead::CompatibilityDirty,
            AcceptedRead::Uninitialized,
            AcceptedRead::IntegrityFailure {
                code: "digest".into(),
                message: "Accepted Plan digest is corrupt".into(),
            },
        ] {
            assert_eq!(
                accepted.clone().into_json_body().unwrap().into_bytes(),
                serde_json::to_vec(&accepted).unwrap()
            );
        }
    }
}
