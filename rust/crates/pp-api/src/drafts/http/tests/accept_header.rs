//! Accept copies the draft header to the Build (GRE-1207): the copy, its
//! preview, the previous header beside the pre-Accept snapshot, and the
//! Build-name refusal.
use super::*;

const APPLY: &str = "/api/v2/plans/1/drafts/2/apply";

fn propose(server: &ReadServer, name: &str) {
    server
        .sql()
        .execute(
            "INSERT INTO plan_draft_headers (draft_id,tenant_id,name,order_number,customer,due_date,header_version,updated_at) VALUES (2,'default',?1,'PO-9','Acme','2026-12-01',1,'2026-11-06T12:00:00.000Z')",
            [name],
        )
        .unwrap();
}

/// An Accept body naming header version `version`, the one `propose` saves.
fn header_body(version: u32) -> Value {
    let mut body = apply_body(DIGEST, 0);
    body["expected_header_version"] = json!(version);
    body
}

/// The Build's name, order number, customer and due date as stored.
fn build_header(server: &ReadServer) -> Header {
    server
        .sql()
        .query_row(
            "SELECT p.name,p.order_number,d.customer,d.due_date FROM build_profiles p LEFT JOIN build_profile_details d ON d.profile_id=p.id WHERE p.id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

type Header = (String, Option<String>, Option<String>, Option<String>);

/// Every saved pre-Accept header: snapshot, name, order number, customer and
/// due date.
fn snapshot_headers(server: &ReadServer) -> Vec<(i64, Header)> {
    let connection = server.sql();
    let mut statement = connection
        .prepare("SELECT h.snapshot_id,h.name,h.order_number,h.customer,h.due_date FROM plan_snapshot_headers h JOIN plan_snapshots s ON s.id=h.snapshot_id AND s.source='pre_apply' ORDER BY h.snapshot_id")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test]
async fn gre_1207_accept_copies_the_draft_header_to_the_build_once() {
    let server = ReadServer::start(Some("true"), "default").await;
    let before = build_header(&server);
    propose(&server, "Renamed kit");
    let receipt = json_reply(
        keyed(&server, APPLY, Some("h-1")).json(&header_body(1)),
        200,
    )
    .await;
    assert_eq!(receipt["revision_id"], 2);
    let accepted = (
        "Renamed kit".to_owned(),
        Some("PO-9".to_owned()),
        Some("Acme".to_owned()),
        Some("2026-12-01".to_owned()),
    );
    assert_eq!(build_header(&server), accepted);
    let saved = snapshot_headers(&server);
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert_eq!(saved[0].1, before);

    // A replay answers the receipt and writes nothing twice.
    let after = logical_database(&server.sql());
    assert_eq!(
        json_reply(
            keyed(&server, APPLY, Some("h-1")).json(&header_body(1)),
            200
        )
        .await,
        receipt
    );
    assert_eq!(logical_database(&server.sql()), after);

    let server = server.restart().await;
    assert_eq!(build_header(&server), accepted);
    let read = json_reply(server.request(Method::GET, "/plans/1/drafts/2/header"), 200).await;
    assert_eq!(
        read["build"],
        json!({"name":"Renamed kit","order_number":"PO-9","customer":"Acme","due_date":"2026-12-01"})
    );
    server.close().await;
}

#[tokio::test]
async fn gre_1207_accept_without_a_header_row_leaves_the_build_header() {
    let server = ReadServer::start(Some("true"), "default").await;
    let before = build_header(&server);
    json_reply(
        keyed(&server, APPLY, Some("h-1")).json(&apply_body(DIGEST, 0)),
        200,
    )
    .await;
    assert_eq!(build_header(&server), before);
    let details: i64 = server
        .sql()
        .query_row("SELECT count(*) FROM build_profile_details", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(details, 0);
    server.close().await;
}

#[tokio::test]
async fn gre_1207_accept_refuses_a_name_another_build_has_without_writing() {
    let server = ReadServer::start(Some("true"), "default").await;
    server
        .sql()
        .execute(
            "INSERT INTO build_profiles(tenant_id,name) VALUES('default','Taken')",
            [],
        )
        .unwrap();
    propose(&server, "Taken");
    let before = logical_database(&server.sql());
    assert_eq!(
        json_reply(
            keyed(&server, APPLY, Some("h-1")).json(&header_body(1)),
            409
        )
        .await,
        json!({"code":"duplicate_build_name","name":"Taken"})
    );
    assert_eq!(
        json_reply(server.request(Method::GET, PREVIEW), 409).await,
        json!({"code":"duplicate_build_name","name":"Taken"})
    );
    assert_eq!(logical_database(&server.sql()), before);
    server.close().await;
}

#[tokio::test]
async fn gre_1207_preview_lists_a_header_only_change_as_changed() {
    let setup = aligned_setup();
    let server = ReadServer::start_limited(8 * 1024 * 1024, &setup).await;
    let workspace = select_units(&server).await;
    assert_eq!(workspace["reconciliation"]["kind"], "ready");
    let (name, order, _, _) = build_header(&server);
    propose(&server, "Renamed kit");
    let preview = json_reply(server.request(Method::GET, PREVIEW), 200).await;
    assert_eq!(preview["kind"], "changed");
    assert_eq!(preview["changes"]["fields"], json!([]));
    let mut expected = vec![json!({"field":"name","old":name,"new":"Renamed kit"})];
    if order.as_deref() != Some("PO-9") {
        expected.push(json!({"field":"order_number","old":order,"new":"PO-9"}));
    }
    expected.push(json!({"field":"customer","old":null,"new":"Acme"}));
    expected.push(json!({"field":"due_date","old":null,"new":"2026-12-01"}));
    assert_eq!(preview["changes"]["header"], json!(expected));
    server.close().await;
}

#[tokio::test]
async fn gre_1207_accept_refuses_a_header_saved_after_the_preview() {
    let setup = aligned_setup();
    let server = ReadServer::start_limited(8 * 1024 * 1024, &setup).await;
    select_units(&server).await;
    propose(&server, "Previewed kit");
    let preview = json_reply(server.request(Method::GET, PREVIEW), 200).await;
    let mut body = preview["revision"].clone();
    assert_eq!(body["expected_header_version"], 1);
    body.as_object_mut().unwrap().remove("draft_id");
    server
        .sql()
        .execute(
            "UPDATE plan_draft_headers SET name='Unreviewed kit',header_version=2 WHERE draft_id=2",
            [],
        )
        .unwrap();
    let before = logical_database(&server.sql());
    let mut unversioned = body.clone();
    unversioned
        .as_object_mut()
        .unwrap()
        .remove("expected_header_version");
    for (key, stale) in [("h-1", &body), ("h-2", &unversioned)] {
        let refused = json_reply(keyed(&server, APPLY, Some(key)).json(stale), 409).await;
        assert_eq!(refused["code"], "draft_changed");
    }
    assert_eq!(logical_database(&server.sql()), before);
    body["expected_header_version"] = json!(2);
    json_reply(keyed(&server, APPLY, Some("h-3")).json(&body), 200).await;
    assert_eq!(build_header(&server).0, "Unreviewed kit");
    server.close().await;
}
