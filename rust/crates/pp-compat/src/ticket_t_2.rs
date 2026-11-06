#![cfg(test)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use tokio_util::sync::CancellationToken;

use super::platform::test_support;

/// Exits 0 when stdin closes.
const STAY: &str = "process.stdin.on('end', () => process.exit(0)); process.stdin.resume();";

/// `script` after [`test_support::SERVE`], so it can call `serve(server, path)`.
fn served(script: &str) -> &'static str {
    Box::leak(format!("{}{script}", test_support::SERVE).into_boxed_str())
}

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-t2-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("runtime")).unwrap();
        Self { root }
    }

    fn pidfile(&self) -> PathBuf {
        self.root.join("pids")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Ok(text) = std::fs::read_to_string(self.pidfile()) {
            for pid in text.lines().filter_map(|line| line.parse().ok()) {
                super::drain::reap_process_group(pid);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn wall_fuse(limit: Duration) -> CancellationToken {
    let token = CancellationToken::new();
    let cancel = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(limit);
        cancel.cancel();
    });
    token
}

fn recorded_pids(path: &Path) -> Vec<u32> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.parse().ok())
        .collect()
}

fn live_pids(path: &Path) -> Vec<u32> {
    recorded_pids(path)
        .into_iter()
        .filter(|pid| super::drain::process_alive(*pid))
        .collect()
}

fn ignore_child(_: &Path, _: u32) -> anyhow::Result<()> {
    Ok(())
}

fn bundle(scratch: &Scratch) -> super::Bundle {
    super::Bundle {
        node: scratch.root.join("node"),
        entry: scratch.root.join("desktop.js"),
        web_root: scratch.root.join("web"),
        runtime_version: "ticket-t-2-unreleased".into(),
        commit: "ticket-t-2-unreleased".into(),
    }
}

fn release_spec(scratch: &Scratch) -> super::SpawnSpec {
    let storage_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(scratch.root.join("lease"))
        .unwrap();
    super::SpawnSpec {
        account_pin: super::DesktopAccountPin::Disabled,
        bundle: super::release::MeasuredBundle::unchecked(bundle(scratch)),
        data_dir: scratch.root.join("data"),
        runtime_dir: scratch.root.join("runtime"),
        setup_padding: String::new(),
        storage_lock,
        port: 0,
        gateway_rate_limit: false,
        settings: Vec::new(),
        before_setup: Box::new(ignore_child),
    }
}

fn shell_spec(scratch: &Scratch) -> super::ShellSpec {
    super::ShellSpec {
        script: STAY,
        bundle: bundle(scratch),
        data_dir: scratch.root.join("data"),
        runtime_dir: scratch.root.join("runtime"),
        pidfile: scratch.pidfile(),
        setup_padding: "test".into(),
        port: 0,
    }
}

async fn wait_status(
    status: &mut tokio::sync::watch::Receiver<super::Status>,
    ready: impl Fn(&super::Status) -> bool,
) {
    let fuse = wall_fuse(Duration::from_secs(8));
    loop {
        if ready(&status.borrow()) {
            return;
        }
        tokio::select! {
            biased;
            _ = fuse.cancelled() => panic!("status stayed {:?} for 8s wall time", *status.borrow()),
            changed = status.changed() => {
                changed.unwrap_or_else(|_| panic!("supervisor ended at {:?}", *status.borrow()));
            }
        }
    }
}

async fn wait_live(path: &Path, expected_records: usize) {
    let fuse = wall_fuse(Duration::from_secs(8));
    loop {
        let recorded = recorded_pids(path);
        let live = live_pids(path);
        if recorded.len() >= expected_records && live.len() == 1 {
            return;
        }
        tokio::select! {
            biased;
            _ = fuse.cancelled() => panic!(
                "expected {expected_records} spawns and 1 live child, recorded={recorded:?} live={live:?}"
            ),
            _ = tokio::task::yield_now() => {}
        }
    }
}

async fn shutdown(supervisor: super::Supervisor) -> super::CompatExit {
    let fuse = wall_fuse(Duration::from_secs(8));
    tokio::select! {
        biased;
        _ = fuse.cancelled() => panic!("shutdown exceeded 8s wall time"),
        result = supervisor.shutdown() => result.expect("supervisor shutdown"),
    }
}

#[tokio::test]
async fn ticket_t_3_drain_supervisor_shutdown_reports_graceful_exit() {
    let scratch = Scratch::new("graceful");
    let supervisor =
        super::Supervisor::launch_shell(shell_spec(&scratch), super::SupervisorConfig::default())
            .unwrap();
    wait_live(&scratch.pidfile(), 1).await;
    assert_eq!(shutdown(supervisor).await, super::CompatExit::Graceful);
    assert!(live_pids(&scratch.pidfile()).is_empty());
}

const READY: &str = r#"const http=require("node:http"); const input=require("node:readline").createInterface({input:process.stdin}); input.on("close",()=>process.exit(0)); input.once("line",line=>{const setup=JSON.parse(line); serve(http.createServer((req,res)=>{res.setHeader("Content-Type","application/json"); res.end(JSON.stringify({ok:true,version:"ticket-t-2-unreleased",release:{commit:"ticket-t-2-unreleased"}}));}), setup.socket_path);});"#;

#[tokio::test]
async fn ticket_t_3_drain_supervisor_crash_after_disarm_is_not_graceful() {
    let scratch = Scratch::new("disarmed-crash");
    let supervisor = super::Supervisor::launch_shell(
        super::ShellSpec {
            script: served(READY),
            ..shell_spec(&scratch)
        },
        super::SupervisorConfig::default(),
    )
    .unwrap();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Ready { .. })
    })
    .await;
    let pid = match *status.borrow() {
        super::Status::Ready { pid, .. } => pid,
        _ => unreachable!(),
    };
    supervisor.disarm_restarts();
    test_support::kill_child(pid);
    wait_status(&mut status, |state| matches!(state, super::Status::Stopped)).await;
    assert!(!supervisor.stop.is_cancelled());
    assert!(live_pids(&scratch.pidfile()).is_empty());
    assert_eq!(recorded_pids(&scratch.pidfile()), vec![pid]);
    assert_eq!(shutdown(supervisor).await, super::CompatExit::Interrupted);
}

#[tokio::test]
async fn ticket_t_3_drain_supervisor_recovered_crash_can_shutdown_gracefully() {
    let scratch = Scratch::new("recovered-crash");
    let supervisor = super::Supervisor::launch_shell(
        super::ShellSpec {
            script: served(READY),
            ..shell_spec(&scratch)
        },
        super::SupervisorConfig::default(),
    )
    .unwrap();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Ready { .. })
    })
    .await;
    let first = recorded_pids(&scratch.pidfile())[0];
    test_support::kill_child(first);
    wait_status(
        &mut status,
        |state| matches!(state, super::Status::Ready { pid, .. } if *pid != first),
    )
    .await;
    supervisor.disarm_restarts();
    assert_eq!(shutdown(supervisor).await, super::CompatExit::Graceful);
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 2);
    assert!(live_pids(&scratch.pidfile()).is_empty());
}

#[tokio::test]
async fn ticket_t_3_drain_supervisor_shutdown_reports_forced_exit() {
    let scratch = Scratch::new("forced");
    let spec = super::ShellSpec {
        script: "process.on('SIGTERM', () => {}); console.log('term-trap-ready'); setInterval(() => {}, 1e9)",
        ..shell_spec(&scratch)
    };
    let supervisor =
        super::Supervisor::launch_shell(spec, super::SupervisorConfig::default()).unwrap();
    wait_live(&scratch.pidfile(), 1).await;
    pump_until(|| {
        diagnostic_rows(&scratch)
            .iter()
            .any(|row| row["event"] == "compat_output")
    })
    .await;
    let started = std::time::Instant::now();
    assert_eq!(
        supervisor.shutdown().await.unwrap(),
        super::CompatExit::Forced
    );
    assert!(started.elapsed() >= super::drain::SIGTERM_GRACE);
    assert!(live_pids(&scratch.pidfile()).is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_shutdown_cancels_pending_restart() {
    let scratch = Scratch::new("shutdown");
    let wall = std::time::Instant::now();
    let supervisor = super::Supervisor::start_with_config(
        release_spec(&scratch),
        super::SupervisorConfig::default(),
    )
    .unwrap();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Backoff { .. })
    })
    .await;
    let mark = tokio::time::Instant::now();
    supervisor.disarm_restarts();
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        mark.elapsed(),
        Duration::ZERO,
        "pending restart timer kept running after controlled shutdown"
    );
    assert!(
        matches!(*supervisor.handle.status.borrow(), super::Status::Stopped),
        "controlled shutdown left the restart timer running: {:?}",
        *supervisor.handle.status.borrow()
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        matches!(*supervisor.handle.status.borrow(), super::Status::Stopped),
        "restart timer fired during the drain window: {:?}",
        *supervisor.handle.status.borrow()
    );
    shutdown(supervisor).await;
    assert!(wall.elapsed() < Duration::from_secs(8));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_repeated_start_stop_no_duplicate_child() {
    let scratch = Scratch::new("restart");
    let wall = std::time::Instant::now();
    let startup_ack_count = || {
        diagnostic_rows(&scratch)
            .iter()
            .filter(|row| row["event"] == "compat_output")
            .count()
    };
    tokio::time::resume();
    for nth in 1..=3 {
        let supervisor = super::Supervisor::launch_shell(
            super::ShellSpec {
                script: "let started = false; const input = require('node:readline').createInterface({ input: process.stdin }); input.once('line', () => { started = true; console.log('started'); }); input.on('close', () => process.exit(started ? 0 : 1));",
                ..shell_spec(&scratch)
            },
            super::SupervisorConfig::default(),
        )
        .unwrap();
        wait_live(&scratch.pidfile(), nth).await;
        pump_until(|| startup_ack_count() == nth).await;
        assert_eq!(startup_ack_count(), nth);
        assert_eq!(
            live_pids(&scratch.pidfile()).len(),
            1,
            "start left more than one child alive"
        );
        shutdown(supervisor).await;
        assert!(
            live_pids(&scratch.pidfile()).is_empty(),
            "stop left a child alive: {:?}",
            live_pids(&scratch.pidfile())
        );
    }
    assert_eq!(
        recorded_pids(&scratch.pidfile()).len(),
        3,
        "repeated start/stop spawned a duplicate child: {:?}",
        recorded_pids(&scratch.pidfile())
    );
    assert_eq!(startup_ack_count(), 3);
    let rows = diagnostic_rows(&scratch);
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "compat_reaped")
            .count(),
        3
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "compat_failure")
            .count(),
        0
    );
    assert!(wall.elapsed() < Duration::from_secs(8));
}

// Unix only until GRE-1203: on Windows the startup probe waits up to 2 s for
// the child's dial, and the child's pipe stdio blocks paused-clock auto-advance.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_3_drain_readiness_timeout_no_leaked_task() {
    let scratch = Scratch::new("ready");
    let wall = std::time::Instant::now();
    let supervisor =
        super::Supervisor::launch_shell(shell_spec(&scratch), super::SupervisorConfig::default())
            .unwrap();
    let mut status = supervisor.handle.status.clone();
    let fuse = wall_fuse(Duration::from_secs(8));
    let mut resumed = false;
    loop {
        let state = status.borrow().clone();
        if matches!(state, super::Status::Stopping) && !resumed {
            // The readiness budget is virtual. Reaping the child is real.
            tokio::time::resume();
            resumed = true;
        }
        if matches!(state, super::Status::Backoff { .. }) {
            break;
        }
        tokio::select! {
            biased;
            _ = fuse.cancelled() => panic!("readiness timeout did not settle from {state:?}"),
            changed = status.changed() => {
                changed.unwrap_or_else(|_| panic!("supervisor ended at {:?}", *status.borrow()));
            }
        }
    }
    assert_eq!(
        recorded_pids(&scratch.pidfile()).len(),
        1,
        "readiness timeout spawned more than one child: {:?}",
        recorded_pids(&scratch.pidfile())
    );
    assert!(
        live_pids(&scratch.pidfile()).is_empty(),
        "readiness timeout left the child alive: {:?}",
        live_pids(&scratch.pidfile())
    );
    if resumed {
        tokio::time::pause();
    }
    let mark = tokio::time::Instant::now();
    supervisor.disarm_restarts();
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        mark.elapsed(),
        Duration::ZERO,
        "readiness timeout left the restart timer running"
    );
    assert_eq!(
        recorded_pids(&scratch.pidfile()).len(),
        1,
        "restart timer survived readiness timeout: {:?}",
        recorded_pids(&scratch.pidfile())
    );
    assert!(
        live_pids(&scratch.pidfile()).is_empty(),
        "restart timer leaked a child: {:?}",
        live_pids(&scratch.pidfile())
    );
    assert!(
        matches!(*supervisor.handle.status.borrow(), super::Status::Stopped),
        "restart task still running after readiness timeout: {:?}",
        *supervisor.handle.status.borrow()
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 1);
    assert!(
        matches!(*supervisor.handle.status.borrow(), super::Status::Stopped),
        "restart timer fired during the drain window"
    );
    assert_eq!(shutdown(supervisor).await, super::CompatExit::Interrupted);
    assert!(live_pids(&scratch.pidfile()).is_empty());
    assert!(wall.elapsed() < Duration::from_secs(8));
    let rows = diagnostic_rows(&scratch);
    let failure = rows
        .iter()
        .find(|row| row["event"] == "compat_failure")
        .unwrap();
    assert_eq!(failure["reason"], "readiness_timeout");
    assert!(failure["termination"].is_null());
}

fn diagnostic_rows(scratch: &Scratch) -> Vec<serde_json::Value> {
    let mut rows = Vec::new();
    for index in (0..=6).rev() {
        let name = if index == 0 {
            "desktop.jsonl".to_owned()
        } else {
            format!("desktop.{index}.jsonl")
        };
        let text = match std::fs::read_to_string(scratch.root.join("data/logs").join(name)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("failed to read diagnostic log: {error}"),
        };
        rows.extend(
            text.lines()
                .map(|row| serde_json::from_str::<serde_json::Value>(row).unwrap()),
        );
    }
    rows
}

#[test]
fn ticket_t_2_diagnostic_rows_include_rotated_raw_records_chronologically() {
    let scratch = Scratch::new("rotated-diagnostics");
    let directory = scratch.root.join("data/logs");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("desktop.6.jsonl"),
        "{\"time\":1,\"event\":\"compat_starting\",\"unknown_private_field\":\"must_remain_visible\"}\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("desktop.1.jsonl"),
        "{\"time\":2,\"event\":\"compat_ready\"}\n{\"time\":3,\"event\":\"compat_failure\"}\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("desktop.jsonl"),
        "{\"time\":4,\"event\":\"compat_crash_guard\"}\n",
    )
    .unwrap();

    assert_eq!(
        diagnostic_rows(&scratch),
        vec![
            serde_json::json!({"time":1,"event":"compat_starting","unknown_private_field":"must_remain_visible"}),
            serde_json::json!({"time":2,"event":"compat_ready"}),
            serde_json::json!({"time":3,"event":"compat_failure"}),
            serde_json::json!({"time":4,"event":"compat_crash_guard"}),
        ]
    );
}

#[tokio::test]
async fn ticket_t_2_crash_diagnostics_preserve_exit_without_backend_secrets() {
    let scratch = Scratch::new("crash-facts");
    let mut spec = shell_spec(&scratch);
    spec.script = r#"require("node:readline").createInterface({ input: process.stdin }).once("line", () => { require("node:fs").writeSync(1, '{"level":50,"event":"desktop_start_failed","message":"SECRET_TOKEN_PRIVATE_PATH"}\n'); process.exit(7); });"#;
    let supervisor =
        super::Supervisor::launch_shell(spec, super::SupervisorConfig::default()).unwrap();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Backoff { .. })
    })
    .await;
    supervisor.disarm_restarts();
    shutdown(supervisor).await;
    assert!(live_pids(&scratch.pidfile()).is_empty());
    let rows = diagnostic_rows(&scratch);
    let failure = rows
        .iter()
        .find(|row| row["event"] == "compat_failure")
        .unwrap();
    assert_eq!(failure["reason"], "child_exit");
    assert_eq!(
        failure["termination"],
        serde_json::json!({"kind":"exit_code","value":7})
    );
    assert!(rows.iter().any(|row| row["event"] == "compat_start_failed"));
    assert!(
        !serde_json::to_string(&rows)
            .unwrap()
            .contains("SECRET_TOKEN_PRIVATE_PATH")
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_3_drain_never_started_child_is_graceful() {
    let scratch = Scratch::new("spawn-facts");
    let supervisor = super::Supervisor::start_with_config(
        release_spec(&scratch),
        super::SupervisorConfig::default(),
    )
    .unwrap();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Backoff { .. })
    })
    .await;
    supervisor.disarm_restarts();
    assert_eq!(shutdown(supervisor).await, super::CompatExit::Graceful);
    let rows = diagnostic_rows(&scratch);
    let failure = rows
        .iter()
        .find(|row| row["event"] == "compat_failure")
        .unwrap();
    assert_eq!(failure["reason"], "spawn_failed");
    assert!(failure["termination"].is_null());
    assert!(
        !serde_json::to_string(&rows)
            .unwrap()
            .contains(scratch.root.to_str().unwrap())
    );
}

#[tokio::test]
async fn ticket_t_2_crash_signal_is_distinct_from_parent_cleanup() {
    let scratch = Scratch::new("signal-facts");
    let mut spec = shell_spec(&scratch);
    spec.script = "require('node:readline').createInterface({ input: process.stdin }).once('line', () => process.kill(process.pid, 'SIGTERM'));";
    let supervisor =
        super::Supervisor::launch_shell(spec, super::SupervisorConfig::default()).unwrap();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Backoff { .. })
    })
    .await;
    supervisor.disarm_restarts();
    shutdown(supervisor).await;
    let rows = diagnostic_rows(&scratch);
    let failure = rows
        .iter()
        .find(|row| row["event"] == "compat_failure")
        .unwrap();
    assert_eq!(failure["reason"], "child_exit");
    assert_eq!(failure["termination"], test_support::self_terminated());
    assert!(live_pids(&scratch.pidfile()).is_empty());
}

async fn health_failure_shutdown(disarm_while_ready: bool) {
    let scratch = Scratch::new("health-facts");
    let mut spec = shell_spec(&scratch);
    spec.script = served(
        r#"const http=require("node:http"); const input=require("node:readline").createInterface({input:process.stdin}); input.on("close",()=>process.exit(0)); input.once("line",line=>{const setup=JSON.parse(line); let checks=0; serve(http.createServer((req,res)=>{checks++; res.setHeader("Content-Type","application/json"); res.end(JSON.stringify({ok:checks===1,version:"ticket-t-2-unreleased",release:{commit:"ticket-t-2-unreleased"}}));}), setup.socket_path);});"#,
    );
    let supervisor =
        super::Supervisor::launch_shell(spec, super::SupervisorConfig::default()).unwrap();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Ready { .. })
    })
    .await;
    if disarm_while_ready {
        supervisor.disarm_restarts();
    }
    tokio::time::timeout(Duration::from_secs(35), async {
        while !matches!(
            *status.borrow(),
            super::Status::Stopped | super::Status::Backoff { .. }
        ) {
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("three real ten-second liveness checks");
    supervisor.disarm_restarts();
    assert!(!supervisor.stop.is_cancelled());
    assert_eq!(shutdown(supervisor).await, super::CompatExit::Interrupted);
    let rows = diagnostic_rows(&scratch);
    assert!(rows.iter().any(|row| row["event"] == "compat_reaped"));
    if !disarm_while_ready {
        let failures: Vec<_> = rows
            .iter()
            .filter(|row| row["event"] == "compat_failure")
            .collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0]["reason"], "health_lost");
        assert!(failures[0]["termination"].is_null());
    }
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 1);
    assert!(live_pids(&scratch.pidfile()).is_empty());
}

#[tokio::test]
async fn ticket_t_3_drain_health_failure_after_disarm_is_interrupted() {
    health_failure_shutdown(true).await;
}

#[tokio::test]
async fn ticket_t_2_three_health_failures_record_cause_without_cleanup_exit() {
    health_failure_shutdown(false).await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_guard_logs_five_failure_facts_then_stops_restarts() {
    let scratch = Scratch::new("guard-facts");
    let supervisor = super::Supervisor::start_with_config(
        release_spec(&scratch),
        super::SupervisorConfig::default(),
    )
    .unwrap();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| matches!(state, super::Status::Guarded)).await;
    let rows = diagnostic_rows(&scratch);
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "compat_failure")
            .count(),
        5
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "compat_crash_guard")
            .count(),
        1
    );
    supervisor.disarm_restarts();
    shutdown(supervisor).await;
    let rows = diagnostic_rows(&scratch);
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "compat_starting")
            .count(),
        5
    );
}

// Unix only until GRE-1203, with the paused-time crash tests that use it.
#[cfg(unix)]
fn crash_guard_spec(scratch: &Scratch) -> super::ShellSpec {
    let mut spec = shell_spec(scratch);
    spec.script = r#"let setup = null; const input = require("node:readline").createInterface({ input: process.stdin }); input.once("line", (line) => { setup = JSON.parse(line); if (!require("node:fs").existsSync(setup.data_dir + "/hold")) process.exit(7); }); input.on("close", () => process.exit(setup === null ? 1 : 0));"#;
    spec
}

// Unix only until GRE-1203, with the paused-time crash tests that use it.
#[cfg(unix)]
fn shell_exited(pid: u32) -> bool {
    if super::drain::process_group_absent(pid) {
        return true;
    }
    test_support::exited_unreaped(pid)
}

// Unix only until GRE-1203, with the paused-time crash tests that use it.
#[cfg(unix)]
async fn wait_shell_crash(
    supervisor: &super::Supervisor,
    scratch: &Scratch,
    expected: usize,
    guarded: bool,
) {
    let fuse = wall_fuse(Duration::from_secs(8));
    let policy_started = tokio::time::Instant::now();
    let mut startup_ticks = 0;
    let mut cleanup_ticks = 0;
    let mut next_tick = std::time::Instant::now();
    let trace_started = std::time::Instant::now();
    let trace = std::env::var_os("PP_GRE671_TRACE").is_some();
    let mut previous = String::new();
    loop {
        let state = supervisor.handle.status.borrow().clone();
        let pids = recorded_pids(&scratch.pidfile());
        if trace {
            let pid = pids.get(expected - 1).copied();
            let observation = format!(
                "state={state:?} pid={pid:?} exited={} probes={} startup_ticks={startup_ticks} cleanup_ticks={cleanup_ticks}",
                pid.is_some_and(shell_exited),
                *supervisor.health_probes_completed.borrow(),
            );
            if observation != previous {
                eprintln!(
                    "GRE671 crash={expected} wall={:?} policy={:?} {observation}",
                    trace_started.elapsed(),
                    policy_started.elapsed(),
                );
                previous = observation;
            }
        }
        if pids.len() == expected
            && if guarded {
                matches!(state, super::Status::Guarded)
            } else {
                matches!(state, super::Status::Backoff { .. })
            }
        {
            break;
        }
        assert!(
            !fuse.is_cancelled(),
            "crash {expected} stalled at {state:?}"
        );
        let pid = pids.get(expected - 1).copied();
        if std::time::Instant::now() >= next_tick {
            let tick = match state {
                super::Status::Starting if startup_ticks < 8 && pid.is_some_and(shell_exited) => {
                    startup_ticks += 1;
                    Some(Duration::from_millis(250))
                }
                super::Status::Stopping
                    if cleanup_ticks < 2 && pid.is_some_and(super::drain::process_group_absent) =>
                {
                    cleanup_ticks += 1;
                    Some(Duration::from_millis(10))
                }
                _ => None,
            };
            if let Some(tick) = tick {
                if trace {
                    eprintln!(
                        "GRE671 crash={expected} wall={:?} advance={tick:?}",
                        trace_started.elapsed()
                    );
                }
                tokio::time::advance(tick).await;
                next_tick = std::time::Instant::now() + Duration::from_secs(1);
            }
        }
        tokio::task::yield_now().await;
    }
    assert!(policy_started.elapsed() <= Duration::from_millis(2020));
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), expected);
    assert!(live_pids(&scratch.pidfile()).is_empty());
    let rows = diagnostic_rows(scratch);
    let failures: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "compat_failure")
        .collect();
    assert_eq!(failures.len(), expected);
    for failure in failures {
        assert_eq!(failure["reason"], "child_exit");
        assert_eq!(
            failure["termination"],
            serde_json::json!({"kind":"exit_code","value":7})
        );
    }
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "compat_reaped")
            .count(),
        expected
    );
}

// Unix only until GRE-1203, with the paused-time crash tests that use it.
#[cfg(unix)]
async fn advance_crash_backoff(supervisor: &super::Supervisor) {
    let attempt = match *supervisor.handle.status.borrow() {
        super::Status::Backoff { attempt } => attempt,
        ref state => panic!("expected pending restart, got {state:?}"),
    };
    tokio::time::advance(super::backoff(attempt)).await;
}

// Unix only until GRE-1203, with the paused-time crash tests that use it.
#[cfg(unix)]
async fn shutdown_guard_shell(supervisor: super::Supervisor, scratch: &Scratch) {
    let pid = *recorded_pids(&scratch.pidfile()).last().unwrap();
    let stopping = supervisor.shutdown();
    tokio::pin!(stopping);
    let fuse = wall_fuse(Duration::from_secs(8));
    let mut cleanup_tick = false;
    loop {
        tokio::select! {
            biased;
            _ = fuse.cancelled() => panic!("shell shutdown stalled"),
            result = &mut stopping => { result.unwrap(); return; }
            _ = tokio::task::yield_now() => {}
        }
        if !cleanup_tick && super::drain::process_group_absent(pid) {
            tokio::time::advance(Duration::from_millis(10)).await;
            cleanup_tick = true;
        }
    }
}

// Unix only until GRE-1203: on Windows the startup probe waits up to 2 s for
// the child's dial, and the child's pipe stdio blocks paused-clock auto-advance.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_duplicate_recovery_cannot_bypass_next_crash_guard() {
    let scratch = Scratch::new("duplicate-recovery");
    let supervisor = super::Supervisor::launch_shell(
        crash_guard_spec(&scratch),
        super::SupervisorConfig::default(),
    )
    .unwrap();
    for count in 1..=5 {
        wait_shell_crash(&supervisor, &scratch, count, count == 5).await;
        if count < 5 {
            advance_crash_backoff(&supervisor).await;
        }
    }
    let handle = supervisor.handle.clone();
    handle.recover().await.unwrap();
    // No yield between requests: both callers observe the same guarded state.
    assert!(
        handle.recover().await.is_err(),
        "duplicate restart accepted"
    );
    for count in 6..=10 {
        wait_shell_crash(&supervisor, &scratch, count, count == 10).await;
        if count < 10 {
            advance_crash_backoff(&supervisor).await;
        }
    }
    tokio::time::advance(Duration::from_secs(300)).await;
    pump_for(Duration::from_millis(20)).await;
    assert!(matches!(*handle.status.borrow(), super::Status::Guarded));
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 10);
    assert!(live_pids(&scratch.pidfile()).is_empty());
    assert_eq!(shutdown(supervisor).await, super::CompatExit::Interrupted);
    tokio::time::advance(Duration::from_secs(300)).await;
    pump_for(Duration::from_millis(20)).await;
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 10);
    assert!(
        recorded_pids(&scratch.pidfile())
            .into_iter()
            .all(super::drain::process_group_absent)
    );
}

// Unix only until GRE-1203: on Windows the startup probe waits up to 2 s for
// the child's dial, and the child's pipe stdio blocks paused-clock auto-advance.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_five_shell_crashes_guard_until_explicit_recovery() {
    let scratch = Scratch::new("guard-runtime");
    let supervisor = super::Supervisor::launch_shell(
        crash_guard_spec(&scratch),
        super::SupervisorConfig::default(),
    )
    .unwrap();
    let started = tokio::time::Instant::now();
    for count in 1..=5 {
        wait_shell_crash(&supervisor, &scratch, count, count == 5).await;
        if count == 4 {
            tokio::time::advance(Duration::from_secs(100)).await;
        } else if count < 5 {
            advance_crash_backoff(&supervisor).await;
        }
    }
    assert!(started.elapsed() >= Duration::from_secs(100));
    assert!(started.elapsed() < Duration::from_secs(120));
    tokio::time::advance(Duration::from_secs(300)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        *supervisor.handle.status.borrow(),
        super::Status::Guarded
    ));
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 5);
    assert!(live_pids(&scratch.pidfile()).is_empty());

    std::fs::write(scratch.root.join("data/hold"), "").unwrap();
    let handle = supervisor.handle.clone();
    handle.recover().await.unwrap();
    wait_live(&scratch.pidfile(), 6).await;
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 6);
    let recovered = *recorded_pids(&scratch.pidfile()).last().unwrap();
    assert_eq!(live_pids(&scratch.pidfile()), vec![recovered]);
    assert!(handle.recover().await.is_err());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 6);
    assert_eq!(live_pids(&scratch.pidfile()), vec![recovered]);
    shutdown_guard_shell(supervisor, &scratch).await;
    tokio::time::advance(Duration::from_secs(300)).await;
    assert!(matches!(*handle.status.borrow(), super::Status::Stopped));
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 6);
    assert!(live_pids(&scratch.pidfile()).is_empty());
    assert_eq!(
        diagnostic_rows(&scratch)
            .iter()
            .filter(|row| row["event"] == "compat_reaped")
            .count(),
        6
    );
}

// Unix only until GRE-1203: on Windows the startup probe waits up to 2 s for
// the child's dial, and the child's pipe stdio blocks paused-clock auto-advance.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_shell_crashes_expire_outside_default_guard_window() {
    let scratch = Scratch::new("guard-expiry");
    let supervisor = super::Supervisor::launch_shell(
        crash_guard_spec(&scratch),
        super::SupervisorConfig::default(),
    )
    .unwrap();
    for count in 1..=4 {
        wait_shell_crash(&supervisor, &scratch, count, false).await;
        if count < 4 {
            advance_crash_backoff(&supervisor).await;
        }
    }
    tokio::time::advance(Duration::from_secs(121)).await;
    let recent = tokio::time::Instant::now();
    for count in 5..=9 {
        wait_shell_crash(&supervisor, &scratch, count, count == 9).await;
        if count < 9 {
            advance_crash_backoff(&supervisor).await;
        }
    }
    assert!(recent.elapsed() < Duration::from_secs(120));
    assert_eq!(
        diagnostic_rows(&scratch)
            .iter()
            .filter(|row| row["event"] == "compat_crash_guard")
            .count(),
        1
    );
    shutdown_guard_shell(supervisor, &scratch).await;
    assert!(live_pids(&scratch.pidfile()).is_empty());
}

// Unix only until GRE-1203: the health fixture plays the child on a Unix
// listener. Its Windows dial loop and Windows counterparts come with GRE-1203.
#[cfg(unix)]
struct HealthFixture {
    delay_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
    hung: std::sync::Arc<std::sync::atomic::AtomicBool>,
    completed: tokio::sync::watch::Receiver<usize>,
    health: std::sync::Arc<std::sync::Mutex<serde_json::Value>>,
    probes: std::sync::Arc<std::sync::Mutex<Vec<tokio::time::Instant>>>,
    task: tokio::task::JoinHandle<()>,
}

#[cfg(unix)]
impl HealthFixture {
    async fn attach(scratch: &Scratch, completed: tokio::sync::watch::Receiver<usize>) -> Self {
        let setup_file = scratch.root.join("data/setup");
        pump_until(|| setup_file.exists()).await;
        let setup: serde_json::Value =
            serde_json::from_slice(&std::fs::read(setup_file).unwrap()).unwrap();
        let key = hex::decode(setup["key"].as_str().unwrap()).unwrap();
        let generation = setup["generation"].as_str().unwrap().to_owned();
        let listener =
            tokio::net::UnixListener::bind(setup["socket_path"].as_str().unwrap()).unwrap();
        let health = std::sync::Arc::new(std::sync::Mutex::new(serde_json::json!({
            "ok": false,
            "version": "ticket-t-2-unreleased",
            "release": {"commit": "ticket-t-2-unreleased"}
        })));
        let probes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let hung = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hang_response = hung.clone();
        let delay_ms = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let response_delay = delay_ms.clone();
        let response = health.clone();
        let received = probes.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let response = response.clone();
                let hung = hang_response.clone();
                let delay_ms = response_delay.clone();
                let received = received.clone();
                let key = key.clone();
                let generation = generation.clone();
                // Each health request owns a fresh IPC connection. Finish it before
                // accepting another so fixture tasks cannot survive teardown.
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        hyper::service::service_fn(move |request| {
                            assert_eq!(request.method(), "GET");
                            assert_eq!(request.uri(), "/health");
                            use hmac::Mac;
                            let principal = request.headers()["x-pp-principal"].to_str().unwrap();
                            let signature = request.headers()["x-pp-signature"].to_str().unwrap();
                            let lowercase_hex = |text: &str| {
                                text.bytes()
                                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                            };
                            assert!(!principal.is_empty() && principal.len() <= 4096);
                            assert!(lowercase_hex(principal));
                            assert_eq!(signature.len(), 64);
                            assert!(lowercase_hex(signature));
                            let signature = hex::decode(signature).unwrap();
                            let mut verifier =
                                super::Hmac::<super::Sha256>::new_from_slice(&key).unwrap();
                            verifier.update(principal.as_bytes());
                            verifier
                                .verify_slice(&signature)
                                .expect("authenticated health request");
                            let assertion: serde_json::Value =
                                serde_json::from_slice(&hex::decode(principal).unwrap()).unwrap();
                            assert_eq!(assertion.as_object().unwrap().len(), 7);
                            assert_eq!(assertion["version"], 1);
                            assert_eq!(assertion["tenant"], "default");
                            assert_eq!(assertion["actor"], "desktop-owner");
                            let issued_at = assertion["issued_at"].as_u64().unwrap();
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as i128;
                            let age = now - i128::from(issued_at);
                            assert!((-1000..=30_000).contains(&age), "principal age {age} ms");
                            assert_eq!(assertion["generation"], generation);
                            assert_eq!(assertion["method"], "GET");
                            assert_eq!(assertion["target"], "/health");
                            received.lock().unwrap().push(tokio::time::Instant::now());
                            let body = response.lock().unwrap().to_string();
                            let hang = hung.load(std::sync::atomic::Ordering::SeqCst);
                            let delay = delay_ms.load(std::sync::atomic::Ordering::SeqCst);
                            async move {
                                if hang {
                                    std::future::pending::<()>().await;
                                }
                                if delay > 0 {
                                    tokio::time::sleep(Duration::from_millis(delay)).await;
                                }
                                Ok::<_, std::convert::Infallible>(axum::http::Response::new(
                                    axum::body::Body::from(body),
                                ))
                            }
                        }),
                    )
                    .await
                    .ok();
            }
        });
        Self {
            delay_ms,
            hung,
            completed,
            health,
            probes,
            task,
        }
    }

    fn count(&self) -> usize {
        self.probes.lock().unwrap().len()
    }

    async fn tick(&self, duration: Duration, expected: usize) {
        tokio::time::advance(duration).await;
        // Completion is published by the supervisor after consuming the body.
        // On this current-thread runtime it registers its next timer before the
        // yielding test can observe the notification. The first missing-socket
        // probe is counted by the supervisor but not received by the fixture.
        pump_until(|| *self.completed.borrow() == expected + 1).await;
        assert_eq!(self.count(), expected);
    }
}

#[cfg(unix)]
impl Drop for HealthFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn pump_until(predicate: impl Fn() -> bool) {
    let wall = std::time::Instant::now();
    while !predicate() {
        assert!(
            wall.elapsed() < Duration::from_secs(8),
            "IPC fixture stalled"
        );
        tokio::task::yield_now().await;
    }
}

// Unix only until GRE-1203, with the paused-time crash tests that use it.
#[cfg(unix)]
async fn pump_for(duration: Duration) {
    let wall = std::time::Instant::now();
    while wall.elapsed() < duration {
        tokio::task::yield_now().await;
    }
}

#[cfg(unix)]
async fn health_supervisor(scratch: &Scratch) -> (super::Supervisor, HealthFixture) {
    let mut spec = shell_spec(scratch);
    spec.script = r#"const fs = require("node:fs"); let setup = null; const input = require("node:readline").createInterface({ input: process.stdin }); input.once("line", (line) => { setup = JSON.parse(line); fs.writeFileSync(setup.data_dir + "/setup.tmp", line); fs.renameSync(setup.data_dir + "/setup.tmp", setup.data_dir + "/setup"); }); input.on("close", () => process.exit(setup === null ? 1 : 0));"#;
    let supervisor =
        super::Supervisor::launch_shell(spec, super::SupervisorConfig::default()).unwrap();
    pump_until(|| *supervisor.health_probes_completed.borrow() == 1).await;
    let fixture = HealthFixture::attach(scratch, supervisor.health_probes_completed.clone()).await;
    fixture.tick(Duration::from_millis(250), 1).await;
    (supervisor, fixture)
}

// Unix only until GRE-1203: the health fixture listens on a Unix socket.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_health_cadence_requires_ok_and_bundled_release() {
    let scratch = Scratch::new("health-cadence");
    let (supervisor, fixture) = health_supervisor(&scratch).await;
    assert!(supervisor.handle.endpoint().is_none());
    for health in [
        serde_json::json!({"ok":true,"version":"wrong","release":{"commit":"ticket-t-2-unreleased"}}),
        serde_json::json!({"ok":true,"version":"ticket-t-2-unreleased","release":{"commit":"wrong"}}),
    ] {
        *fixture.health.lock().unwrap() = health;
        let count = fixture.count();
        tokio::time::advance(Duration::from_millis(249)).await;
        pump_for(Duration::from_millis(20)).await;
        assert_eq!(fixture.count(), count, "startup polled before 250 ms");
        fixture.tick(Duration::from_millis(1), count + 1).await;
        assert!(matches!(
            *supervisor.handle.status.borrow(),
            super::Status::Starting
        ));
        assert!(
            supervisor.handle.endpoint().is_none(),
            "mismatch published an endpoint"
        );
    }
    *fixture.health.lock().unwrap() = serde_json::json!({
        "ok":true,"version":"ticket-t-2-unreleased","release":{"commit":"ticket-t-2-unreleased"}
    });
    fixture.tick(Duration::from_millis(250), 4).await;
    pump_until(|| {
        matches!(
            *supervisor.handle.status.borrow(),
            super::Status::Ready { .. }
        )
    })
    .await;
    assert!(supervisor.handle.endpoint().is_some());
    for expected in 5..=6 {
        tokio::time::advance(Duration::from_millis(9999)).await;
        pump_for(Duration::from_millis(20)).await;
        assert_eq!(fixture.count(), expected - 1, "liveness polled before 10 s");
        fixture.tick(Duration::from_millis(1), expected).await;
        assert!(matches!(
            *supervisor.handle.status.borrow(),
            super::Status::Ready { .. }
        ));
    }
    let times = fixture.probes.lock().unwrap().clone();
    let gaps: Vec<_> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
    assert_eq!(
        gaps,
        vec![
            Duration::from_millis(250),
            Duration::from_millis(250),
            Duration::from_millis(250),
            Duration::from_secs(10),
            Duration::from_secs(10)
        ]
    );
    tokio::time::resume();
    assert_eq!(shutdown(supervisor).await, super::CompatExit::Graceful);
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 1);
    assert!(live_pids(&scratch.pidfile()).is_empty());
}

// Unix only until GRE-1203: the health fixture listens on a Unix socket.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_unhealthy_startup_stops_polling_at_thirty_seconds() {
    let scratch = Scratch::new("health-budget");
    let started = tokio::time::Instant::now();
    let (supervisor, fixture) = health_supervisor(&scratch).await;
    while started.elapsed() < Duration::from_millis(29750) {
        fixture
            .tick(Duration::from_millis(250), fixture.count() + 1)
            .await;
        assert!(supervisor.handle.endpoint().is_none());
        assert!(matches!(
            *supervisor.handle.status.borrow(),
            super::Status::Starting
        ));
    }
    assert_eq!(started.elapsed(), Duration::from_millis(29750));
    tokio::time::advance(Duration::from_millis(249)).await;
    pump_for(Duration::from_millis(20)).await;
    assert!(matches!(
        *supervisor.handle.status.borrow(),
        super::Status::Starting
    ));
    tokio::time::advance(Duration::from_millis(1)).await;
    pump_until(|| matches!(*supervisor.handle.status.borrow(), super::Status::Stopping)).await;
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    let times = fixture.probes.lock().unwrap().clone();
    let first = times[0] - started;
    assert_eq!(first, Duration::from_millis(250));
    assert_eq!(times.len(), 119);
    assert_eq!(
        *times.last().unwrap() - started,
        Duration::from_millis(29750)
    );
    assert!(
        times
            .windows(2)
            .all(|pair| pair[1] - pair[0] == Duration::from_millis(250))
    );
    tokio::time::resume();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Backoff { .. })
    })
    .await;
    supervisor.disarm_restarts();
    shutdown(supervisor).await;
    assert!(live_pids(&scratch.pidfile()).is_empty());
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 1);
    assert!(
        diagnostic_rows(&scratch)
            .iter()
            .any(|row| row["reason"] == "readiness_timeout")
    );
}

#[tokio::test]
async fn ticket_t_3_drain_repeated_eof_interrupts_mutation_without_resend() {
    use axum::{body::Body, http::Request};

    let scratch = Scratch::new("repeated-eof-upload");
    let events = scratch.root.join("data/upload-events");
    let mut expected = String::new();
    let mut previous = Vec::new();
    for cycle in 0..3 {
        assert!(
            previous
                .iter()
                .all(|pid| super::drain::process_group_absent(*pid))
        );

        let supervisor = super::Supervisor::launch_shell(
            super::ShellSpec {
                script: served(include_str!("fixtures/drain-upload.cjs")),
                ..shell_spec(&scratch)
            },
            super::SupervisorConfig::default(),
        )
        .unwrap();
        let handle = supervisor.handle.clone();
        let mut status = handle.status.clone();
        wait_status(&mut status, |state| {
            matches!(state, super::Status::Ready { .. })
        })
        .await;
        let pid = match *status.borrow() {
            super::Status::Ready { pid, .. } => pid,
            _ => unreachable!(),
        };
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), cycle + 1);
        assert_eq!(live_pids(&scratch.pidfile()), vec![pid]);
        let endpoint = handle.endpoint().unwrap();
        let sending = endpoint.clone();
        let upload = tokio::spawn(async move {
            sending
                .forward(
                    Request::builder()
                        .method("POST")
                        .uri("/upload")
                        .body(Body::from(format!("payload-{cycle}")))
                        .unwrap(),
                )
                .await
        });
        expected.push_str(&format!("POST /upload payload-{cycle}\n"));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(&events).unwrap_or_default() == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixture did not accept mutation");
        assert!(!upload.is_finished());
        supervisor.disarm_restarts();
        let started = tokio::time::Instant::now();
        let exit = shutdown(supervisor).await;
        let shutdown_elapsed = started.elapsed();
        assert_eq!(exit, super::CompatExit::Graceful);
        assert!(shutdown_elapsed < Duration::from_secs(5));
        let error = tokio::time::timeout(Duration::from_secs(1), upload)
            .await
            .unwrap()
            .unwrap()
            .expect_err("interrupted mutation cannot report success");
        assert_eq!(error.to_string(), "Compatibility dispatch outcome unknown");
        assert!(handle.endpoint().is_none());
        assert!(endpoint.connections.tasks.is_empty());
        assert!(super::drain::process_group_absent(pid));
        assert!(live_pids(&scratch.pidfile()).is_empty());
        let refused = endpoint
            .forward(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .body(Body::from("must-not-resend"))
                    .unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(refused.to_string(), "Compatibility generation is stopping");
        expected.push_str("EOF\n");
        assert_eq!(std::fs::read_to_string(&events).unwrap(), expected);
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), cycle + 1);
        println!(
            "{}",
            serde_json::json!({
                "case": "repeated_eof_upload", "cycle": cycle, "pid": pid,
                "compat_exit": "graceful", "old_group_absent": true,
                "shutdown_ms": shutdown_elapsed.as_millis(),
                "connection_tasks": 0, "mutation_error": error.to_string(),
                "stale_endpoint_error": refused.to_string(), "events": expected,
            })
        );
        previous.push(pid);
    }
}

// Unix only until GRE-1203: on Windows the startup probe waits up to 2 s for
// the child's dial, and the child's pipe stdio blocks paused-clock auto-advance.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_process_exit_obeys_every_backoff_deadline_and_cap() {
    let scratch = Scratch::new("backoff-deadlines");
    // Expire previous crashes so the independent five-crash guard does not
    // prevent observation of the later backoff attempts. No child reaches Ready.
    let supervisor = super::Supervisor::launch_shell(
        super::ShellSpec {
            // Exits 7 once the test creates its trigger file.
            script: r#"const fs = require("node:fs"); let armed = false; const input = require("node:readline").createInterface({ input: process.stdin }); input.on("close", () => { if (!armed) process.exit(0); }); input.once("line", (line) => { armed = true; const data = JSON.parse(line).data_dir; fs.writeFileSync(data + "/armed", String(process.pid)); setInterval(() => { if (fs.existsSync(data + "/crash-" + process.pid)) process.exit(7); }, 10); });"#,
            ..shell_spec(&scratch)
        },
        super::SupervisorConfig {
            crash_guard_window: Duration::ZERO,
            ..super::SupervisorConfig::default()
        },
    )
    .unwrap();
    for (attempt, millis) in [500, 1000, 2000, 4000, 8000, 30_000, 30_000]
        .into_iter()
        .enumerate()
    {
        let count = attempt + 1;
        wait_live(&scratch.pidfile(), count).await;
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), count);
        let pid = recorded_pids(&scratch.pidfile())[attempt];
        pump_until(|| {
            std::fs::read_to_string(scratch.root.join("data/armed"))
                .is_ok_and(|text| text.trim() == pid.to_string())
        })
        .await;
        std::fs::write(scratch.root.join(format!("data/crash-{pid}")), "").unwrap();
        wait_shell_crash(&supervisor, &scratch, count, false).await;
        assert!(matches!(
            *supervisor.handle.status.borrow(),
            super::Status::Backoff { attempt: actual } if actual == attempt
        ));
        let started = tokio::time::Instant::now();
        tokio::time::advance(Duration::from_millis(millis - 1)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), count);
        assert!(live_pids(&scratch.pidfile()).is_empty());
        assert!(matches!(
            *supervisor.handle.status.borrow(),
            super::Status::Backoff { attempt: actual } if actual == attempt
        ));
        tokio::time::advance(Duration::from_millis(1)).await;
        wait_live(&scratch.pidfile(), count + 1).await;
        assert_eq!(started.elapsed(), Duration::from_millis(millis));
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), count + 1);
        assert!(super::drain::process_group_absent(pid));
        assert!(supervisor.handle.endpoint.borrow().is_none());
    }
    shutdown_guard_shell(supervisor, &scratch).await;
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 8);
    assert!(live_pids(&scratch.pidfile()).is_empty());
}

#[cfg(unix)]
async fn healthy_generation(
    supervisor: &super::Supervisor,
    scratch: &Scratch,
    fixture: &HealthFixture,
) -> String {
    *fixture.health.lock().unwrap() = serde_json::json!({
        "ok":true,"version":"ticket-t-2-unreleased","release":{"commit":"ticket-t-2-unreleased"}
    });
    let completed = *supervisor.health_probes_completed.borrow();
    tokio::time::advance(Duration::from_millis(250)).await;
    pump_until(|| *supervisor.health_probes_completed.borrow() == completed + 1).await;
    assert!(supervisor.handle.endpoint().is_some());
    match &*supervisor.handle.status.borrow() {
        super::Status::Ready { generation, pid } => {
            assert_eq!(live_pids(&scratch.pidfile()), vec![*pid]);
            generation.clone()
        }
        state => panic!("authenticated health did not publish Ready: {state:?}"),
    }
}

#[cfg(unix)]
async fn lose_health(supervisor: &super::Supervisor, scratch: &Scratch, fixture: &HealthFixture) {
    let count = recorded_pids(&scratch.pidfile()).len();
    let pid = recorded_pids(&scratch.pidfile())[count - 1];
    *fixture.health.lock().unwrap() = serde_json::json!({"ok":false});
    for failure in 1..=3 {
        let received = fixture.count();
        let completed = *supervisor.health_probes_completed.borrow();
        tokio::time::advance(Duration::from_millis(9999)).await;
        pump_for(Duration::from_millis(20)).await;
        assert_eq!(fixture.count(), received, "liveness polled before 10 s");
        tokio::time::advance(Duration::from_millis(1)).await;
        pump_until(|| *supervisor.health_probes_completed.borrow() == completed + 1).await;
        assert_eq!(fixture.count(), received + 1);
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), count);
        if failure < 3 {
            assert!(matches!(
                *supervisor.handle.status.borrow(),
                super::Status::Ready { pid: actual, .. } if actual == pid
            ));
            assert!(supervisor.handle.endpoint().is_some());
            assert_eq!(live_pids(&scratch.pidfile()), vec![pid]);
        }
    }
    let times = fixture.probes.lock().unwrap().clone();
    for pair in times[times.len() - 4..].windows(2) {
        assert_eq!(pair[1] - pair[0], Duration::from_secs(10));
    }
    assert!(supervisor.handle.endpoint().is_none());
    let wall = std::time::Instant::now();
    let started = tokio::time::Instant::now();
    let mut cleanup_ticks = 0;
    let mut next_tick = wall;
    while !matches!(
        *supervisor.handle.status.borrow(),
        super::Status::Backoff { .. }
    ) {
        assert!(
            wall.elapsed() < Duration::from_secs(8),
            "health cleanup stalled"
        );
        if cleanup_ticks < 2
            && std::time::Instant::now() >= next_tick
            && super::drain::process_group_absent(pid)
        {
            tokio::time::advance(Duration::from_millis(10)).await;
            cleanup_ticks += 1;
            next_tick = std::time::Instant::now() + Duration::from_secs(1);
        }
        tokio::task::yield_now().await;
    }
    assert!(started.elapsed() <= Duration::from_millis(20));
    assert!(super::drain::process_group_absent(pid));
    assert!(live_pids(&scratch.pidfile()).is_empty());
    let rows = diagnostic_rows(scratch);
    let failures: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "compat_failure")
        .collect();
    assert_eq!(failures.len(), count);
    assert!(failures.iter().all(|row| row["reason"] == "health_lost"));
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "compat_reaped")
            .count(),
        count
    );
}

#[cfg(unix)]
enum HealthBackoffEnd {
    Restart,
    Shutdown,
    Disarm,
}

#[cfg(unix)]
async fn health_backoff_deadlines(end: HealthBackoffEnd) {
    let scratch = Scratch::new("health-backoff");
    let (supervisor, mut fixture) = health_supervisor(&scratch).await;
    let mut generation = healthy_generation(&supervisor, &scratch, &fixture).await;
    for (attempt, millis) in [500, 1000, 2000, 4000, 8000, 30_000, 30_000]
        .into_iter()
        .enumerate()
    {
        let count = attempt + 1;
        lose_health(&supervisor, &scratch, &fixture).await;
        assert!(matches!(
            *supervisor.handle.status.borrow(),
            super::Status::Backoff { attempt: actual } if actual == attempt
        ));
        let started = tokio::time::Instant::now();
        tokio::time::advance(Duration::from_millis(millis - 1)).await;
        pump_for(Duration::from_millis(20)).await;
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), count);
        assert!(live_pids(&scratch.pidfile()).is_empty());
        assert!(matches!(
            *supervisor.handle.status.borrow(),
            super::Status::Backoff { attempt: actual } if actual == attempt
        ));
        if attempt == 6 {
            match end {
                HealthBackoffEnd::Restart => {}
                HealthBackoffEnd::Shutdown | HealthBackoffEnd::Disarm => {
                    if matches!(end, HealthBackoffEnd::Disarm) {
                        supervisor.disarm_restarts();
                        pump_until(|| {
                            matches!(*supervisor.handle.status.borrow(), super::Status::Stopped)
                        })
                        .await;
                    }
                    assert_eq!(shutdown(supervisor).await, super::CompatExit::Interrupted);
                    tokio::time::advance(Duration::from_secs(300)).await;
                    pump_for(Duration::from_millis(20)).await;
                    assert_eq!(recorded_pids(&scratch.pidfile()).len(), count);
                    assert!(
                        recorded_pids(&scratch.pidfile())
                            .into_iter()
                            .all(super::drain::process_group_absent)
                    );
                    return;
                }
            }
        }
        let completed = *supervisor.health_probes_completed.borrow();
        drop(fixture);
        tokio::time::advance(Duration::from_millis(1)).await;
        wait_live(&scratch.pidfile(), count + 1).await;
        assert_eq!(started.elapsed(), Duration::from_millis(millis));
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), count + 1);
        pump_until(|| {
            std::fs::read(scratch.root.join("data/setup"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_some_and(|setup| setup["generation"] != generation)
        })
        .await;
        pump_until(|| *supervisor.health_probes_completed.borrow() == completed + 1).await;
        fixture = HealthFixture::attach(&scratch, supervisor.health_probes_completed.clone()).await;
        let next = healthy_generation(&supervisor, &scratch, &fixture).await;
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), count + 1);
        assert_ne!(next, generation);
        generation = next;
    }
    shutdown_guard_shell(supervisor, &scratch).await;
    tokio::time::advance(Duration::from_secs(300)).await;
    pump_for(Duration::from_millis(20)).await;
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 8);
    assert!(
        recorded_pids(&scratch.pidfile())
            .into_iter()
            .all(super::drain::process_group_absent)
    );
}

// Unix only until GRE-1203: the health fixture listens on a Unix socket.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_health_loss_obeys_every_backoff_deadline_and_cap() {
    health_backoff_deadlines(HealthBackoffEnd::Restart).await;
}

// Unix only until GRE-1203: the health fixture listens on a Unix socket.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_health_loss_shutdown_cancels_late_capped_restart() {
    health_backoff_deadlines(HealthBackoffEnd::Shutdown).await;
}

// Unix only until GRE-1203: the health fixture listens on a Unix socket.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_health_loss_disarm_cancels_late_capped_restart() {
    health_backoff_deadlines(HealthBackoffEnd::Disarm).await;
}

#[cfg(unix)]
async fn readiness_remaining_budget(hung: bool) {
    let scratch = Scratch::new("hung-readiness");
    let started = tokio::time::Instant::now();
    let (supervisor, fixture) = health_supervisor(&scratch).await;
    while started.elapsed() < Duration::from_millis(29500) {
        fixture
            .tick(Duration::from_millis(250), fixture.count() + 1)
            .await;
    }
    fixture
        .hung
        .store(hung, std::sync::atomic::Ordering::SeqCst);
    fixture
        .delay_ms
        .store(300, std::sync::atomic::Ordering::SeqCst);
    *fixture.health.lock().unwrap() = serde_json::json!({
        "ok":true,"version":"ticket-t-2-unreleased","release":{"commit":"ticket-t-2-unreleased"}
    });
    let count = fixture.count();
    tokio::time::advance(Duration::from_millis(250)).await;
    pump_until(|| fixture.count() == count + 1).await;
    tokio::time::advance(Duration::from_millis(249)).await;
    pump_for(Duration::from_millis(20)).await;
    assert!(matches!(
        *supervisor.handle.status.borrow(),
        super::Status::Starting
    ));
    assert_eq!(live_pids(&scratch.pidfile()).len(), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    pump_until(|| matches!(*supervisor.handle.status.borrow(), super::Status::Stopping)).await;
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    assert!(supervisor.handle.endpoint().is_none());
    tokio::time::resume();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Backoff { .. })
    })
    .await;
    supervisor.disarm_restarts();
    shutdown(supervisor).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    pump_for(Duration::from_millis(20)).await;
    assert!(
        recorded_pids(&scratch.pidfile())
            .iter()
            .all(|pid| super::drain::process_group_absent(*pid))
    );
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 1);
    assert!(live_pids(&scratch.pidfile()).is_empty());
    assert!(
        diagnostic_rows(&scratch)
            .iter()
            .any(|row| row["reason"] == "readiness_timeout")
    );
}

// Unix only until GRE-1203: the health fixture listens on a Unix socket.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_hung_liveness_times_out_three_times_and_shutdown_cancels_restart() {
    let scratch = Scratch::new("hung-liveness");
    let (supervisor, fixture) = health_supervisor(&scratch).await;
    *fixture.health.lock().unwrap() = serde_json::json!({
        "ok":true,"version":"ticket-t-2-unreleased","release":{"commit":"ticket-t-2-unreleased"}
    });
    fixture.tick(Duration::from_millis(250), 2).await;
    pump_until(|| supervisor.handle.endpoint().is_some()).await;
    fixture
        .hung
        .store(true, std::sync::atomic::Ordering::SeqCst);
    for failure in 1..=3 {
        tokio::time::advance(Duration::from_secs(10)).await;
        pump_until(|| fixture.count() == 2 + failure).await;
        tokio::time::advance(Duration::from_millis(1999)).await;
        pump_for(Duration::from_millis(20)).await;
        assert!(supervisor.handle.endpoint().is_some());
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), 1);
        assert_eq!(live_pids(&scratch.pidfile()).len(), 1);
        fixture.tick(Duration::from_millis(1), 2 + failure).await;
        if failure < 3 {
            assert!(supervisor.handle.endpoint().is_some());
        }
    }
    pump_until(|| matches!(*supervisor.handle.status.borrow(), super::Status::Stopping)).await;
    assert!(supervisor.handle.endpoint().is_none());
    // Reap with real time, then stop while the restart timer is pending.
    tokio::time::resume();
    let mut status = supervisor.handle.status.clone();
    wait_status(&mut status, |state| {
        matches!(state, super::Status::Backoff { .. })
    })
    .await;
    tokio::time::pause();
    assert!(live_pids(&scratch.pidfile()).is_empty());
    supervisor.disarm_restarts();
    tokio::time::resume();
    shutdown(supervisor).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    pump_for(Duration::from_millis(20)).await;
    assert!(
        recorded_pids(&scratch.pidfile())
            .iter()
            .all(|pid| super::drain::process_group_absent(*pid))
    );
    assert!(
        diagnostic_rows(&scratch)
            .iter()
            .any(|row| row["reason"] == "health_lost")
    );
    assert_eq!(recorded_pids(&scratch.pidfile()).len(), 1);
    assert!(live_pids(&scratch.pidfile()).is_empty());
}

// Unix only until GRE-1203: the health fixture listens on a Unix socket.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_hung_readiness_is_cut_off_at_remaining_budget() {
    readiness_remaining_budget(true).await;
}

// Unix only until GRE-1203: the health fixture listens on a Unix socket.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ticket_t_2_slow_readiness_cannot_publish_after_remaining_budget() {
    readiness_remaining_budget(false).await;
}

// Unix: the child exits on SIGTERM. Windows counterpart:
// ticket_t_3_drain_shutdown_forces_backpressured_startup_setup_windows.
#[cfg(unix)]
#[tokio::test]
async fn ticket_t_3_drain_shutdown_cancels_backpressured_startup_setup() {
    let scratch = Scratch::new("blocked-startup-setup");
    let supervisor = super::Supervisor::launch_shell(
        super::ShellSpec {
            script: "setInterval(() => {}, 1e9)",
            setup_padding: "x".repeat(1024 * 1024),
            ..shell_spec(&scratch)
        },
        super::SupervisorConfig::default(),
    )
    .unwrap();
    wait_live(&scratch.pidfile(), 1).await;
    let handle = supervisor.handle.clone();
    let probes = supervisor.health_probes_completed.clone();
    assert!(matches!(*handle.status.borrow(), super::Status::Starting));
    assert_eq!(*probes.borrow(), 0);
    let pid = recorded_pids(&scratch.pidfile())[0];
    assert_eq!(live_pids(&scratch.pidfile()), vec![pid]);
    let result = tokio::time::timeout(Duration::from_secs(5), supervisor.shutdown())
        .await
        .expect("shutdown remained blocked writing startup setup")
        .expect("supervisor shutdown");
    assert_eq!(result, super::CompatExit::Graceful);
    assert!(matches!(*handle.status.borrow(), super::Status::Stopped));
    assert!(handle.endpoint().is_none());
    assert_eq!(*probes.borrow(), 0);
    assert!(super::drain::process_group_absent(pid));
    assert_eq!(recorded_pids(&scratch.pidfile()), vec![pid]);
    println!("startup setup cancelled; spawns=1; probes=0; live_groups=0; status=stopped");
}

// Windows: the child never reads stdin and there is no SIGTERM, so stop ends in
// the Job Object kill after SIGTERM_GRACE. It must not hang on the blocked write.
#[cfg(windows)]
#[tokio::test]
async fn ticket_t_3_drain_shutdown_forces_backpressured_startup_setup_windows() {
    let scratch = Scratch::new("blocked-startup-setup-windows");
    let supervisor = super::Supervisor::launch_shell(
        super::ShellSpec {
            script: "setInterval(() => {}, 1e9)",
            setup_padding: "x".repeat(1024 * 1024),
            ..shell_spec(&scratch)
        },
        super::SupervisorConfig::default(),
    )
    .unwrap();
    wait_live(&scratch.pidfile(), 1).await;
    let handle = supervisor.handle.clone();
    let probes = supervisor.health_probes_completed.clone();
    assert!(matches!(*handle.status.borrow(), super::Status::Starting));
    assert_eq!(*probes.borrow(), 0);
    let pid = recorded_pids(&scratch.pidfile())[0];
    assert_eq!(live_pids(&scratch.pidfile()), vec![pid]);
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        super::drain::SIGTERM_GRACE + Duration::from_secs(5),
        supervisor.shutdown(),
    )
    .await
    .expect("shutdown remained blocked writing startup setup")
    .expect("supervisor shutdown");
    let elapsed = started.elapsed();
    assert_eq!(result, super::CompatExit::Forced);
    assert!(elapsed >= super::drain::SIGTERM_GRACE, "{elapsed:?}");
    assert!(matches!(*handle.status.borrow(), super::Status::Stopped));
    assert!(handle.endpoint().is_none());
    assert_eq!(*probes.borrow(), 0);
    assert!(super::drain::process_group_absent(pid));
    assert_eq!(recorded_pids(&scratch.pidfile()), vec![pid]);
    println!(
        "startup setup forced after {elapsed:?}; spawns=1; probes=0; live_groups=0; status=stopped"
    );
}

#[cfg(unix)]
const STARTUP_OWNER_ROOT: &str = "PP_COMPAT_STARTUP_DEATH_ROOT";

#[cfg(unix)]
fn startup_process_state(pid: u32) -> Option<char> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat[stat.rfind(')')? + 1..]
            .split_whitespace()
            .next()?
            .chars()
            .next()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .chars()
            .next()
    }
}

#[cfg(unix)]
fn startup_process_live(pid: u32) -> bool {
    super::drain::process_alive(pid) && !matches!(startup_process_state(pid), Some('Z' | 'X'))
}

fn startup_wait(limit: Duration, message: &str, mut ready: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !ready() {
        assert!(started.elapsed() < limit, "{message}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn phased_startup_wait(
    verified: &Path,
    ready: &Path,
    verification_timeout: Duration,
    acknowledgement_timeout: Duration,
    mut owner_alive: impl FnMut() -> bool,
) {
    startup_wait(
        verification_timeout,
        "measured release verification did not finish",
        || {
            assert!(owner_alive(), "owner exited before release verification");
            verified.exists()
        },
    );
    startup_wait(
        acknowledgement_timeout,
        "owner never acknowledged pending setup",
        || {
            assert!(owner_alive(), "owner exited before startup acknowledgement");
            ready.exists()
        },
    );
}

#[test]
fn measured_release_ack_deadline_excludes_verification() {
    let root = std::env::temp_dir().join(format!(
        "pp-phase-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let verified = root.join("release-verified");
    let ready = root.join("owner-ready");
    let verified_signal = verified.clone();
    let ready_signal = ready.clone();
    std::thread::spawn(move || {
        // Verification runs long enough that an ack budget measured from phase
        // start would already be exhausted by the time ready appears
        // (verified@1000ms + ready@+100ms = 1100ms > 600ms ack). The real
        // helper starts the ack window only after verified exists.
        std::thread::sleep(Duration::from_millis(1000));
        std::fs::write(&verified_signal, b"ok").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(&ready_signal, b"ok").unwrap();
    });
    phased_startup_wait(
        &verified,
        &ready,
        Duration::from_secs(2),
        Duration::from_millis(600),
        || true,
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
struct StartupDeathFixture {
    owner: std::process::Child,
    peer: Option<std::process::Child>,
    scratch: Scratch,
}

#[cfg(unix)]
impl Drop for StartupDeathFixture {
    fn drop(&mut self) {
        unsafe { libc::kill(-(self.owner.id() as i32), libc::SIGKILL) };
        let _ = self.owner.kill();
        for pid in recorded_pids(&self.scratch.pidfile()) {
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
        if let Some(peer) = &mut self.peer {
            let _ = peer.kill();
            let _ = peer.wait();
        }
        let _ = self.owner.wait();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn startup_owner_death_fixture() {
    let Some(root) = std::env::var_os(STARTUP_OWNER_ROOT) else {
        return;
    };
    let scratch = Scratch { root: root.into() };
    let node_fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/startup-no-stdin.cjs");
    let quote = |path: &Path| serde_json::to_string(path.to_str().unwrap()).unwrap();
    let script = Box::leak(
        format!(
            "process.argv = [process.argv[0], {fixture}, {ready}]; require({fixture});",
            fixture = quote(&node_fixture),
            ready = quote(&scratch.root.join("node-execed"))
        )
        .into_boxed_str(),
    );
    let supervisor = super::Supervisor::launch_shell(
        super::ShellSpec {
            script,
            setup_padding: "x".repeat(1024 * 1024),
            ..shell_spec(&scratch)
        },
        super::SupervisorConfig::default(),
    )
    .unwrap();
    let started = std::time::Instant::now();
    while !scratch.root.join("node-execed").exists() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "Node never execed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pid: u32 = std::fs::read_to_string(scratch.root.join("node-execed"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(matches!(
        *supervisor.handle.status.borrow(),
        super::Status::Starting
    ));
    assert!(supervisor.handle.endpoint().is_none());
    assert_eq!(*supervisor.health_probes_completed.borrow(), 0);
    assert_eq!(recorded_pids(&scratch.pidfile()), vec![pid]);
    assert_eq!(live_pids(&scratch.pidfile()), vec![pid]);
    let ready = scratch.root.join("owner-ready");
    let temporary = ready.with_extension("tmp");
    std::fs::write(&temporary, pid.to_string()).unwrap();
    std::fs::rename(temporary, ready).unwrap();
    std::future::pending::<()>().await;
    drop(supervisor);
}

const MEASURED_STARTUP_STAGE: &str = "PP_COMPAT_MEASURED_STARTUP_STAGE";

#[cfg(unix)]
fn measured_startup_before_setup(data: &Path, pid: u32) -> anyhow::Result<()> {
    let root = data.parent().unwrap();
    super::record_test_pid(&root.join("pids"), pid)?;
    if let Some(stage) = std::env::var_os(MEASURED_STARTUP_STAGE) {
        let executed = PathBuf::from(stage).join("web/startup-executed.json");
        startup_wait(
            Duration::from_secs(5),
            "measured Node entry never executed",
            || executed.exists(),
        );
        let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(executed)?)?;
        assert_eq!(receipt["pid"], pid);
        assert_eq!(receipt["preload"], "gre448-measured-simulator");
        assert_eq!(receipt["stdin_flowing"], serde_json::Value::Null);
    }
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
    startup_wait(Duration::from_secs(5), "measured Node did not stop", || {
        startup_process_state(pid) == Some('T')
    });
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn measured_startup_owner_death_fixture() {
    let Some(root) = std::env::var_os(STARTUP_OWNER_ROOT) else {
        return;
    };
    let scratch = Scratch { root: root.into() };
    let bundle = measured_startup_bundle();
    let assets = bundle.web_root.join("apps/web/dist");
    let bundle = super::release::verify_bundle(bundle, &assets).unwrap();
    let verified = scratch.root.join("release-verified");
    let temporary = verified.with_extension("tmp");
    std::fs::write(&temporary, b"ok").unwrap();
    std::fs::rename(&temporary, &verified).unwrap();
    if let Some(stage) = std::env::var_os(MEASURED_STARTUP_STAGE) {
        let executed = PathBuf::from(stage).join("web/startup-executed.json");
        if executed.exists() {
            std::fs::remove_file(executed).unwrap();
        }
    }
    let spec = super::SpawnSpec {
        bundle,
        gateway_rate_limit: false,
        before_setup: Box::new(measured_startup_before_setup),
        setup_padding: "x".repeat(1024 * 1024),
        ..release_spec(&scratch)
    };
    let supervisor = super::Supervisor::start(spec).unwrap();
    let started = std::time::Instant::now();
    while recorded_pids(&scratch.pidfile()).is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "measured spawn never recorded child"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // On this current-thread runtime, reaching here means spawn yielded after before_setup returned.
    tokio::task::yield_now().await;
    let pid = recorded_pids(&scratch.pidfile());
    assert_eq!(pid.len(), 1);
    assert_eq!(startup_process_state(pid[0]), Some('T'));
    assert!(matches!(
        *supervisor.handle.status.borrow(),
        super::Status::Starting
    ));
    assert!(supervisor.handle.endpoint().is_none());
    assert_eq!(*supervisor.health_probes_completed.borrow(), 0);
    println!(
        "measured bundle verified; Node stopped; startup setup pending; status=starting; probes=0; spawns=1"
    );
    std::fs::write(scratch.root.join("owner-ready.tmp"), pid[0].to_string()).unwrap();
    std::fs::rename(
        scratch.root.join("owner-ready.tmp"),
        scratch.root.join("owner-ready"),
    )
    .unwrap();
    std::future::pending::<()>().await;
    drop(supervisor);
}

/// Windows counterpart: `windows::tests::ticket_t_3_drain_job_object_kills_children_when_core_is_killed`.
#[cfg(unix)]
#[test]
fn ticket_t_3_drain_force_killed_owner_during_startup_kills_group() {
    startup_owner_death_cycles(
        "ticket_t_2::startup_owner_death_fixture",
        &crate::tests::node_binary(),
        Duration::from_secs(8),
        None,
    );
}

fn measured_startup_bundle() -> super::Bundle {
    let manifest: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/bundle-manifest.json"
    )))
    .unwrap();
    let (node, web_root) = match std::env::var_os(MEASURED_STARTUP_STAGE) {
        Some(stage) => {
            let stage = PathBuf::from(stage);
            (stage.join("node"), stage.join("web"))
        }
        None => (
            crate::tests::node_binary(),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../web"),
        ),
    };
    super::Bundle {
        node,
        entry: web_root.join("apps/server/dist/current/desktop.js"),
        web_root,
        runtime_version: manifest["runtime_version"].as_str().unwrap().into(),
        commit: manifest["commit"].as_str().unwrap().into(),
    }
}

/// Windows counterpart: `windows::tests::ticket_t_3_drain_job_object_kills_children_when_core_is_killed`.
#[cfg(unix)]
#[test]
fn ticket_t_3_drain_measured_release_startup_parent_death() {
    let bundle = measured_startup_bundle();
    let assets = bundle.web_root.join("apps/web/dist");
    let bundle = super::release::verify_bundle(bundle, &assets).unwrap();
    // Run 38941881212 timed one idle macOS verify_bundle at 10.7–19.7s. This
    // budget also covers re-executing the helper. The 30s acknowledgement
    // starts only after the helper publishes release-verified. The budget
    // holds only when the test runs alone, so nextest.toml gives it every test
    // thread. On hosted macos-26, sharing the default nextest profile's test
    // threads with other real-process tests, the whole test took 65.9s (run
    // 40169439947) and cycle 1 alone took more than 60s (run 40194530057). The
    // same runners took 12.9s and 20.8s in the later desktop_rebuild-on pass.
    startup_owner_death_cycles(
        "ticket_t_2::measured_startup_owner_death_fixture",
        &bundle.node,
        Duration::from_secs(30),
        Some(Duration::from_secs(60)),
    );
}

#[cfg(unix)]
fn startup_owner_death_cycles(
    helper: &str,
    node: &Path,
    startup_timeout: Duration,
    release_verification_timeout: Option<Duration>,
) {
    use std::os::unix::process::{CommandExt, ExitStatusExt};

    assert!(node.is_file(), "native Node executable missing");
    for cycle in 0..2 {
        let scratch = Scratch::new("startup-owner-death");
        let owner = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", helper, "--nocapture"])
            .env(STARTUP_OWNER_ROOT, &scratch.root)
            .process_group(0)
            .spawn()
            .unwrap();
        let mut fixture = StartupDeathFixture {
            owner,
            peer: None,
            scratch,
        };
        let ready = fixture.scratch.root.join("owner-ready");
        if let Some(verification_timeout) = release_verification_timeout {
            phased_startup_wait(
                &fixture.scratch.root.join("release-verified"),
                &ready,
                verification_timeout,
                startup_timeout,
                || fixture.owner.try_wait().unwrap().is_none(),
            );
        } else {
            startup_wait(
                startup_timeout,
                "owner never acknowledged pending setup",
                || {
                    assert!(
                        fixture.owner.try_wait().unwrap().is_none(),
                        "owner exited before startup acknowledgement"
                    );
                    ready.exists()
                },
            );
        }
        let pid: u32 = std::fs::read_to_string(ready).unwrap().parse().unwrap();
        assert_eq!(recorded_pids(&fixture.scratch.pidfile()), vec![pid]);
        assert!(startup_process_live(pid));
        assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                std::fs::canonicalize(format!("/proc/{pid}/exe")).unwrap(),
                std::fs::canonicalize(node).unwrap(),
            );
        }
        fixture.peer = Some(
            std::process::Command::new("/bin/sleep")
                .arg("600")
                .process_group(pid as i32)
                .spawn()
                .unwrap(),
        );
        let peer_pid = fixture.peer.as_ref().unwrap().id();
        assert_eq!(unsafe { libc::getpgid(peer_pid as i32) }, pid as i32);
        assert!(startup_process_live(peer_pid));
        assert!(fixture.peer.as_mut().unwrap().try_wait().unwrap().is_none());
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
        startup_wait(
            Duration::from_secs(5),
            "Node did not enter stopped state",
            || startup_process_state(pid) == Some('T'),
        );
        assert!(startup_process_live(peer_pid));
        println!(
            "startup death armed helper={helper}; cycle={cycle}; owner={}; node={pid}; peer={peer_pid}; node_stopped=true; peer_live=true",
            fixture.owner.id()
        );
        let killed = std::time::Instant::now();
        assert_eq!(
            unsafe { libc::kill(-(fixture.owner.id() as i32), libc::SIGKILL) },
            0
        );
        let mut owner_exit = None;
        startup_wait(
            Duration::from_secs(5),
            "startup group outlived force-killed owner",
            || {
                if owner_exit.is_none() {
                    owner_exit = fixture.owner.try_wait().unwrap();
                }
                owner_exit.is_some()
                    && !startup_process_live(pid)
                    && !startup_process_live(peer_pid)
            },
        );
        assert!(killed.elapsed() <= Duration::from_secs(5));
        assert_eq!(owner_exit.unwrap().signal(), Some(libc::SIGKILL));
        assert_eq!(
            fixture.peer.as_mut().unwrap().wait().unwrap().signal(),
            Some(libc::SIGKILL)
        );
        assert_eq!(recorded_pids(&fixture.scratch.pidfile()), vec![pid]);
        println!(
            "startup owner death helper={helper}; cycle={cycle}; spawns=1; Node and peer dead; elapsed_ms={}",
            killed.elapsed().as_millis()
        );
    }
}

#[tokio::test]
async fn ticket_t_3_drain_repeated_eof_joins_unread_response_body() {
    use axum::{body::Body, http::Request};

    let scratch = Scratch::new("unread-response-eof");
    let events = scratch.root.join("data/stream-events");
    let mut expected = String::new();
    let mut previous = Vec::new();
    for cycle in 0..2 {
        assert!(
            previous
                .iter()
                .all(|pid| super::drain::process_group_absent(*pid))
        );
        let supervisor = super::Supervisor::launch_shell(
            super::ShellSpec {
                script: served(include_str!("fixtures/drain-stream.cjs")),
                ..shell_spec(&scratch)
            },
            super::SupervisorConfig::default(),
        )
        .unwrap();
        let handle = supervisor.handle.clone();
        let mut status = handle.status.clone();
        wait_status(&mut status, |state| {
            matches!(state, super::Status::Ready { .. })
        })
        .await;
        let pid = match *status.borrow() {
            super::Status::Ready { pid, .. } => pid,
            _ => unreachable!(),
        };
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), cycle + 1);
        assert_eq!(live_pids(&scratch.pidfile()), vec![pid]);
        let endpoint = handle.endpoint().unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            endpoint.forward(
                Request::builder()
                    .uri("/stream")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("fixture did not send response headers")
        .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        let held_body = response.into_body();
        expected.push_str("GET /stream\n");
        assert_eq!(std::fs::read_to_string(&events).unwrap(), expected);
        assert!(!endpoint.connections.tasks.is_empty());
        supervisor.disarm_restarts();
        let started = std::time::Instant::now();
        let exit = tokio::time::timeout(Duration::from_secs(5), supervisor.shutdown())
            .await
            .expect("unread response blocked shutdown")
            .expect("unread response connection was not joined");
        let elapsed = started.elapsed();
        assert_eq!(exit, super::CompatExit::Graceful);
        assert!(elapsed < Duration::from_secs(5));
        assert!(endpoint.connections.tasks.is_empty());
        assert!(handle.endpoint().is_none());
        assert!(matches!(*handle.status.borrow(), super::Status::Stopped));
        assert!(super::drain::process_group_absent(pid));
        assert!(live_pids(&scratch.pidfile()).is_empty());
        let refused = endpoint
            .forward(
                Request::builder()
                    .uri("/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(refused.to_string(), "Compatibility generation is stopping");
        expected.push_str("EOF\n");
        assert_eq!(std::fs::read_to_string(&events).unwrap(), expected);
        assert_eq!(recorded_pids(&scratch.pidfile()).len(), cycle + 1);
        println!(
            "{}",
            serde_json::json!({
                "case": "unread_response_eof", "cycle": cycle, "pid": pid,
                "compat_exit": "graceful", "shutdown_ms": elapsed.as_millis(),
                "connection_tasks": 0, "old_group_absent": true,
                "stale_endpoint_error": refused.to_string(), "events": expected,
            })
        );
        drop(held_body);
        previous.push(pid);
    }
}

struct MeasuredLifecycleCycle {
    scratch: Scratch,
    pid: u32,
    events: String,
}

impl MeasuredLifecycleCycle {
    fn assert_stopped(&self) {
        assert!(super::drain::process_group_absent(self.pid));
        assert_eq!(
            std::fs::read_to_string(self.scratch.root.join("data/lifecycle-events")).unwrap(),
            self.events
        );
        assert_eq!(recorded_pids(&self.scratch.pidfile()), vec![self.pid]);
    }

    fn before_setup(data: &Path, pid: u32) -> anyhow::Result<()> {
        super::record_test_pid(&data.parent().unwrap().join("pids"), pid)?;
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires measured lifecycle assets; run prove-measured-startup-death.py --scenario lifecycle"]
async fn ticket_t_3_drain_measured_release_repeated_eof_cleanup() {
    use axum::{body::Body, http::Request};

    let bundle = measured_startup_bundle();
    let assets = bundle.web_root.join("apps/web/dist");
    assert_eq!(bundle.runtime_version, "0.0.0-gre448-simulator-web");
    let mut completed: Vec<MeasuredLifecycleCycle> = Vec::new();
    for cycle in 0..3 {
        for previous in &completed {
            previous.assert_stopped();
        }
        let scratch = Scratch::new("measured-lifecycle");
        let events = scratch.root.join("data/lifecycle-events");
        let supervisor = super::Supervisor::start(super::SpawnSpec {
            bundle: super::release::verify_bundle(bundle.clone(), &assets).unwrap(),
            gateway_rate_limit: false,
            before_setup: Box::new(MeasuredLifecycleCycle::before_setup),
            ..release_spec(&scratch)
        })
        .unwrap();
        let handle = supervisor.handle.clone();
        let mut status = handle.status.clone();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if matches!(*status.borrow(), super::Status::Ready { .. }) {
                    break;
                }
                status.changed().await.unwrap();
            }
        })
        .await
        .expect("measured lifecycle did not become ready");
        let pid = match *status.borrow() {
            super::Status::Ready { pid, .. } => pid,
            _ => unreachable!(),
        };
        assert_eq!(recorded_pids(&scratch.pidfile()), vec![pid]);
        assert_eq!(live_pids(&scratch.pidfile()), vec![pid]);
        for previous in &completed {
            previous.assert_stopped();
        }
        println!("measured lifecycle cycle={cycle}; node={pid};");
        let mut observed = MeasuredLifecycleCycle {
            scratch,
            pid,
            events: format!("SPAWN {pid}\n"),
        };
        assert_eq!(std::fs::read_to_string(&events).unwrap(), observed.events);
        let endpoint = handle.endpoint().unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            endpoint.forward(
                Request::builder()
                    .uri("/stream")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("measured stream headers timed out")
        .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        let held_body = response.into_body();
        observed.events.push_str(&format!("GET /stream {pid}\n"));
        let sending = endpoint.clone();
        let upload = tokio::spawn(async move {
            sending
                .forward(
                    Request::builder()
                        .method("POST")
                        .uri("/upload")
                        .body(Body::from(format!("payload-{cycle}")))
                        .unwrap(),
                )
                .await
        });
        observed
            .events
            .push_str(&format!("POST /upload payload-{cycle} {pid}\n"));
        tokio::time::timeout(Duration::from_secs(3), async {
            while std::fs::read_to_string(&events).unwrap() != observed.events {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("measured mutation not accepted exactly once");
        assert!(!upload.is_finished());
        assert!(endpoint.connections.tasks.len() >= 2);
        supervisor.disarm_restarts();
        let started = std::time::Instant::now();
        let exit = tokio::time::timeout(Duration::from_secs(5), supervisor.shutdown())
            .await
            .expect("measured EOF shutdown timed out")
            .expect("measured EOF owned connection cleanup failed");
        assert_eq!(exit, super::CompatExit::Graceful);
        let mutation_error = tokio::time::timeout(Duration::from_secs(1), upload)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            mutation_error.to_string(),
            "Compatibility dispatch outcome unknown"
        );
        assert!(endpoint.connections.tasks.is_empty());
        assert!(handle.endpoint().is_none());
        assert!(matches!(*handle.status.borrow(), super::Status::Stopped));
        assert!(super::drain::process_group_absent(pid));
        assert!(live_pids(&observed.scratch.pidfile()).is_empty());
        let stale_error = endpoint
            .forward(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .body(Body::from("must-not-resend"))
                    .unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            stale_error.to_string(),
            "Compatibility generation is stopping"
        );
        observed
            .events
            .push_str(&format!("EOF {pid}\nEXIT {pid} 0\n"));
        assert_eq!(std::fs::read_to_string(&events).unwrap(), observed.events);
        assert_eq!(recorded_pids(&observed.scratch.pidfile()), vec![pid]);
        println!(
            "{}",
            serde_json::json!({
                "case": "measured_lifecycle", "cycle": cycle, "pid": pid,
                "events": observed.events, "compat_exit": "graceful",
                "connection_tasks": endpoint.connections.tasks.len(),
                "mutation_error": mutation_error.to_string(),
                "stale_endpoint_error": stale_error.to_string(),
                "shutdown_ms": started.elapsed().as_millis(),
                "previous_cycles": completed.iter().map(|record| record.pid).collect::<Vec<_>>(),
            })
        );
        drop(held_body);
        completed.push(observed);
    }
    for record in &completed {
        record.assert_stopped();
    }
    assert_eq!(completed.len(), 3);
}

#[cfg(target_os = "linux")]
#[path = "tests/built_startup.rs"]
mod built_startup;

#[test]
fn ticket_t_3_drain_rate_limit_debits_require_current_generation_signature() {
    use hmac::Mac;
    let endpoint = super::Endpoint::bound_to("unused-quota-test.sock");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let debit = |generation: &str, bucket: &str, time: u64| {
        let encoded = hex::encode(
            serde_json::to_vec(&serde_json::json!({
                "generation":generation, "bucket":bucket, "issued_at":time,
            }))
            .unwrap(),
        );
        let mut signer = hmac::Hmac::<sha2::Sha256>::new_from_slice(&[9; 32]).unwrap();
        signer.update(encoded.as_bytes());
        let signature = hex::encode(signer.finalize().into_bytes());
        (encoded, signature)
    };
    for (bucket, expected) in [("app", false), ("media", true)] {
        let (encoded, signature) = debit("test-generation", bucket, now);
        assert_eq!(
            endpoint.rate_limit_bucket(&encoded, &signature),
            Some(expected)
        );
        assert_eq!(endpoint.rate_limit_bucket(&encoded, &"00".repeat(32)), None);
    }
    for (generation, bucket, time) in [
        ("prior-generation", "app", now),
        ("test-generation", "unknown", now),
        ("test-generation", "app", now - 6000),
        ("test-generation", "app", now + 6000),
    ] {
        let (encoded, signature) = debit(generation, bucket, time);
        assert_eq!(endpoint.rate_limit_bucket(&encoded, &signature), None);
    }
}

// Unix only until GRE-1204: release verification rejects Windows npm junctions
// with "Dependency link inventory mismatch".
#[cfg(unix)]
#[tokio::test]
async fn ticket_t_2_respawn_blocked_during_verification_starts_no_child() {
    let scratch = Scratch::new("respawn-disarmed");
    let bundle = measured_startup_bundle();
    let assets = bundle.web_root.join("apps/web/dist");
    let executed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let recorded = executed.clone();
    let spec = super::SpawnSpec {
        bundle: super::release::verify_bundle(bundle, &assets).unwrap(),
        before_setup: Box::new(move |_, _| {
            recorded.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }),
        ..release_spec(&scratch)
    };
    let logs = super::logs::Logs::open(spec.data_dir.join("logs")).unwrap();
    let disarmed = CancellationToken::new();
    disarmed.cancel();
    let error = super::Process::spawn(&spec, &logs, &CancellationToken::new(), Some(&disarmed))
        .await
        .err()
        .expect("a blocked respawn must not start a child");
    assert_eq!(
        error.to_string(),
        "Compatibility restart blocked during release verification"
    );
    assert!(!executed.load(std::sync::atomic::Ordering::SeqCst));
}
