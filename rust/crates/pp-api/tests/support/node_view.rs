//! Shared by the API-key parity tests.
use serde_json::Value;

/// The Node view of a Rust body. Node sends `title` and `status` only on its
/// key-not-found body, so those stay there and go everywhere else.
pub fn node_view(body: Value) -> Value {
    let key_not_found = body["status"] == 404 && body["detail"] == "API key not found";
    let mut node = pp_api::problem::without_envelope(body);
    if key_not_found {
        node["title"] = serde_json::json!("Not Found");
        node["status"] = serde_json::json!(404);
    }
    node
}
