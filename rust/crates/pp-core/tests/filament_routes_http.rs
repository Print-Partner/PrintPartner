#![cfg(feature = "desktop_rebuild")]
use pp_api::{
    admin::{AdminHttpConfig, admin_router},
    filaments::{CatalogReader, filament_router},
    fixtures::Served,
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc};
struct NoNetwork;
impl CatalogReader for NoNetwork {
    fn read(&self, _: Value) -> Pin<Box<dyn Future<Output = Option<Vec<Value>>> + Send>> {
        panic!("disabled catalog must not contact Spoolman")
    }
}
fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}
fn seeded_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("print-partner.db"),
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    directory
}
async fn start(directory: &std::path::Path, flag: Option<&str>) -> (WriterOwner, Served, String) {
    start_with_reader(directory, flag, Arc::new(NoNetwork)).await
}
async fn start_with_reader(
    directory: &std::path::Path,
    flag: Option<&str>,
    reader: Arc<dyn CatalogReader>,
) -> (WriterOwner, Served, String) {
    let owner = WriterOwner::open(directory, Limits::default()).unwrap().0;
    let server = Served::start(|origin| {
        let config = AdminHttpConfig::from_startup(origin, "default", flag).unwrap();
        admin_router(
            config.clone(),
            owner.accepted_reads().unwrap(),
            owner.admin_files().unwrap(),
        )
        .merge(filament_router(
            config,
            owner.accepted_reads().unwrap(),
            owner.filaments_with_policy(policy()).unwrap(),
            reader,
        ))
    })
    .await
    .unwrap();
    let origin = server.origin.clone();
    (owner, server, origin)
}
#[tokio::test(flavor = "multi_thread")]
async fn filament_journey_aliases_validation_admission_and_restart() {
    let directory = seeded_directory();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let corrupt = b"not json\xff";
    let path = directory.path().join("custom_filaments.json");
    std::fs::write(&path, corrupt).unwrap();
    let (owner, serving, origin) = start(directory.path(), Some("1")).await;
    let empty: Value = client
        .get(format!("{origin}/filaments/custom"))
        .header("cookie", "pp_session=read-fixture-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty, json!({"filaments":[]}));
    assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    exercise_aliases(&client, &origin).await;
    let copies: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .unwrap()
                .starts_with("custom_filaments.json.corrupt-")
        })
        .collect();
    assert_eq!(copies.len(), 1);
    assert!(copies[0].file_name().to_str().unwrap().ends_with('Z'));
    assert_eq!(std::fs::read(copies[0].path()).unwrap(), corrupt);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
        json!({"filaments":[]})
    );
    let kept: Value = client
        .post(format!("{origin}/filaments/custom"))
        .header("cookie", "pp_session=read-fixture-secret")
        .header("origin", &origin)
        .json(&json!({"display_name":"Keep after restart", "hex":"123456"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let path = directory.path().join("custom_filaments.json");
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(
        client
            .post(format!("{origin}/filaments/custom"))
            .header("origin", &origin)
            .json(&json!({"display_name":"x","hex":"123456"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{origin}/filaments/custom"))
            .header("cookie", "pp_session=read-fixture-secret")
            .json(&json!({"display_name":"x","hex":"123456"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(saved, std::fs::read(&path).unwrap());
    serving.stop().await.unwrap();
    owner.shutdown().unwrap();
    let (owner, serving, origin) = start(directory.path(), Some("1")).await;
    let body: Value = client
        .get(format!("{origin}/filaments/custom"))
        .header("cookie", "pp_session=read-fixture-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body, json!({"filaments":[kept]}));
    serving.stop().await.unwrap();
    owner.shutdown().unwrap();
    let (owner, serving, origin) = start(directory.path(), None).await;
    assert_eq!(
        client
            .get(format!("{origin}/filaments/catalog"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    serving.stop().await.unwrap();
    owner.shutdown().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_node_store_outside_serde_range_is_never_replaced() {
    let surrogate = r#"{"filaments":[{"id":"keep","display_name":"\ud800"}]}"#.to_owned();
    let nested = format!("{}0{}", "[".repeat(130), "]".repeat(130));
    let deep = format!(
        r##"{{"filaments":[{{"id":"keep","display_name":"Keep","product_line":"Custom","hex":"#123456","combo_label":"Keep","metadata":{nested}}}]}}"##
    );
    for (original, custom) in [
        (
            surrogate,
            r#"[{"id":"keep","display_name":"\ud800","swatch_url":""}]"#,
        ),
        (deep, "[]"),
    ] {
        let directory = seeded_directory();
        let path = directory.path().join("custom_filaments.json");
        std::fs::write(&path, &original).unwrap();
        let (owner, serving, origin) = start(directory.path(), Some("1")).await;
        let client = reqwest::Client::new();
        for prefix in ["", "/api/v1"] {
            let reply = client
                .get(format!("{origin}{prefix}/filaments/custom"))
                .header("cookie", "pp_session=read-fixture-secret")
                .send()
                .await
                .unwrap();
            assert_eq!(reply.status(), 200);
            assert_eq!(
                reply.text().await.unwrap(),
                if custom == "[]" {
                    r#"{"filaments":[]}"#
                } else {
                    &original
                }
            );
            let reply = client
                .get(format!("{origin}{prefix}/filaments/catalog"))
                .header("cookie", "pp_session=read-fixture-secret")
                .send()
                .await
                .unwrap();
            assert_eq!(reply.status(), 200);
            let body: std::collections::HashMap<String, Box<serde_json::value::RawValue>> =
                serde_json::from_str(&reply.text().await.unwrap()).unwrap();
            assert_eq!(body["custom_colors"].get(), custom);
            assert_eq!(
                serde_json::from_str::<Vec<Value>>(body["colors"].get())
                    .unwrap()
                    .len(),
                252
            );
            assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
            for (method, suffix) in [
                (reqwest::Method::POST, ""),
                (reqwest::Method::DELETE, "/keep"),
            ] {
                let request = client
                    .request(
                        method.clone(),
                        format!("{origin}{prefix}/filaments/custom{suffix}"),
                    )
                    .header("cookie", "pp_session=read-fixture-secret")
                    .header("origin", &origin);
                let request = if method == reqwest::Method::POST {
                    request.json(&json!({"display_name":"new", "hex":"123456"}))
                } else {
                    request
                };
                let reply = request.send().await.unwrap();
                assert_eq!(reply.status(), 400);
                assert_eq!(reply.headers()["content-type"], "application/problem+json");
                assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
            }
        }
        serving.stop().await.unwrap();
        owner.shutdown().unwrap();
    }
}

struct RevokeOnRead(std::path::PathBuf);
impl CatalogReader for RevokeOnRead {
    fn read(&self, _: Value) -> Pin<Box<dyn Future<Output = Option<Vec<Value>>> + Send>> {
        let path = self.0.clone();
        Box::pin(async move {
            rusqlite::Connection::open(path).unwrap().execute("UPDATE app_settings SET value='{\"spoolman\":false}' WHERE key='feature_toggles'", []).unwrap();
            Some(vec![json!({"id":"withheld-color"})])
        })
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn catalog_withholds_inflight_colors_after_feature_revocation() {
    let directory = seeded_directory();
    let path = directory.path().join("print-partner.db");
    rusqlite::Connection::open(&path).unwrap().execute_batch("INSERT OR REPLACE INTO app_settings(tenant_id,key,value) VALUES('default','feature_toggles','{\"spoolman\":true}'),('default','default_spoolman_integration_id','s1'),('default','integrations','[{\"id\":\"s1\",\"type\":\"spoolman\",\"config\":{}}]');").unwrap();
    let (owner, serving, origin) =
        start_with_reader(directory.path(), Some("1"), Arc::new(RevokeOnRead(path))).await;
    let body: Value = reqwest::Client::new()
        .get(format!("{origin}/filaments/catalog"))
        .header("cookie", "pp_session=read-fixture-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["spoolman_status"], "disabled");
    assert_eq!(body["spoolman_colors"], json!([]));
    assert!(body.get("default_spoolman_integration_id").is_none());
    serving.stop().await.unwrap();
    owner.shutdown().unwrap();
}

async fn exercise_aliases(client: &reqwest::Client, origin: &str) {
    let request = |method, path: &str| {
        client
            .request(method, format!("{origin}{path}"))
            .header("cookie", "pp_session=read-fixture-secret")
            .header("origin", origin)
    };
    for prefix in ["", "/api/v1"] {
        let path = format!("{prefix}/filaments/custom");
        for body in [
            json!({}),
            json!({"display_name":" ","hex":"123456"}),
            json!({"display_name":"x","hex":"bad"}),
        ] {
            assert_eq!(
                request(reqwest::Method::POST, &path)
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                400
            );
        }
        let entry: Value = request(reqwest::Method::POST, &path)
            .json(&json!({"display_name":" Blue ","hex":"aAbBcC","product_line":" "}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(entry["display_name"], "Blue");
        assert_eq!(entry["hex"], "#aAbBcC");
        assert_eq!(entry["product_line"], "Custom");
        assert!(entry["id"].as_str().unwrap().starts_with("custom-"));
        let catalog: Value = request(reqwest::Method::GET, &format!("{prefix}/filaments/catalog"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(catalog["colors"].as_array().unwrap().len(), 252);
        assert_eq!(catalog["custom_colors"][0]["id"], entry["id"]);
        assert_eq!(catalog["spoolman_status"], "disabled");
        assert!(catalog.get("default_spoolman_integration_id").is_none());
        let delete = format!("{path}/{}", entry["id"].as_str().unwrap());
        assert_eq!(
            request(reqwest::Method::DELETE, &delete)
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        assert_eq!(
            request(reqwest::Method::DELETE, &delete)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
}
