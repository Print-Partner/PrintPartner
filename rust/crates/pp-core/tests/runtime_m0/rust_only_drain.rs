//! T-3 (GRE-448): bounded shutdown of the Rust-only runtime, the runtime that
//! ADR 0010's whole-runtime cutover ships. `drain.rs` proves the same sequence
//! for the Node-backed runtime. Here there is no Node child: tracked work is
//! cancelled, new tracked work and HTTP requests are refused while the drain
//! runs, the drain gives up after five seconds, and a timed-out drain keeps
//! the storage lock until the owning process exits. Each case runs in a child
//! process that holds `PP_DESKTOP_REBUILD=1`.
use super::watcher_proof::{proof_data, run_in_child, start_rust_only};
use super::{launch, request};
use axum::http::StatusCode;
use fs2::FileExt;
use pp_core::{CoreRuntime, MUTATION_DRAIN, ShutdownReceipt};
use serde_json::json;
use std::{fs, path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Runs the ignored `rust_only_drain::{case}` in a child and prints the
/// evidence lines it wrote.
async fn in_child(case: &str, data: &Path) {
    let stdout = run_in_child(&format!("rust_only_drain::{case}"), data).await;
    for line in stdout.lines().filter(|line| line.starts_with('{')) {
        println!("{line}");
    }
}

/// A Rust-only start on `data`, refused or not.
async fn start_for_launcher(data: &Path) -> anyhow::Result<CoreRuntime> {
    CoreRuntime::start_for_launcher(launch(data.to_owned())).await
}

fn lock_held(data: &Path) -> bool {
    let lock = fs::File::open(data.join("printpartner.lock")).unwrap();
    let held = lock.try_lock_exclusive().is_err();
    if !held {
        FileExt::unlock(&lock).unwrap();
    }
    held
}

/// Sends the head of a POST whose 100-byte body never arrives, waits for
/// `100 Continue`, and returns the open socket. The gateway counts the request
/// as in flight until the socket closes.
async fn stalled_post(origin: &str, path: &str, cookie: &str) -> tokio::net::TcpStream {
    let authority = origin.strip_prefix("http://").unwrap();
    let mut socket = tokio::net::TcpStream::connect(authority).await.unwrap();
    socket
        .write_all(
            format!(
                "POST {path} HTTP/1.1\r\nHost: {authority}\r\nOrigin: {origin}\r\nCookie: {cookie}\r\nContent-Type: application/json\r\nContent-Length: 100\r\nExpect: 100-continue\r\n\r\n"
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
    assert_eq!(
        response,
        b"HTTP/1.1 100 Continue\r\n\r\n",
        "{}",
        String::from_utf8_lossy(&response)
    );
    socket
}

/// A timed-out drain: the receipt names exactly `errors`, keeps storage, and
/// a start from this process is refused even after the work ends.
async fn assert_retained(
    data: &Path,
    receipt: &ShutdownReceipt,
    gateway_idle: bool,
    errors: &[&str],
) -> String {
    assert_eq!(
        serde_json::to_value(receipt).unwrap(),
        json!({
            "proof_class":"headless_unsigned", "tracked_idle":false,
            "gateway_idle":gateway_idle, "compat_exit":null, "compat_reaped":true,
            "gateway_stopped":true, "storage_released":false, "marker_removed":false,
            "runtime_removed":false, "errors":errors
        })
    );
    assert!(!receipt.complete());
    assert!(
        lock_held(data),
        "a timed-out drain released the storage lock"
    );
    let error = start_for_launcher(data)
        .await
        .err()
        .expect("a timed-out drain must keep storage owned");
    format!("{error:#}")
}

#[tokio::test]
async fn ticket_t_3_drain_rust_only_runtime_cancels_rejects_and_restarts() {
    let data = super::temporary("t3-rust-only-drain");
    in_child(
        "ticket_t_3_drain_rust_only_runtime_cancels_rejects_and_restarts_in_child",
        &data,
    )
    .await;
    let _ = fs::remove_dir_all(data);
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn ticket_t_3_drain_rust_only_runtime_cancels_rejects_and_restarts_in_child() {
    let data = proof_data();
    for cycle in 1..=3u32 {
        let (runtime, origin, desktop) = start_rust_only(&data).await;
        assert!(
            runtime.handle().compat_status().is_none(),
            "the Rust-only runtime started a Node child"
        );
        assert!(lock_held(&data));
        // One owner at a time: a second start on the same directory is refused
        // while this one runs.
        assert!(start_for_launcher(&data).await.is_err());
        let handle = runtime.handle();
        let work = handle.track_shutdown_work().unwrap();
        let cancelled = work.cancellation();
        let shutdown = tokio::spawn(runtime.shutdown());
        tokio::time::timeout(Duration::from_secs(2), cancelled.cancelled())
            .await
            .expect("shutdown did not cancel tracked work");
        assert!(handle.track_shutdown_work().is_err());
        let (code, _, _) = request(
            &origin,
            "/auth/login",
            "POST",
            Some(&desktop),
            None,
            None,
            Some(json!({"email": "drain@example.test", "password": "password-1234"})),
        )
        .await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert!(lock_held(&data));
        assert!(
            !shutdown.is_finished(),
            "shutdown ended before tracked work"
        );
        drop(work);
        let receipt = tokio::time::timeout(Duration::from_secs(15), shutdown)
            .await
            .expect("shutdown exceeded fifteen seconds")
            .unwrap();
        assert!(receipt.complete(), "{receipt:?}");
        assert_eq!(receipt.compat_exit, None);
        assert!(receipt.tracked_idle && receipt.gateway_idle && receipt.storage_released);
        assert!(!lock_held(&data));
        println!(
            "{}",
            json!({"case":"rust_only_cooperative", "cycle":cycle, "shutdown":receipt, "node_child":false})
        );
    }
}

#[tokio::test]
async fn ticket_t_3_drain_rust_only_runtime_timeout_retains_storage_until_exit() {
    let data = super::temporary("t3-rust-only-timeout");
    in_child(
        "ticket_t_3_drain_rust_only_runtime_timeout_retains_storage_until_exit_in_child",
        &data,
    )
    .await;
    // The owning process has exited, so the next process starts.
    in_child("ticket_t_3_drain_rust_only_restart_in_child", &data).await;
    let _ = fs::remove_dir_all(data);
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn ticket_t_3_drain_rust_only_runtime_timeout_retains_storage_until_exit_in_child() {
    let data = proof_data();
    let (runtime, _, _) = start_rust_only(&data).await;
    let handle = runtime.handle();
    let work = handle.track_shutdown_work().unwrap();
    let started = tokio::time::Instant::now();
    let receipt = runtime.shutdown().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= MUTATION_DRAIN && elapsed < Duration::from_secs(12),
        "{elapsed:?}"
    );
    assert!(work.cancellation().is_cancelled());
    assert!(handle.track_shutdown_work().is_err());
    let errors = ["tracked_drain_timed_out", "storage_ownership_retained"];
    let restart_error = assert_retained(&data, &receipt, true, &errors).await;
    drop(work);
    assert!(
        start_for_launcher(&data).await.is_err(),
        "the lock outlived the work but not this process"
    );
    println!(
        "{}",
        json!({"case":"rust_only_tracked_timeout", "shutdown":receipt, "elapsed_ms":elapsed.as_millis(), "restart_error":restart_error})
    );
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn ticket_t_3_drain_rust_only_restart_in_child() {
    let data = proof_data();
    let (runtime, _, _) = start_rust_only(&data).await;
    let receipt = runtime.shutdown().await;
    assert!(receipt.complete(), "{receipt:?}");
    println!(
        "{}",
        json!({"case":"rust_only_restart_after_owner_exit", "shutdown":receipt})
    );
}

#[tokio::test]
async fn ticket_t_3_drain_rust_only_runtime_shares_one_deadline() {
    let data = super::temporary("t3-rust-only-deadline");
    in_child(
        "ticket_t_3_drain_rust_only_runtime_shares_one_deadline_in_child",
        &data,
    )
    .await;
    let _ = fs::remove_dir_all(data);
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn ticket_t_3_drain_rust_only_runtime_shares_one_deadline_in_child() {
    let data = proof_data();
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let handle = runtime.handle();
    let work = handle.track_shutdown_work().unwrap();
    let socket = stalled_post(&origin, "/auth/login", &desktop).await;
    let started = tokio::time::Instant::now();
    let receipt = runtime.shutdown().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= MUTATION_DRAIN && elapsed < MUTATION_DRAIN + Duration::from_secs(4),
        "the gateway and tracked work must share one five-second deadline, got {elapsed:?}"
    );
    let errors = [
        "tracked_drain_timed_out",
        "gateway_drain_timed_out",
        "storage_ownership_retained",
    ];
    assert_retained(&data, &receipt, false, &errors).await;
    drop(work);
    drop(socket);
    println!(
        "{}",
        json!({"case":"rust_only_shared_deadline", "shutdown":receipt, "elapsed_ms":elapsed.as_millis()})
    );
}
