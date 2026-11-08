//! One order is one Build (GRE-1095): Accept and its preview refuse a draft
//! that gives a Build an order number another unarchived Build holds, two
//! racing Accepts of one order on different Builds publish exactly one,
//! duplicates already in copied data are reported and never merged, and MCP
//! proposals obey the same rule.
use super::*;

const APPLY: &str = "/api/v2/plans/1/drafts/2/apply";

/// Saves `order` as draft 2's proposed order number, header version 1.
fn propose_order(server: &ReadServer, order: &str) {
    server
        .sql()
        .execute(
            "INSERT INTO plan_draft_headers (draft_id,tenant_id,name,order_number,customer,due_date,header_version,updated_at) VALUES (2,'default','Bracket kit',?1,NULL,NULL,1,'2026-11-08T12:00:00.000Z')",
            [order],
        )
        .unwrap();
}

fn header_body() -> Value {
    let mut body = apply_body(DIGEST, 0);
    body["expected_header_version"] = json!(1);
    body
}

/// Every Build's id, name and order number, by id.
fn orders(server: &ReadServer) -> Vec<(i64, String, Option<String>)> {
    let connection = server.sql();
    let mut statement = connection
        .prepare("SELECT id,name,order_number FROM build_profiles ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test]
async fn order_authority_accept_and_preview_refuse_an_order_another_build_holds() {
    let server = ReadServer::start(Some("true"), "default").await;
    server
        .sql()
        .execute_batch(
            "INSERT INTO build_profiles(id,tenant_id,name,order_number) VALUES(3,'default','Holder',' po-9 ');
             INSERT INTO build_profiles(id,tenant_id,name,order_number,archived_at) VALUES(4,'default','Archived','PO-9','2026-01-01');
             INSERT INTO build_profiles(id,tenant_id,name,order_number) VALUES(5,'other','Other tenant','PO-9');",
        )
        .unwrap();
    propose_order(&server, "PO-9");
    let before = logical_database(&server.sql());
    let refused = json!({
        "code": "duplicate_order",
        "order_number": "PO-9",
        "builds": [{"profile_id": 3, "name": "Holder", "order_number": " po-9 "}],
    });
    assert_eq!(
        json_reply(keyed(&server, APPLY, Some("o-1")).json(&header_body()), 409).await,
        refused
    );
    assert_eq!(
        json_reply(server.request(Method::GET, PREVIEW), 409).await,
        refused
    );
    assert_eq!(
        logical_database(&server.sql()),
        before,
        "a refusal writes nothing"
    );

    // Archiving the holder frees the order: the same draft now publishes it.
    server
        .sql()
        .execute(
            "UPDATE build_profiles SET archived_at='2026-11-08' WHERE id=3",
            [],
        )
        .unwrap();
    json_reply(keyed(&server, APPLY, Some("o-2")).json(&header_body()), 200).await;
    assert_eq!(orders(&server)[0].2.as_deref(), Some("PO-9"));
    server.close().await;
}

/// ORD-04: two Builds in copied data that already share an order stay as they
/// are. Each read reports the other, a Build keeps the order it already
/// holds through Accept, and nothing merges or renumbers them.
#[tokio::test]
async fn order_authority_reports_existing_duplicates_and_never_merges_them() {
    let server = ReadServer::start_limited(8 * 1024 * 1024, &aligned_setup()).await;
    select_units(&server).await;
    server
        .sql()
        .execute_batch(
            "UPDATE build_profiles SET order_number='#1001' WHERE id=1;
             INSERT INTO build_profiles(id,tenant_id,name,order_number) VALUES(3,'default','Copied twin','#1001 ');",
        )
        .unwrap();
    let copied = orders(&server);
    // The draft keeps the order in another spelling: Accept allows it.
    propose_order(&server, " #1001");
    let preview = json_reply(server.request(Method::GET, PREVIEW), 200).await;
    assert_eq!(
        preview["changes"]["duplicate_orders"],
        json!([{"profile_id": 3, "name": "Copied twin", "order_number": "#1001 "}])
    );
    let header = json_reply(server.request(Method::GET, "/plans/1/drafts/2/header"), 200).await;
    assert_eq!(header["build"]["order_number"], "#1001");
    let mut body = preview["revision"].clone();
    body.as_object_mut().unwrap().remove("draft_id");
    json_reply(keyed(&server, APPLY, Some("o-1")).json(&body), 200).await;
    let after = orders(&server);
    assert_eq!(
        after[0].2.as_deref(),
        Some(" #1001"),
        "copied as the draft holds it"
    );
    assert_eq!(after[1], copied[1], "the twin is untouched");
    assert_eq!(after.len(), copied.len(), "no Build is merged away");
    let server = server.restart().await;
    assert_eq!(orders(&server)[1], copied[1]);
    server.close().await;
}

/// Creates Build 3 with no Parts and an open draft whose header proposes
/// `order`, through the header save; returns the Accept path and body its
/// preview names.
async fn second_build(server: &ReadServer, order: &str) -> (String, Value) {
    server
        .sql()
        .execute_batch(
            "INSERT INTO build_profiles(id,tenant_id,name) VALUES(3,'default','Second Build');
             INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES('default',3,0,'base',1);",
        )
        .unwrap();
    with_source_revision(server);
    std::fs::write(
        server
            .directory
            .join("repos/1/revisions/fixture/second.stl"),
        "solid second\nendsolid second\n",
    )
    .unwrap();
    let saved = json_reply(
        server
            .request(Method::PUT, "/plans/3/drafts/header")
            .header("Idempotency-Key", "second-build")
            .json(&json!({
                "expected_base": {"revision_id": null, "plan_version": 0},
                "expected_draft": null,
                "expected_header_version": null,
                "header": {"name": "Second Build", "order_number": order,
                    "customer": null, "due_date": null},
            })),
        200,
    )
    .await;
    let draft = saved["receipt"]["draft_id"].as_i64().unwrap();
    let workspace = json_reply(
        server.request(Method::GET, &format!("/plans/3/drafts/{draft}")),
        200,
    )
    .await;
    json_reply(
        server
            .request(
                Method::PUT,
                &format!("/api/v2/plans/3/drafts/{draft}/reconciliation"),
            )
            .header("Idempotency-Key", "second-build-units")
            .json(
                &json!({"expected_snapshot_digest": workspace["draft"]["snapshot_digest"],
                "decisions": []}),
            ),
        200,
    )
    .await;
    let preview = json_reply(
        server.request(
            Method::GET,
            &format!("/api/v2/plans/3/drafts/{draft}/accept-preview"),
        ),
        200,
    )
    .await;
    let mut body = preview["revision"].clone();
    body.as_object_mut().unwrap().remove("draft_id");
    (format!("/api/v2/plans/3/drafts/{draft}/apply"), body)
}

/// ORD-01: Accepts of one order number on two Builds, sent at once, publish
/// exactly one; the other answers `duplicate_order` naming the winner and
/// writes nothing of its own. Repeated so the race starts either way round.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn order_authority_two_concurrent_accepts_of_one_order_publish_exactly_one() {
    for round in 0..4 {
        let server = ReadServer::start(Some("true"), "default").await;
        propose_order(&server, "PO-9");
        let (second, second_body) = second_build(&server, "po-9").await;
        let first = keyed(&server, APPLY, Some("race-1")).json(&header_body());
        let other = keyed(&server, &second, Some("race-2")).json(&second_body);
        let (a, b) = if round % 2 == 0 {
            tokio::join!(first.send(), other.send())
        } else {
            let (b, a) = tokio::join!(other.send(), first.send());
            (a, b)
        };
        let (a, b) = (a.unwrap(), b.unwrap());
        let statuses = [a.status().as_u16(), b.status().as_u16()];
        let bodies: [Value; 2] = [a.json().await.unwrap(), b.json().await.unwrap()];
        assert!(
            statuses == [200, 409] || statuses == [409, 200],
            "round {round}: {statuses:?} {bodies:?}"
        );
        let (winner, refused) = if statuses[0] == 200 {
            (1, &bodies[1])
        } else {
            (3, &bodies[0])
        };
        assert_eq!(refused["code"], "duplicate_order", "{refused}");
        assert_eq!(refused["builds"][0]["profile_id"], winner);
        let holders: Vec<i64> = orders(&server)
            .into_iter()
            .filter(|(_, _, order)| {
                order
                    .as_deref()
                    .and_then(pp_storage::order_authority::normalize)
                    .as_deref()
                    == Some("po-9")
            })
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(holders, [winner], "round {round}");
        let loser = if winner == 1 { 3 } else { 1 };
        let revisions: i64 = server
            .sql()
            .query_row(
                "SELECT count(*) FROM plan_revisions WHERE profile_id=?1",
                [loser],
                |row| row.get(0),
            )
            .unwrap();
        // Build 1 starts with revision 1; Build 3 with none.
        assert_eq!(revisions, if loser == 1 { 1 } else { 0 }, "round {round}");
        let count = |sql: &str| -> i64 {
            server
                .sql()
                .query_row(sql, [loser], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(
            count("SELECT count(*) FROM plan_apply_requests WHERE profile_id=?1"),
            0,
            "round {round}"
        );
        assert_eq!(
            count("SELECT count(*) FROM plan_snapshots WHERE profile_id=?1 AND source='pre_apply'"),
            0,
            "round {round}"
        );
        assert_eq!(
            count("SELECT count(*) FROM plan_drafts WHERE profile_id=?1 AND state='open'"),
            1,
            "round {round}: the loser's draft stays open"
        );
        server.close().await;
    }
}

mod mcp {
    use super::super::mcp_accept::{Mcp, brief};
    use super::*;

    /// ORD-05: MCP proposes nothing for a draft that takes a held order, and
    /// a proposal made before another Build took the order is refused at
    /// confirm and stays pending.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn order_authority_mcp_proposals_obey_the_same_rule() {
        let server = ReadServer::start_limited(8 * 1024 * 1024, &aligned_setup()).await;
        select_units(&server).await;
        brief(&server, 2, json!([]), json!({}));
        propose_order(&server, "PO-9");
        let mcp = Mcp::start(&server).await;
        let (failed, proposed) = mcp.propose().await;
        assert!(!failed, "{proposed}");
        let id = proposed["action"]["id"].as_str().unwrap().to_owned();

        server
            .sql()
            .execute(
                "INSERT INTO build_profiles(id,tenant_id,name,order_number) VALUES(3,'default','Holder','po-9')",
                [],
            )
            .unwrap();
        let holder = json!([{"profile_id": 3, "name": "Holder", "order_number": "po-9"}]);
        let before = orders(&server);
        let (failed, refused) = mcp.confirm(&id).await;
        assert!(failed, "{refused}");
        let result = &refused["result"]["apply_result"];
        assert_eq!(result["kind"], "duplicate_order", "{refused}");
        assert_eq!(result["builds"], holder);
        assert_eq!(orders(&server), before);
        let (_, pending) = mcp.call("list_pending_actions", json!({})).await;
        assert_eq!(pending["actions"][0]["id"], id);

        let (failed, again) = mcp.propose().await;
        assert!(failed, "{again}");
        assert_eq!(again["preview_result"]["kind"], "duplicate_order");
        assert_eq!(again["preview_result"]["builds"], holder);
        let (_, pending) = mcp.call("list_pending_actions", json!({})).await;
        assert_eq!(pending["actions"].as_array().unwrap().len(), 1);
        drop(mcp);
        server.close().await;
    }
}
