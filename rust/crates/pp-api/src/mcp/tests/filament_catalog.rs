use std::path::PathBuf;

use super::{
    super::filament_catalog::{ADD, MATCH},
    reads::{assert_needs_key, confirm, session, tool},
    *,
};

const IMPORTED: &str = r##"{"filaments":[
  {"id":"custom-b","display_name":"Fixture Pearl","product_line":"Imported","hex":"#a1b2c3","combo_label":"Fixture Pearl"},
  {"id":"custom-a","display_name":"Fixture Pearl","product_line":"Imported","hex":"#a1b2c3","combo_label":"Fixture Pearl"},
  {"id":"custom-c","display_name":"Fixture Pearl","product_line":"Other","hex":"#a1b2c2","combo_label":"Fixture Pearl"},
  {"id":"custom-d","display_name":"Fixture Incomplete"}
]}"##;

async fn fixture(body: &str) -> (McpServer, PathBuf, String) {
    let server = McpServer::start(Some("1")).await;
    let store = server._directory.join("custom_filaments.json");
    std::fs::write(&store, body).unwrap();
    let bearer = format!("Bearer {}", server.create_key().await.1);
    (server, store, bearer)
}

async fn propose(server: &McpServer, headers: &[(String, String)], args: Value) -> Value {
    let (proposal, refused) = tool(server, headers, ADD, args).await;
    assert!(!refused, "{proposal}");
    proposal
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn match_reads_local_and_bundled_colors_in_stable_order() {
    let (server, store, bearer) = fixture(IMPORTED).await;
    let headers = session(&server, &bearer).await;
    let query = json!({"name":" fixture PEARL ", "brand":"IMPORTED", "color_hex":"#a1b2c2"});
    assert_needs_key(&server, MATCH, query.clone()).await;
    let (result, refused) = tool(&server, &headers, MATCH, query).await;
    assert!(!refused, "{result}");
    assert_eq!(result["exact_available"], true);
    let matches = result["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 20);
    assert_eq!(matches[0]["id"], "custom-a");
    assert_eq!(matches[0]["color_distance"], 1.0);
    assert_eq!(matches[1]["id"], "custom-b");
    assert_eq!(matches[2]["id"], "custom-c");
    assert_eq!(matches[2]["color_distance"], 0.0);
    assert_eq!(matches[4]["id"].as_str().unwrap().split("::").count(), 2);
    let (_, refused) = tool(
        &server,
        &headers,
        MATCH,
        json!({"name":"x","color_hex":"abc"}),
    )
    .await;
    assert!(refused);
    assert_eq!(std::fs::read_to_string(store).unwrap(), IMPORTED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_filament_confirms_after_restart_and_reuses_existing_color() {
    let (server, store, bearer) = fixture(IMPORTED).await;
    let headers = session(&server, &bearer).await;
    let proposal = propose(
        &server,
        &headers,
        json!({
            "display_name": " Fixture Violet ", "hex": "AABBFF", "product_line": " "
        }),
    )
    .await;
    assert_eq!(std::fs::read_to_string(&store).unwrap(), IMPORTED);

    let server = server.restart().await;
    let headers = session(&server, &bearer).await;
    let (applied, refused) = confirm(&server, &headers, &proposal).await;
    assert!(!refused, "{applied}");
    let result = &applied["result"];
    let filament = &result["filament"];
    assert_eq!(result["inventory_tracking"], "external_named_color_only");
    assert_eq!(result["idempotent_replay"], false);
    assert_eq!(filament["product_line"], "Custom");
    let bytes = std::fs::read(&store).unwrap();
    let saved: Value = serde_json::from_slice(&bytes).unwrap();
    let imported: Value = serde_json::from_str(IMPORTED).unwrap();
    assert_eq!(
        &saved["filaments"].as_array().unwrap()[..4],
        imported["filaments"].as_array().unwrap()
    );
    assert_eq!(saved["filaments"][4], *filament);
    let (again, refused) = confirm(&server, &headers, &proposal).await;
    assert!(refused, "{again}");
    assert_eq!(again["code"], "already_applied");

    let duplicate = propose(
        &server,
        &headers,
        json!({
            "display_name": "fixture VIOLET", "hex": "#aabbff"
        }),
    )
    .await;
    let (reused, refused) = confirm(&server, &headers, &duplicate).await;
    assert!(!refused, "{reused}");
    assert_eq!(reused["result"]["idempotent_replay"], true);
    assert_eq!(reused["result"]["filament"], *filament);
    assert_eq!(std::fs::read(store).unwrap(), bytes);
    let (matches, refused) = tool(&server, &headers, MATCH, json!({"name":"Fixture Violet"})).await;
    assert!(!refused, "{matches}");
    assert_eq!(matches["matches"][0]["id"], filament["id"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idless_imported_color_is_repaired_in_place_on_confirmation() {
    let imported = json!({"display_name":"Fixture Pearl","product_line":"Imported","hex":"#a1b2c3","notes":"keep"});
    let (server, store, bearer) = fixture(&json!({"filaments":[imported]}).to_string()).await;
    let headers = session(&server, &bearer).await;
    let args = json!({"display_name":"Fixture Pearl","product_line":"Imported","hex":"#A1B2C3"});
    let proposal = propose(&server, &headers, args).await;
    let (applied, refused) = confirm(&server, &headers, &proposal).await;
    assert!(!refused, "{applied}");
    let item = &applied["result"]["filament"];
    assert_eq!(applied["result"]["idempotent_replay"], false);
    assert_eq!(item["notes"], "keep");
    let saved: Value = serde_json::from_slice(&std::fs::read(store).unwrap()).unwrap();
    assert_eq!(saved["filaments"], json!([item]));
    let (matches, refused) = tool(&server, &headers, MATCH, json!({"name":"Fixture Pearl"})).await;
    assert!(!refused, "{matches}");
    assert!(item["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(matches["matches"][0]["id"], item["id"]);
}
