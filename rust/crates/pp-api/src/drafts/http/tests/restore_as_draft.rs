//! Restore a Plan snapshot as a draft (GRE-1094): the restored draft, the
//! Accept that publishes it, and the refusals.
use super::*;
use rusqlite::params;
use std::collections::BTreeMap;

/// A completed and assembled bracket unit, a completed gear unit, one Checkoff
/// burst, one finished send that checked off the bracket unit and one verified
/// Checkoff link on the gear unit, all of Build 1.
const EVIDENCE_SETUP: &str = "
UPDATE print_progress SET completed=1, assembled=1 WHERE part_id=1 AND unit_index=0;
UPDATE print_progress SET completed=1 WHERE part_id=3 AND unit_index=1;
INSERT INTO print_jobs(id,tenant_id,profile_id,filename,at,printer_id,status)
  VALUES('job-1','default',1,'bracket.3mf','2026-08-21T20:00:00.000Z','sim-1','done');
INSERT INTO print_job_parts(id,tenant_id,job_id,at,profile_id,part_id,unit_index,result,filename,match_key)
  VALUES('jp-1','default','job-1','2026-08-21T20:00:00.000Z',1,1,0,'confirmed','bracket.3mf','bracket.stl');
INSERT INTO app_settings(tenant_id,key,value) VALUES('default','printer.send_queue',
  '[{\"id\":\"send-1\",\"state\":\"done\",\"filename\":\"bracket.3mf\",\"artifact_path\":\"exports/bracket.3mf\",\"printer_id\":\"sim-1\",\"profile_id\":1,\"checkoff_units\":[{\"part_id\":1,\"unit_index\":0}],\"created_at\":\"2026-08-21T19:00:00.000Z\"}]');
INSERT INTO app_settings(tenant_id,key,value) VALUES('default','printer.checkoff_links',
  '[{\"id\":\"link-1\",\"state\":\"verified\",\"profile_id\":1,\"filename\":\"gear.3mf\",\"units\":[{\"part_id\":3,\"unit_index\":1}]}]');
";

/// Snapshots: 1 records Accepted revision 1 of Build 1; 2 is a Node snapshot;
/// 3 belongs to another tenant's Build; 4 to Build 3 but names Build 1's
/// revision.
const SNAPSHOT_SETUP: &str = "
INSERT INTO build_profiles(id,tenant_id,name) VALUES(3,'default','Other Build');
INSERT INTO plan_snapshots(id,tenant_id,profile_id,name,created_at,source,payload_json) VALUES
  (1,'default',1,'Before accepting draft 9','2026-08-21T00:00:00.000Z','pre_apply',
   '{\"layers\":[],\"kit_manifest\":null,\"recipe_summary\":null,\"captured_at\":\"2026-08-21T00:00:00.000Z\",\"accepted_plan_revision_id\":1,\"accepted_plan_version\":1}'),
  (2,'default',1,'Node snapshot','2026-08-21T00:00:00.000Z','user','{\"layers\":[],\"kit_manifest\":null}'),
  (3,'foreign',2,'Foreign','2026-08-21T00:00:00.000Z','pre_apply','{\"accepted_plan_revision_id\":1}'),
  (4,'default',3,'Other','2026-08-21T00:00:00.000Z','pre_apply','{\"accepted_plan_revision_id\":1}');
";

/// A server on [`SNAPSHOT_SETUP`] and a restore body naming its open draft.
async fn snapshot_server() -> (ReadServer, Value) {
    let server = ReadServer::start_limited(8 * 1024 * 1024, SNAPSHOT_SETUP).await;
    (server, restore_body(json!(1), 1, open_draft()))
}

fn restore(
    server: &ReadServer,
    profile: i64,
    snapshot: i64,
    key: Option<&str>,
) -> reqwest::RequestBuilder {
    keyed(
        server,
        &format!("/api/v2/plans/{profile}/snapshots/{snapshot}/restore"),
        key,
    )
}

fn restore_body(revision: Value, version: u32, draft: Value) -> Value {
    json!({"mode": "draft", "expected_base": {"revision_id": revision, "plan_version": version},
        "expected_draft": draft})
}

/// Every row outside the draft tables and the domain event log: the Accepted
/// Plan, its history, Checkoff, sends, Plates and snapshots.
fn accepted_state(server: &ReadServer) -> Vec<(String, Vec<String>)> {
    logical_database(&server.sql())
        .into_iter()
        .filter(|(table, _)| {
            !table.starts_with("plan_draft")
                && !table.starts_with("domain_event")
                && table != "sqlite_sequence"
        })
        .collect()
}

/// Printed evidence and printer commands: Checkoff bursts, the send queue,
/// Checkoff links and Plate history.
fn evidence(server: &ReadServer) -> Vec<(String, Vec<String>)> {
    let tables = [
        "print_jobs",
        "print_job_parts",
        "printer_send_queue",
        "accepted_plate_revisions",
        "accepted_plates",
        "accepted_plate_units",
    ];
    let mut dump: Vec<_> = logical_database(&server.sql())
        .into_iter()
        .filter(|(table, _)| tables.contains(&table.as_str()))
        .collect();
    let connection = server.sql();
    let mut statement = connection
        .prepare("SELECT key,value FROM app_settings WHERE key IN ('printer.send_queue','printer.checkoff_links') ORDER BY key")
        .unwrap();
    let settings = statement
        .query_map([], |row| {
            Ok(format!(
                "{}={}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    dump.push(("app_settings".into(), settings));
    dump
}

/// Each Checkoff coordinate in the send queue (table or setting) and the
/// Checkoff links, by owner, resolved to the Required-unit token at that
/// Part row and unit index in Build 1's revision history. Accept projects
/// each revision onto new Part rows, so a coordinate names one revision's
/// unit.
fn coordinate_tokens(server: &ReadServer) -> Vec<(String, Option<String>)> {
    let connection = server.sql();
    let mut owned: Vec<(String, Value)> = connection
        .prepare(
            "SELECT id,checkoff_units FROM printer_send_queue WHERE checkoff_units IS NOT NULL",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                format!("{}/checkoff_units", row.get::<_, String>(0)?),
                serde_json::from_str(&row.get::<_, String>(1)?).unwrap(),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let settings: Vec<String> = connection
        .prepare("SELECT value FROM app_settings WHERE key IN ('printer.send_queue','printer.checkoff_links')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for setting in settings {
        let items: Vec<Value> = serde_json::from_str(&setting).unwrap();
        for item in items {
            for field in ["checkoff_units", "units", "resolved_units"] {
                if item[field].is_array() {
                    owned.push((
                        format!("{}/{field}", item["id"].as_str().unwrap()),
                        item[field].clone(),
                    ));
                }
            }
        }
    }
    let mut resolved: Vec<_> = owned
        .into_iter()
        .flat_map(|(owner, units)| {
            units
                .as_array()
                .unwrap()
                .iter()
                .map(|unit| {
                    let tokens: Vec<String> = connection
                        .prepare("SELECT DISTINCT m.required_unit_token FROM plan_revisions v JOIN plan_revision_parts r ON r.revision_id=v.id JOIN plan_revision_required_units m ON m.revision_part_id=r.id WHERE v.profile_id=1 AND r.projection_part_id=?1 AND m.unit_index=?2")
                        .unwrap()
                        .query_map(
                            params![unit["part_id"].as_i64(), unit["unit_index"].as_i64()],
                            |row| row.get(0),
                        )
                        .unwrap()
                        .collect::<rusqlite::Result<_>>()
                        .unwrap();
                    assert!(tokens.len() <= 1, "{unit}: {tokens:?}");
                    let token = tokens.into_iter().next();
                    (owner.clone(), token)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    resolved.sort();
    resolved
}

/// The Accepted Plan's Required units by token: Object name, creation time,
/// completed and assembled.
fn units(server: &ReadServer) -> BTreeMap<String, (String, String, i64, i64)> {
    rows(
        server,
        "SELECT u.token,u.object_name,u.created_at,COALESCE(p.completed,0),COALESCE(p.assembled,0) FROM build_profiles b JOIN plan_revision_required_units m ON m.revision_id=b.accepted_plan_revision_id JOIN required_units u ON u.token=m.required_unit_token JOIN plan_revision_parts r ON r.id=m.revision_part_id LEFT JOIN print_progress p ON p.part_id=r.projection_part_id AND p.unit_index=m.unit_index WHERE b.id=1",
        [],
        |row| {
            let unit = (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?);
            Ok((row.get(0)?, unit))
        },
    )
}

/// Part key, quantity, inclusion and artifact of each Part of revision
/// `revision`, by key.
fn revision_parts(server: &ReadServer, revision: i64) -> Vec<(String, i64, i64, Option<String>)> {
    rows(
        server,
        "SELECT part_key,quantity_effective,included,artifact_digest FROM plan_revision_parts WHERE revision_id=?1 ORDER BY part_key",
        [revision],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
}

/// Accepts the workspace's draft through HTTP Accept.
async fn accept_workspace(
    server: &ReadServer,
    workspace: &Value,
    key: &str,
    header: Value,
) -> Value {
    let mut body = revision_of(workspace);
    let draft_id = body.as_object_mut().unwrap().remove("draft_id").unwrap();
    body["expected_header_version"] = header;
    json_reply(
        keyed(
            server,
            &format!("/api/v2/plans/1/drafts/{draft_id}/apply"),
            Some(key),
        )
        .json(&body),
        200,
    )
    .await
}

/// Sets the open draft's bracket quantity and accepts it as revision 2.
async fn accept_bracket_quantity(server: &ReadServer, quantity: u32) -> Value {
    let edit = json!({"expected_snapshot_digest": DIGEST,
        "decision": {"kind": "set_quantity_override", "draft_part_ids": [4], "value": quantity}});
    let workspace = json_reply(
        server
            .request(Method::PATCH, "/api/v2/plans/1/drafts/2/parts")
            .json(&edit),
        200,
    )
    .await;
    let receipt = accept_workspace(server, &workspace, "accept-2", Value::Null).await;
    assert_eq!(receipt["revision_id"], 2);
    receipt
}

/// Restarts the server. The restored draft and its header survive, and a retry
/// of key `r-1` still answers the same draft and writes nothing, while the
/// key with another payload is refused. Answers the reread workspace.
async fn restart_and_replay(
    server: ReadServer,
    body: &Value,
    workspace: &Value,
) -> (ReadServer, Value) {
    let server = server.restart().await;
    let written = logical_database(&server.sql());
    let replay = json_reply(restore(&server, 1, 1, Some("r-1")).json(body), 200).await;
    assert_eq!(replay["draft"], workspace["draft"]);
    assert_eq!(
        json_reply(
            restore(&server, 1, 1, Some("r-1")).json(&restore_body(json!(1), 1, Value::Null)),
            409
        )
        .await,
        json!({"code": "idempotency_conflict"})
    );
    assert_eq!(logical_database(&server.sql()), written);
    let listed = json_reply(server.request(Method::GET, "/plans/1/drafts"), 200).await;
    let open: Vec<_> = listed["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|draft| draft["state"] == "open")
        .map(|draft| draft["draft_id"].clone())
        .collect();
    assert_eq!(open, vec![json!(3)]);
    let reread = json_reply(server.request(Method::GET, "/plans/1/drafts/3"), 200).await;
    assert_eq!(reread["draft"], workspace["draft"]);
    let header_read =
        json_reply(server.request(Method::GET, "/plans/1/drafts/3/header"), 200).await;
    assert_eq!(header_read["draft"]["header"]["name"], "Fixture Build");
    (server, reread)
}

#[tokio::test]
async fn restore_as_draft_creates_a_draft_that_accept_publishes_without_rewinding_evidence() {
    let server = ReadServer::start_limited(8 * 1024 * 1024, EVIDENCE_SETUP).await;
    // Revision 2 drops the third bracket unit; Accept saves snapshot 1 of
    // revision 1 first.
    accept_bracket_quantity(&server, 2).await;
    server.sql().execute_batch(&plate_setup(2)).unwrap();
    let snapshot: (i64, String) = server
        .sql()
        .query_row(
            "SELECT id,source FROM plan_snapshots WHERE profile_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(snapshot, (1, "pre_apply".to_owned()));
    let (accepted, printed, units_before) =
        (accepted_state(&server), evidence(&server), units(&server));
    assert_eq!(units_before.len(), 5);
    // The finished send's and the verified link's Checkoff coordinates name
    // the completed bracket and gear units.
    let checked_off = coordinate_tokens(&server);
    let tokens = |part: &str, completed: i64| -> Vec<String> {
        units_before
            .iter()
            .filter(|(_, (name, _, done, _))| name.starts_with(part) && *done == completed)
            .map(|(token, _)| token.clone())
            .collect()
    };
    assert_eq!(
        checked_off,
        vec![
            ("link-1/units".to_owned(), tokens("gear", 1).pop()),
            (
                "send-1/checkoff_units".to_owned(),
                tokens("bracket", 1).pop()
            ),
        ]
    );
    let body = restore_body(json!(2), 2, Value::Null);

    // RST-01: the restore creates a draft and the Accepted Plan stays put.
    let workspace = json_reply(restore(&server, 1, 1, Some("r-1")).json(&body), 200).await;
    assert_eq!(workspace["draft"]["draft_id"], 3);
    assert_eq!(workspace["draft"]["state"], "open");
    assert_eq!(
        workspace["draft"]["base"],
        json!({"revision_id": 2, "plan_version": 2})
    );
    assert_eq!(workspace["reconciliation"]["kind"], "ready", "{workspace}");
    assert_eq!(accepted_state(&server), accepted);
    assert_eq!(evidence(&server), printed);
    assert_eq!(coordinate_tokens(&server), checked_off);
    // RST-03: each restored Part's predecessor is the revision 2 Part holding
    // its tokens.
    let restored = triples::<String, i64, i64>(
        &server,
        "SELECT d.part_key,d.quantity_effective,r.revision_id FROM plan_draft_parts d JOIN plan_revision_parts r ON r.id=d.base_revision_part_id WHERE d.draft_id=3 AND r.part_key=d.part_key ORDER BY d.part_key",
    );
    assert_eq!(
        restored,
        vec![
            ("bracket.stl".to_owned(), 3, 2),
            ("excluded.stl".to_owned(), 1, 2),
            ("gear.stl".to_owned(), 2, 2),
        ]
    );
    let header: (String, u32) = server
        .sql()
        .query_row(
            "SELECT name,header_version FROM plan_draft_headers WHERE draft_id=3",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(header, ("Fixture Build".to_owned(), 1));

    let (server, reread) = restart_and_replay(server, &body, &workspace).await;
    assert_eq!(accepted_state(&server), accepted);
    assert_eq!(coordinate_tokens(&server), checked_off);

    // Accept publishes revision 1's Parts as revision 3.
    let receipt = accept_workspace(&server, &reread, "accept-3", json!(1)).await;
    assert_eq!(receipt["revision_id"], 3);
    assert_eq!(revision_parts(&server, 3), revision_parts(&server, 1));
    // RST-02: Checkoff, sends and Plate history are unchanged, their Checkoff
    // coordinates name the same Required units, and every surviving unit
    // keeps its token, name and progress.
    assert_eq!(evidence(&server), printed);
    assert_eq!(coordinate_tokens(&server), checked_off);
    let after = units(&server);
    for (token, unit) in &units_before {
        assert_eq!(after.get(token), Some(unit), "{token}");
    }
    // The units the send and the link checked off are still in the Plan and
    // still completed.
    for (owner, token) in &checked_off {
        assert_eq!(after[token.as_ref().unwrap()].2, 1, "{owner}");
    }
    // RST-03: the regrown bracket unit is new; no token is reused.
    let added: Vec<_> = after
        .keys()
        .filter(|token| !units_before.contains_key(*token))
        .collect();
    assert_eq!(added.len(), 1, "{after:?}");
    assert!(
        !["ppu_00000000000000000000000000000003"].contains(&added[0].as_str()),
        "{added:?}"
    );
    assert_eq!(after[added[0]].2, 0);
    server.close().await;
}

#[tokio::test]
async fn restore_as_draft_refuses_other_builds_snapshots_and_stale_state_without_writing() {
    let (server, body) = snapshot_server().await;
    let before = logical_database(&server.sql());
    let refusals = [
        // RST-04: another tenant's and another Build's snapshots are not found.
        (
            (1, 3),
            body.clone(),
            404,
            json!({"code": "snapshot_not_found"}),
        ),
        (
            (1, 4),
            body.clone(),
            404,
            json!({"code": "snapshot_not_found"}),
        ),
        (
            (1, 99),
            body.clone(),
            404,
            json!({"code": "snapshot_not_found"}),
        ),
        (
            (2, 3),
            body.clone(),
            404,
            json!({"code": "profile_not_found"}),
        ),
        // Build 3's own snapshot names Build 1's revision.
        (
            (3, 4),
            restore_body(Value::Null, 0, Value::Null),
            409,
            json!({"code": "snapshot_not_restorable"}),
        ),
        // A Node snapshot holds no Accepted revision.
        (
            (1, 2),
            body.clone(),
            409,
            json!({"code": "snapshot_not_restorable"}),
        ),
        (
            (1, 1),
            restore_body(json!(1), 2, open_draft()),
            409,
            json!({"code": "base_changed"}),
        ),
        (
            (1, 1),
            restore_body(json!(1), 1, Value::Null),
            409,
            json!({"code": "draft_changed"}),
        ),
    ];
    for (index, ((profile, snapshot), body, status, expected)) in refusals.into_iter().enumerate() {
        let key = format!("refused-{index}");
        let mut reply = json_reply(
            restore(&server, profile, snapshot, Some(&key)).json(&body),
            status,
        )
        .await;
        let reply = reply.as_object_mut().unwrap();
        reply.remove("detail");
        reply.remove("status");
        reply.remove("title");
        reply.remove("type");
        assert_eq!(
            Value::Object(reply.clone()),
            expected,
            "{profile}/{snapshot}"
        );
    }
    assert_eq!(logical_database(&server.sql()), before);
    server.close().await;
}

#[tokio::test]
async fn restore_as_draft_refuses_invalid_requests_and_changed_layers() {
    let (server, body) = snapshot_server().await;
    let before = logical_database(&server.sql());
    // Without a key, or in another mode, the request is invalid.
    let response = restore(&server, 1, 1, None)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let mut direct = body.clone();
    direct["mode"] = json!("direct");
    let response = restore(&server, 1, 1, Some("direct"))
        .json(&direct)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    // Source layers that differ from the snapshot's cannot be accepted.
    server
        .sql()
        .execute(
            "UPDATE profile_layers SET layer_order=5 WHERE profile_id=1",
            [],
        )
        .unwrap();
    let moved = logical_database(&server.sql());
    assert_eq!(
        json_reply(restore(&server, 1, 1, Some("layers")).json(&body), 409).await,
        json!({"code": "inputs_changed"})
    );
    assert_eq!(logical_database(&server.sql()), moved);
    server
        .sql()
        .execute(
            "UPDATE profile_layers SET layer_order=0 WHERE profile_id=1",
            [],
        )
        .unwrap();
    assert_eq!(logical_database(&server.sql()), before);
    server.close().await;
}

/// The observed open draft is abandoned for the restored one, which has no
/// header row because the snapshot saved none.
#[tokio::test]
async fn restore_as_draft_abandons_the_observed_open_draft() {
    let (server, body) = snapshot_server().await;
    let workspace = json_reply(restore(&server, 1, 1, Some("restored")).json(&body), 200).await;
    assert_eq!(workspace["draft"]["draft_id"], 3);
    let states = triples::<i64, String, i64>(
        &server,
        "SELECT id,state,lifecycle_version FROM plan_drafts WHERE profile_id=1 AND id>1 ORDER BY id",
    );
    assert_eq!(
        states,
        vec![(2, "abandoned".to_owned(), 1), (3, "open".to_owned(), 0)]
    );
    let bases = triples::<String, i64, i64>(
        &server,
        "SELECT part_key,base_revision_part_id,quantity_effective FROM plan_draft_parts WHERE draft_id=3 ORDER BY id",
    );
    assert_eq!(
        bases,
        vec![
            ("bracket.stl".to_owned(), 1, 3),
            ("excluded.stl".to_owned(), 2, 1),
            ("gear.stl".to_owned(), 3, 2),
        ]
    );
    let headers: i64 = server
        .sql()
        .query_row("SELECT count(*) FROM plan_draft_headers", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(headers, 0);
    server.close().await;
}

/// A failure between the draft commit and its selection, as a crash there
/// leaves it; the draft is then abandoned. The replay answers with the closed
/// draft and selects nothing.
#[tokio::test]
async fn restore_as_draft_replays_a_draft_closed_before_its_selection() {
    let (server, body) = snapshot_server().await;
    server
        .sql()
        .execute_batch(
            "CREATE TRIGGER fail_selection BEFORE INSERT ON plan_draft_required_unit_reconciliations
             BEGIN SELECT RAISE(ABORT, 'selection interrupted'); END;",
        )
        .unwrap();
    let interrupted = restore(&server, 1, 1, Some("interrupted"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(interrupted.status().as_u16(), 500);
    server
        .sql()
        .execute_batch(
            "DROP TRIGGER fail_selection;
             UPDATE plan_drafts SET state='abandoned', lifecycle_version=1 WHERE id=3 AND current_required_unit_reconciliation_id IS NULL;",
        )
        .unwrap();
    let before = logical_database(&server.sql());
    let replay = json_reply(restore(&server, 1, 1, Some("interrupted")).json(&body), 200).await;
    assert_eq!(replay["draft"]["draft_id"], 3);
    assert_eq!(replay["draft"]["state"], "abandoned");
    assert_eq!(logical_database(&server.sql()), before);
    server.close().await;
}

#[tokio::test]
async fn restore_as_draft_refuses_to_drop_printed_units() {
    let server = ReadServer::start(Some("true"), "default").await;
    // Revision 2 grows the bracket to 4 units and all of them are printed.
    accept_bracket_quantity(&server, 4).await;
    server
        .sql()
        .execute(
            "UPDATE print_progress SET completed=1 WHERE part_id=(SELECT projection_part_id FROM plan_revision_parts WHERE revision_id=2 AND part_key='bracket.stl')",
            [],
        )
        .unwrap();
    let bracket: Vec<String> = units(&server)
        .into_iter()
        .filter(|(_, (name, _, completed, _))| name.starts_with("bracket") && *completed == 1)
        .map(|(token, _)| token)
        .collect();
    assert_eq!(bracket.len(), 4);
    let before = logical_database(&server.sql());
    assert_eq!(
        json_reply(
            restore(&server, 1, 1, Some("printed")).json(&restore_body(json!(2), 2, Value::Null)),
            409
        )
        .await,
        json!({"code": "printed_units_affected", "tokens": bracket})
    );
    assert_eq!(logical_database(&server.sql()), before);
    server.close().await;
}

/// A snapshot header copied from a Node Build stores an unset field as empty
/// text, which reads as none. A field a header save would refuse refuses the
/// restore before any write.
#[tokio::test]
async fn restore_as_draft_checks_the_snapshot_header_as_a_header_save_does() {
    let (server, body) = snapshot_server().await;
    let header = |name: &str, due_date: &str| {
        server
            .sql()
            .execute(
                "INSERT OR REPLACE INTO plan_snapshot_headers(snapshot_id,tenant_id,name,order_number,customer,due_date,captured_at) VALUES(1,'default',?1,'','  ',?2,'2026-08-21T00:00:00.000Z')",
                params![name, due_date],
            )
            .unwrap();
    };
    for (name, due_date, field, reason) in [
        (" ", "", "name", "text_length"),
        ("Fixture Build", "2026-02-30", "due_date", "calendar_date"),
    ] {
        header(name, due_date);
        let before = logical_database(&server.sql());
        assert_eq!(
            json_reply(restore(&server, 1, 1, Some(field)).json(&body), 422).await,
            json!({"code": "invalid_header", "field": field, "reason": reason})
        );
        assert_eq!(logical_database(&server.sql()), before);
    }
    header(" Fixture Build ", "");
    let workspace = json_reply(restore(&server, 1, 1, Some("blank")).json(&body), 200).await;
    let draft = workspace["draft"]["draft_id"].clone();
    let saved = json_reply(
        server.request(Method::GET, &format!("/plans/1/drafts/{draft}/header")),
        200,
    )
    .await;
    assert_eq!(
        saved["draft"]["header"],
        json!({"name": "Fixture Build", "order_number": null, "customer": null, "due_date": null})
    );
    server.close().await;
}

/// A current Accepted revision older than Required units has no set, so the
/// restore answers the selection's refusal and the open draft stays open.
#[tokio::test]
async fn restore_as_draft_refuses_without_a_required_unit_set() {
    let (server, body) = snapshot_server().await;
    server
        .sql()
        .execute_batch(
            "DROP TRIGGER trg_required_units_immutable_delete;
             DROP TRIGGER trg_plan_revision_required_units_immutable_delete;
             DROP TRIGGER trg_plan_revision_required_unit_sets_immutable_delete;
             DELETE FROM plan_revision_required_unit_sets WHERE revision_id=1;
             DELETE FROM plan_revision_required_units WHERE revision_id=1;
             DELETE FROM required_units WHERE created_in_revision_id=1;",
        )
        .unwrap();
    let before = logical_database(&server.sql());
    assert_eq!(
        json_reply(restore(&server, 1, 1, Some("no-set")).json(&body), 422).await,
        json!({"code": "required_unit_set_unavailable"})
    );
    assert_eq!(logical_database(&server.sql()), before);
    server.close().await;
}

/// Draft 2's planning digest with gear's role override, pinned; a wrong value
/// fails the setup's PATCH with `500`.
const CURATED_DIGEST: &str = "734ffd8108a04378643317831807935a026f15a4a6ea9df0b5d380f92ae21f63";

/// Draft 2's gear Part gets a role override, resealed with
/// [`CURATED_DIGEST`], and its live accepted Part a filament and spool.
fn curated_setup() -> String {
    format!(
        "
UPDATE plan_draft_parts SET role_override='accent' WHERE id=6;
UPDATE plan_drafts SET snapshot_digest='{CURATED_DIGEST}' WHERE id=2;
UPDATE parts SET filament_color_id='red',filament_custom_hex='#ff0000',spoolman_spool_id='7' WHERE id=3;
"
    )
}

/// Part key, inclusion, role override, filament color, custom hex and
/// Spoolman spool of each Part of `table` whose `owner` column is `id`, by
/// key.
fn curated(server: &ReadServer, table: &str, owner: &str, id: i64) -> Value {
    let parts: Vec<String> = rows(
        server,
        &format!(
            "SELECT json_array(part_key,included,role_override,filament_color_id,filament_custom_hex,spoolman_spool_id) FROM {table} WHERE {owner}=?1 ORDER BY part_key"
        ),
        [id],
        |row| row.get(0),
    );
    parts
        .iter()
        .map(|part| serde_json::from_str::<Value>(part).unwrap())
        .collect()
}

fn revision_curated(server: &ReadServer, revision: i64) -> Value {
    curated(server, "plan_revision_parts", "revision_id", revision)
}

fn draft_curated(server: &ReadServer, draft: i64) -> Value {
    curated(server, "plan_draft_parts", "draft_id", draft)
}

/// Selects Required units for the workspace's draft when it is unresolved,
/// replacing the units of each Part whose predecessor is unsafe, and answers
/// the ready workspace.
async fn resolve(server: &ReadServer, workspace: &Value, key: &str) -> Value {
    if workspace["reconciliation"]["kind"] == "ready" {
        return workspace.clone();
    }
    let draft = &workspace["draft"];
    let decisions: Vec<Value> = workspace["reconciliation"]["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|conflict| {
            assert_eq!(conflict["kind"], "unsafe_predecessor", "{conflict}");
            json!({"kind": "replace", "target_draft_part_id": conflict["target_draft_part_id"]})
        })
        .collect();
    let selected = json_reply(
        reconcile_request(
            server,
            &format!(
                "/api/v2/plans/1/drafts/{}/reconciliation",
                draft["draft_id"]
            ),
            Some(key),
        )
        .json(
            &json!({"expected_snapshot_digest": draft["snapshot_digest"], "decisions": decisions}),
        ),
        200,
    )
    .await;
    assert_eq!(selected["reconciliation"]["kind"], "ready", "{selected}");
    selected
}

/// GRE-479: a snapshot keeps each Part's inclusion, role override and
/// filament, and restoring it brings them back into the draft and, through
/// Accept, into the Accepted Plan.
#[tokio::test]
async fn restore_as_draft_brings_back_part_selection_and_filament_roles() {
    let server = ReadServer::start_limited(8 * 1024 * 1024, &curated_setup()).await;
    // Revision 2 includes the excluded Part, makes gear an accent Part and
    // publishes its live red filament on spool 7: after the `replace`
    // decision, draft 2's gear still has revision 1's gear as its base, so
    // Accept takes that Part's live filament. Accept first saves snapshot 1
    // of revision 1.
    let edit = json!({"expected_snapshot_digest": CURATED_DIGEST,
        "decision": {"kind": "set_included", "draft_part_ids": [5], "value": true}});
    let workspace = json_reply(
        server
            .request(Method::PATCH, "/api/v2/plans/1/drafts/2/parts")
            .json(&edit),
        200,
    )
    .await;
    let workspace = resolve(&server, &workspace, "select-2").await;
    let second = accept_workspace(&server, &workspace, "accept-2", Value::Null).await;
    assert_eq!(second["revision_id"], 2);
    let first_parts = json!([
        ["bracket.stl", 1, null, null, null, null],
        ["excluded.stl", 0, null, null, null, null],
        ["gear.stl", 1, null, null, null, null]
    ]);
    let second_parts = json!([
        ["bracket.stl", 1, null, null, null, null],
        ["excluded.stl", 1, null, null, null, null],
        ["gear.stl", 1, "accent", "red", "#ff0000", "7"]
    ]);
    assert_eq!(revision_curated(&server, 1), first_parts);
    assert_eq!(revision_curated(&server, 2), second_parts);

    // Restoring snapshot 1 drafts revision 1's selection and roles, and Accept
    // publishes them as revision 3, saving snapshot 2 of revision 2 first.
    let workspace = json_reply(
        restore(&server, 1, 1, Some("r-1")).json(&restore_body(json!(2), 2, Value::Null)),
        200,
    )
    .await;
    assert_eq!(workspace["draft"]["draft_id"], 3);
    assert_eq!(draft_curated(&server, 3), first_parts);
    let workspace = resolve(&server, &workspace, "select-3").await;
    let third = accept_workspace(&server, &workspace, "accept-3", json!(1)).await;
    assert_eq!(third["revision_id"], 3);
    assert_eq!(revision_curated(&server, 3), first_parts);
    assert_eq!(revision_parts(&server, 3), revision_parts(&server, 1));
    let recorded: i64 = server
        .sql()
        .query_row(
            "SELECT json_extract(payload_json,'$.accepted_plan_revision_id') FROM plan_snapshots WHERE id=2 AND profile_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 2);

    // The current gear Part moves to blue on spool 9. Revision 3's gear had no
    // base and got new tokens, so no current Part holds revision 2's gear
    // tokens: restored gear has no predecessor and keeps the snapshot's
    // filament instead of taking the live one.
    let moved = server
        .sql()
        .execute(
            "UPDATE parts SET filament_color_id='blue',filament_custom_hex=NULL,spoolman_spool_id='9' WHERE id=(SELECT projection_part_id FROM plan_revision_parts WHERE revision_id=3 AND part_key='gear.stl')",
            [],
        )
        .unwrap();
    assert_eq!(moved, 1);

    // Restoring snapshot 2 brings back the included Part, the accent role and
    // the red filament on spool 7, in the draft and in revision 4.
    let workspace = json_reply(
        restore(&server, 1, 2, Some("r-2")).json(&restore_body(json!(3), 3, Value::Null)),
        200,
    )
    .await;
    assert_eq!(workspace["draft"]["draft_id"], 4);
    assert_eq!(draft_curated(&server, 4), second_parts);
    let gear_base: Option<i64> = server
        .sql()
        .query_row(
            "SELECT base_revision_part_id FROM plan_draft_parts WHERE draft_id=4 AND part_key='gear.stl'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(gear_base, None);
    let workspace = resolve(&server, &workspace, "select-4").await;
    let fourth = accept_workspace(&server, &workspace, "accept-4", json!(1)).await;
    assert_eq!(fourth["revision_id"], 4);
    assert_eq!(revision_curated(&server, 4), second_parts);
    assert_eq!(revision_parts(&server, 4), revision_parts(&server, 2));
    assert_eq!(fourth["revision_digest"], second["revision_digest"]);
    server.close().await;
}
