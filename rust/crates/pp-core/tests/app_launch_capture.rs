//! T-53 (GRE-388): `perf_launch` measures real runs of a process and writes
//! Measurement files. CPU ticks and PSS come from Linux `/proc`.
#![cfg(target_os = "linux")]

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use pp_core::perf_launch::{
    CAPTURE_FILES, CaptureError, CaptureOptions, CaptureRequest, capture_run, write_capture,
};
use pp_perf::{Comparison, Measurement, Metric, Toolchain, Verdict, compare};

fn options(script: &str, idle_ms: u64) -> CaptureOptions {
    CaptureOptions {
        command: ["sh", "-c", script].map(OsString::from).to_vec(),
        ready_timeout: Duration::from_secs(10),
        idle: Duration::from_millis(idle_ms),
        stop_timeout: Duration::from_secs(5),
    }
}

/// The process has left /proc, allowing init a moment to reap an orphan.
fn gone(pid: &str) -> bool {
    let path = Path::new("/proc").join(pid);
    for _ in 0..100 {
        if !path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn ticket_t_53_capture_times_readiness_and_stops_the_app() {
    // The shell prints its own pid as the ready line, then sleeps idle.
    let sample = capture_run(&options("echo $$; exec sleep 30", 500)).unwrap();
    assert!(sample.startup_ready_ms > 0.0);
    assert!(sample.startup_ready_ms < 10_000.0);
    assert!(sample.idle_cpu_percent < 20.0, "{sample:?}");
    assert!(sample.pss_bytes > 0);
    assert!(
        gone(&sample.ready_line),
        "app {} still running",
        sample.ready_line
    );
}

#[test]
fn ticket_t_53_capture_counts_a_busy_child_in_the_process_tree() {
    // The ready shell stays idle; its child spins one core.
    let script = "(while :; do :; done) & echo $!; wait";
    let sample = capture_run(&options(script, 1_000)).unwrap();
    assert!(sample.idle_cpu_percent > 50.0, "{sample:?}");
    assert!(
        gone(&sample.ready_line),
        "busy child {} survived",
        sample.ready_line
    );
}

#[test]
fn ticket_t_53_capture_kills_an_app_that_ignores_sigterm() {
    // Both the ready shell and its child ignore SIGTERM, so the stop timeout,
    // the kill fallback and the group SIGKILL all run.
    let mut stubborn = options(
        "trap '' TERM; (exec sleep 30) & echo $!; exec sleep 30",
        100,
    );
    stubborn.stop_timeout = Duration::from_millis(300);
    let started = std::time::Instant::now();
    let sample = capture_run(&stubborn).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(
        gone(&sample.ready_line),
        "child {} survived",
        sample.ready_line
    );
}

#[test]
fn ticket_t_53_capture_refuses_apps_that_never_get_ready() {
    assert!(matches!(
        capture_run(&options("exit 0", 0)),
        Err(CaptureError::ExitedBeforeReady)
    ));
    // Never ready: the shell records its pid, then sleeps without a line.
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let mut slow = options(
        &format!("echo $$ > '{}'; exec sleep 30", pid_file.display()),
        0,
    );
    slow.ready_timeout = Duration::from_millis(300);
    assert!(matches!(capture_run(&slow), Err(CaptureError::NotReady(_))));
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    assert!(gone(pid.trim()), "never-ready app {pid} still running");
    let mut empty = options("", 0);
    empty.command.clear();
    assert!(matches!(
        capture_run(&empty),
        Err(CaptureError::EmptyCommand)
    ));
}

#[test]
fn ticket_t_53_capture_writes_one_record_per_metric() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("pp-server");
    std::fs::write(&artifact, b"binary under test").unwrap();
    let out = dir.path().join("run-1");
    let request = CaptureRequest {
        kind: "rust-server".into(),
        name: "pp-server".into(),
        artifact,
        package: None,
        runs: 5,
        options: options("echo ready; exec sleep 30", 200),
        noise_tolerance_pct: 50.0,
        printers: 0,
        toolchain: Toolchain {
            rustc: Some("1.99.0".into()),
            node: None,
        },
        out: out.clone(),
    };
    let summary = write_capture(&request).unwrap();
    assert_eq!(
        summary.files,
        CAPTURE_FILES.map(|(file, _)| out.join(file)).to_vec()
    );
    assert_eq!(
        (summary.simulated_printers, summary.simulator_requests),
        (0, 0)
    );

    let read = |file: &str| -> Measurement {
        serde_json::from_slice(&std::fs::read(out.join(file)).unwrap()).unwrap()
    };
    for (file, metric) in CAPTURE_FILES {
        let record = read(file);
        assert_eq!(record.metric, metric);
        assert_eq!(record.samples.len(), 5);
        assert_eq!(record.simulated_printers, 0);
        assert_eq!(record.machine.os, "linux");
        assert_eq!(record.artifact.kind, "rust-server");
        assert_eq!(
            record.artifact.sha256,
            "d8f1d7d7c46e8c286db2384895cb75c18010d60e5908d032a05efe73be9fc0c6"
        );
        assert_eq!(record.toolchain.rustc.as_deref(), Some("1.99.0"));
    }
    let size = read("package_bytes.json");
    assert_eq!(size.samples, [17.0; 5]);
    assert!(
        read("memory_linux_pss_bytes.json")
            .samples
            .iter()
            .all(|&b| b > 0.0)
    );
    assert_eq!(read("startup_ready_ms.json").metric, Metric::StartupReadyMs);
    // A run compares with itself: same setup, steady package size.
    assert!(matches!(
        compare(&size, &size),
        Comparison::Comparable {
            verdict: Verdict::Pass,
            ..
        }
    ));

    // A second capture into the same directory keeps the first.
    let again = write_capture(&request).unwrap_err();
    assert!(again.contains("startup_ready_ms.json"), "{again}");
    assert_eq!(read("package_bytes.json"), size);

    let mut none = request.clone();
    none.runs = 0;
    none.out = dir.path().join("run-0");
    assert_eq!(
        write_capture(&none).unwrap_err(),
        "at least one run is required"
    );
}
