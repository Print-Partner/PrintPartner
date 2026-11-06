#[path = "tests/client.rs"]
mod client;
#[path = "tests/policy.rs"]
mod policy;

use super::*;
use crate::manifest_text::{JsText, ManifestText, OptionGroupId, PartManifestMetadata};
use rusqlite::Connection;

fn golden() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../fixtures/parity/plan-publication/node.json"
    ))
    .unwrap()
}

fn part(mut row: Value) -> DraftPart {
    let mut take = |field: &str| {
        row.as_object_mut()
            .unwrap()
            .remove(field)
            .filter(|v| !v.is_null())
            .map(|v| JsText::scalar(v.as_str().unwrap()))
    };
    let requirement = take("requirement").map(ManifestText);
    let option_group_id = take("optionGroupId").map(OptionGroupId);
    let manifest_source = take("manifestSource").map(|v| v.as_scalar().unwrap());
    DraftPart::new(
        row,
        PartManifestMetadata {
            requirement,
            option_group_id,
            manifest_source,
        },
    )
}

fn prepared(case: &Value) -> Result<Prepared> {
    let input = &case["input"];
    let parts = input["draft"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .map(part)
        .collect();
    let base = input["baseUnits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| {
            (
                unit["token"].as_str().unwrap().into(),
                (
                    Value::Null,
                    unit["completed"].as_bool().unwrap(),
                    unit["assembled"].as_bool().unwrap(),
                ),
            )
        })
        .collect();
    model::prepare(parts, &input["assignments"], &base)
}

fn error(result: Result<impl Sized>) -> String {
    result.err().unwrap().to_string()
}

#[test]
fn preparation_matches_literal_node_digests_progress_and_slots() {
    for case in golden()["prepare"].as_array().unwrap() {
        if case["expected"].get("error").is_some() {
            continue;
        }
        let prepared = prepared(case).unwrap();
        let slots: Vec<_> = prepared.assignments.iter().map(Assignment::slot).collect();
        assert_eq!(
            json!({"digest":prepared.digest,"progress":prepared.progress,"slots":slots}),
            case["expected"]["value"],
            "{}",
            case["id"]
        );
        let mut unsorted = case.clone();
        if let Some(assignments) = unsorted["input"]["assignments"].as_array_mut() {
            assignments.reverse();
        }
        let unsorted = self::prepared(&unsorted).unwrap();
        let unsorted_slots: Vec<_> = unsorted.assignments.iter().map(Assignment::slot).collect();
        assert_eq!(
            (unsorted.digest, unsorted.progress, unsorted_slots),
            (prepared.digest.clone(), prepared.progress.clone(), slots),
            "{} unsorted",
            case["id"]
        );
        let mut reversed = prepared.parts;
        reversed.reverse();
        assert_eq!(model::revision_digest(&reversed).unwrap(), prepared.digest);
        if let Some(first) = reversed.first_mut() {
            first["notes"] = json!("changed");
            assert_ne!(model::revision_digest(&reversed).unwrap(), prepared.digest);
        }
    }
}

#[test]
fn preparation_text_limit_counts_node_fields_as_utf8() {
    let case = golden()["prepare"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "create")
        .unwrap()
        .clone();
    let prepare = |surrogates: usize| {
        let mut parts: Vec<DraftPart> = case["input"]["draft"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(part)
            .collect();
        // Buffer.byteLength counts a lone surrogate as 3 bytes; its JSON escape is 6.
        parts[0].manifest.requirement =
            Some(ManifestText(JsText::from_units(vec![0xD800; surrogates])));
        model::prepare(parts, &case["input"]["assignments"], &HashMap::new()).map(|_| ())
    };
    // The other named fields hold 51 bytes: 51 + 3 * 21828 = 65535.
    prepare(21828).unwrap();
    assert_eq!(error(prepare(21829)), "Accepted operational row text limit");
}

#[test]
fn inserted_rows_match_node_publication_text_boundaries() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/parity/publication-row-text/node.json"
    ))
    .unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let table = case["table"].as_str().unwrap();
        let mut row = case["row"].clone();
        for value in row.as_object_mut().unwrap().values_mut() {
            if value.is_object() {
                *value = json!(format!(
                    "{}{}",
                    value["repeat"]
                        .as_str()
                        .unwrap()
                        .repeat(value["count"].as_u64().unwrap() as usize),
                    value["suffix"].as_str().unwrap_or("")
                ));
            }
        }
        let mut connection = Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        let columns = row
            .as_object()
            .unwrap()
            .keys()
            .map(|key| model::snake(key))
            .collect::<Vec<_>>()
            .join(",");
        tx.execute_batch(&format!("CREATE TABLE {table}({columns})"))
            .unwrap();
        let result = super::insert(&tx, table, &row, false);
        match case["expected"].as_str().unwrap() {
            "accepted" => {
                result.unwrap_or_else(|err| panic!("{}: {err}", case["id"]));
                let extra: String = tx
                    .query_row(
                        &format!("SELECT fixture_extra_text FROM {table}"),
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(extra, "😀".repeat(16385), "{}", case["id"]);
            }
            "accepted_operational_row_text_limit" => {
                assert_eq!(
                    error(result),
                    "Accepted operational row text limit",
                    "{}",
                    case["id"]
                );
                let count: i64 = tx
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(count, 0, "{}", case["id"]);
            }
            outcome => panic!("Unknown Node outcome: {outcome}"),
        }
    }
}

#[test]
fn inserted_rows_count_only_node_named_text_fields() {
    for (table, fields, excluded) in [
        (
            "plan_revision_input_sets",
            vec!["tenantId", "inputSetDigest", "recordedAt", "recordedAt"],
            "formatVersion",
        ),
        (
            "plan_revision_inputs",
            vec![
                "tenant_id",
                "source_layer",
                "tracking_kind",
                "manifest_digest",
                "effective_naming_digest",
            ],
            "source_id",
        ),
        (
            "plan_revisions",
            vec![
                "tenantId",
                "provenanceKind",
                "digestFormat",
                "snapshotDigest",
                "createdBy",
                "acceptedBy",
                "createdAt",
                "acceptedAt",
            ],
            "profileId",
        ),
        (
            "required_units",
            vec!["tenantId", "token", "objectName", "createdAt"],
            "profileId",
        ),
        (
            "plan_revision_required_unit_sets",
            vec!["tenantId", "format", "mappingDigest", "createdAt"],
            "profileId",
        ),
        ("plan_revision_required_units", vec![], "requiredUnitToken"),
        ("print_progress", vec![], "tenantId"),
        ("plan_apply_requests", vec![], "actorId"),
        ("plan_apply_admissions", vec![], "requestFormat"),
    ] {
        let mut connection = Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        let mut row = json!({excluded: "extra"});
        for field in &fields {
            row[*field] = json!("");
        }
        let columns = row
            .as_object()
            .unwrap()
            .keys()
            .map(|key| model::snake(key))
            .collect::<Vec<_>>()
            .join(",");
        tx.execute_batch(&format!("CREATE TABLE {table}({columns})"))
            .unwrap();
        if fields.is_empty() {
            row[excluded] = json!("é".repeat(32769));
            super::insert(&tx, table, &row, false).unwrap();
        } else {
            for field in &fields {
                let occurrences = fields.iter().filter(|other| *other == field).count();
                row[*field] = json!("é".repeat(32768 / occurrences));
                super::insert(&tx, table, &row, false)
                    .unwrap_or_else(|err| panic!("{table}.{field}: {err}"));
                row[*field] = json!(format!("{}x", row[*field].as_str().unwrap()));
                assert_eq!(
                    error(super::insert(&tx, table, &row, false)),
                    "Accepted operational row text limit",
                    "{table}.{field}"
                );
                row[*field] = json!("");
            }
        }
        let stored: String = tx
            .query_row(
                &format!("SELECT {} FROM {table} LIMIT 1", model::snake(excluded)),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, row[excluded].as_str().unwrap());
    }
}

#[test]
fn inserted_parts_count_tenant_and_lossless_manifest_utf8() {
    for table in ["parts", "plan_revision_parts"] {
        let mut connection = Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        tx.execute_batch(&format!("CREATE TABLE {table}(tenant_id,filename,match_key,requirement,option_group_id,manifest_source)")).unwrap();
        let mut part = DraftPart::new(
            json!({"tenantId":"tenant", "filename":"é", "matchKey":"ignored".repeat(10000)}),
            PartManifestMetadata {
                requirement: Some(ManifestText(JsText::from_units(vec![0xD800; 21842]))),
                option_group_id: Some(OptionGroupId(JsText::scalar("é"))),
                manifest_source: None,
            },
        );
        super::insert_part(&tx, table, &part).unwrap();
        let mut statement = tx
            .prepare(&format!("SELECT requirement FROM {table}"))
            .unwrap();
        let mut rows = statement.query([]).unwrap();
        let stored = crate::manifest_text::read_manifest_text(
            rows.next().unwrap().unwrap().get_ref(0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            stored.as_ref(),
            part.manifest.requirement.as_ref().map(|text| &text.0)
        );
        part.manifest.option_group_id = Some(OptionGroupId(JsText::scalar("éx")));
        assert_eq!(
            error(super::insert_part(&tx, table, &part)),
            "Accepted operational row text limit"
        );
    }
}

#[test]
fn input_validation_counts_named_fields_and_tenant() {
    let mut connection = Connection::open_in_memory().unwrap();
    let tx = connection.transaction().unwrap();
    tx.execute_batch("CREATE TABLE projects(id,tenant_id); CREATE TABLE source_revisions(id,tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at,completeness,extra)").unwrap();
    let tenant = "é".repeat(32731);
    tx.execute("INSERT INTO projects VALUES(1,?)", [&tenant])
        .unwrap();
    let mut input = json!({"sourceId":1,"sourceLayer":"s","trackingKind":"untracked","sourceRevisionId":null,"manifestDigest":null,"effectiveNamingDigest":"a".repeat(64),"extra":"ignored".repeat(10000)});
    validate_inputs(&tx, &tenant, std::slice::from_ref(&input)).unwrap();
    input["sourceLayer"] = json!("sx");
    assert_eq!(
        error(validate_inputs(&tx, &tenant, &[input])),
        "Accepted operational row text limit"
    );

    tx.execute("INSERT INTO projects VALUES(2,'tenant')", [])
        .unwrap();
    let upstream = "é".repeat(32729);
    tx.execute(
        "INSERT INTO source_revisions VALUES(3,'tenant',2,?,?,'','','complete',?)",
        params![upstream, "a".repeat(64), "ignored".repeat(1000)],
    )
    .unwrap();
    let input = json!({"sourceId":2,"sourceLayer":"s","trackingKind":"revision","sourceRevisionId":3,"manifestDigest":"a".repeat(64),"effectiveNamingDigest":"b".repeat(64)});
    validate_inputs(&tx, "tenant", std::slice::from_ref(&input)).unwrap();
    tx.execute(
        "UPDATE source_revisions SET upstream_revision_key=upstream_revision_key || 'x'",
        [],
    )
    .unwrap();
    assert_eq!(
        error(validate_inputs(&tx, "tenant", &[input])),
        "Accepted operational row text limit"
    );
}

#[test]
fn preparation_rejections_preserve_reference_diagnostics() {
    let cases = golden();
    for (id, node, reference) in [
        (
            "duplicate-part",
            "Plan publication draft Part is duplicated",
            "Duplicate draft Part",
        ),
        (
            "missing-part",
            "Plan publication assignment Part is missing",
            "Invalid assignment coordinate",
        ),
        (
            "noncontiguous",
            "Plan publication assignment indexes are not contiguous",
            "Invalid assignment coordinate",
        ),
        (
            "duplicate-reuse",
            "Plan publication reused token is duplicated",
            "Duplicate reused unit",
        ),
        (
            "unknown-reuse",
            "Plan publication reused token is unknown",
            "Unknown reused unit",
        ),
        (
            "invalid-progress",
            "Plan publication base progress is corrupt",
            "Invalid base progress",
        ),
        (
            "incomplete",
            "Plan publication assignment count is incomplete",
            "Incomplete publication quantity",
        ),
        (
            "invalid-quantity",
            "Plan publication quantity is invalid",
            "Incomplete publication quantity",
        ),
    ] {
        let case = cases["prepare"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap();
        assert_eq!(case["expected"]["error"], node);
        assert_eq!(error(prepared(case)), reference, "{id}");
    }
}

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    let fields = ru::model::PART_FIELDS
        .iter()
        .map(|field| model::snake(field))
        .collect::<Vec<_>>()
        .join(",");
    connection.execute_batch(&format!(
        "CREATE TABLE parts(id INTEGER,tenant_id TEXT,profile_id INTEGER,match_key,quantity_auto,role,{fields});
         CREATE TABLE plan_revision_parts(id INTEGER,tenant_id TEXT,revision_id INTEGER,projection_part_id,{fields});
         CREATE TABLE plan_revisions(id INTEGER,tenant_id TEXT,profile_id INTEGER,digest_format,snapshot_digest);
         CREATE TABLE print_progress(tenant_id TEXT,part_id INTEGER,unit_index INTEGER,completed INTEGER,assembled INTEGER);
         CREATE TABLE app_settings(tenant_id TEXT,key TEXT,value TEXT);"
    )).unwrap();
    connection
        .execute_batch(include_str!("../send_queue/schema.sql"))
        .unwrap();
    connection
}

fn insert_part(tx: &Transaction<'_>, table: &str, row: &Value) {
    let part = part(row.clone());
    let fields: Vec<_> = part
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .chain(["requirement", "optionGroupId", "manifestSource"])
        .collect();
    let fields = part.sql_fields(&fields).unwrap();
    let columns = fields
        .iter()
        .map(|(key, _)| model::snake(key))
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = vec!["?"; fields.len()].join(",");
    tx.execute(
        &format!("INSERT INTO {table}(tenant_id,{columns}) VALUES('tenant',{placeholders})"),
        rusqlite::params_from_iter(fields.iter().map(|(_, value)| value)),
    )
    .unwrap();
}

fn seed_projection(tx: &Transaction<'_>, case: &Value) {
    let input = &case["input"];
    let revision = &input["revision"];
    if !revision.is_null() {
        tx.execute(
            "INSERT INTO plan_revisions VALUES(1,'tenant',1,?,?)",
            params![
                revision["digestFormat"].as_str(),
                revision["snapshotDigest"].as_str()
            ],
        )
        .unwrap();
        for row in revision["parts"].as_array().unwrap() {
            let mut row = row.clone();
            for key in ["draftId", "baseRevisionPartId", "effectiveRole"] {
                row.as_object_mut().unwrap().remove(key);
            }
            row["revisionId"] = json!(1);
            insert_part(tx, "plan_revision_parts", &row);
        }
    }
    for row in input["projections"].as_array().unwrap() {
        let mut row = row.clone();
        row["profileId"] = json!(1);
        insert_part(tx, "parts", &row);
        for index in 0..2 {
            tx.execute(
                "INSERT INTO print_progress VALUES('tenant',?, ?,1,0)",
                params![row["id"].as_i64(), index],
            )
            .unwrap();
        }
    }
}

#[test]
fn projection_acceptance_and_rejection_match_node_with_reference_diagnostics() {
    for case in golden()["projection"].as_array().unwrap() {
        let mut connection = database();
        let tx = connection.transaction().unwrap();
        seed_projection(&tx, case);
        let result = validate_projection(&tx, "tenant", 1, 1);
        assert_eq!(
            result.is_ok(),
            case["expected"]["value"] == true,
            "{}",
            case["id"]
        );
        if let Some(node) = case["expected"]["error"].as_str() {
            let reference = match case["id"].as_str().unwrap() {
                "missing-revision" => "Missing accepted revision",
                "digest" | "format" => "Accepted revision digest mismatch",
                "count" => "Accepted projection count mismatch",
                "missing-projection" => "Missing accepted projection",
                "filename" => "Accepted projection mismatch filename",
                "manifest" => "Accepted manifest projection mismatch",
                _ => unreachable!(),
            };
            assert_eq!(
                node,
                if matches!(
                    case["id"].as_str(),
                    Some("missing-revision" | "digest" | "format")
                ) {
                    "Accepted Plan revision digest is corrupt"
                } else {
                    "Accepted Plan compatibility projection is corrupt"
                }
            );
            assert_eq!(error(result), reference);
        } else {
            let parts = result.unwrap();
            assert_eq!(parts.len(), 1);
            for field in ["filamentColorId", "filamentCustomHex", "spoolmanSpoolId"] {
                assert_eq!(parts[0][field], case["input"]["projections"][0][field]);
            }
            assert_eq!(
                error(validate_projection(&tx, "other", 1, 1)),
                "Missing accepted revision"
            );
            assert_eq!(
                error(validate_projection(&tx, "tenant", 2, 1)),
                "Missing accepted revision"
            );
        }
    }
}

#[test]
fn projection_progress_checks_preserve_reference_behavior() {
    for (sql, expected) in [
        (
            "DELETE FROM print_progress WHERE unit_index=1",
            "Incomplete accepted progress",
        ),
        (
            "UPDATE print_progress SET completed=0,assembled=1",
            "Invalid accepted progress",
        ),
        (
            "UPDATE print_progress SET completed=2",
            "Invalid accepted progress",
        ),
        (
            "UPDATE print_progress SET assembled=-1",
            "Invalid accepted progress",
        ),
    ] {
        let mut connection = database();
        let tx = connection.transaction().unwrap();
        seed_projection(&tx, &golden()["projection"][0]);
        tx.execute_batch(sql).unwrap();
        assert_eq!(error(validate_projection(&tx, "tenant", 1, 1)), expected);
    }
}

fn settings(tx: &Transaction<'_>, links: &Value, queue: &Value) {
    for (key, value) in [
        ("printer.checkoff_links", links),
        ("printer.send_queue", queue),
    ] {
        tx.execute(
            "INSERT INTO app_settings VALUES('tenant',?,?)",
            params![key, value.to_string()],
        )
        .unwrap();
    }
    tx.execute_batch("INSERT INTO parts(id,tenant_id,profile_id,match_key) VALUES(11,'tenant',1,'bracket'),(99,'tenant',2,'other'),(11,'other',2,'other')").unwrap();
}

#[test]
fn remapped_settings_match_literal_node_results() {
    let cases = golden();
    for case in cases["remap"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["expected"]["kind"] != "unsafe")
    {
        let mut connection = database();
        let tx = connection.transaction().unwrap();
        settings(&tx, &case["input"]["links"], &case["input"]["queue"]);
        let production = production(&tx, "tenant", 1).unwrap();
        let active = usize::from(case["id"] == "links-queue-resolved");
        assert_eq!(
            (production.link_count, production.queue_count),
            (active, active)
        );
        let prepared = prepared(&cases["prepare"][2]).unwrap();
        let tokens = HashMap::from([
            ((11, 0), format!("ppu_{:032x}", 1)),
            ((11, 1), format!("ppu_{:032x}", 2)),
        ]);
        let Remap::Mapped(mapping) = remap(&production, 1, &tokens, &prepared).unwrap() else {
            panic!("unexpected unmappable link")
        };
        let mut actual: Vec<_> = mapping.iter().map(|(from, to)| (*from, *to)).collect();
        actual.sort();
        assert_eq!(json!(actual), case["expected"]["mapping"]);
        write_remap(
            &tx,
            "tenant",
            production,
            &mapping,
            &HashMap::from([(21, 31)]),
        )
        .unwrap();
        for key in ["printer.checkoff_links", "printer.send_queue"] {
            assert_eq!(
                json!(setting(&tx, "tenant", key).unwrap().unwrap()),
                case["expected"]["settings"][key]
            );
            assert!(setting(&tx, "other", key).unwrap().is_none());
        }
    }
}

/// The same remap cases with the queue moved into `printer_send_queue`:
/// Accept reads the table, counts the same items and writes the remapped
/// units to the table rows, and the legacy setting stays absent.
#[test]
fn remapped_queue_rows_match_literal_node_results() {
    let cases = golden();
    for case in cases["remap"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["expected"]["kind"] != "unsafe")
    {
        // The golden items carry only what Accept reads; a stored item also
        // has the fields Node's `parseItem` requires.
        let mut queue = case["input"]["queue"].clone();
        for (index, item) in queue.as_array_mut().unwrap().iter_mut().enumerate() {
            for (key, value) in [
                ("id", format!("q{index}")),
                ("filename", "plate.gcode".into()),
                (
                    "artifact_path",
                    "/exports/printer-uploads/q/plate.gcode".into(),
                ),
                ("printer_id", "p1".into()),
            ] {
                if item.get(key).is_none() {
                    item[key] = json!(value);
                }
            }
        }
        let mut connection = database();
        let tx = connection.transaction().unwrap();
        settings(&tx, &case["input"]["links"], &queue);
        tx.commit().unwrap();
        crate::send_queue::recover(&mut connection).unwrap();
        let tx = connection.transaction().unwrap();
        assert!(
            setting(&tx, "tenant", "printer.send_queue")
                .unwrap()
                .is_none()
        );
        let production = production(&tx, "tenant", 1).unwrap();
        assert!(production.queue_in_table);
        let active = usize::from(case["id"] == "links-queue-resolved");
        assert_eq!(
            (production.link_count, production.queue_count),
            (active, active)
        );
        let prepared = prepared(&cases["prepare"][2]).unwrap();
        let tokens = HashMap::from([
            ((11, 0), format!("ppu_{:032x}", 1)),
            ((11, 1), format!("ppu_{:032x}", 2)),
        ]);
        let Remap::Mapped(mapping) = remap(&production, 1, &tokens, &prepared).unwrap() else {
            panic!("unexpected unmappable link")
        };
        write_remap(
            &tx,
            "tenant",
            production,
            &mapping,
            &HashMap::from([(21, 31)]),
        )
        .unwrap();
        let units = |items: &Value| -> Vec<Value> {
            items
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["checkoff_units"].clone())
                .collect()
        };
        let rows = json!(crate::send_queue::rows(&tx, "tenant").unwrap().unwrap());
        assert_eq!(
            units(&rows),
            units(&case["expected"]["settings"]["printer.send_queue"])
        );
        assert!(
            setting(&tx, "tenant", "printer.send_queue")
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn ownerless_queue_with_deleted_parts_is_unmappable_like_node() {
    let cases = golden();
    let case = cases["remap"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "ownerless-queue-deleted-part")
        .unwrap();
    assert_eq!(case["expected"]["kind"], "unsafe");
    assert_eq!(
        case["expected"]["unmappable"][0]["reason"],
        "Checked-off part id 500 no longer exists in this Plan's parts"
    );
    let mut connection = database();
    let tx = connection.transaction().unwrap();
    settings(&tx, &case["input"]["links"], &case["input"]["queue"]);
    let state = production(&tx, "tenant", 1).unwrap();
    assert_eq!((state.link_count, state.queue_count), (0, 0));
    let prepared = prepared(&cases["prepare"][2]).unwrap();
    let Remap::Unmappable(missing) = remap(&state, 1, &HashMap::new(), &prepared).unwrap() else {
        panic!("expected unmappable deleted queue part")
    };
    assert_eq!(missing.len(), 1);
    assert_eq!(
        serde_json::to_value(missing).unwrap(),
        case["expected"]["unmappable"]
    );
}

#[test]
fn production_accepts_checkoff_coordinates_owned_by_another_profile_like_node() {
    for case in golden()["production"].as_array().unwrap() {
        assert_eq!(
            case["input"]["parts"],
            json!([{ "id": 99, "profileId": 2 }])
        );
        let mut connection = database();
        let tx = connection.transaction().unwrap();
        settings(&tx, &case["input"]["links"], &case["input"]["queue"]);
        let state = production(&tx, "tenant", 1).unwrap();
        assert_eq!(
            json!({ "checkoffLinkCount": state.link_count, "sendQueueItemCount": state.queue_count }),
            case["expected"]["value"]
        );
        assert_eq!((state.link_count, state.queue_count), (1, 0));
    }
}

#[test]
fn token_remap_handles_exclusion_missing_tokens_and_changed_indexes() {
    let cases = golden();
    let mut connection = database();
    let tx = connection.transaction().unwrap();
    let case = &cases["remap"][0];
    settings(&tx, &case["input"]["links"], &case["input"]["queue"]);
    let state = production(&tx, "tenant", 1).unwrap();
    let mut prepared = prepared(&cases["prepare"][2]).unwrap();
    let tokens = HashMap::from([
        ((11, 0), format!("ppu_{:032x}", 2)),
        ((11, 1), format!("ppu_{:032x}", 1)),
    ]);
    let Remap::Mapped(mapping) = remap(&state, 1, &tokens, &prepared).unwrap() else {
        panic!("expected mapping")
    };
    assert_eq!(
        mapping,
        HashMap::from([((11, 0), (21, 1)), ((11, 1), (21, 0))])
    );
    assert_eq!(
        error(write_remap(&tx, "tenant", state, &mapping, &HashMap::new())),
        "Missing remap projection"
    );
    let state = production(&tx, "tenant", 1).unwrap();
    prepared.parts[0]["included"] = json!(false);
    for tokens in [&tokens, &HashMap::new()] {
        let Remap::Unmappable(missing) = remap(&state, 1, tokens, &prepared).unwrap() else {
            panic!("expected unmappable links")
        };
        assert_eq!(
            serde_json::to_value(missing).unwrap(),
            json!([
                {"linkId":"link","filename":"bracket.stl","reason":"Required unit at Part 11 index 0 is absent or excluded in the selected Plan"},
                {"linkId":"link","filename":"bracket.stl","reason":"Required unit at Part 11 index 1 is absent or excluded in the selected Plan"}
            ])
        );
    }
}

#[test]
fn production_rejects_corrupt_settings_coordinates_and_owners() {
    for (key, value, expected) in [
        ("printer.checkoff_links", "{}", "Invalid production setting"),
        (
            "printer.checkoff_links",
            r#"[{"profile_id":1,"state":"bad","units":[]}]"#,
            "Invalid Checkoff link",
        ),
        (
            "printer.checkoff_links",
            r#"[{"profile_id":1,"state":"watching","units":{}}]"#,
            "Invalid Checkoff units",
        ),
        (
            "printer.checkoff_links",
            r#"[{"profile_id":1,"state":"watching","units":[{"part_id":0,"unit_index":0}]}]"#,
            "Invalid production coordinate",
        ),
        (
            "printer.send_queue",
            r#"[{"state":"bad"}]"#,
            "Invalid queue state",
        ),
        (
            "printer.send_queue",
            r#"[{"state":"queued","profile_id":0}]"#,
            "Invalid queue owner",
        ),
        (
            "printer.send_queue",
            r#"[{"state":"queued","checkoff_units":{}}]"#,
            "Invalid queue units",
        ),
        (
            "printer.send_queue",
            r#"[{"state":"queued","checkoff_units":[{"part_id":11,"unit_index":0},{"part_id":99,"unit_index":0}]}]"#,
            "Queue owner mismatch",
        ),
    ] {
        let mut connection = database();
        let tx = connection.transaction().unwrap();
        settings(&tx, &json!([]), &json!([]));
        tx.execute(
            "UPDATE app_settings SET value=? WHERE tenant_id='tenant' AND key=?",
            params![value, key],
        )
        .unwrap();
        assert_eq!(error(production(&tx, "tenant", 1)), expected);
    }
}

#[test]
fn empty_settings_and_model_utilities_preserve_reference_behavior() {
    let cases = golden();
    for case in cases["objects"].as_array().unwrap() {
        assert_eq!(
            model::object_name(
                case["filename"].as_str().unwrap(),
                case["token"].as_str().unwrap()
            ),
            case["expected"]
        );
    }
    assert_eq!(model::REQUEST_FORMAT, "plan-apply-request-v1");
    assert_eq!(
        model::inputs(&[json!({"sourceId":2}), json!({"sourceId":1})])[0]["source_id"],
        1
    );
    let mut connection = database();
    let tx = connection.transaction().unwrap();
    assert!(setting(&tx, "tenant", "missing").unwrap().is_none());
    tx.execute_batch("INSERT INTO app_settings VALUES('tenant','blank','  ')")
        .unwrap();
    assert!(setting(&tx, "tenant", "blank").unwrap().is_none());
    let empty = production(&tx, "tenant", 1).unwrap();
    write_remap(&tx, "tenant", empty, &HashMap::new(), &HashMap::new()).unwrap();
}

#[test]
fn client_validates_reused_required_unit_named_utf8_text_at_boundary() {
    use crate::auth::Secret;
    use sha2::{Digest, Sha256};

    let data = golden();
    let case = data["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "changed")
        .unwrap();
    let input = &case["input"];
    let unit = &case["before"]["required_units"][0];
    let other_bytes: usize = ["tenant_id", "token", "object_name"]
        .iter()
        .map(|field| unit[*field].as_str().unwrap().len())
        .sum();
    for excess in [0, 1] {
        let bytes = 65536 - other_bytes + excess;
        let created_at = format!("{}{}", "é".repeat(bytes / 2), "x".repeat(bytes % 2));
        assert_eq!(created_at.len() + other_bytes, 65536 + excess);
        let mut corrupted = case.clone();
        corrupted["before"]["required_units"][0]["created_at"] = json!(created_at);
        let c = transaction::database(
            &json!({"definitions":data["transactionSchema"]}),
            &corrupted,
        );
        c.execute_batch("DROP TABLE plan_apply_admissions; DROP TABLE IF EXISTS domain_events; DROP TABLE IF EXISTS printer_send_queue; UPDATE app_settings SET value='35' WHERE key='schema_version' AND tenant_id='default'").unwrap();
        let session = "reused-unit-text-session";
        let digest = hex::encode(Sha256::digest(session.as_bytes()));
        c.execute_batch("INSERT INTO users(id,display_name,is_admin,created_at) VALUES('default','Fixture',0,'2026-09-06T00:00:00.000Z')").unwrap();
        c.execute("INSERT INTO sessions(id,user_id,expires_at,provider) VALUES(?,'default','2099-01-01T00:00:00.000Z','email')", [digest]).unwrap();
        let dir = crate::tests::directory("reused-unit-text");
        c.backup("main", dir.join("print-partner.db"), None)
            .unwrap();
        drop(c);
        let (owner, _) = WriterOwner::open(&dir, crate::Limits::default()).unwrap();
        let command = PublicationCommand::new(
            serde_json::from_value(input["profileId"].clone()).unwrap(),
            serde_json::from_value(input["draftId"].clone()).unwrap(),
            serde_json::from_value(json!({
                "expected_snapshot_digest":input["expectedSnapshotDigest"],
                "expected_lifecycle_version":input["expectedLifecycleVersion"],
                "expected_base":{
                    "revision_id":input["expectedBase"]["revisionId"],
                    "plan_version":input["expectedBase"]["planVersion"]
                },
                "remap_checkoff_links":true
            }))
            .unwrap(),
            input["idempotencyKey"].as_str().unwrap().into(),
            Credential::Session(Secret::new(session.into())),
        )
        .unwrap();
        let result = owner
            .publication()
            .apply(command, &AtomicBool::new(false), Duration::ZERO);
        owner.shutdown().unwrap();
        let c = Connection::open(dir.join("print-partner.db")).unwrap();
        let state: (i64, i64, i64) = c.query_row(
            "SELECT accepted_plan_version,(SELECT COUNT(*) FROM plan_revisions),(SELECT COUNT(*) FROM plan_apply_requests) FROM build_profiles WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        let stored: String = c
            .query_row(
                "SELECT created_at FROM required_units WHERE token=?",
                [unit["token"].as_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        drop(c);
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(stored, created_at);
        if excess == 0 {
            assert!(matches!(result.unwrap(), Outcome::Applied { .. }));
            assert_eq!(state, (2, 2, 2));
        } else {
            let error = result.err().unwrap();
            assert_eq!(error.to_string(), "Plan draft update failed");
            assert_eq!(
                error.root_cause().to_string(),
                "Accepted operational row text limit"
            );
            assert_eq!(state, (1, 1, 1));
        }
    }
}

mod transaction {
    use super::super::*;
    use rusqlite::{Connection, types::ValueRef};

    fn fixture() -> Value {
        let data = super::golden();
        json!({"definitions":data["transactionSchema"],"at":data["transactionAt"],"cases":data["transactions"]})
    }
    fn value(row: &Value) -> rusqlite::types::Value {
        match row {
            Value::Number(n) if !n.is_i64() => rusqlite::types::Value::Real(n.as_f64().unwrap()),
            _ => sql_value(row).unwrap(),
        }
    }
    pub(super) fn database(fixture: &Value, case: &Value) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        let definitions = fixture["definitions"].as_array().unwrap();
        for d in definitions.iter().filter(|d| d["type"] == "table") {
            c.execute_batch(d["sql"].as_str().unwrap()).unwrap();
        }
        for (table, rows) in case["before"].as_object().unwrap() {
            for row in rows.as_array().unwrap() {
                let fields = row.as_object().unwrap();
                let sql = format!(
                    "INSERT INTO {table}({}) VALUES({})",
                    fields.keys().cloned().collect::<Vec<_>>().join(","),
                    vec!["?"; fields.len()].join(",")
                );
                c.execute(&sql, rusqlite::params_from_iter(fields.values().map(value)))
                    .unwrap();
            }
        }
        for d in definitions.iter().filter(|d| d["type"] != "table") {
            c.execute_batch(d["sql"].as_str().unwrap()).unwrap();
        }
        c.execute_batch(include_str!("schema.sql")).unwrap();
        c.execute_batch(include_str!("../domain_events/schema.sql"))
            .unwrap();
        c.execute_batch(include_str!("../send_queue/schema.sql"))
            .unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        c
    }
    fn snapshot(c: &Connection, table: &str) -> Value {
        let mut statement = c
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let names: Vec<_> = statement
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let result = statement
            .query_map([], |row| {
                let mut result = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let v = match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(n) => json!(n),
                        ValueRef::Text(s) => json!(std::str::from_utf8(s).unwrap()),
                        ValueRef::Blob(_) => panic!("Unexpected blob"),
                    };
                    result.insert(name.clone(), v);
                }
                Ok(Value::Object(result))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        json!(result)
    }
    struct Tokens(u128);
    impl RequiredUnitTokenAllocator for Tokens {
        fn allocate(&mut self) -> std::result::Result<[u8; 16], TokenAllocationFailure> {
            self.0 += 1;
            Ok(self.0.to_be_bytes())
        }
    }
    fn publish(
        tx: &Transaction<'_>,
        case: &Value,
        tokens: &mut dyn RequiredUnitTokenAllocator,
    ) -> Value {
        let input = &case["input"];
        let request: ApplyRequest = serde_json::from_value(json!({
        "expected_snapshot_digest": input["expectedSnapshotDigest"],
        "expected_lifecycle_version": input["expectedLifecycleVersion"],
        "expected_base": {"revision_id":input["expectedBase"].get("revisionId"),"plan_version":input["expectedBase"]["planVersion"]},
        "remap_checkoff_links":input["remapCheckoffLinks"]
    })).unwrap();
        let outcome = publish_in_transaction(
            tx,
            &AuthenticatedPublicationContext {
                tenant: "default".into(),
                actor: input["actorId"].as_str().unwrap().into(),
            },
            serde_json::from_value(input["profileId"].clone()).unwrap(),
            serde_json::from_value(input["draftId"].clone()).unwrap(),
            PublicationRequests::direct(&request),
            input["idempotencyKey"].as_str().unwrap(),
            tokens,
        )
        .unwrap_or_else(|e| panic!("{}: {e:#}", case["id"]));
        serde_json::to_value(outcome).unwrap()
    }
    fn node_outcome(outcome: &Value) -> Value {
        let mut result = outcome.clone();
        if let Some(receipt) = result.get_mut("receipt") {
            *receipt = model::canonical(
                receipt,
                &[
                    "profileId",
                    "draftId",
                    "revisionId",
                    "planVersion",
                    "draftLifecycleVersion",
                    "revisionDigest",
                    "requiredUnitMappingDigest",
                    "appliedAt",
                ],
            );
        }
        if result["kind"] == "production_active" {
            result = json!({
                "kind": "production_active",
                "checkoff_link_count": outcome["checkoffLinkCount"],
                "send_queue_item_count": outcome["sendQueueItemCount"],
            });
        }
        result
    }
    pub(super) fn replace_time(value: &mut Value, actual: &str, expected: &Value) {
        if actual.is_empty() {
            return;
        }
        match value {
            Value::String(s) if s == actual => *value = expected.clone(),
            Value::Array(rows) => rows
                .iter_mut()
                .for_each(|v| replace_time(v, actual, expected)),
            Value::Object(row) => row
                .values_mut()
                .for_each(|v| replace_time(v, actual, expected)),
            _ => {}
        }
    }
    /// An applied Accept writes one domain event; any other outcome writes none.
    fn assert_accept_event(c: &Connection, case: &Value, actual: &Value) {
        let events: Vec<(String, String, Option<i64>)> = c
            .prepare("SELECT event_type,aggregate_id,aggregate_version FROM domain_events")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let expected = (actual["kind"] == "applied").then(|| {
            (
                "plan.revision_accepted".to_owned(),
                case["input"]["profileId"].to_string(),
                actual["receipt"]["plan_version"].as_i64(),
            )
        });
        assert_eq!(events, Vec::from_iter(expected), "{}", case["id"]);
    }
    /// An applied Accept that replaces a Plan saves one `pre_apply` snapshot
    /// of that Plan's recorded inputs in the same transaction; the first Accept
    /// and every other outcome save none.
    fn assert_pre_accept_snapshot(c: &Connection, case: &Value, actual: &Value) {
        let rows: Vec<Value> = snapshot(c, "plan_snapshots")
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["source"] == "pre_apply")
            .cloned()
            .collect();
        let input = &case["input"];
        let previous = &input["expectedBase"]["revisionId"];
        if actual["kind"] != "applied" || previous.is_null() {
            assert_eq!(rows, Vec::<Value>::new(), "{}", case["id"]);
            return;
        }
        let [row] = rows.as_slice() else {
            panic!("{}: {rows:?}", case["id"]);
        };
        let created_at = &row["created_at"];
        let payload: Value = serde_json::from_str(row["payload_json"].as_str().unwrap()).unwrap();
        assert_eq!(row["profile_id"], input["profileId"], "{}", case["id"]);
        let before = &case["before"];
        let rows = |table: &str| before[table].as_array().cloned().unwrap_or_default();
        let set = rows("plan_revisions")
            .into_iter()
            .find(|row| &row["id"] == previous)
            .unwrap()["input_set_id"]
            .clone();
        let mut inputs: Vec<Value> = rows("plan_revision_inputs")
            .into_iter()
            .filter(|row| row["input_set_id"] == set)
            .collect();
        inputs.sort_by_key(|row| (row["layer_order"].as_i64(), row["source_id"].as_i64()));
        let layers: Vec<Value> = inputs
            .iter()
            .map(|row| {
                let name = rows("projects")
                    .into_iter()
                    .find(|project| project["id"] == row["source_id"])
                    .unwrap()["name"]
                    .clone();
                let kind = row["source_layer"].as_str().unwrap().split_once(':').unwrap().0;
                json!({"layer_type":kind,"project_id":row["source_id"],"source_name":name,"tag":null,"branch":null,"order":row["layer_order"]})
            })
            .collect();
        assert!(!layers.is_empty(), "{}", case["id"]);
        assert_eq!(
            payload,
            json!({
                "layers": layers,
                "kit_manifest": null,
                "recipe_summary": null,
                "captured_at": created_at,
                "accepted_plan_revision_id": previous,
                "accepted_plan_version": input["expectedBase"]["planVersion"],
            }),
            "{}",
            case["id"]
        );
    }
    fn changed_case(fixture: &Value) -> Value {
        fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == "changed")
            .unwrap()
            .clone()
    }
    /// The snapshot holds the replaced Plan's recorded inputs, not the working
    /// `profile_layers`: a working layer with no Source, which Accept does not
    /// publish, stays out of it.
    #[test]
    fn transaction_snapshot_holds_the_replaced_plan_inputs() {
        let fixture = fixture();
        let mut case = changed_case(&fixture);
        let mut extra = case["before"]["profile_layers"][0].clone();
        extra["id"] = json!(99);
        extra["layer_type"] = json!("addon");
        extra["project_id"] = Value::Null;
        extra["layer_order"] = json!(1);
        case["before"]["profile_layers"]
            .as_array_mut()
            .unwrap()
            .push(extra);
        let mut c = database(&fixture, &case);
        let tx = c.transaction().unwrap();
        let actual = publish(
            &tx,
            &case,
            &mut Tokens(case["allocationStart"].as_u64().unwrap().into()),
        );
        tx.commit().unwrap();
        assert_eq!(actual["kind"], "applied");
        assert_pre_accept_snapshot(&c, &case, &actual);
        let payload = &snapshot(&c, "plan_snapshots")[0]["payload_json"];
        let payload: Value = serde_json::from_str(payload.as_str().unwrap()).unwrap();
        assert_eq!(payload["layers"].as_array().unwrap().len(), 1);
    }
    /// The preview promises a snapshot exactly when Accept saves one: a Ready
    /// preview reports `saved_before_accept`, and accepting the same case leaves
    /// a `pre_apply` row, both or neither.
    #[test]
    fn transaction_preview_promises_the_snapshot_accept_saves() {
        let fixture = fixture();
        let mut seen = HashSet::new();
        for case in fixture["cases"].as_array().unwrap() {
            let mut c = database(&fixture, case);
            let tx = c.transaction().unwrap();
            let input = &case["input"];
            let profile = input["profileId"].as_i64().unwrap();
            let draft = input["draftId"].as_i64().unwrap();
            let promised = match preview::preview(&tx, "default", profile, draft).unwrap() {
                preview::PreviewOutcome::Ready(ready) => match *ready {
                    preview::AcceptPreview::NoChange { snapshot, .. }
                    | preview::AcceptPreview::Changed { snapshot, .. } => {
                        snapshot.saved_before_accept
                    }
                },
                preview::PreviewOutcome::Refused(_) => continue,
            };
            let actual = publish(
                &tx,
                case,
                &mut Tokens(case["allocationStart"].as_u64().unwrap().into()),
            );
            if actual["kind"] != "applied" {
                continue;
            }
            let saved: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM plan_snapshots WHERE source='pre_apply'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(saved == 1, promised, "{}", case["id"]);
            seen.insert(promised);
        }
        assert_eq!(
            seen.len(),
            2,
            "both a first Accept and a replacing Accept are covered"
        );
    }
    /// Input sets migrated by schema v18 store `legacy:<id>` layers, which Node's
    /// restore cannot place, so they count as nothing to restore. An empty input
    /// set is a Plan with no Source layers and still restores.
    #[test]
    fn transaction_legacy_input_layers_are_not_restorable() {
        let fixture = fixture();
        let case = changed_case(&fixture);
        let mut c = database(&fixture, &case);
        let tx = c.transaction().unwrap();
        let layers = super::super::snapshot::replaced_layers(&tx, "default", 1, Some(1)).unwrap();
        assert_eq!(layers.unwrap()[0]["layer_type"], "base");
        tx.execute(
            "UPDATE plan_revision_inputs SET source_layer='legacy:1'",
            [],
        )
        .unwrap();
        let layers = super::super::snapshot::replaced_layers(&tx, "default", 1, Some(1)).unwrap();
        assert_eq!(layers, None);
        tx.execute("DELETE FROM plan_revision_inputs", []).unwrap();
        let layers = super::super::snapshot::replaced_layers(&tx, "default", 1, Some(1)).unwrap();
        assert_eq!(
            layers,
            Some(vec![]),
            "a Plan with no Source layers restores as []"
        );
    }
    #[test]
    fn transaction_replays_node_outcomes_receipts_and_persisted_rows() {
        let fixture = fixture();
        let cases = fixture["cases"].as_array().unwrap();
        for id in [
            "mapped-remap",
            "mapped-remap-index-change",
            "mapped-remap-index-0",
            "mapped-remap-index-2",
            "renamed-part",
            "prior-completion-index-0",
            "prior-completion-index-1",
            "prior-completion-index-2",
        ] {
            assert!(
                cases.iter().any(|case| case["id"] == id),
                "Missing Node {id} transaction"
            );
        }
        for case in cases {
            let mut c = database(&fixture, case);
            let tx = c.transaction().unwrap();
            let mut actual = publish(
                &tx,
                case,
                &mut Tokens(case["allocationStart"].as_u64().unwrap().into()),
            );
            tx.commit().unwrap();
            let at = actual["receipt"]["applied_at"]
                .as_str()
                .unwrap_or("")
                .to_string();
            replace_time(&mut actual, &at, &fixture["at"]);
            let mut expected = node_outcome(&case["outcome"]);
            if case["id"] == "running-export" {
                // Node has no durable-job publication guard. Its literal outcome is applied.
                assert_eq!(expected["kind"], "applied");
                let job = &case["before"]["durable_jobs"][0];
                expected = json!({"kind":"execution_conflict","operations":[{"operation_id":job["id"],"kind":job["kind"],"state":job["state"]}]});
            }
            assert_eq!(actual, expected, "{}", case["id"]);
            let state = if actual["kind"] == "execution_conflict" {
                &case["before"]
            } else {
                &case["after"]
            };
            for table in [
                "build_profiles",
                "parts",
                "print_progress",
                "plan_revisions",
                "plan_revision_parts",
                "plan_revision_input_sets",
                "plan_revision_inputs",
                "plan_accepted_input_sets",
                "required_units",
                "plan_revision_required_unit_sets",
                "plan_revision_required_units",
                "plan_drafts",
                "plan_apply_requests",
                "app_settings",
            ] {
                let mut actual = snapshot(&c, table);
                replace_time(&mut actual, &at, &fixture["at"]);
                assert_eq!(
                    actual,
                    state.get(table).cloned().unwrap_or(json!([])),
                    "{} {table}",
                    case["id"]
                );
            }
            assert_accept_event(&c, case, &actual);
            assert_pre_accept_snapshot(&c, case, &actual);
            if actual["kind"] == "checkoff_remap_unsafe" {
                assert_eq!(snapshot(&c, "plan_apply_admissions"), json!([]));
            }
            if actual["kind"] == "applied" {
                let tx = c.transaction().unwrap();
                let mut replay = publish(&tx, case, &mut Tokens(0));
                tx.commit().unwrap();
                replace_time(&mut replay, &at, &fixture["at"]);
                assert_eq!(replay["kind"], "existing");
                assert_eq!(replay["receipt"], actual["receipt"]);
                assert_accept_event(&c, case, &actual);
                assert_pre_accept_snapshot(&c, case, &actual);
                assert!(
                    c.execute(
                        "UPDATE plan_apply_admissions SET request_digest='tampered'",
                        []
                    )
                    .is_err()
                );
                assert!(c.execute("DELETE FROM plan_apply_admissions", []).is_err());
            }
            assert_eq!(
                c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
                0
            );
        }
    }

    #[test]
    fn caller_rollback_and_token_exhaustion_leave_publication_unwritten() {
        let fixture = fixture();
        let case = &fixture["cases"][0];
        let mut c = database(&fixture, case);
        {
            let tx = c.transaction().unwrap();
            assert_eq!(publish(&tx, case, &mut Tokens(0))["kind"], "applied");
        }
        struct Unavailable(usize);
        impl RequiredUnitTokenAllocator for Unavailable {
            fn allocate(&mut self) -> std::result::Result<[u8; 16], TokenAllocationFailure> {
                self.0 += 1;
                Err(TokenAllocationFailure)
            }
        }
        let mut tokens = Unavailable(0);
        let tx = c.transaction().unwrap();
        assert_eq!(
            publish(&tx, case, &mut tokens),
            json!({"kind":"token_allocation_failed"})
        );
        tx.commit().unwrap();
        assert_eq!(tokens.0, 32);
        for (table, rows) in case["before"].as_object().unwrap() {
            assert_eq!(snapshot(&c, table), *rows, "{table}");
        }
        for table in [
            "plan_revisions",
            "parts",
            "plan_apply_requests",
            "plan_apply_admissions",
            "plan_revision_input_sets",
            "required_units",
        ] {
            assert_eq!(snapshot(&c, table), json!([]), "{table}");
        }
    }
}

#[test]
fn publication_job_scope_terminal_states_and_deferred_start_bindings() {
    let golden = golden();
    let fixture = json!({"definitions":golden["transactionSchema"]});
    let cases = golden["transactions"].as_array().unwrap();
    let export = cases.iter().find(|c| c["id"] == "running-export").unwrap();
    let mut c = transaction::database(&fixture, export);
    let tx = c.transaction().unwrap();
    let update = |id: &str, record: &Value| {
        tx.execute(
            "UPDATE durable_jobs SET state=?,document=? WHERE id=?",
            params![record["state"].as_str().unwrap(), record.to_string(), id],
        )
        .unwrap();
    };
    let mut job: Value = serde_json::from_str(
        export["before"]["durable_jobs"][0]["document"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    job["state"] = json!("succeeded");
    update("running-export", &job);
    assert!(
        crate::jobs::publication_conflicts(&tx, "default", 1)
            .unwrap()
            .is_empty()
    );
    job["state"] = json!("running");
    job["payload"]["payload"]["profile_id"] = json!(2);
    update("running-export", &job);
    assert!(
        crate::jobs::publication_conflicts(&tx, "default", 1)
            .unwrap()
            .is_empty()
    );
    tx.rollback().unwrap();

    let upload = cases.iter().find(|c| c["id"] == "uploaded-only").unwrap();
    let mut c = transaction::database(&fixture, upload);
    let tx = c.transaction().unwrap();
    let mut parent: Value = serde_json::from_str(
        upload["before"]["durable_jobs"][0]["document"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(
        crate::jobs::publication_conflicts(&tx, "default", 1)
            .unwrap()
            .is_empty()
    );
    let mut start = parent.clone();
    start["job_id"] = json!("start");
    start["payload"] = json!({"kind":"printer-start","payload":{"uploaded_job_id":"uploaded-only","binding":{"printer_id":"printer","upload_effect":parent["effects"][0]}}});
    start["state"] = json!("running");
    start["effects"] = json!([]);
    start["result"] = Value::Null;
    start["finished_at"] = Value::Null;
    tx.execute("INSERT INTO durable_jobs VALUES ('start','default','printer-upload','running','fixture',1,1,NULL,1,1,?)", [start.to_string()]).unwrap();
    let conflicts = crate::jobs::publication_conflicts(&tx, "default", 1).unwrap();
    assert_eq!(
        conflicts
            .iter()
            .map(|c| c.operation_id.as_str())
            .collect::<Vec<_>>(),
        ["start"]
    );
    assert!(
        crate::jobs::publication_conflicts(&tx, "other", 1)
            .unwrap()
            .is_empty()
    );
    start["payload"]["payload"]["binding"]["printer_id"] = json!("different-printer");
    tx.execute(
        "UPDATE durable_jobs SET document=? WHERE id='start'",
        [start.to_string()],
    )
    .unwrap();
    assert!(crate::jobs::publication_conflicts(&tx, "default", 1).is_err());
    start["payload"]["payload"]["binding"]["printer_id"] = json!("printer");
    tx.execute(
        "UPDATE durable_jobs SET document=? WHERE id='start'",
        [start.to_string()],
    )
    .unwrap();
    parent["payload"]["payload"]["profile_id"] = Value::Null;
    parent["payload"]["payload"]["checkoff_units"] = json!([{"part_id":999,"unit_index":0}]);
    tx.execute(
        "UPDATE durable_jobs SET document=? WHERE id='uploaded-only'",
        [parent.to_string()],
    )
    .unwrap();
    assert_eq!(
        crate::jobs::publication_conflicts(&tx, "default", 1)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn running_supplied_import_is_not_a_publication_conflict() {
    let golden = golden();
    let fixture = json!({"definitions":golden["transactionSchema"]});
    let cases = golden["transactions"].as_array().unwrap();
    let export = cases.iter().find(|c| c["id"] == "running-export").unwrap();
    let mut c = transaction::database(&fixture, export);
    let tx = c.transaction().unwrap();
    assert_eq!(
        crate::jobs::publication_conflicts(&tx, "default", 1)
            .unwrap()
            .len(),
        1
    );
    let mut job: Value = serde_json::from_str(
        export["before"]["durable_jobs"][0]["document"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    job["kind"] = json!("supplied-source-import");
    job["payload"] = json!({"kind":"supplied-source-import","payload":{"project_id":1,"operation_key":"import","input_version":1}});
    tx.execute(
        "UPDATE durable_jobs SET kind='supplied-source-import',document=? WHERE id='running-export'",
        [job.to_string()],
    )
    .unwrap();
    assert!(
        crate::jobs::publication_conflicts(&tx, "default", 1)
            .unwrap()
            .is_empty()
    );
}
