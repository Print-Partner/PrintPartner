//! GRE-1353: the Rust-only launcher's gateway (`Gateway::start_rust_only`)
//! sends `/mcp` and `/api/v1/mcp` to the key-only MCP guard without the
//! desktop cookie, and keeps the cookie gate on every other path.
use super::mcp_gateway_support::{assert_key_only, bearer};
use super::temporary;
use super::watcher_proof::{proof_data, run_in_child, shut_down, signed_in, start_rust_only};

#[tokio::test]
async fn mcp_gateway_rust_only_runtime_admits_keys_not_cookies() {
    let data = temporary("mcp-gateway");
    run_in_child("mcp_gateway::mcp_gateway_rust_only_runtime_in_child", &data).await;
    let _ = std::fs::remove_dir_all(data);
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn mcp_gateway_rust_only_runtime_in_child() {
    let data = proof_data();
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookies = signed_in(&origin, &desktop, 1).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = bearer(&client, &origin, &cookies).await;
    assert_key_only(&client, &origin, &desktop, &cookies, &bearer).await;
    shut_down(runtime).await;
}
