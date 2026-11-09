use pp_storage::{Limits, WriterOwner};
use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;

#[path = "common/process.rs"]
mod child_process;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pp-plan-fixture-{}", rand::random::<u64>()));
        std::fs::create_dir_all(root.join("repos/1/revisions/accepted")).unwrap();
        std::fs::write(
            root.join("print-partner.db"),
            include_bytes!("fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        owner.shutdown().unwrap();
        Self(root)
    }
    fn connection(&self) -> Connection {
        Connection::open(self.0.join("print-partner.db")).unwrap()
    }
    #[cfg(feature = "desktop_rebuild")]
    fn mutate(&self, edit: impl FnOnce(&Connection)) {
        let connection = self.connection();
        let triggers = connection
            .prepare("SELECT name,sql FROM sqlite_master WHERE type='trigger'")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for (name, _) in &triggers {
            connection
                .execute_batch(&format!("DROP TRIGGER \"{name}\";"))
                .unwrap();
        }
        edit(&connection);
        for (_, sql) in triggers {
            connection.execute_batch(&sql).unwrap();
        }
    }
    fn run(&self, observations: bool) -> String {
        let path = self.0.join("observations.json");
        std::fs::write(&path, r#"{"availableInputRoots":[1],"mediaByPartId":{"1":{"artifactMissing":false,"thumbEmpty":false},"2":{"artifactMissing":false,"thumbEmpty":false}}}"#).unwrap();
        let mut command = child_process::command(child_process::bin_exe("plan-read-fixture"));
        command
            .arg(&self.0)
            .args(["1", "2", "999"])
            .env("PP_READ_SESSION", "read-fixture-secret")
            .env("PP_DESKTOP_REBUILD", "1")
            .env_remove("PP_REVIEW_OBSERVATIONS");
        if observations {
            command.env("PP_REVIEW_OBSERVATIONS", path);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    #[cfg(feature = "desktop_rebuild")]
    fn metadata(
        &self,
        requirement: pp_storage::manifest_text::JsText,
        option: pp_storage::manifest_text::JsText,
    ) {
        use pp_storage::manifest_text::{
            DraftPart, ManifestText, OptionGroupId, PartManifestMetadata,
        };
        use sha2::{Digest, Sha256};
        self.mutate(|connection| {
            for table in ["parts", "plan_revision_parts"] {
                connection
                    .execute(
                        &format!("UPDATE {table} SET requirement=?1,option_group_id=?2"),
                        rusqlite::params![
                            requirement.sqlite_value().unwrap(),
                            option.sqlite_value().unwrap()
                        ],
                    )
                    .unwrap();
            }
            let fields = [
                "part_key",
                "relative_path",
                "filename",
                "source_layer",
                "status",
                "role_inferred",
                "role_override",
                "filament_color_id",
                "filament_custom_hex",
                "spoolman_spool_id",
                "quantity_inferred",
                "quantity_override",
                "quantity_effective",
                "included",
                "notes",
                "github_blob_url",
                "geometry_same",
                "requirement",
                "option_group_id",
                "manifest_source",
                "artifact_digest",
            ];
            let revisions = connection
                .prepare("SELECT id FROM plan_revisions")
                .unwrap()
                .query_map([], |r| r.get::<_, i64>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            for revision in revisions {
                let mut statement = connection
                    .prepare("SELECT * FROM plan_revision_parts WHERE revision_id=?1")
                    .unwrap();
                let names = statement
                    .column_names()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                let mut rows = statement.query([revision]).unwrap();
                let mut parts = Vec::new();
                while let Some(row) = rows.next().unwrap() {
                    let mut scalar = serde_json::Map::new();
                    for field in fields {
                        if matches!(field, "requirement" | "option_group_id" | "manifest_source") {
                            continue;
                        }
                        let index = names.iter().position(|n| n == field).unwrap();
                        let value = match row.get_ref(index).unwrap() {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(n)
                                if matches!(field, "included" | "geometry_same") =>
                            {
                                Value::Bool(n != 0)
                            }
                            rusqlite::types::ValueRef::Integer(n) => Value::from(n),
                            rusqlite::types::ValueRef::Real(n) => Value::from(n),
                            rusqlite::types::ValueRef::Text(s) => {
                                Value::from(std::str::from_utf8(s).unwrap())
                            }
                            value => panic!("unexpected {field}: {value:?}"),
                        };
                        scalar.insert(field.to_string(), value);
                    }
                    let metadata = PartManifestMetadata {
                        requirement: Some(ManifestText(requirement.clone())),
                        option_group_id: Some(OptionGroupId(option.clone())),
                        manifest_source: row.get("manifest_source").unwrap(),
                    };
                    parts.push(
                        String::from_utf8(
                            DraftPart::new(Value::Object(scalar), metadata)
                                .json_fields(&fields, false),
                        )
                        .unwrap(),
                    );
                }
                parts.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                let body = format!(
                    r#"{{"format":"plan-revision-parts-v1","parts":[{}]}}"#,
                    parts.join(",")
                );
                let digest = hex::encode(Sha256::digest(body.as_bytes()));
                connection
                    .execute(
                        "UPDATE plan_revisions SET snapshot_digest=?1 WHERE id=?2",
                        rusqlite::params![digest, revision],
                    )
                    .unwrap();
            }
        });
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn scalar_empty_and_unavailable_output() {
    let fixture = Fixture::new();
    fixture.connection().execute("INSERT INTO build_profiles(id,tenant_id,name,accepted_plan_version) VALUES(2,'default','empty',0)", []).unwrap();
    let output: Value = serde_json::from_str(&fixture.run(true)).unwrap();
    assert_eq!(output["readOnly"]["unchanged"], true);
    let builds = output["builds"].as_array().unwrap();
    assert_eq!(builds[0]["accepted"]["kind"], "ready");
    assert_eq!(builds[0]["review"]["kind"], "ready");
    assert_eq!(builds[0]["reviewAll"]["kind"], "ready");
    assert!(builds[0]["reviewSummary"]["totals"].is_object());
    assert_eq!(builds[1]["accepted"]["kind"], "empty");
    assert_eq!(builds[2]["accepted"]["kind"], "missing");
    assert_eq!(builds[1]["review"]["kind"], "empty");
    assert_eq!(builds[2]["review"]["kind"], "not_found");
}

#[cfg(feature = "desktop_rebuild")]
fn assert_surrogate_output(reviews: bool) {
    let fixture = Fixture::new();
    fixture.metadata(
        pp_storage::manifest_text::JsText::from_units(vec![0x61, 0xd800, 0x22, 0xa]),
        pp_storage::manifest_text::JsText::from_units(vec![0xdc00]),
    );
    let output = fixture.run(reviews);
    assert!(!output.contains('�'), "{output}");
    assert!(
        output.contains(r#""requirement":"a\ud800\"\n""#),
        "{output}"
    );
    assert!(output.contains(r#""optionGroupId":"\udc00""#), "{output}");
    #[derive(serde::Deserialize)]
    struct Output {
        builds: Vec<std::collections::BTreeMap<String, Box<serde_json::value::RawValue>>>,
    }
    let parsed: Output = serde_json::from_str(&output).unwrap();
    let build = &parsed.builds[0];
    assert!(
        build["accepted"]
            .get()
            .contains(r#""requirement":"a\ud800\"\n""#)
    );
    if reviews {
        for field in ["review", "reviewAll"] {
            assert!(
                build[field]
                    .get()
                    .contains(r#""requirement":"a\ud800\"\n""#)
            );
            assert!(build[field].get().contains(r#""option_group_id":"\udc00""#));
        }
        let summary: Value = serde_json::from_str(build["reviewSummary"].get()).unwrap();
        assert!(summary["totals"]["included_parts"].as_u64().unwrap() > 0);
    } else {
        assert!(!build.contains_key("review"));
    }
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn binary_preserves_unpaired_surrogates_without_reviews() {
    assert_surrogate_output(false);
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn binary_preserves_unpaired_surrogates_with_reviews() {
    assert_surrogate_output(true);
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn binary_preserves_scalar_and_empty_metadata() {
    use pp_storage::manifest_text::JsText;
    for (requirement, option) in [("Needs \"glue\"\n🧩", "group/α"), ("", "")] {
        let fixture = Fixture::new();
        fixture.metadata(JsText::scalar(requirement), JsText::scalar(option));
        let output: Value = serde_json::from_str(&fixture.run(true)).unwrap();
        let build = &output["builds"][0];
        let parts = build["accepted"]["snapshot"]["parts"].as_array().unwrap();
        assert!(!parts.is_empty());
        for part in parts {
            assert_eq!(part["requirement"], requirement);
            assert_eq!(part["optionGroupId"], option);
        }
        for field in ["review", "reviewAll"] {
            let groups = build[field]["body"]["part_groups"].as_array().unwrap();
            assert!(!groups.is_empty());
            for group in groups {
                for part in group["parts"].as_array().unwrap() {
                    assert_eq!(part["requirement"], requirement);
                    assert_eq!(part["option_group_id"], option);
                }
            }
        }
    }
}
