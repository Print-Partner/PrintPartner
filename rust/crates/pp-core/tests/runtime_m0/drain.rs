use super::{compat, launch, request, temporary};
use axum::http::StatusCode;
use fs2::FileExt;
use pp_compat::CompatExit;
use pp_core::{CoreRuntime, CoreStatus, MUTATION_DRAIN, ShutdownReceipt};
use pp_gateway::ownership::cutover::{CUTOVER_FILE, CutoverRefusal};
use serde_json::json;
use std::{fs, path::PathBuf, time::Duration};

#[path = "ticket_t_3_drain_compression.rs"]
mod compression;

async fn isolated_case(name: &str, rebuild: bool) -> Option<PathBuf> {
    isolated_case_for(name, rebuild, Duration::from_secs(240)).await
}

async fn isolated_case_for(name: &str, rebuild: bool, limit: Duration) -> Option<PathBuf> {
    if std::env::var("PP_DRAIN_CASE").ok().as_deref() == Some(name) {
        return Some(PathBuf::from(std::env::var_os("PP_DRAIN_DATA").unwrap()));
    }
    let data = temporary("t3-runtime-drain");
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", &format!("drain::{name}"), "--nocapture"])
        .env("PP_DRAIN_CASE", name)
        .env("PP_DRAIN_DATA", &data)
        .env("PP_DESKTOP_REBUILD", if rebuild { "1" } else { "0" })
        .kill_on_drop(true);
    let output = tokio::time::timeout(limit, child.output())
        .await
        .unwrap_or_else(|_| panic!("isolated drain case exceeded {limit:?}"))
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{stdout}");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("1 passed"), "child ran no tests");
    if name.contains("timeout") {
        assert!(!data.join(CUTOVER_FILE).exists());
        let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
        let receipt = runtime.shutdown().await;
        assert!(receipt.complete(), "{receipt:?}");
        println!(
            "{}",
            json!({"case": name, "restart_after_owner_exit": receipt})
        );
    }
    fs::remove_dir_all(data).unwrap();
    None
}

fn pid(runtime: &CoreRuntime) -> u32 {
    match *compat(runtime).borrow() {
        CoreStatus::Ready { pid, .. } => pid,
        ref state => panic!("expected ready child: {state:?}"),
    }
}

async fn bootstrap(runtime: &mut CoreRuntime) -> String {
    let url = runtime.take_launch_target().unwrap().into_url();
    let path = url.strip_prefix(runtime.origin()).unwrap();
    let (status, headers, _) = request(runtime.origin(), path, "GET", None, None, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

fn assert_clean(receipt: &ShutdownReceipt, child: u32) {
    assert!(receipt.complete(), "{receipt:?}");
    assert!(receipt.tracked_idle && receipt.gateway_idle);
    assert_eq!(receipt.compat_exit, Some(CompatExit::Graceful));
    assert!(pp_core::process_group_absent(child));
}

#[tokio::test]
async fn ticket_t_3_drain_runtime_cancels_rejects_and_restarts() {
    let Some(data) = isolated_case(
        "ticket_t_3_drain_runtime_cancels_rejects_and_restarts",
        false,
    )
    .await
    else {
        return;
    };
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let child = pid(&runtime);
    let cookie = bootstrap(&mut runtime).await;
    let origin = runtime.origin().to_owned();
    let handle = runtime.handle();
    let work = handle.track_shutdown_work().unwrap();
    let cancelled = work.cancellation();
    let shutdown = tokio::spawn(runtime.shutdown());
    tokio::time::timeout(Duration::from_secs(2), cancelled.cancelled())
        .await
        .unwrap();
    assert!(handle.track_shutdown_work().is_err());
    let lock = fs::File::open(data.join("printpartner.lock")).unwrap();
    assert!(lock.try_lock_exclusive().is_err());
    assert_eq!(
        request(
            &origin,
            "/api/v1/plans",
            "POST",
            Some(&cookie),
            None,
            None,
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(!shutdown.is_finished());
    drop(work);
    let receipt = tokio::time::timeout(Duration::from_secs(15), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert_clean(&receipt, child);
    lock.try_lock_exclusive().unwrap();
    FileExt::unlock(&lock).unwrap();
    println!(
        "{}",
        json!({"case": "cooperative", "shutdown": receipt, "old_child_absent": true})
    );
    let restarted = CoreRuntime::start(launch(data.clone())).await.unwrap();
    assert_ne!(pid(&restarted), child);
    assert!(CoreRuntime::start(launch(data)).await.is_err());
    let next_child = pid(&restarted);
    assert_clean(&restarted.shutdown().await, next_child);
}

async fn tracked_timeout(data: PathBuf, cutover: bool) {
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let child = pid(&runtime);
    let handle = runtime.handle();
    let work = handle.track_shutdown_work().unwrap();
    let started = tokio::time::Instant::now();
    let receipt = if cutover {
        #[cfg(feature = "desktop_rebuild")]
        {
            let (receipt, cutover) = runtime
                .shutdown_with_cutover(OPERATION, &parity())
                .await
                .unwrap_or_else(|error| panic!("{:?}", error.1));
            assert_eq!(
                serde_json::to_value(&cutover).unwrap(),
                json!({"outcome":"drain_timed_out", "operation":OPERATION, "tracked_idle":false, "gateway_idle":true})
            );
            assert_eq!(
                events(&data).last().unwrap(),
                &json!({"event":"cutover", "outcome":"drain_timed_out", "operation":OPERATION, "tracked_idle":false, "gateway_idle":true})
            );
            receipt
        }
        #[cfg(not(feature = "desktop_rebuild"))]
        panic!("cutover requires desktop_rebuild");
    } else {
        runtime.shutdown().await
    };
    let elapsed = started.elapsed();
    assert!(
        elapsed >= MUTATION_DRAIN && elapsed < Duration::from_secs(12),
        "{elapsed:?}"
    );
    assert_eq!(
        serde_json::to_value(&receipt).unwrap(),
        json!({
            "proof_class":"headless_unsigned", "tracked_idle":false, "gateway_idle":true,
            "compat_exit":"graceful", "compat_reaped":true, "gateway_stopped":true,
            "storage_released":false, "marker_removed":false, "runtime_removed":false,
            "errors":["tracked_drain_timed_out", "storage_ownership_retained"]
        })
    );
    assert!(!receipt.complete());
    assert!(pp_core::process_group_absent(child));
    assert!(handle.track_shutdown_work().is_err());
    assert!(work.cancellation().is_cancelled());
    assert!(!data.join(CUTOVER_FILE).exists());
    let lock = fs::File::open(data.join("printpartner.lock")).unwrap();
    assert!(lock.try_lock_exclusive().is_err());
    let error = CoreRuntime::start(launch(data.clone()))
        .await
        .err()
        .expect("timed-out task must retain ownership");
    assert!(
        error.to_string().contains("Closure was not proved"),
        "{error:#}"
    );
    drop(work);
    assert!(CoreRuntime::start(launch(data)).await.is_err());
    println!(
        "{}",
        json!({"case":"tracked_timeout", "shutdown":receipt, "elapsed_ms":elapsed.as_millis(), "restart_error":error.to_string(), "old_child_absent":true})
    );
}

#[tokio::test]
async fn ticket_t_3_drain_runtime_timeout_retains_storage_until_exit() {
    if let Some(data) = isolated_case(
        "ticket_t_3_drain_runtime_timeout_retains_storage_until_exit",
        false,
    )
    .await
    {
        tracked_timeout(data, false).await;
    }
}

#[tokio::test]
async fn ticket_t_3_drain_runtime_gateway_timeout_reports_incomplete() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let Some(data) = isolated_case(
        "ticket_t_3_drain_runtime_gateway_timeout_reports_incomplete",
        false,
    )
    .await
    else {
        return;
    };
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let child = pid(&runtime);
    let cookie = bootstrap(&mut runtime).await;
    let origin = runtime.origin();
    let authority = origin.strip_prefix("http://").unwrap();
    let mut socket = tokio::net::TcpStream::connect(authority).await.unwrap();
    socket.write_all(format!(
        "POST /plans HTTP/1.1\r\nHost: {authority}\r\nOrigin: {origin}\r\nCookie: {cookie}\r\nContent-Type: application/json\r\nContent-Length: 100\r\nExpect: 100-continue\r\n\r\n"
    ).as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !response.ends_with(b"\r\n\r\n") {
            response.push(socket.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    assert_eq!(response, b"HTTP/1.1 100 Continue\r\n\r\n");
    let started = tokio::time::Instant::now();
    let receipt = runtime.shutdown().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= MUTATION_DRAIN && elapsed < Duration::from_secs(12),
        "{elapsed:?}"
    );
    assert_eq!(
        serde_json::to_value(&receipt).unwrap(),
        json!({
            "proof_class":"headless_unsigned", "tracked_idle":true, "gateway_idle":false,
            "compat_exit":"graceful", "compat_reaped":true, "gateway_stopped":true,
            "storage_released":true, "marker_removed":true, "runtime_removed":true,
            "errors":["gateway_drain_timed_out"]
        })
    );
    assert!(!receipt.complete());
    assert!(pp_core::process_group_absent(child));
    drop(socket);
    println!(
        "{}",
        json!({"case":"gateway_timeout", "shutdown":receipt, "elapsed_ms":elapsed.as_millis()})
    );
}

#[tokio::test]
async fn ticket_t_3_drain_runtime_shares_one_deadline() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let Some(data) = isolated_case("ticket_t_3_drain_runtime_shares_one_deadline", false).await
    else {
        return;
    };
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let child = pid(&runtime);
    let cookie = bootstrap(&mut runtime).await;
    let handle = runtime.handle();
    let work = handle.track_shutdown_work().unwrap();
    let origin = runtime.origin().to_owned();
    let authority = origin.strip_prefix("http://").unwrap().to_owned();
    let mut socket = tokio::net::TcpStream::connect(&authority).await.unwrap();
    socket
        .write_all(
            format!(
                "POST /plans HTTP/1.1\r\nHost: {authority}\r\nOrigin: {origin}\r\nCookie: {cookie}\r\nContent-Type: application/json\r\nContent-Length: 100\r\nExpect: 100-continue\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !response.ends_with(b"\r\n\r\n") {
            response.push(socket.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    assert_eq!(response, b"HTTP/1.1 100 Continue\r\n\r\n");
    let started = tokio::time::Instant::now();
    let receipt = runtime.shutdown().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= MUTATION_DRAIN && elapsed < MUTATION_DRAIN + Duration::from_secs(4),
        "live Gateway and tracked work must share one five-second deadline, got {elapsed:?}"
    );
    assert_eq!(
        serde_json::to_value(&receipt).unwrap(),
        json!({
            "proof_class":"headless_unsigned", "tracked_idle":false, "gateway_idle":false,
            "compat_exit":"graceful", "compat_reaped":true, "gateway_stopped":true,
            "storage_released":false, "marker_removed":false, "runtime_removed":false,
            "errors":["tracked_drain_timed_out", "gateway_drain_timed_out", "storage_ownership_retained"]
        })
    );
    assert!(!receipt.complete());
    assert!(pp_core::process_group_absent(child));
    assert!(work.cancellation().is_cancelled());
    assert!(handle.track_shutdown_work().is_err());
    let lock = fs::File::open(data.join("printpartner.lock")).unwrap();
    assert!(lock.try_lock_exclusive().is_err());
    let error = CoreRuntime::start(launch(data.clone()))
        .await
        .err()
        .expect("timed-out drains must retain ownership");
    assert!(
        error.to_string().contains("Closure was not proved"),
        "{error:#}"
    );
    drop(work);
    drop(socket);
    assert!(CoreRuntime::start(launch(data)).await.is_err());
    println!(
        "{}",
        json!({"case":"shared_deadline", "shutdown":receipt, "elapsed_ms":elapsed.as_millis()})
    );
}

#[tokio::test]
async fn ticket_t_3_drain_runtime_flag_off_refusal_keeps_running() {
    let Some(data) = isolated_case(
        "ticket_t_3_drain_runtime_flag_off_refusal_keeps_running",
        false,
    )
    .await
    else {
        return;
    };
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let child = pid(&runtime);
    let (runtime, refusal) = *runtime
        .shutdown_with_cutover("http:GET /filaments/catalog current", "{}")
        .await
        .expect_err("flag-off refusal");
    assert_eq!(refusal, CutoverRefusal::FlagOff);
    assert_eq!(pid(&runtime), child);
    assert!(runtime.handle().track_shutdown_work().is_ok());
    assert!(!data.join(CUTOVER_FILE).exists());
    assert_clean(&runtime.shutdown().await, child);
}

#[cfg(feature = "desktop_rebuild")]
const OPERATION: &str = "http:GET /filaments/catalog current";

#[cfg(feature = "desktop_rebuild")]
fn parity() -> String {
    parity_for(OPERATION)
}

#[cfg(feature = "desktop_rebuild")]
fn parity_for(operation: &str) -> String {
    json!({"schema":"pp-operation-parity-v1", "operation":operation,
        "manifest_sha256":pp_gateway::ownership::manifest::admit_bundled_manifest().unwrap().digest(),
        "fixtures":1, "passed":1, "failed":0}).to_string()
}

#[cfg(feature = "desktop_rebuild")]
fn events(data: &std::path::Path) -> Vec<serde_json::Value> {
    fs::read_to_string(data.join(pp_gateway::ownership::cutover::EVIDENCE_FILE))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn ticket_t_3_drain_runtime_cutover_timeout_preserves_node() {
    if let Some(data) = isolated_case(
        "ticket_t_3_drain_runtime_cutover_timeout_preserves_node",
        true,
    )
    .await
    {
        tracked_timeout(data, true).await;
    }
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn ticket_t_3_drain_runtime_cutover_recovers_every_alias() {
    let Some(data) = isolated_case(
        "ticket_t_3_drain_runtime_cutover_recovers_every_alias",
        true,
    )
    .await
    else {
        return;
    };
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    assert!(runtime.desktop_rebuild().enabled());
    let child = pid(&runtime);
    let (shutdown, cutover) = runtime
        .shutdown_with_cutover(OPERATION, &parity())
        .await
        .unwrap_or_else(|error| panic!("{:?}", error.1));
    assert_clean(&shutdown, child);
    assert!(matches!(
        cutover,
        pp_gateway::ownership::cutover::CutoverReceipt::Committed {
            participants: 4,
            ..
        }
    ));
    let record = fs::read(data.join(CUTOVER_FILE)).unwrap();
    println!(
        "{}",
        json!({"case":"commit", "shutdown":shutdown, "cutover":cutover})
    );
    for _ in 0..2 {
        let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
        assert!(pp_core::process_group_absent(child));
        let new_child = pid(&runtime);
        assert_ne!(child, new_child);
        let cookie = bootstrap(&mut runtime).await;
        for method in ["GET", "HEAD"] {
            for path in ["/filaments/catalog", "/api/v1/filaments/catalog"] {
                let (status, _, body) = request(
                    runtime.origin(),
                    path,
                    method,
                    Some(&cookie),
                    None,
                    None,
                    None,
                )
                .await;
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method} {path}");
                if method == "GET" {
                    assert!(String::from_utf8_lossy(&body).contains("rust_owner_unavailable"));
                }
            }
        }
        assert_eq!(
            request(
                runtime.origin(),
                "/auth/me",
                "GET",
                Some(&cookie),
                None,
                None,
                None
            )
            .await
            .0,
            StatusCode::OK
        );
        let (runtime, refusal) = *runtime
            .shutdown_with_cutover(OPERATION, &parity())
            .await
            .expect_err("already owned");
        assert_eq!(refusal, CutoverRefusal::AlreadyRustOwned);
        assert_clean(&runtime.shutdown().await, new_child);
        assert_eq!(fs::read(data.join(CUTOVER_FILE)).unwrap(), record);
    }
    let boots: Vec<_> = events(&data)
        .into_iter()
        .filter(|event| event["event"] == "boot")
        .collect();
    assert_eq!(
        boots,
        vec![
            json!({"event":"boot", "rust_operations":[OPERATION], "abandoned_temporaries":0, "evidence_saved":true});
            2
        ]
    );
    println!(
        "{}",
        json!({"case":"restart", "boots":boots, "authority_unchanged":true})
    );
}

#[cfg(feature = "desktop_rebuild")]
async fn slicer_instance_ids(runtime: &CoreRuntime, cookie: &str) -> Vec<String> {
    let (status, _, body) = request(
        runtime.origin(),
        "/slicer-instances",
        "GET",
        Some(cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    body["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|instance| instance["id"].as_str().unwrap().to_owned())
        .collect()
}

/// Node's profile-sync worker seeds the stock slicer instances while
/// `buildApp` starts it, before Node reports ready. An empty list after a
/// restart therefore shows that Node never started the worker.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn ticket_t_3_drain_runtime_cutover_fences_profile_sync_worker() {
    const PROFILE_SYNC: &str = "profile.sync";
    let Some(data) = isolated_case(
        "ticket_t_3_drain_runtime_cutover_fences_profile_sync_worker",
        true,
    )
    .await
    else {
        return;
    };
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let child = pid(&runtime);
    let cookie = bootstrap(&mut runtime).await;
    let seeded = slicer_instance_ids(&runtime, &cookie).await;
    assert!(!seeded.is_empty(), "Node worker must seed before cutover");
    for id in &seeded {
        let path = format!("/slicer-instances/{id}");
        let (status, _, _) = request(
            runtime.origin(),
            &path,
            "DELETE",
            Some(&cookie),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{path}");
    }
    assert!(slicer_instance_ids(&runtime, &cookie).await.is_empty());
    // The parity receipt is synthetic, as in the other runtime cutover tests.
    // Rust has no profile-sync handler, so no real parity fixture exists; the
    // receipt checks are in pp-gateway `ticket_t_3_drain_cutover_parity_refusals`.
    let (shutdown, cutover) = runtime
        .shutdown_with_cutover(PROFILE_SYNC, &parity_for(PROFILE_SYNC))
        .await
        .unwrap_or_else(|error| panic!("{:?}", error.1));
    assert_clean(&shutdown, child);
    assert!(matches!(
        cutover,
        pp_gateway::ownership::cutover::CutoverReceipt::Committed {
            participants: 3,
            ..
        }
    ));
    let record = fs::read(data.join(CUTOVER_FILE)).unwrap();
    println!(
        "{}",
        json!({"case":"profile_sync_commit", "seeded":seeded.len(), "shutdown":shutdown, "cutover":cutover})
    );
    for _ in 0..2 {
        let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
        assert!(pp_core::process_group_absent(child));
        let new_child = pid(&runtime);
        assert_ne!(child, new_child);
        let cookie = bootstrap(&mut runtime).await;
        assert!(
            slicer_instance_ids(&runtime, &cookie).await.is_empty(),
            "Node started the Rust-owned profile-sync worker"
        );
        for method in ["GET", "HEAD"] {
            let (status, _, body) = request(
                runtime.origin(),
                "/ws/profile-sync",
                method,
                Some(&cookie),
                None,
                None,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method}");
            if method == "GET" {
                assert!(String::from_utf8_lossy(&body).contains("rust_owner_unavailable"));
            }
        }
        let (runtime, refusal) = *runtime
            .shutdown_with_cutover(PROFILE_SYNC, &parity_for(PROFILE_SYNC))
            .await
            .expect_err("already owned");
        assert_eq!(refusal, CutoverRefusal::AlreadyRustOwned);
        assert_clean(&runtime.shutdown().await, new_child);
        assert_eq!(fs::read(data.join(CUTOVER_FILE)).unwrap(), record);
    }
    let boots: Vec<_> = events(&data)
        .into_iter()
        .filter(|event| event["event"] == "boot")
        .collect();
    assert_eq!(
        boots,
        vec![
            json!({"event":"boot", "rust_operations":[PROFILE_SYNC], "abandoned_temporaries":0, "evidence_saved":true});
            2
        ]
    );
    println!(
        "{}",
        json!({"case":"profile_sync_restart", "boots":boots, "worker_started":false})
    );
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn ticket_t_3_drain_runtime_forced_compat_refuses_cutover() {
    let Some(data) = isolated_case(
        "ticket_t_3_drain_runtime_forced_compat_refuses_cutover",
        true,
    )
    .await
    else {
        return;
    };
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let child = pid(&runtime);
    crate::process::stop(child);
    let started = tokio::time::Instant::now();
    let (shutdown, cutover) = runtime
        .shutdown_with_cutover(OPERATION, &parity())
        .await
        .unwrap_or_else(|error| panic!("{:?}", error.1));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= pp_core::SIGTERM_GRACE && elapsed < Duration::from_secs(18),
        "{elapsed:?}"
    );
    assert!(shutdown.complete(), "{shutdown:?}");
    assert_eq!(shutdown.compat_exit, Some(CompatExit::Forced));
    assert!(pp_core::process_group_absent(child));
    assert_eq!(
        serde_json::to_value(&cutover).unwrap(),
        json!({"outcome":"compat_force_killed", "operation":OPERATION})
    );
    assert_eq!(
        events(&data).last().unwrap(),
        &json!({"event":"cutover", "outcome":"compat_force_killed", "operation":OPERATION})
    );
    assert!(!data.join(CUTOVER_FILE).exists());
    let restarted = CoreRuntime::start(launch(data)).await.unwrap();
    let next_child = pid(&restarted);
    assert_ne!(child, next_child);
    assert_clean(&restarted.shutdown().await, next_child);
    println!(
        "{}",
        json!({"case":"forced_compat", "shutdown":shutdown, "cutover":cutover, "elapsed_ms":elapsed.as_millis(), "old_child_absent":true})
    );
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn ticket_t_3_drain_legal_summary_alias_parity() {
    let Some(data) = isolated_case("ticket_t_3_drain_legal_summary_alias_parity", true).await
    else {
        return;
    };
    const LEGAL: &str = "http:GET /legal/summary current";
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    let mut fixtures = Vec::new();
    for method in ["GET", "HEAD"] {
        for path in ["/legal/summary", "/api/v1/legal/summary"] {
            let (status, headers, body) = request(
                runtime.origin(),
                path,
                method,
                Some(&cookie),
                None,
                None,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
            assert_eq!(
                headers["content-length"]
                    .to_str()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap(),
                include_bytes!("../../../../../web/apps/server/src/data/legal/LICENSE-SUMMARY.md")
                    .len()
            );
            if method == "GET" {
                assert_eq!(
                    body,
                    include_bytes!(
                        "../../../../../web/apps/server/src/data/legal/LICENSE-SUMMARY.md"
                    )
                );
            } else {
                assert!(body.is_empty());
            }
            fixtures.push((method, path, status, headers, body));
        }
    }
    println!("legal_summary Node fixtures: 4 passed; 0 failed");
    let failed = json!({"schema":"pp-operation-parity-v1", "operation":LEGAL,
        "manifest_sha256":pp_gateway::ownership::manifest::admit_bundled_manifest().unwrap().digest(),
        "fixtures":4,"passed":3,"failed":1}).to_string();
    let (returned, refusal) = *runtime
        .shutdown_with_cutover(LEGAL, &failed)
        .await
        .expect_err("failed parity must not cut over");
    assert!(matches!(refusal, CutoverRefusal::ParityFailures { .. }));
    assert!(!data.join(CUTOVER_FILE).exists());
    let child = pid(&returned);
    // A separate gateway with no compat endpoint probes the Rust candidate.
    // Its test authority never changes the running desktop's ownership record.
    let candidate_data = data.join("parity-candidate");
    fs::create_dir(&candidate_data).unwrap();
    let index = pp_gateway::ownership::manifest::admit_bundled_manifest().unwrap();
    let candidate_receipt = json!({"schema":"pp-operation-parity-v1", "operation":LEGAL,
        "manifest_sha256":index.digest(),"fixtures":1,"passed":1,"failed":0})
    .to_string();
    let candidate_plan = pp_gateway::ownership::cutover::plan(
        Some(&index),
        &pp_gateway::ownership::cutover::OperationOwners::node_only(),
        LEGAL,
        &candidate_receipt,
    )
    .unwrap();
    assert!(matches!(
        candidate_plan.commit(
            &candidate_data,
            pp_gateway::ownership::cutover::DrainEvidence {
                tracked_idle: true,
                gateway_idle: true,
                compat: Some(CompatExit::Graceful),
            }
        ),
        pp_gateway::ownership::cutover::CutoverReceipt::Committed { .. }
    ));
    let (owners, _) =
        pp_gateway::ownership::cutover::recover(&candidate_data, Some(&index)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut candidate = pp_gateway::Gateway::start(
        listener,
        PathBuf::new(),
        pp_compat::CompatHandle::unpublished(),
        &owners,
    )
    .unwrap();
    let target = candidate.take_launch_target().unwrap().into_url();
    let (status, headers, _) = request(
        candidate.origin(),
        target.strip_prefix(candidate.origin()).unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    for (method, path, expected_status, expected_headers, expected_body) in &fixtures {
        let (status, headers, body) = request(
            candidate.origin(),
            path,
            method,
            Some(cookie),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(status, *expected_status, "Rust candidate {method} {path}");
        for header in [
            "content-type",
            "content-length",
            "cache-control",
            "x-ratelimit-limit",
            "x-ratelimit-remaining",
        ] {
            assert_eq!(
                headers.get(header),
                expected_headers.get(header),
                "candidate {method} {path} {header}"
            );
        }
        assert_eq!(&body, expected_body, "candidate {method} {path}");
    }
    candidate.stop().await.unwrap();
    assert!(!data.join(CUTOVER_FILE).exists());
    println!(
        "legal_summary Node/Rust HTTP parity: 4 passed; 0 failed; desktop authority still Node"
    );
    let passed = json!({"schema":"pp-operation-parity-v1", "operation":LEGAL,
        "manifest_sha256":pp_gateway::ownership::manifest::admit_bundled_manifest().unwrap().digest(),
        "fixtures":fixtures.len(),"passed":fixtures.len(),"failed":0}).to_string();
    let (shutdown, cutover) = returned
        .shutdown_with_cutover(LEGAL, &passed)
        .await
        .unwrap_or_else(|error| panic!("{:?}", error.1));
    assert_clean(&shutdown, child);
    assert!(matches!(
        cutover,
        pp_gateway::ownership::cutover::CutoverReceipt::Committed {
            participants: 4,
            ..
        }
    ));
    let record = fs::read(data.join(CUTOVER_FILE)).unwrap();
    for restart in 1..=2 {
        let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
        let child = pid(&runtime);
        let cookie = bootstrap(&mut runtime).await;
        for (method, path, expected_status, expected_headers, expected_body) in &fixtures {
            struct Resume(u32);
            impl Drop for Resume {
                fn drop(&mut self) {
                    crate::process::resume(self.0);
                }
            }
            crate::process::stop(child);
            let resume = Resume(child);
            let response = tokio::time::timeout(
                Duration::from_secs(5),
                request(
                    runtime.origin(),
                    path,
                    method,
                    Some(&cookie),
                    None,
                    None,
                    None,
                ),
            )
            .await;
            drop(resume);
            let (status, headers, body) =
                response.expect("Rust alias must not wait for stopped Node");
            assert_eq!(status, *expected_status, "Rust alias {method} {path}");
            for header in [
                "content-type",
                "content-length",
                "cache-control",
                "x-ratelimit-limit",
                "x-ratelimit-remaining",
            ] {
                assert_eq!(
                    headers.get(header),
                    expected_headers.get(header),
                    "{method} {path} {header}"
                );
            }
            assert_eq!(&body, expected_body, "{method} {path}");
        }
        assert_clean(&runtime.shutdown().await, child);
        assert_eq!(fs::read(data.join(CUTOVER_FILE)).unwrap(), record);
        println!(
            "{}",
            json!({"case":"legal_summary_parity","restart":restart,"aliases":4,"passed":4,"failed":0,"node_stopped_during_requests":true,"authority_unchanged":true})
        );
    }
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn ticket_t_3_drain_legal_summary_flag_off() {
    let Some(data) = isolated_case("ticket_t_3_drain_legal_summary_flag_off", false).await else {
        return;
    };
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let child = pid(&runtime);
    let (mut runtime, refusal) = *runtime
        .shutdown_with_cutover("http:GET /legal/summary current", "{}")
        .await
        .expect_err("flag is off");
    assert_eq!(refusal, CutoverRefusal::FlagOff);
    assert!(!data.join(CUTOVER_FILE).exists());
    let cookie = bootstrap(&mut runtime).await;
    let (status, _, body) = request(
        runtime.origin(),
        "/api/v1/legal/summary",
        "GET",
        Some(&cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        include_bytes!("../../../../../web/apps/server/src/data/legal/LICENSE-SUMMARY.md")
    );
    assert_clean(&runtime.shutdown().await, child);
    println!("legal_summary flag_off: refused cutover; Node alias remains 200");
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn ticket_t_3_drain_legal_shared_rate_limit_parity() {
    let Some(data) = isolated_case("ticket_t_3_drain_legal_shared_rate_limit_parity", true).await
    else {
        return;
    };
    const LEGAL: &str = "http:GET /legal/summary current";
    let index = pp_gateway::ownership::manifest::admit_bundled_manifest().unwrap();
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let mut golden: Vec<(axum::http::HeaderValue, axum::http::HeaderValue)> = Vec::new();
    for rust_owned in [false, true] {
        let cookie = bootstrap(&mut runtime).await;
        for n in 0..1000 {
            let path = if n % 2 == 0 {
                "/legal/summary"
            } else {
                "/api/v1/legal/license"
            };
            let (status, _, _) = request(
                runtime.origin(),
                path,
                "GET",
                Some(&cookie),
                None,
                None,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "request {n}, Rust={rust_owned}");
        }
        // Every alias and HEAD must observe the same exhausted app budget.
        for method in ["GET", "HEAD"] {
            for path in [
                "/legal/summary",
                "/api/v1/legal/summary",
                "/api/v1/legal/license",
            ] {
                let (status, headers, body) = request(
                    runtime.origin(),
                    path,
                    method,
                    Some(&cookie),
                    None,
                    None,
                    None,
                )
                .await;
                assert_eq!(
                    status,
                    StatusCode::TOO_MANY_REQUESTS,
                    "exhausted {method} {path}, Rust={rust_owned}"
                );
                assert_eq!(headers["x-ratelimit-remaining"], "0");
                let reset: u64 = headers["x-ratelimit-reset"]
                    .to_str()
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!((1..=60).contains(&reset));
                assert_eq!(headers["retry-after"], headers["x-ratelimit-reset"]);
                if method == "GET" {
                    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(value["statusCode"], 429);
                    assert_eq!(value["error"], "Too Many Requests");
                    assert!(
                        value["message"]
                            .as_str()
                            .unwrap()
                            .starts_with("Rate limit exceeded, retry in ")
                    );
                } else {
                    assert!(body.is_empty());
                }
                let representation = (
                    headers["content-type"].clone(),
                    headers["content-length"].clone(),
                );
                if rust_owned {
                    assert_eq!(representation.0, golden[0].0);
                } else {
                    golden.push(representation);
                }
            }
        }
        let (status, headers, _) = request(
            runtime.origin(),
            "/health",
            "GET",
            Some(&cookie),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!headers.contains_key("x-ratelimit-limit"));
        let (status, headers, _) = request(
            runtime.origin(),
            "/api/v1/parts/999999/thumbnail",
            "GET",
            Some(&cookie),
            None,
            None,
            None,
        )
        .await;
        assert_ne!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            headers["x-ratelimit-remaining"], "999",
            "media budget is independent"
        );
        for n in 0..=120 {
            let (status, headers, _) = request(
                runtime.origin(),
                "/api/v1/printers/missing/capabilities",
                "GET",
                Some(&cookie),
                None,
                None,
                None,
            )
            .await;
            assert_eq!(
                headers["x-ratelimit-limit"], "120",
                "route-specific limit retained"
            );
            if n < 120 {
                assert_ne!(status, StatusCode::TOO_MANY_REQUESTS);
            } else {
                assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
            }
        }
        if rust_owned {
            let (status, _, _) = request(
                runtime.origin(),
                "/__desktop/rate-limit",
                "POST",
                Some(&cookie),
                None,
                None,
                None,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "public callers cannot debit quota"
            );
        }
        println!(
            "health exemption, separate media budget, stricter route quota and private debit boundary: passed; Rust={rust_owned}"
        );
        println!(
            "legal shared quota Rust={rust_owned}: 1000 mixed Node/legal requests admitted exactly once; 6 exhausted GET/HEAD requests returned 429"
        );
        if !rust_owned {
            let receipt = json!({"schema":"pp-operation-parity-v1", "operation":LEGAL,
                "manifest_sha256":index.digest(),"fixtures":4,"passed":4,"failed":0})
            .to_string();
            runtime
                .shutdown_with_cutover(LEGAL, &receipt)
                .await
                .unwrap_or_else(|e| panic!("{:?}", e.1));
            runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
        } else {
            let child = pid(&runtime);
            assert_clean(&runtime.shutdown().await, child);
            break;
        }
    }
}

#[cfg(feature = "desktop_rebuild")]
async fn legal_representation_request(
    origin: &str,
    cookie: &str,
    method: &str,
    path: &str,
    with_origin: bool,
    encoding: Option<&str>,
    accept: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    use http_body_util::BodyExt;
    let socket = tokio::net::TcpStream::connect(origin.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(socket))
            .await
            .unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", origin.strip_prefix("http://").unwrap())
        .header("cookie", cookie);
    if with_origin {
        request = request.header("origin", origin);
    }
    if let Some(encoding) = encoding {
        request = request.header("accept-encoding", encoding);
    }
    if let Some(accept) = accept {
        request = request.header("accept", accept);
    }
    let response = sender
        .send_request(request.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let (head, body) = response.into_parts();
    (
        head.status,
        head.headers,
        body.collect().await.unwrap().to_bytes().to_vec(),
    )
}

#[cfg(feature = "desktop_rebuild")]
const LEGAL_ENCODINGS: [Option<&str>; 12] = [
    None,
    Some(""),
    Some("identity"),
    Some("identity;q=0"),
    Some("gzip"),
    Some("br"),
    Some("br, gzip"),
    Some("gzip;q=0.8, br;q=0.2"),
    Some("*"),
    Some("deflate"),
    Some("gzip;q=0, br;q=0, identity;q=0"),
    Some("*;q=0"),
];

#[cfg(feature = "desktop_rebuild")]
const LEGAL_ACCEPTS: [Option<&str>; 4] = [
    None,
    Some("text/plain"),
    Some("application/json"),
    Some("*/*;q=0"),
];

#[cfg(feature = "desktop_rebuild")]
const LEGAL_REPRESENTATION_FIXTURES: usize = LEGAL_ENCODINGS.len() * LEGAL_ACCEPTS.len() * 16;

#[cfg(feature = "desktop_rebuild")]
async fn legal_representation_matrix(
    origin: &str,
    cookie: &str,
) -> Vec<std::collections::BTreeMap<String, String>> {
    for method in ["GET", "HEAD"] {
        for path in ["/legal/summary", "/api/v1/legal/summary"] {
            let (status, headers, _) = request(
                origin,
                path,
                method,
                Some(cookie),
                None,
                Some("http://foreign.invalid"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(!headers.contains_key("access-control-allow-origin"));
        }
    }
    let mut fixtures = Vec::new();
    let successful = LEGAL_REPRESENTATION_FIXTURES / 2;
    for status in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
        for method in ["GET", "HEAD"] {
            for path in ["/legal/summary", "/api/v1/legal/summary"] {
                for with_origin in [true, false] {
                    for encoding in LEGAL_ENCODINGS {
                        for accept in LEGAL_ACCEPTS {
                            let (actual, headers, body) = legal_representation_request(
                                origin,
                                cookie,
                                method,
                                path,
                                with_origin,
                                encoding,
                                accept,
                            )
                            .await;
                            assert_eq!(actual, status, "{method} {path}, Origin={with_origin}");
                            assert_eq!(
                                headers
                                    .get("access-control-allow-origin")
                                    .map(|v| v.to_str().unwrap()),
                                with_origin.then_some(origin),
                                "{status} {method} {path}, Origin={with_origin}"
                            );
                            assert_eq!(headers["access-control-allow-credentials"], "true");
                            let vary: Vec<_> = headers
                                .get_all("vary")
                                .iter()
                                .flat_map(|v| v.to_str().unwrap().split(','))
                                .map(str::trim)
                                .collect();
                            assert!(
                                !headers.contains_key("content-encoding"),
                                "{encoding:?} {headers:?}"
                            );
                            assert_eq!(
                                vary.iter()
                                    .map(|token| token.to_ascii_lowercase())
                                    .collect::<std::collections::BTreeSet<_>>(),
                                std::collections::BTreeSet::from(["origin".to_owned()])
                            );
                            assert_eq!(headers["cache-control"], "no-store");
                            assert_eq!(headers["x-ratelimit-limit"], "1000");
                            let remaining = if status == StatusCode::OK {
                                999 - fixtures.len()
                            } else {
                                0
                            };
                            assert_eq!(headers["x-ratelimit-remaining"], remaining.to_string());
                            let reset: u64 = headers["x-ratelimit-reset"]
                                .to_str()
                                .unwrap()
                                .parse()
                                .unwrap();
                            assert!((1..=60).contains(&reset));
                            let expected = if status == StatusCode::OK {
                                assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
                                assert!(!headers.contains_key("retry-after"));
                                include_bytes!("../../../../../web/apps/server/src/data/legal/LICENSE-SUMMARY.md").to_vec()
                            } else {
                                assert_eq!(
                                    headers["content-type"],
                                    "application/json; charset=utf-8"
                                );
                                assert_eq!(headers["retry-after"], reset.to_string());
                                let after = match reset {
                                    60 => "1 minute".to_owned(),
                                    1 => "1 second".to_owned(),
                                    n => format!("{n} seconds"),
                                };
                                format!(r#"{{"statusCode":429,"error":"Too Many Requests","message":"Rate limit exceeded, retry in {after}"}}"#).into_bytes()
                            };
                            assert_eq!(headers["content-length"], expected.len().to_string());
                            if method == "HEAD" {
                                assert!(body.is_empty());
                            } else {
                                assert_eq!(
                                    body, expected,
                                    "{status} {method} {path} {encoding:?} {accept:?}"
                                );
                            }
                            let metadata = headers
                                .iter()
                                .filter(|(name, _)| {
                                    name.as_str().starts_with("access-control-")
                                        || matches!(
                                            name.as_str(),
                                            "vary"
                                                | "content-type"
                                                | "content-encoding"
                                                | "cache-control"
                                                | "x-ratelimit-limit"
                                                | "x-ratelimit-remaining"
                                        )
                                })
                                .map(|(name, value)| {
                                    let value = if name.as_str() == "access-control-allow-origin" {
                                        "<request-origin>".to_owned()
                                    } else {
                                        value.to_str().unwrap().to_owned()
                                    };
                                    (name.to_string(), value)
                                })
                                .collect();
                            fixtures.push(metadata);
                        }
                    }
                }
            }
        }
        if status == StatusCode::OK {
            for n in successful..1000 {
                assert_eq!(
                    legal_representation_request(
                        origin,
                        cookie,
                        "GET",
                        "/legal/summary",
                        false,
                        None,
                        None
                    )
                    .await
                    .0,
                    StatusCode::OK,
                    "quota filler {n}"
                );
            }
        }
    }
    assert_eq!(fixtures.len(), successful * 2);
    fixtures
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn ticket_t_3_drain_legal_summary_representation_parity() {
    let Some(data) =
        isolated_case("ticket_t_3_drain_legal_summary_representation_parity", true).await
    else {
        return;
    };
    const LEGAL: &str = "http:GET /legal/summary current";
    let index = pp_gateway::ownership::manifest::admit_bundled_manifest().unwrap();
    let receipt = |passed, failed| {
        json!({
            "schema":"pp-operation-parity-v1", "operation":LEGAL,
            "manifest_sha256":index.digest(), "fixtures":LEGAL_REPRESENTATION_FIXTURES, "passed":passed, "failed":failed,
        })
        .to_string()
    };
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    let golden = legal_representation_matrix(runtime.origin(), &cookie).await;
    println!(
        "legal representation Node: {LEGAL_REPRESENTATION_FIXTURES} passed; 0 failed; allowed/absent Origin, GET/HEAD, both aliases, 200/429"
    );
    let (returned, refusal) = *runtime
        .shutdown_with_cutover(LEGAL, &receipt(LEGAL_REPRESENTATION_FIXTURES - 1, 1))
        .await
        .expect_err("incomplete representation parity must refuse cutover");
    assert!(matches!(refusal, CutoverRefusal::ParityFailures { .. }));
    assert!(!data.join(CUTOVER_FILE).exists());

    let candidate_data = data.join("representation-candidate");
    fs::create_dir(&candidate_data).unwrap();
    let plan = pp_gateway::ownership::cutover::plan(
        Some(&index),
        &pp_gateway::ownership::cutover::OperationOwners::node_only(),
        LEGAL,
        &receipt(LEGAL_REPRESENTATION_FIXTURES, 0),
    )
    .unwrap();
    assert!(matches!(
        plan.commit(
            &candidate_data,
            pp_gateway::ownership::cutover::DrainEvidence {
                tracked_idle: true,
                gateway_idle: true,
                compat: Some(CompatExit::Graceful),
            }
        ),
        pp_gateway::ownership::cutover::CutoverReceipt::Committed { .. }
    ));
    let (owners, _) =
        pp_gateway::ownership::cutover::recover(&candidate_data, Some(&index)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut candidate = pp_gateway::Gateway::start(
        listener,
        PathBuf::new(),
        pp_compat::CompatHandle::unpublished(),
        &owners,
    )
    .unwrap();
    let target = candidate.take_launch_target().unwrap().into_url();
    let (status, headers, _) = request(
        candidate.origin(),
        target.strip_prefix(candidate.origin()).unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let actual = legal_representation_matrix(candidate.origin(), cookie).await;
    assert_eq!(
        actual, golden,
        "candidate representation metadata differs from Node"
    );
    candidate.stop().await.unwrap();
    assert!(!data.join(CUTOVER_FILE).exists());
    println!(
        "legal representation candidate: {LEGAL_REPRESENTATION_FIXTURES} passed; 0 failed; desktop authority still Node"
    );

    let child = pid(&returned);
    let (shutdown, cutover) = returned
        .shutdown_with_cutover(LEGAL, &receipt(actual.len(), 0))
        .await
        .unwrap_or_else(|e| panic!("{:?}", e.1));
    assert_clean(&shutdown, child);
    assert!(matches!(
        cutover,
        pp_gateway::ownership::cutover::CutoverReceipt::Committed {
            participants: 4,
            ..
        }
    ));
    let record = fs::read(data.join(CUTOVER_FILE)).unwrap();
    for restart in 1..=2 {
        let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
        let child = pid(&runtime);
        let cookie = bootstrap(&mut runtime).await;
        struct Resume(u32);
        impl Drop for Resume {
            fn drop(&mut self) {
                crate::process::resume(self.0);
            }
        }
        crate::process::stop(child);
        let resume = Resume(child);
        let actual = tokio::time::timeout(
            Duration::from_secs(30),
            legal_representation_matrix(runtime.origin(), &cookie),
        )
        .await
        .expect("Rust representation matrix must not wait for stopped Node");
        drop(resume);
        assert_eq!(
            actual, golden,
            "recovered representation metadata differs from Node"
        );
        assert_clean(&runtime.shutdown().await, child);
        assert_eq!(fs::read(data.join(CUTOVER_FILE)).unwrap(), record);
        println!(
            "legal representation restart {restart}: {LEGAL_REPRESENTATION_FIXTURES} passed; 0 failed; Node stopped; authority unchanged"
        );
    }
}

include!("ticket_t_3_drain_repeated_upload.rs");
