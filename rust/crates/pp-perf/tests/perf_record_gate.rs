//! T-53 (GRE-388): perf record round trip, the compare gate and its mismatches.
use std::fs;
use std::path::Path;

use pp_perf::{
    Artifact, Comparison, MIN_SAMPLES, Machine, Measurement, Metric, Mismatch, PssError,
    SCHEMA_VERSION, Side, Toolchain, Verdict, compare, sample_process_tree, sample_process_tree_at,
};

fn run(metric: Metric, samples: &[f64]) -> Measurement {
    Measurement {
        schema: SCHEMA_VERSION,
        metric,
        samples: samples.to_vec(),
        noise_tolerance_pct: 5.0,
        simulated_printers: 5,
        captured_at: "2026-10-30T08:00:00Z".into(),
        machine: Machine {
            os: "linux".into(),
            arch: "x86_64".into(),
            kernel: "6.8.0".into(),
            cpu_model: "AMD Ryzen AI Max+ 395".into(),
            logical_cpus: 3,
        },
        artifact: Artifact {
            kind: "node-server".into(),
            name: "print-partner".into(),
            sha256: "ab".repeat(32),
        },
        toolchain: Toolchain {
            rustc: Some("1.99.0".into()),
            node: Some("24.21.0".into()),
        },
    }
}

fn steady(value: f64) -> Vec<f64> {
    vec![value; MIN_SAMPLES]
}

fn verdict(baseline: &Measurement, candidate: &Measurement) -> Verdict {
    match compare(baseline, candidate) {
        Comparison::Comparable { verdict, .. } => verdict,
        other => panic!("expected a comparable pair, got {other:?}"),
    }
}

fn mismatches(baseline: &Measurement, candidate: &Measurement) -> Vec<Mismatch> {
    match compare(baseline, candidate) {
        Comparison::NotComparable(reasons) => reasons,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn ticket_t_53_record_round_trips_raw_samples_and_metadata() {
    let record = run(Metric::PlanSaveP95Ms, &[41.5, 40.25, 42.0, 41.0, 40.75]);
    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(json["metric"], "plan_save_p95_ms");
    assert_eq!(json["samples"].as_array().unwrap().len(), 5);
    assert_eq!(json["machine"]["cpu_model"], "AMD Ryzen AI Max+ 395");
    assert_eq!(json["toolchain"]["node"], "24.21.0");
    assert_eq!(serde_json::from_value::<Measurement>(json).unwrap(), record);
}

#[test]
fn ticket_t_53_gate_fails_only_above_ten_percent() {
    let baseline = run(Metric::StartupReadyMs, &steady(1000.0));
    assert_eq!(
        verdict(&baseline, &run(Metric::StartupReadyMs, &steady(1100.0))),
        Verdict::Pass
    );
    assert_eq!(
        verdict(&baseline, &run(Metric::StartupReadyMs, &steady(1100.5))),
        Verdict::Regression
    );
    match compare(&baseline, &run(Metric::StartupReadyMs, &steady(1250.0))) {
        Comparison::Comparable {
            baseline_median,
            candidate_median,
            change_pct,
            verdict,
        } => {
            assert_eq!(
                (baseline_median, candidate_median, change_pct),
                (1000.0, 1250.0, 25.0)
            );
            assert_eq!(verdict, Verdict::Regression);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ticket_t_53_faster_candidate_passes_without_an_improvement_verdict() {
    let baseline = run(Metric::IdleCpuPercent, &steady(4.0));
    assert_eq!(
        verdict(&baseline, &run(Metric::IdleCpuPercent, &steady(1.0))),
        Verdict::Pass
    );
}

#[test]
fn ticket_t_53_gate_uses_medians_of_raw_samples() {
    // One slow outlier in an even-sized set does not move the median past the gate.
    let mut baseline = run(
        Metric::PackageBytes,
        &[100.0, 100.0, 100.0, 100.0, 100.0, 100.0],
    );
    let mut candidate = run(
        Metric::PackageBytes,
        &[104.0, 106.0, 108.0, 110.0, 112.0, 114.0],
    );
    baseline.noise_tolerance_pct = 10.0;
    candidate.noise_tolerance_pct = 10.0;
    match compare(&baseline, &candidate) {
        Comparison::Comparable {
            candidate_median,
            verdict,
            ..
        } => {
            assert_eq!(candidate_median, 109.0);
            assert_eq!(verdict, Verdict::Pass);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ticket_t_53_refuses_runs_from_different_setups() {
    let baseline = run(Metric::StartupReadyMs, &steady(1000.0));
    let mut candidate = baseline.clone();
    candidate.machine.arch = "aarch64".into();
    candidate.machine.cpu_model = "Apple M4".into();
    candidate.machine.logical_cpus = 10;
    candidate.artifact.kind = "rust-desktop".into();
    candidate.simulated_printers = 4;
    assert_eq!(
        mismatches(&baseline, &candidate),
        [
            Mismatch::Arch,
            Mismatch::CpuModel,
            Mismatch::LogicalCpus,
            Mismatch::ArtifactKind,
            Mismatch::SimulatedPrinters
        ]
    );
    let other_metric = run(Metric::PlanSaveP95Ms, &steady(1000.0));
    assert_eq!(mismatches(&baseline, &other_metric), [Mismatch::Metric]);
}

#[test]
fn ticket_t_53_keeps_linux_pss_and_macos_memory_apart() {
    let linux = run(Metric::MemoryLinuxPssBytes, &steady(200e6));
    let mut mac = run(Metric::MemoryMacosFootprintBytes, &steady(200e6));
    mac.machine.os = "macos".into();
    assert_eq!(mismatches(&linux, &mac), [Mismatch::Metric, Mismatch::Os]);

    let mut pss_on_mac = linux.clone();
    pss_on_mac.machine.os = "macos".into();
    let reasons = mismatches(&linux, &pss_on_mac);
    assert!(
        matches!(&reasons[0], Mismatch::Invalid { side: Side::Candidate, defect } if defect.contains("only captured on linux"))
    );
    assert_eq!(reasons[1..], [Mismatch::Os]);

    let mut rss_on_linux = run(Metric::MemoryMacosRssBytes, &steady(1.0));
    rss_on_linux.machine.os = "linux".into();
    assert!(matches!(
        &mismatches(&rss_on_linux, &rss_on_linux)[0],
        Mismatch::Invalid {
            side: Side::Baseline,
            ..
        }
    ));
}

#[test]
fn ticket_t_53_requires_repeatable_samples() {
    let baseline = run(Metric::StartupReadyMs, &steady(1000.0));
    let short = run(Metric::StartupReadyMs, &[1000.0; MIN_SAMPLES - 1]);
    assert_eq!(
        mismatches(&baseline, &short),
        [Mismatch::TooFewSamples {
            side: Side::Candidate,
            count: MIN_SAMPLES - 1
        }]
    );

    // One 2000 ms sample among four 1000s: mean 1200, deviation 400, so 33.3%.
    let noisy = run(
        Metric::StartupReadyMs,
        &[1000.0, 1000.0, 1000.0, 1000.0, 2000.0],
    );
    match mismatches(&noisy, &baseline).as_slice() {
        [
            Mismatch::Noisy {
                side: Side::Baseline,
                spread_pct,
                tolerance_pct,
            },
        ] => {
            assert!((spread_pct - 100.0 / 3.0).abs() < 1e-9, "{spread_pct}");
            assert_eq!(*tolerance_pct, 5.0);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ticket_t_53_refuses_invalid_records_and_zero_baselines() {
    let good = run(Metric::IdleCpuPercent, &steady(2.0));
    let mut bad = good.clone();
    bad.schema = 2;
    bad.samples[1] = f64::NAN;
    bad.noise_tolerance_pct = 100.0;
    let defects: Vec<_> = mismatches(&good, &bad)
        .into_iter()
        .map(|m| match m {
            Mismatch::Invalid {
                side: Side::Candidate,
                defect,
            } => defect,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(defects.len(), 3, "{defects:?}");

    // Both sides invalid: baseline reasons first, then candidate, then setup differences.
    let mut bad_baseline = good.clone();
    bad_baseline.schema = 0;
    let mut bad_candidate = good.clone();
    bad_candidate.samples[0] = -1.0;
    bad_candidate.simulated_printers = 4;
    match mismatches(&bad_baseline, &bad_candidate).as_slice() {
        [
            Mismatch::Invalid {
                side: Side::Baseline,
                defect: first,
            },
            Mismatch::Invalid {
                side: Side::Candidate,
                defect: second,
            },
            Mismatch::SimulatedPrinters,
        ] => {
            assert_eq!(first, "schema 0 is not 1");
            assert_eq!(second, "sample 0 is not a finite non-negative number");
        }
        other => panic!("{other:?}"),
    }

    let idle = run(Metric::IdleCpuPercent, &steady(0.0));
    assert_eq!(mismatches(&idle, &good), [Mismatch::ZeroBaseline]);
}

fn fake_process(proc_root: &Path, pid: u32, comm: &str, parent: u32, pss_kib: Option<u64>) {
    let dir = proc_root.join(pid.to_string());
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("stat"),
        format!("{pid} ({comm}) S {parent} {pid} {pid} 0 -1 4194560\n"),
    )
    .unwrap();
    if let Some(kib) = pss_kib {
        let rollup = format!(
            "55d0-7ffd ---p 00000000 00:00 0 [rollup]\nRss: {} kB\nPss: {kib} kB\nPss_Anon: 1 kB\n",
            kib * 2
        );
        fs::write(dir.join("smaps_rollup"), rollup).unwrap();
    }
}

#[test]
fn ticket_t_53_pss_sums_the_root_and_its_descendants_only() {
    let proc_root = tempfile::tempdir().unwrap();
    let root = proc_root.path();
    fake_process(root, 1, "systemd", 0, Some(9_000));
    fake_process(root, 100, "pp-desktop", 1, Some(1_000));
    fake_process(root, 101, "node) S 1 (", 100, Some(250));
    fake_process(root, 102, "pp server", 101, Some(30));
    // Exited between listing and reading: smaps_rollup is gone, so it is skipped.
    fake_process(root, 103, "short", 100, None);
    fake_process(root, 200, "unrelated", 1, Some(7_000));
    fs::create_dir_all(root.join("self")).unwrap();
    fs::write(root.join("uptime"), "1.0 1.0\n").unwrap();

    let tree = sample_process_tree_at(root, 100).unwrap();
    assert_eq!(tree.root, 100);
    assert_eq!(tree.pids, [100, 101, 102]);
    assert_eq!(tree.pss_bytes, (1_000 + 250 + 30) * 1024);
}

#[test]
fn ticket_t_53_pss_reports_a_missing_root_and_malformed_files() {
    let proc_root = tempfile::tempdir().unwrap();
    let root = proc_root.path();
    fake_process(root, 100, "gone", 1, None);
    assert_eq!(
        sample_process_tree_at(root, 100),
        Err(PssError::RootMissing(100))
    );
    assert_eq!(
        sample_process_tree_at(root, 999),
        Err(PssError::RootMissing(999))
    );

    fs::write(root.join("100").join("smaps_rollup"), "Rss: 4 kB\n").unwrap();
    assert!(
        matches!(sample_process_tree_at(root, 100), Err(PssError::Malformed { detail, .. }) if detail == "no Pss line")
    );
    fs::write(
        root.join("100").join("smaps_rollup"),
        format!("Pss: {} kB\n", u64::MAX / 1024 + 1),
    )
    .unwrap();
    assert!(
        matches!(sample_process_tree_at(root, 100), Err(PssError::Malformed { detail, .. }) if detail == "Pss overflows u64 bytes")
    );
    fs::write(root.join("100").join("stat"), "100 no-parens\n").unwrap();
    assert!(matches!(
        sample_process_tree_at(root, 100),
        Err(PssError::Malformed { .. })
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn ticket_t_53_pss_reads_this_test_process_on_linux() {
    let tree = sample_process_tree(std::process::id()).unwrap();
    assert!(tree.pids.contains(&std::process::id()));
    assert!(tree.pss_bytes > 0);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn ticket_t_53_pss_is_linux_only() {
    assert_eq!(
        sample_process_tree(std::process::id()),
        Err(PssError::Unsupported)
    );
}
