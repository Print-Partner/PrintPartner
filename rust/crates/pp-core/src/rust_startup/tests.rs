//! Rust-only startup order on disposable pp-testkit installations, including a
//! copied Node schema 34 database: the marker decision comes first, and a
//! refused directory sees none of release verification, cutover recovery or
//! source-sync admission.
//!
//! pp-core's tests build pp-storage with `test-unmarked-writer`, so
//! `WriterOwner::open` skips the marker decision here. These tests pass the
//! gate explicitly with `WriterOwner::open_with_gate`.

use super::{self as rust_startup, Admitted};
use pp_gateway::ownership::cutover::{CUTOVER_FILE, EVIDENCE_FILE};
use pp_storage::{
    Limits, WriterOwner,
    ownership_marker::{MARKER_FILE, OwnershipRefusal, WriterGate},
};
use pp_testkit::{Fixture, Installation, MarkerState, Spec};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

/// Matches cutover recovery's private `.operation-cutover.*.tmp` pattern.
const CUTOVER_TEMPORARY: &str = ".operation-cutover.planted.tmp";
/// pp-gateway's receipt name is private.
const SOURCE_SYNC_RECEIPT: &str = "source-sync-admission.json";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    /// `open_writer` started: lease and marker decision.
    MarkerDecision,
    /// `open_writer` returned an open writer.
    Accepted,
    Verify,
}

/// What a step saw in the data directory when it ran.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    step: Step,
    marker: bool,
    cutover_temporary: bool,
    cutover_evidence: bool,
    source_sync_receipt: bool,
}

type Trace = Arc<Mutex<Vec<Seen>>>;

fn observe(trace: &Trace, directory: &Path, step: Step) {
    trace.lock().unwrap().push(Seen {
        step,
        marker: directory.join(MARKER_FILE).exists(),
        cutover_temporary: directory.join(CUTOVER_TEMPORARY).exists(),
        cutover_evidence: directory.join(EVIDENCE_FILE).exists(),
        source_sync_receipt: directory.join(SOURCE_SYNC_RECEIPT).exists(),
    });
}

fn snapshot(root: &Path) -> pp_testkit::Snapshot {
    pp_testkit::snapshot(root).unwrap()
}

/// Run `rust_startup::admit` with `gate` and a recording `verify` that
/// returns `verify_result`.
async fn admit(
    installation: &Installation,
    gate: WriterGate,
    trace: &Trace,
    verify_result: anyhow::Result<()>,
) -> anyhow::Result<Admitted<()>> {
    let directory = installation.path().to_owned();
    let (open_trace, open_directory) = (trace.clone(), directory.clone());
    let (verify_trace, verify_directory) = (trace.clone(), directory.clone());
    rust_startup::admit(
        directory,
        move |data_dir| {
            observe(&open_trace, &open_directory, Step::MarkerDecision);
            let (writer, _) = WriterOwner::open_with_gate(data_dir, Limits::default(), gate)?;
            observe(&open_trace, &open_directory, Step::Accepted);
            Ok(writer)
        },
        move || {
            observe(&verify_trace, &verify_directory, Step::Verify);
            verify_result
        },
    )
    .await
}

fn steps(trace: &Trace) -> Vec<Step> {
    trace
        .lock()
        .unwrap()
        .iter()
        .map(|seen| seen.step.clone())
        .collect()
}

fn plant_cutover_temporary(installation: &Installation) {
    std::fs::write(installation.path().join(CUTOVER_TEMPORARY), b"planted").unwrap();
}

fn reopen_and_close(installation: &Installation) {
    let (writer, _) =
        WriterOwner::open_with_gate(installation.path(), Limits::default(), WriterGate::SwitchOn)
            .expect("the lease was released after the failed startup");
    writer.shutdown().unwrap();
}

/// A directory with a valid marker and a copied Node schema 34 database.
fn marked_schema_34() -> Installation {
    Installation::create(Spec {
        node_database: true,
        marker: MarkerState::Valid,
        ..Spec::default()
    })
    .unwrap()
}

#[tokio::test]
async fn refused_directory_runs_no_startup_step_and_stays_byte_for_byte_unchanged() {
    let cases = [
        (Fixture::NodeSchema34, WriterGate::SwitchOn, "not empty"),
        (
            Fixture::MarkedCorrupt,
            WriterGate::SwitchOn,
            "invalid marker",
        ),
        (Fixture::Empty, WriterGate::SwitchOff, "switch off"),
    ];
    for (fixture, gate, kind) in cases {
        let installation = Installation::create(fixture).unwrap();
        // Cutover recovery would remove this; source-sync admission would add
        // its receipt next to it.
        plant_cutover_temporary(&installation);
        let before = snapshot(installation.path());
        let trace = Trace::default();
        let error = match admit(&installation, gate, &trace, Ok(())).await {
            Ok(_) => panic!("{fixture:?}: admitted"),
            Err(error) => error,
        };
        let refusal = error
            .downcast_ref::<OwnershipRefusal>()
            .unwrap_or_else(|| panic!("{fixture:?}: not an ownership refusal: {error:#}"));
        match (kind, refusal) {
            ("not empty", OwnershipRefusal::NotEmptyUnmarked { .. })
            | ("invalid marker", OwnershipRefusal::Marker(_))
            | ("switch off", OwnershipRefusal::SwitchOff) => {}
            _ => panic!("{fixture:?}: expected {kind}, got {refusal:?}"),
        }
        assert_eq!(
            steps(&trace),
            [Step::MarkerDecision],
            "{fixture:?}: a step ran after the refusal"
        );
        assert_eq!(
            snapshot(installation.path()),
            before,
            "{fixture:?}: the refused directory changed"
        );
    }
}

#[tokio::test]
async fn empty_directory_gets_the_marker_before_any_startup_step() {
    let installation = Installation::create(Fixture::Empty).unwrap();
    let trace = Trace::default();
    let admitted = admit(&installation, WriterGate::SwitchOn, &trace, Ok(()))
        .await
        .unwrap();
    let seen = trace.lock().unwrap().clone();
    let at = |step: Step| seen.iter().find(|seen| seen.step == step).unwrap().clone();
    assert_eq!(
        steps(&trace),
        [Step::MarkerDecision, Step::Accepted, Step::Verify]
    );
    let before_decision = at(Step::MarkerDecision);
    assert!(!before_decision.marker && !before_decision.source_sync_receipt);
    // The decision wrote the marker; verification runs after it, and source-sync
    // admission has not run yet.
    let verify = at(Step::Verify);
    assert!(verify.marker, "verification ran before the marker decision");
    assert!(!verify.source_sync_receipt && !verify.cutover_evidence);
    assert!(installation.path().join(SOURCE_SYNC_RECEIPT).exists());
    assert_eq!(
        admitted.owners.rust_operations().count(),
        0,
        "Rust-only mode admits no cutover"
    );
    admitted.writer.shutdown().unwrap();
}

#[tokio::test]
async fn accepted_schema_34_directory_runs_all_three_steps_after_the_marker_decision() {
    let installation = marked_schema_34();
    plant_cutover_temporary(&installation);
    let trace = Trace::default();
    let admitted = admit(&installation, WriterGate::SwitchOn, &trace, Ok(()))
        .await
        .unwrap();
    assert_eq!(
        steps(&trace),
        [Step::MarkerDecision, Step::Accepted, Step::Verify]
    );
    // Neither cutover recovery nor source-sync admission ran before the marker
    // decision or before verification.
    for seen in trace.lock().unwrap().iter() {
        assert!(
            seen.cutover_temporary,
            "{seen:?}: cutover recovery ran early"
        );
        assert!(
            !seen.cutover_evidence,
            "{seen:?}: cutover recovery ran early"
        );
        assert!(
            !seen.source_sync_receipt,
            "{seen:?}: source-sync admission ran early"
        );
    }
    // Afterwards both ran: recovery removed the temporary and logged its boot,
    // and source-sync admission wrote its receipt.
    let directory = installation.path();
    assert!(!directory.join(CUTOVER_TEMPORARY).exists());
    let evidence = std::fs::read_to_string(directory.join(EVIDENCE_FILE)).unwrap();
    assert!(
        evidence.contains("\"abandoned_temporaries\":1"),
        "{evidence}"
    );
    assert!(directory.join(SOURCE_SYNC_RECEIPT).exists());
    admitted.writer.shutdown().unwrap();
}

#[tokio::test]
async fn failed_verification_skips_recovery_and_admission_and_releases_the_lease() {
    let installation = marked_schema_34();
    plant_cutover_temporary(&installation);
    let trace = Trace::default();
    let error = admit(
        &installation,
        WriterGate::SwitchOn,
        &trace,
        Err(anyhow::anyhow!("planted release mismatch")),
    )
    .await
    .err()
    .expect("verification failed");
    assert_eq!(error.to_string(), "planted release mismatch");
    assert_eq!(
        steps(&trace),
        [Step::MarkerDecision, Step::Accepted, Step::Verify]
    );
    let directory = installation.path();
    assert!(directory.join(CUTOVER_TEMPORARY).exists());
    assert!(!directory.join(EVIDENCE_FILE).exists());
    assert!(!directory.join(SOURCE_SYNC_RECEIPT).exists());
    assert!(directory.join(MARKER_FILE).exists());
    reopen_and_close(&installation);
}

#[tokio::test]
async fn refused_cutover_record_skips_admission_and_releases_the_lease() {
    let installation = marked_schema_34();
    std::fs::write(installation.path().join(CUTOVER_FILE), b"{}").unwrap();
    let trace = Trace::default();
    let error = admit(&installation, WriterGate::SwitchOn, &trace, Ok(()))
        .await
        .err()
        .expect("a cutover record without an ownership index refuses startup");
    assert!(error.to_string().contains("RecordWithoutFlag"), "{error:#}");
    assert_eq!(
        steps(&trace),
        [Step::MarkerDecision, Step::Accepted, Step::Verify]
    );
    assert!(!installation.path().join(SOURCE_SYNC_RECEIPT).exists());
    reopen_and_close(&installation);
}
