//! PUT /plans/{id}/drafts/header (GRE-1197): the draft header save, its
//! replay and refusals, the header read, and carry-forward.
use super::*;
use rusqlite::OptionalExtension;

const PATH: &str = "/plans/1/drafts/header";
const HEADER: &str = "/plans/1/drafts/2/header";

fn put(server: &ReadServer, key: Option<&str>, body: &Value) -> reqwest::RequestBuilder {
    let request = server.request(Method::PUT, PATH).json(body);
    match key {
        Some(key) => request.header("Idempotency-Key", key),
        None => request,
    }
}

fn header(name: &str, order: Value, customer: Value, due: Value) -> Value {
    json!({"name": name, "order_number": order, "customer": customer, "due_date": due})
}

fn body(draft: Value, version: Value, header: Value) -> Value {
    json!({
        "expected_base": {"revision_id": 1, "plan_version": 1},
        "expected_draft": draft,
        "expected_header_version": version,
        "header": header,
    })
}

fn acme() -> Value {
    header(
        " Renamed build ",
        json!("PO-7"),
        json!("Acme"),
        json!("2026-12-01"),
    )
}

/// The Build's stored header, as the workspace reports it before Accept.
fn build_header(server: &ReadServer) -> Value {
    server
        .sql()
        .query_row(
            "SELECT name,order_number FROM build_profiles WHERE id=1",
            [],
            |row| {
                Ok(header(
                    &row.get::<_, String>(0)?,
                    json!(row.get::<_, Option<String>>(1)?),
                    Value::Null,
                    Value::Null,
                ))
            },
        )
        .unwrap()
}

fn draft_header(server: &ReadServer, draft: i64) -> Option<(String, Option<String>, i64)> {
    server
        .sql()
        .query_row(
            "SELECT name,due_date,header_version FROM plan_draft_headers WHERE draft_id=?",
            [draft],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .unwrap()
}

#[tokio::test]
async fn gre_1197_header_save_writes_the_open_draft_once_and_never_reaches_the_build() {
    let server = ReadServer::start(Some("true"), "default").await;
    let accepted = accepted_rows(&server);
    let build = build_header(&server);
    let workspace = json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await;
    assert_eq!(
        json_reply(server.request(Method::GET, HEADER), 200).await,
        json!({"draft_id": 2, "build": build, "draft": null})
    );

    let open = observed(&json!(DIGEST));
    let saved = json_reply(
        put(
            &server,
            Some(" h-1 "),
            &body(open.clone(), Value::Null, acme()),
        ),
        200,
    )
    .await;
    let receipt = &saved["receipt"];
    assert_eq!(
        (&receipt["profile_id"], &receipt["draft_id"]),
        (&json!(1), &json!(2))
    );
    assert_eq!(receipt["header"]["header_version"], 1);
    assert_eq!(
        receipt["header"]["header"],
        header(
            "Renamed build",
            json!("PO-7"),
            json!("Acme"),
            json!("2026-12-01")
        )
    );
    assert_eq!(
        saved["header"],
        json!({"draft_id": 2, "build": build, "draft": receipt["header"]})
    );
    assert_eq!(
        json_reply(server.request(Method::GET, HEADER), 200).await,
        saved["header"]
    );
    assert_eq!(
        json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await,
        workspace
    );
    assert_eq!(accepted_rows(&server), accepted);
    assert_eq!(build_header(&server), build);

    // A retry under the same key, or the same header under a new key that
    // observed no header, writes nothing and answers the saved header.
    let after = logical_database(&server.sql());
    let events = draft_events(&server);
    for key in ["h-1", "h-2"] {
        assert_eq!(
            json_reply(
                put(&server, Some(key), &body(open.clone(), Value::Null, acme())),
                200
            )
            .await,
            saved
        );
    }
    assert_eq!(logical_database(&server.sql()), after);
    assert_eq!(draft_events(&server), events);

    // A different header that observed an older version is refused.
    let other = header("Other", Value::Null, Value::Null, Value::Null);
    assert_eq!(
        json_reply(
            put(
                &server,
                Some("h-3"),
                &body(open.clone(), Value::Null, other.clone())
            ),
            409
        )
        .await,
        json!({"code": "header_changed", "header": receipt["header"]})
    );
    assert_eq!(logical_database(&server.sql()), after);

    let next = json_reply(
        put(&server, Some("h-4"), &body(open, json!(1), other.clone())),
        200,
    )
    .await;
    assert_eq!(next["receipt"]["draft_id"], 2);
    assert_eq!(next["receipt"]["header"]["header_version"], 2);
    assert_eq!(next["receipt"]["header"]["header"], other);
    assert_eq!(accepted_rows(&server), accepted);

    let server = server.restart().await;
    let reread = json_reply(server.request(Method::GET, HEADER), 200).await;
    assert_eq!(reread, next["header"]);
    assert_eq!(reread["draft"], next["receipt"]["header"]);
    assert_eq!(reread["build"], build);
    assert_eq!(accepted_rows(&server), accepted);
    server.close().await;
}

#[tokio::test]
async fn gre_1197_header_save_creates_a_draft_when_none_is_open() {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    json_reply(
        server
            .request(Method::POST, "/plans/1/drafts/2/abandon")
            .json(&abandon_body(0)),
        200,
    )
    .await;
    let accepted = accepted_rows(&server);
    let saved = json_reply(
        put(
            &server,
            Some("n-1"),
            &body(Value::Null, Value::Null, acme()),
        ),
        200,
    )
    .await;
    let draft = saved["receipt"]["draft_id"].as_i64().unwrap();
    assert!(draft > 2, "{saved}");
    let workspace = json_reply(
        server.request(Method::GET, &format!("/plans/1/drafts/{draft}")),
        200,
    )
    .await;
    assert_eq!(workspace["draft"]["state"], "open");
    assert_eq!(saved["header"]["draft"], saved["receipt"]["header"]);
    assert_eq!(
        draft_header(&server, draft),
        Some(("Renamed build".into(), Some("2026-12-01".into()), 1))
    );
    assert_eq!(accepted_rows(&server), accepted);

    let after = logical_database(&server.sql());
    assert_eq!(
        json_reply(
            put(
                &server,
                Some("n-1"),
                &body(Value::Null, Value::Null, acme())
            ),
            200
        )
        .await,
        saved
    );
    let other = header("Other", Value::Null, Value::Null, Value::Null);
    assert_eq!(
        json_reply(
            put(&server, Some("n-1"), &body(Value::Null, Value::Null, other)),
            409
        )
        .await,
        json!({"code": "idempotency_conflict"})
    );
    assert_eq!(
        json_reply(
            put(
                &server,
                Some("n-2"),
                &body(Value::Null, Value::Null, acme())
            ),
            409
        )
        .await,
        json!({"code": "draft_changed"})
    );
    assert_eq!(logical_database(&server.sql()), after);
    server.close().await;
}

#[tokio::test]
async fn gre_1197_header_save_refuses_bad_headers_versions_bases_and_keys_without_writing() {
    let server = ReadServer::start(Some("true"), "default").await;
    let before = logical_database(&server.sql());
    let open = observed(&json!(DIGEST));
    let invalid = |field: &str, reason: &str| json!({"code": "invalid_header", "field": field, "reason": reason});
    let cases = [
        (
            header(" \u{a0}", Value::Null, Value::Null, Value::Null),
            invalid("name", "text_length"),
        ),
        (
            header(&"n".repeat(4097), Value::Null, Value::Null, Value::Null),
            invalid("name", "text_length"),
        ),
        (
            header("Build", json!(" "), Value::Null, Value::Null),
            invalid("order_number", "text_length"),
        ),
        (
            header("Build", Value::Null, json!(""), Value::Null),
            invalid("customer", "text_length"),
        ),
        (
            header("Build", Value::Null, Value::Null, json!("2026-02-30")),
            invalid("due_date", "calendar_date"),
        ),
        (
            header("Build", Value::Null, Value::Null, json!("2026-1-05")),
            invalid("due_date", "calendar_date"),
        ),
    ];
    for (index, (bad, expected)) in cases.into_iter().enumerate() {
        let key = format!("bad-{index}");
        assert_eq!(
            json_reply(
                put(&server, Some(&key), &body(open.clone(), Value::Null, bad)),
                422
            )
            .await,
            expected
        );
    }
    assert_eq!(
        json_reply(
            put(&server, Some("v-1"), &body(open.clone(), json!(1), acme())),
            409
        )
        .await,
        json!({"code": "header_changed", "header": null})
    );
    let mut stale = body(open.clone(), Value::Null, acme());
    stale["expected_base"] = json!({"revision_id": null, "plan_version": 0});
    assert_eq!(
        json_reply(put(&server, Some("b-1"), &stale), 409).await,
        json!({"code": "base_changed"})
    );
    assert_eq!(
        json_reply(
            put(&server, None, &body(open.clone(), Value::Null, acme())),
            400
        )
        .await,
        json!({"detail":"Request is invalid","code":"invalid_request"})
    );
    let mut missing = body(open.clone(), Value::Null, acme());
    missing["header"]
        .as_object_mut()
        .unwrap()
        .remove("due_date");
    json_reply(put(&server, Some("m-1"), &missing), 400).await;
    assert_eq!(
        json_reply(
            server.request(Method::GET, "/plans/1/drafts/99/header"),
            404
        )
        .await["code"],
        "draft_not_found"
    );
    let foreign = server
        .request(Method::PUT, "/plans/7/drafts/header")
        .header("Idempotency-Key", "p-1")
        .json(&body(open, Value::Null, acme()));
    assert_eq!(json_reply(foreign, 404).await["code"], "profile_not_found");
    assert_eq!(logical_database(&server.sql()), before);
    assert!(draft_events(&server).is_empty());
    server.close().await;
}

/// Autosave writes Plan choices into the same draft row, so the header stays.
/// Recompute then abandons that draft and writes a new one, which must carry
/// its header.
#[tokio::test]
async fn gre_1197_autosave_keeps_and_recompute_carries_the_draft_header() {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    let open = observed(&json!(DIGEST));
    let saved = json_reply(
        put(
            &server,
            Some("h-1"),
            &body(open.clone(), Value::Null, acme()),
        ),
        200,
    )
    .await;
    let autosaved = json_reply(
        keyed(&server, "/plans/1/drafts/save", Some("s-1")).json(&autosave_body(open, 4)),
        200,
    )
    .await;
    assert_eq!(autosaved["receipt"]["draft_id"], 2);
    assert_eq!(
        json_reply(server.request(Method::GET, HEADER), 200).await,
        saved["header"]
    );
    let created = json_reply(
        keyed(&server, "/plans/1/drafts/recompute", Some("r-1"))
            .json(&json!({"apply_manifest": false})),
        200,
    )
    .await;
    assert_eq!(created["draft"]["draft_id"], 3);
    let mut carried = saved["header"].clone();
    carried["draft_id"] = json!(3);
    assert_eq!(
        json_reply(server.request(Method::GET, "/plans/1/drafts/3/header"), 200).await,
        carried
    );
    assert_eq!(draft_header(&server, 3), draft_header(&server, 2));
    server.close().await;
}
