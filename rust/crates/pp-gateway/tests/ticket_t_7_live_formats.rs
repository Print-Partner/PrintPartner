use pp_gateway::ownership::{
    AdmissionError,
    manifest::{OwnershipIndex, admit_bundled_manifest, admit_manifest_documents},
    resources::{
        FileFamily,
        ResourceId::{File, Table},
        TableId::*,
    },
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn manifest() -> Value {
    serde_json::from_str(include_str!("../ownership-manifest.json")).unwrap()
}

fn admit(value: &Value) -> Result<OwnershipIndex, AdmissionError> {
    admit_manifest_documents(
        &value.to_string(),
        include_str!("../operations.json"),
        include_str!("../../../../web/apps/server/src/data/source-sync-ownership.json"),
    )
}

fn number_after(source: &str, marker: &str) -> u64 {
    let start = source
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} not found"))
        + marker.len();
    source[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap()
}

type Edit = fn(&mut Value);

fn sqlite_refusal(operation: &str) -> AdmissionError {
    AdmissionError::IncompatibleLiveFormat {
        operation: operation.into(),
        format: "sqlite-schema".into(),
        node_reads: 34,
        rust_writes: Some(35),
    }
}

#[test]
fn declared_versions_match_the_node_and_rust_sources() {
    let formats = &manifest()["live_formats"];
    let node_schema = number_after(
        include_str!("../../../../web/apps/server/src/db/schema.ts"),
        "export const currentSchemaVersion = ",
    );
    let rust_schema_source = include_str!("../../pp-storage/src/schema.rs");
    // This reads the default-build constant as text. The search fails on
    // purpose if that cfg-gated constant is removed, because the shipped
    // schema then changed and live_formats and the Some(35) refusals must
    // change with it.
    let rust_schema_marker =
        "#[cfg(not(feature = \"desktop_rebuild\"))]\npub(crate) const CURRENT_VERSION: u64 = ";
    assert!(
        rust_schema_source.contains(rust_schema_marker),
        "pp-storage/src/schema.rs no longer has the default-build CURRENT_VERSION gated on \
         not(desktop_rebuild). If desktop_rebuild became the default, the shipped Rust schema \
         changed: update ownership-manifest.json live_formats.sqlite-schema.rust_writes, the \
         Some(35) rollback refusals in this file, and this marker in the same change"
    );
    let rust_schema = number_after(rust_schema_source, rust_schema_marker);
    let node_snapshot = number_after(
        include_str!("../../../../web/apps/server/src/services/local-source-snapshot.ts"),
        "const SOURCE_SNAPSHOT_MANIFEST_VERSION = ",
    );
    let rust_snapshot = number_after(
        include_str!("../../pp-source/src/lib.rs"),
        "manifest.version != ",
    );
    let node_path_hints = number_after(
        include_str!("../../../../web/apps/server/src/services/path-hints.ts"),
        "document.version !== ",
    );
    let node_thumbnails = number_after(
        include_str!("../../../../web/packages/contracts/src/accepted-media.ts"),
        "export const THUMBNAIL_RENDERER_VERSION = \"",
    );
    let rust_thumbnail_reader = number_after(
        include_str!("../../pp-core/src/review_observations.rs"),
        "\"accepted-thumbnail-v",
    );
    assert_eq!((node_schema, rust_schema), (34, 35));
    assert_eq!((node_thumbnails, rust_thumbnail_reader), (3, 3));
    assert_eq!((node_snapshot, rust_snapshot), (1, 1));
    assert_eq!(node_path_hints, 1);
    assert_eq!(
        formats,
        &json!({
            "sqlite-schema": {"node_reads": node_schema, "rust_writes": rust_schema},
            "source-revision-workspace": {"node_reads": node_snapshot, "rust_writes": rust_snapshot},
            "source-covers": {"node_reads": 1, "rust_writes": "none"},
            "kit-catalog": {"node_reads": 1, "rust_writes": "none"},
            "path-hints": {"node_reads": node_path_hints, "rust_writes": "none"},
            "community-manifests": {"node_reads": 1, "rust_writes": "none"},
            "accepted-thumbnails": {"node_reads": node_thumbnails, "rust_writes": "none"},
            "custom-filaments": {"node_reads": 1, "rust_writes": 1},
            "filament-catalog": {"node_reads": 1, "rust_writes": "none"},
            "assistant-domain": {"node_reads": 1, "rust_writes": "none"},
        })
    );
}

#[test]
fn bundled_rollback_refuses_every_reviewed_operation_on_the_newer_schema() {
    let index = admit_bundled_manifest().unwrap();
    for (participant, operation) in [
        ("job.sync", "source.sync"),
        ("http.POST /api/v1/jobs/sync", "source.sync"),
        ("http.HEAD /sources/:id/docs", "source.sync"),
        ("mcp.start_sync", "source.sync"),
        ("tool.read_source_file", "tool.read_source_file"),
        ("tool.search_source_files", "tool.search_source_files"),
        ("tool.preview_source_naming", "tool.preview_source_naming"),
        ("tool.list_sources", "tool.list_sources"),
        ("tool.list_source_categories", "tool.list_source_categories"),
        ("tool.get_plan_snapshot", "tool.get_plan_snapshot"),
    ] {
        assert_eq!(
            index.admit_rollback(participant),
            Err(sqlite_refusal(operation)),
            "{participant}"
        );
    }
    assert_eq!(
        index.admit_rollback("http.GET /api/v2/plans"),
        Err(AdmissionError::UnresolvedResources {
            operation: "http:GET /plans current".into()
        })
    );
    assert_eq!(
        index.admit_rollback("tool.missing"),
        Err(AdmissionError::UnknownParticipant {
            id: "tool.missing".into()
        })
    );
    assert_eq!(
        sqlite_refusal("tool.list_sources").to_string(),
        "rollback of tool.list_sources refused: Rust writes sqlite-schema 35, Node reads up to 34"
    );
}

#[test]
fn rollback_admits_only_when_node_reads_every_written_format() {
    let mut value = manifest();
    value["live_formats"]["sqlite-schema"]["node_reads"] = json!(35);
    let index = admit(&value).unwrap();
    for participant in ["tool.read_source_file", "tool.search_source_files"] {
        assert_eq!(
            index.admit_rollback(participant),
            Ok(&BTreeSet::from([
                Table(Projects),
                Table(SourceDocs),
                File(FileFamily::SourceWorkspace),
            ]))
        );
    }
    for participant in ["tool.list_sources", "tool.list_source_categories"] {
        assert_eq!(
            index.admit_rollback(participant),
            Ok(&BTreeSet::from([
                Table(Projects),
                Table(SourceDocs),
                Table(AppSettings),
                File(FileFamily::SourceWorkspace),
            ]))
        );
    }
    assert_eq!(
        index.admit_rollback("tool.preview_source_naming"),
        Ok(&BTreeSet::from([Table(Projects), Table(AppSettings)]))
    );
    assert_eq!(
        index.admit_rollback("job.sync"),
        index.resources_for("job.sync")
    );
    assert_eq!(
        index.admit_rollback("tool.get_plan_snapshot"),
        index.resources_for("tool.get_plan_snapshot")
    );

    value["live_formats"]["source-revision-workspace"]["rust_writes"] = json!(2);
    for participant in [
        "tool.read_source_file",
        "tool.search_source_files",
        "tool.list_sources",
        "tool.list_source_categories",
    ] {
        assert_eq!(
            admit(&value).unwrap().admit_rollback(participant),
            Err(AdmissionError::IncompatibleLiveFormat {
                operation: participant.into(),
                format: "source-revision-workspace".into(),
                node_reads: 1,
                rust_writes: Some(2),
            })
        );
    }
}

#[test]
fn malformed_live_format_declarations_fail_admission() {
    let malformed = |edit: &dyn Fn(&mut Value)| {
        let mut value = manifest();
        edit(&mut value);
        match admit(&value) {
            Err(AdmissionError::Malformed(detail)) => detail,
            other => panic!("expected malformed, got {other:?}"),
        }
    };
    let cases: [(Edit, &str); 8] = [
        (
            |v| {
                v.as_object_mut().unwrap().remove("live_formats");
            },
            "missing field `live_formats`",
        ),
        (
            |v| {
                v["live_formats"]
                    .as_object_mut()
                    .unwrap()
                    .remove("source-covers");
            },
            "missing field `source-covers`",
        ),
        (
            |v| v["live_formats"]["thumbnails"] = json!({"node_reads": 1, "rust_writes": 1}),
            "unknown field `thumbnails`",
        ),
        (
            |v| {
                v["live_formats"]["source-covers"]
                    .as_object_mut()
                    .unwrap()
                    .remove("rust_writes");
            },
            "missing field `rust_writes`",
        ),
        (
            |v| {
                v["live_formats"]["sqlite-schema"]
                    .as_object_mut()
                    .unwrap()
                    .remove("node_reads");
            },
            "missing field `node_reads`",
        ),
        (
            |v| v["live_formats"]["sqlite-schema"]["rollback"] = json!("compatible"),
            "unknown field `rollback`",
        ),
        (
            |v| v["live_formats"]["sqlite-schema"]["rust_writes"] = json!("35"),
            "unknown variant",
        ),
        (
            |v| v["live_formats"]["source-covers"]["node_reads"] = json!(-1),
            "invalid value",
        ),
    ];
    for (edit, expected) in cases {
        let detail = malformed(&edit);
        assert!(detail.contains(expected), "{expected}: {detail}");
    }
    for (format, field) in [
        ("sqlite-schema", "node_reads"),
        ("source-covers", "rust_writes"),
    ] {
        assert_eq!(
            malformed(&|v| v["live_formats"][format][field] = json!(0)),
            format!("live format {format} has version 0")
        );
    }
}

#[test]
fn workspace_format_must_match_the_source_sync_record() {
    let duplicate = include_str!("../ownership-manifest.json").replacen(
        "\"live_formats\": {",
        "\"live_formats\": {\n    \"source-covers\": {\"node_reads\": 1, \"rust_writes\": null},",
        1,
    );
    assert!(matches!(
        admit_manifest_documents(
            &duplicate,
            include_str!("../operations.json"),
            include_str!("../../../../web/apps/server/src/data/source-sync-ownership.json"),
        ),
        Err(AdmissionError::Malformed(detail)) if detail.contains("duplicate field `source-covers`")
    ));
    let mut value = manifest();
    value["live_formats"]["source-revision-workspace"]["node_reads"] = json!(2);
    assert_eq!(
        admit(&value).unwrap_err(),
        AdmissionError::UnknownLiveFormat {
            value: "source-revision-workspace-v2".into()
        }
    );
}

#[test]
fn no_writer_declarations_have_family_literal_rules() {
    let families = std::collections::BTreeMap::from([
        ("source-covers", "covers"),
        ("kit-catalog", "kit-catalog"),
        ("path-hints", "path-hints"),
        ("community-manifests", "manifests"),
        ("accepted-thumbnails", "thumbs"),
        ("custom-filaments", "custom_filaments"),
        ("filament-catalog", "ambrosia_fallback"),
        ("assistant-domain", "assistant-domain"),
    ]);
    let manifest = manifest();
    let no_writers: std::collections::BTreeSet<_> = manifest["live_formats"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, format)| format["rust_writes"] == "none")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(no_writers, families.keys().copied().collect());

    let rule = include_str!("../../../rules/no-writer-family-literal.yml");
    let mut regexes = rule
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- regex: '"));
    let regex = regexes.next().expect("family literal regex");
    assert!(
        regexes.next().is_none(),
        "expected one family literal regex"
    );
    let alternation = regex
        .strip_suffix('\'')
        .and_then(|regex| regex.strip_prefix(r#"(?i)(^|["/\\])("#))
        .and_then(|regex| regex.strip_suffix(r##")([./\\"#]|$)"##))
        .expect("family literal regex must retain its path-component boundaries");
    let tokens: std::collections::BTreeSet<_> = alternation.split('|').collect();
    assert_eq!(tokens, families.values().copied().collect());
}

#[test]
fn read_only_formats_require_explicit_none_and_fail_closed_otherwise() {
    for (format, node_reads, participant, operation) in [
        (
            "kit-catalog",
            1,
            "tool.get_kit_catalog",
            "tool.get_kit_catalog",
        ),
        ("source-covers", 1, "job.sync", "source.sync"),
        (
            "path-hints",
            1,
            "tool.get_plan_option_groups",
            "tool.get_plan_option_groups",
        ),
        (
            "community-manifests",
            1,
            "tool.get_plan_option_groups",
            "tool.get_plan_option_groups",
        ),
        (
            "accepted-thumbnails",
            3,
            "tool.get_plan_review",
            "tool.get_plan_review",
        ),
        (
            "custom-filaments",
            1,
            "tool.get_plan_review",
            "tool.get_plan_review",
        ),
        (
            "filament-catalog",
            1,
            "tool.get_plan_review",
            "tool.get_plan_review",
        ),
        (
            "assistant-domain",
            1,
            "tool.get_interaction_graph",
            "tool.get_interaction_graph",
        ),
    ] {
        let mut value = manifest();
        value["live_formats"]["sqlite-schema"]["node_reads"] = json!(35);
        value["live_formats"][format]["rust_writes"] = json!("none");
        let index = admit(&value).unwrap();
        assert_eq!(
            index.admit_rollback(participant),
            index.resources_for(participant)
        );

        for rust_writes in [None, Some(node_reads + 1)] {
            value["live_formats"][format]["rust_writes"] = json!(rust_writes);
            assert_eq!(
                admit(&value).unwrap().admit_rollback(participant),
                Err(AdmissionError::IncompatibleLiveFormat {
                    operation: operation.into(),
                    format: format.into(),
                    node_reads,
                    rust_writes,
                })
            );
        }
        value["live_formats"][format]["rust_writes"] = Value::Null;
        assert_eq!(
            admit(&value)
                .unwrap()
                .admit_rollback(participant)
                .unwrap_err()
                .to_string(),
            format!("rollback of {operation} refused: Rust writer for {format} is undeclared")
        );
        for writer in [
            json!(0),
            json!("unknown"),
            json!("1"),
            json!("None"),
            json!(false),
            json!(-1),
            json!(1.5),
            json!(4294967296_u64),
            json!({}),
            json!({"none": null}),
            json!([]),
        ] {
            value["live_formats"][format]["rust_writes"] = writer;
            assert!(
                matches!(admit(&value), Err(AdmissionError::Malformed(_))),
                "{format}: {value}"
            );
        }
        value["live_formats"][format]
            .as_object_mut()
            .unwrap()
            .remove("rust_writes");
        assert!(
            matches!(admit(&value), Err(AdmissionError::Malformed(detail)) if detail.contains("missing field `rust_writes`"))
        );
    }
}
