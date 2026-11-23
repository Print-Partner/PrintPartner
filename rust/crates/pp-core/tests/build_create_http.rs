#![cfg(feature = "desktop_rebuild")]
//! `POST /api/v2/plans` (`createBuild`, GRE-1876) through the composed
//! `product_router` on a disposable installation opened as the Rust-only
//! runtime opens it: create, the name refusals, the list, a restart, the
//! repeat rule and the credential check.
//!
//! The repeat rule: a Build name is unique per installation
//! (`uq_profiles_tenant_name`), and the core checks it in the create's write
//! transaction. A repeated or retried create of the same trimmed name is
//! refused with 400 and writes nothing. A client unsure whether its create
//! landed lists Builds and finds the name instead of creating again.

#[path = "support/installation_server.rs"]
mod support_installation;
#[path = "support/installation_response.rs"]
mod support_installation_response;

use pp_storage::{Limits, WriterOwner, ownership_marker::WriterGate};
use pp_testkit::{Fixture, Installation};
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::Arc;
use support_installation::Server;

impl Server {
    /// Stops serving and shuts the owner down, then opens the same data
    /// directory as the Rust-only runtime does and serves the product routes
    /// on a new loopback port with the same session.
    async fn restart(self) -> Self {
        let (data, root, cookie) = self.shutdown().await;
        let opened = WriterOwner::open_with_gate(&data, Limits::default(), WriterGate::SwitchOn);
        let mut server = Self::serve(opened.unwrap().0, data, root, Some("1")).await;
        server.cookie = cookie;
        server
    }
}

const PLANS: &str = "/api/v2/plans";

async fn start() -> Server {
    let installation = Installation::create(Fixture::Empty).unwrap();
    let data = installation.path().canonicalize().unwrap();
    let opened = WriterOwner::open_with_gate(&data, Limits::default(), WriterGate::SwitchOn);
    Server::start(opened.unwrap().0, data, installation, Some("1")).await
}

async fn answer(request: reqwest::RequestBuilder) -> (u16, Value) {
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn create(server: &Server, body: Value) -> (u16, Value) {
    answer(server.request(Method::POST, PLANS).json(&body)).await
}

/// Each listed Build's id and name, in list order.
async fn builds(server: &Server) -> Vec<(i64, String)> {
    let (status, body) = server.json(Method::GET, PLANS).await;
    assert_eq!(status, 200, "{body}");
    let profiles = body["profiles"].as_array().unwrap();
    let pairs = profiles.iter().map(|profile| {
        let id = profile["id"].as_i64().unwrap();
        (id, profile["name"].as_str().unwrap().to_owned())
    });
    pairs.collect()
}

fn assert_problem(status: u16, body: &Value, expected: u16, detail: &str) {
    assert_eq!(status, expected, "{body}");
    assert_eq!(body["status"], expected, "{body}");
    assert_eq!(body["detail"], detail, "{body}");
}

#[tokio::test]
async fn a_created_build_has_no_layers_is_listed_and_survives_a_restart() {
    let server = start().await;
    assert_eq!(builds(&server).await, []);

    let (status, created) = create(&server, json!({"name": "  First Build \u{3000}"})).await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["name"], "First Build");
    assert_eq!(created["layers"], json!([]));
    assert_eq!(created["part_count"], 0);
    assert_eq!(created["archived_at"], Value::Null);

    let (status, layers) = server
        .json(Method::GET, &format!("{PLANS}/{id}/layers"))
        .await;
    assert_eq!(status, 200, "{layers}");
    assert_eq!(layers, json!({"profile_id": id, "layers": []}));
    for path in [
        format!("{PLANS}/{id}/drafts"),
        format!("{PLANS}/{id}/history"),
    ] {
        let (status, body) = server.json(Method::GET, &path).await;
        assert_eq!(status, 200, "GET {path}: {body}");
    }
    let listed = builds(&server).await;
    assert_eq!(listed, [(id, "First Build".to_owned())]);

    let (old_origin, old_owner) = (server.origin.clone(), Arc::downgrade(&server.owner));
    let server = server.restart().await;
    // A real restart shut the old writer owner down and closed its listener.
    assert!(
        old_owner.upgrade().is_none(),
        "the old writer owner is still open"
    );
    assert_ne!(server.origin, old_origin);
    let old = server
        .client
        .get(format!("{old_origin}{PLANS}"))
        .send()
        .await;
    assert!(old.is_err(), "the old listener still answers: {old:?}");
    assert_eq!(builds(&server).await, listed);
    let (status, read) = server.json(Method::GET, &format!("{PLANS}/{id}")).await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["name"], "First Build");
    server.stop().await;
}

#[tokio::test]
async fn a_bad_name_is_refused_with_a_problem_and_nothing_is_written() {
    let server = start().await;
    let required = "Profile name is required";
    let too_long = "Profile name is too long";
    for (body, detail) in [
        (json!({"name": ""}), required),
        (json!({"name": " \t\n\u{a0}\u{3000}\u{feff} "}), required),
        (json!({"name": null}), required),
        (json!({}), required),
        (json!({"name": "\u{1f9f5}".repeat(4097)}), too_long),
        (
            json!({"name": format!("  {}  ", "x".repeat(4097))}),
            too_long,
        ),
    ] {
        let (status, refused) = create(&server, body).await;
        assert_problem(status, &refused, 400, detail);
    }
    assert_eq!(builds(&server).await, []);

    // The bound is 4096 Unicode scalars after trimming, not bytes.
    let longest = "\u{1f9f5}".repeat(4096);
    let (status, created) = create(&server, json!({"name": format!(" {longest} ")})).await;
    assert_eq!(status, 200, "{created}");
    assert_eq!(created["name"], longest);
    server.stop().await;
}

#[tokio::test]
async fn a_repeated_create_is_refused_and_the_client_reconciles_by_listing() {
    let server = start().await;
    let (status, first) = create(&server, json!({"name": "Rack"})).await;
    assert_eq!(status, 200, "{first}");
    let rack = first["id"].as_i64().unwrap();

    // A retry of the same action, and the same name with other whitespace.
    for name in ["Rack", "  Rack\t"] {
        let (status, refused) = create(&server, json!({"name": name})).await;
        assert_problem(status, &refused, 400, "Profile already exists: Rack");
    }
    // Names compare exactly (`uq_profiles_tenant_name` is case-sensitive), so a
    // case variant is a different Build.
    let (status, lower) = create(&server, json!({"name": "rack"})).await;
    assert_eq!(status, 200, "{lower}");
    let lower = lower["id"].as_i64().unwrap();

    // Two creates in flight at once: one Build, one refusal.
    let (one, two) = tokio::join!(
        create(&server, json!({"name": "Shelf"})),
        create(&server, json!({"name": "Shelf"})),
    );
    let mut statuses = [one.0, two.0];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 400], "{one:?} {two:?}");
    let shelf = [one, two]
        .into_iter()
        .find(|(status, _)| *status == 200)
        .map(|(_, body)| body["id"].as_i64().unwrap())
        .unwrap();

    // An uncertain create is reconciled by name from the list, not sent again.
    let mut listed = builds(&server).await;
    listed.sort_unstable();
    assert_eq!(
        listed,
        [
            (rack, "Rack".to_owned()),
            (lower, "rack".to_owned()),
            (shelf, "Shelf".to_owned())
        ]
    );
    server.stop().await;
}

/// The Builds list reads at most 64 Builds, so create stops there and the
/// list a client reconciles from stays readable.
#[tokio::test]
async fn the_create_after_64_builds_is_refused_and_the_list_still_reads() {
    let server = start().await;
    for n in 0..64 {
        let (status, created) = create(&server, json!({"name": format!("Build {n:02}")})).await;
        assert_eq!(status, 200, "{created}");
    }
    let (status, refused) = create(&server, json!({"name": "One too many"})).await;
    let detail = "Build limit reached: an installation holds at most 64 Builds";
    assert_problem(status, &refused, 409, detail);
    // A retry of a create that landed still says the name is taken.
    let (status, refused) = create(&server, json!({"name": "Build 63"})).await;
    assert_problem(status, &refused, 400, "Profile already exists: Build 63");

    let listed = builds(&server).await;
    assert_eq!(listed.len(), 64);
    assert!(!listed.iter().any(|(_, name)| name == "One too many"));
    server.stop().await;
}

#[tokio::test]
async fn a_create_without_credentials_or_from_another_origin_writes_nothing() {
    let server = start().await;
    let url = format!("{}{PLANS}", server.origin);
    let body = json!({"name": "Not signed in"});

    let anonymous = server.client.post(&url).header("Origin", &server.origin);
    let (status, refused) = answer(anonymous.json(&body)).await;
    assert_problem(status, &refused, 401, "Authentication required");

    let foreign = server
        .client
        .post(&url)
        .header("Origin", "http://127.0.0.1:9")
        .header("Cookie", &server.cookie);
    let (status, refused) = answer(foreign.json(&body)).await;
    assert_eq!(status, 403, "{refused}");

    assert_eq!(builds(&server).await, []);
    server.stop().await;
}

/// Creating a Build leaves `PUT /plans/{id}/layers/base` as it was: it adds
/// the base layer, and a second PUT moves that layer to the other Source.
#[tokio::test]
async fn set_base_adds_then_moves_the_base_layer_of_a_created_build() {
    let server = start().await;
    let mut sources = Vec::new();
    for name in ["Base A", "Base B"] {
        let request = server.request(Method::POST, "/api/v1/sources");
        let (status, source) = answer(request.json(&json!({"name": name}))).await;
        assert_eq!(status, 200, "{source}");
        sources.push(source["id"].as_i64().unwrap());
    }
    let (status, created) = create(&server, json!({"name": "Kit"})).await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_i64().unwrap();
    let base = format!("{PLANS}/{id}/layers/base");

    let mut layer_ids = Vec::new();
    for source in &sources {
        let request = server.request(Method::PUT, &base);
        let (status, layers) = answer(request.json(&json!({"project_id": source}))).await;
        assert_eq!(status, 200, "{layers}");
        let layers = layers["layers"].as_array().unwrap().clone();
        assert_eq!(layers.len(), 1, "{layers:?}");
        assert_eq!(layers[0]["layer_type"], "base");
        assert_eq!(layers[0]["project_id"], *source);
        layer_ids.push(layers[0]["id"].clone());
    }
    assert_eq!(layer_ids[0], layer_ids[1]);
    server.stop().await;
}
