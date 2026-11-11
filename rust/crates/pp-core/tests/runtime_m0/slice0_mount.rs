//! The launcher start that `pp-server` and `pp-desktop` share, with each
//! runtime value in a child process so the environment stays isolated.
use super::{desktop_cookie, launch, process_tree::descendants, request, temporary};
use axum::http::StatusCode;
use pp_core::{CoreRuntime, CoreStatus, RuntimeStatus, RustStatus, StatusReceiver};
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn slice0_mount_launcher_gates() {
    for value in [None, Some("0"), Some("TRUE"), Some("1"), Some("true")] {
        let data = temporary("slice0-launcher");
        let rust = cfg!(feature = "desktop_rebuild") && matches!(value, Some("1" | "true"));
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--ignored",
                "--exact",
                "slice0_mount::slice0_mount_launcher_in_child",
                "--nocapture",
            ])
            .env("PP_SLICE0_DATA", &data)
            .env("PP_SLICE0_RUST", rust.to_string())
            .env_remove("PP_DESKTOP_REBUILD")
            .kill_on_drop(true);
        if let Some(value) = value {
            child.env("PP_DESKTOP_REBUILD", value);
        }
        let output = tokio::time::timeout(Duration::from_secs(240), child.output())
            .await
            .expect("launcher case exceeded four minutes")
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{value:?}\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("1 passed"), "{value:?} ran no test");
        println!(
            "{}",
            json!({"PP_DESKTOP_REBUILD": value, "rust_only": rust})
        );
        let _ = std::fs::remove_dir_all(data);
    }
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn absent_replacement_bundle_preserves_legacy_navigation() {
    let data = temporary("legacy-ui-launcher");
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "slice0_mount::slice0_mount_launcher_in_child",
            "--nocapture",
        ])
        .env("PP_SLICE0_DATA", &data)
        .env("PP_SLICE0_RUST", "true")
        .env("PP_UI_ABSENT", "1")
        .env("PP_DESKTOP_REBUILD", "1")
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("Replacement UI bundle absent")
            .count(),
        1
    );
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn slice0_mount_launcher_in_child() {
    let data = std::path::PathBuf::from(std::env::var_os("PP_SLICE0_DATA").unwrap());
    let rust = std::env::var("PP_SLICE0_RUST").unwrap() == "true";
    let absent = std::env::var_os("PP_UI_ABSENT").is_some();
    let mut input = launch(data.clone());
    if absent {
        let web = data.join("image-web");
        super::copy_directory(&input.assets, &web.join("apps/web/dist"));
        super::copy_directory(
            &input.bundle.community_manifests_dir().unwrap(),
            &web.join("apps/server/dist/current/data/manifests"),
        );
        input.assets = web.join("apps/web/dist");
        input.bundle.entry = web.join("apps/server/dist/current/desktop.js");
        input.bundle.web_root = web;
        // Keep the writer's data directory separate from the image's static files.
        input.data_dir = data.join("data");
    }
    let mut runtime = CoreRuntime::start_for_launcher(input).await.unwrap();
    assert_eq!(runtime.rust_only(), rust);
    // The desktop tray holds a handle and its subscription for the process lifetime.
    let handle = runtime.handle();
    let mut status = handle.subscribe();
    let node = match status.borrow_and_update() {
        RuntimeStatus::Rust(RustStatus::Ready) => None,
        RuntimeStatus::Node(CoreStatus::Ready { pid, .. }) => Some(pid),
        other => panic!("launcher not ready: {other:?}"),
    };
    assert_eq!(node.is_some(), !rust, "a Node child runs only without Rust");
    assert_eq!(handle.compat_status().is_none(), rust);
    // The process table, not the runtime's own report: Rust-only mode starts
    // no child process at all, and the Node-backed mode shows its Node child.
    let check_children = || {
        let children = descendants(std::process::id());
        match node {
            None => assert_eq!(children, [], "Rust-only mode started a process"),
            Some(pid) => assert!(
                children.iter().any(|(child, _)| *child == pid),
                "Node child {pid} missing from {children:?}"
            ),
        }
    };
    check_children();
    let origin = runtime.origin().to_owned();
    let desktop = desktop_cookie(&mut runtime).await;
    assert_runtime_json(&origin, &desktop, rust).await;
    assert_served_ui(&origin, &desktop, rust && !absent).await;
    if absent {
        for path in ["/settings", "/builds"] {
            let response = reqwest::Client::new()
                .get(format!("{origin}{path}"))
                .header("cookie", &desktop)
                .header("accept", "text/html")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["content-security-policy"],
                "frame-src 'none'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'"
            );
            assert_eq!(
                response.text().await.unwrap(),
                std::fs::read_to_string(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../web/apps/web/dist/index.html")
                )
                .unwrap()
            );
        }
        let response = reqwest::Client::new()
            .get(format!("{origin}/today"))
            .header("cookie", &desktop)
            .header("accept", "text/html")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    if rust {
        let (status, _, body) = request(
            &origin,
            "/api/v1/plans",
            "GET",
            Some(&desktop),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["detail"], "Authentication required");
        let (status, headers, _) = request(
            &origin,
            "/auth/register",
            "POST",
            Some(&desktop),
            None,
            None,
            Some(json!({"email": "launcher@example.test", "password": "password-1234"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let session = headers["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let cookies = format!("{desktop}; {session}");
        let (status, _, body) = request(
            &origin,
            "/api/v1/plans",
            "GET",
            Some(&cookies),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["profiles"], json!([]), "{body}");
    }
    // Serving requests did not start one later either.
    check_children();
    let receipt = runtime.shutdown().await;
    assert!(receipt.complete(), "{receipt:?}");
    if rust {
        // Shutdown published Stopping then Stopped, and the channel stays open.
        let StatusReceiver::Rust(mut status) = status else {
            panic!("Rust-only runtime returned a Node status channel");
        };
        assert!(
            status.has_changed().unwrap(),
            "shutdown published no status"
        );
        assert_eq!(*status.borrow_and_update(), RustStatus::Stopped);
        assert!(status.has_changed().is_ok(), "status channel closed");
    }
}

/// `/__runtime` reports the Rust status and an unused compat backend in
/// Rust-only mode, and the Node child with no `rust` key otherwise.
async fn assert_runtime_json(origin: &str, desktop: &str, rust: bool) {
    let (code, _, body) =
        request(origin, "/__runtime", "GET", Some(desktop), None, None, None).await;
    assert_eq!(code, StatusCode::OK);
    let state: Value = serde_json::from_slice(&body).unwrap();
    if rust {
        assert_eq!(state["rust"], json!({"state": "ready"}), "{state}");
        assert_eq!(state["compat"], json!({"state": "not_used"}), "{state}");
    } else {
        assert!(state.get("rust").is_none(), "{state}");
        assert_eq!(state["compat"]["state"], "ready", "{state}");
        assert!(state["compat"]["pid"].is_u64(), "{state}");
    }
}

/// Both gates select the replacement bundle and policy together. The legacy
/// runtime still serves the old bundle, including when only one gate is on.
async fn assert_served_ui(origin: &str, desktop: &str, rust: bool) {
    let response = reqwest::Client::new()
        .get(format!("{origin}/"))
        .header("cookie", desktop)
        .header("accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let policy = response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    let html = response.text().await.unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../web/apps");
    let expected = std::fs::read_to_string(root.join(if rust {
        "ui/dist/index.html"
    } else {
        "web/dist/index.html"
    }))
    .unwrap();
    if rust {
        assert!(policy.starts_with("default-src 'none'; script-src 'self';"));
        let nonce = policy
            .split("'nonce-")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap();
        let element = format!("<meta name=\"pp-style-nonce\" nonce=\"{nonce}\">");
        assert_eq!(html.replace(&element, ""), expected);
    } else {
        assert_eq!(html, expected);
        assert_eq!(
            policy,
            "frame-src 'none'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'"
        );
    }
}
