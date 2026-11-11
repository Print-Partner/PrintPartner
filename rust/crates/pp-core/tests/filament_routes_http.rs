#![cfg(feature = "desktop_rebuild")]
use pp_api::{
    admin::{AdminHttpConfig, admin_router},
    filaments::{CatalogReader, filament_router},
    serving::Serving,
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
async fn start(directory: &std::path::Path, flag: Option<&str>) -> (WriterOwner, Serving, String) {
    start_with_reader(directory, flag, Arc::new(NoNetwork)).await
}
async fn start_with_reader(
    directory: &std::path::Path,
    flag: Option<&str>,
    reader: Arc<dyn CatalogReader>,
) -> (WriterOwner, Serving, String) {
    let owner = WriterOwner::open(directory, Limits::default()).unwrap().0;
    let (listener, origin) = pp_testkit::http_listener().await.unwrap();
    let config = AdminHttpConfig::from_startup(&origin, "default", flag).unwrap();
    let router = admin_router(
        config.clone(),
        owner.accepted_reads().unwrap(),
        owner.admin_files().unwrap(),
    )
    .merge(filament_router(
        config,
        owner.accepted_reads().unwrap(),
        owner.filaments_with_policy(policy()).unwrap(),
        reader,
    ));
    (owner, Serving::start(listener, router), origin)
}
#[tokio::test(flavor = "multi_thread")]
async fn filament_journey_aliases_validation_admission_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("print-partner.db"),
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let (owner, mut serving, origin) = start(directory.path(), Some("1")).await;
    exercise_aliases(&client, &origin).await;
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
    let (owner, mut serving, origin) = start(directory.path(), Some("1")).await;
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
    let (owner, mut serving, origin) = start(directory.path(), None).await;
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
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("print-partner.db"),
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    let path = directory.path().join("custom_filaments.json");
    let original = br#"{"filaments":[{"id":"keep","display_name":"\ud800"}]}"#;
    std::fs::write(&path, original).unwrap();
    let (owner, mut serving, origin) = start(directory.path(), Some("1")).await;
    let client = reqwest::Client::new();
    for prefix in ["", "/api/v1"] {
        let reply = client
            .get(format!("{origin}{prefix}/filaments/catalog"))
            .header("cookie", "pp_session=read-fixture-secret")
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 200);
        let body: std::collections::HashMap<String, Box<serde_json::value::RawValue>> =
            serde_json::from_str(&reply.text().await.unwrap()).unwrap();
        assert_eq!(
            body["custom_colors"].get(),
            r#"[{"id":"keep","display_name":"\ud800","swatch_url":""}]"#
        );
        assert_eq!(
            serde_json::from_str::<Vec<Value>>(body["colors"].get())
                .unwrap()
                .len(),
            252
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            client
                .delete(format!("{origin}{prefix}/filaments/custom/keep"))
                .header("cookie", "pp_session=read-fixture-secret")
                .header("origin", &origin)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    let reply = client
        .post(format!("{origin}/filaments/custom"))
        .header("cookie", "pp_session=read-fixture-secret")
        .header("origin", &origin)
        .json(&json!({"display_name":"new","hex":"123456"}))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 400);
    assert_eq!(std::fs::read(path).unwrap(), original);
    serving.stop().await.unwrap();
    owner.shutdown().unwrap();
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
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("print-partner.db");
    std::fs::write(
        &path,
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    rusqlite::Connection::open(&path).unwrap().execute_batch("INSERT OR REPLACE INTO app_settings(tenant_id,key,value) VALUES('default','feature_toggles','{\"spoolman\":true}'),('default','default_spoolman_integration_id','s1'),('default','integrations','[{\"id\":\"s1\",\"type\":\"spoolman\",\"config\":{}}]');").unwrap();
    let (owner, mut serving, origin) =
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
