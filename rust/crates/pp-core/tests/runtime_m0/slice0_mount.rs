//! The launcher start that `pp-server` and `pp-desktop` share, with each
//! runtime value in a child process so the environment stays isolated.
use super::{launch, request, temporary};
use axum::http::StatusCode;
use pp_core::{CoreRuntime, CoreStatus};
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

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn slice0_mount_launcher_in_child() {
    let data = std::path::PathBuf::from(std::env::var_os("PP_SLICE0_DATA").unwrap());
    let rust = std::env::var("PP_SLICE0_RUST").unwrap() == "true";
    let mut runtime = CoreRuntime::start_for_launcher(launch(data)).await.unwrap();
    assert_eq!(runtime.rust_only(), rust);
    let node_ready = matches!(
        *runtime.handle().subscribe().borrow(),
        CoreStatus::Ready { .. }
    );
    assert_eq!(node_ready, !rust, "a Node child runs only without Rust");
    let origin = runtime.origin().to_owned();
    let target = runtime.take_launch_target().unwrap().into_url();
    let bootstrap = target.strip_prefix(&origin).unwrap();
    let (status, headers, _) = request(&origin, bootstrap, "GET", None, None, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let desktop = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
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
    let receipt = runtime.shutdown().await;
    assert!(receipt.complete(), "{receipt:?}");
}
