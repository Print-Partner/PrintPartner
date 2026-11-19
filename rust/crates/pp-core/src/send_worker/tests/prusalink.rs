//! The send worker owns PrusaLink uploads as it owns Moonraker's (GRE-1240):
//! one Digest probe `GET /api/v1/status`, then one `PUT` into the pinned
//! storage. A start rides on `print-after-upload`, so there is no second send
//! request. A refusal fails the send and frees the printer; an unknown outcome
//! waits for reconciliation and is never resent, across a restart too.
use super::*;

const PUT: &str = "/api/v1/files/usb/a.gcode";
const PROBE: &str = "/api/v1/status";

/// A loopback PrusaLink whose probe answers 200 and whose upload answers
/// `upload` (0 is a malformed status line, an unknown outcome).
fn prusalink(upload: u16, body: &[u8]) -> RoutedPrinter {
    RoutedPrinter::start(vec![
        (PROBE.into(), 200, b"{}".to_vec()),
        (PUT.into(), upload, body.to_vec()),
    ])
    .unwrap()
}

/// The request heads that would send something: every request but the probe.
fn sends(printer: &RoutedPrinter) -> Vec<String> {
    printer
        .request_heads()
        .into_iter()
        .filter(|head| !head.starts_with(&format!("GET {PROBE} ")))
        .collect()
}

fn put_head(head: &str) -> bool {
    head.starts_with(&format!("PUT {PUT} "))
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1240_prusalink_send_puts_once_and_starts_only_through_its_header() {
    for start in [false, true] {
        let printer = prusalink(201, b"{}");
        let farm = Farm::open(printer.url(), &[("q1", start, true)]);
        farm.as_prusalink();
        let worker = farm.worker();
        assert_eq!(worker.run_next().await.unwrap(), None, "add sends nothing");
        assert!(printer.request_lines().is_empty());
        assert!(matches!(farm.send("q1"), Reply::Dispatched { .. }));
        assert_eq!(worker.run_next().await.unwrap(), Some(Sent::Done));

        let heads = sends(&printer);
        assert_eq!(heads.len(), 1, "one PUT, no start request: {heads:?}");
        assert!(put_head(&heads[0]), "{heads:?}");
        let head = heads[0].to_lowercase();
        let flag = if start { "?1" } else { "?0" };
        assert!(
            head.contains(&format!("print-after-upload: {flag}")),
            "{head}"
        );
        assert!(head.contains("overwrite: ?1"), "{head}");
        assert!(printer.request_lines()[0].starts_with(&format!("GET {PROBE} ")));

        let (state, error, job, result) = farm.outcome("q1");
        assert_eq!(
            (state.as_str(), error, job.as_str()),
            ("done", None, "succeeded")
        );
        assert_eq!(result["integration_type"], "prusalink");
        assert_eq!(result["host_name"], "Micron host");
        assert_eq!(result["remote_path"], "usb/a.gcode");
        assert_eq!(result["started"], start);
        let link = &farm.links()[0];
        assert_eq!(result["checkoff_link_id"], link["id"]);
        assert_eq!(link["started"], start);
        assert_eq!(farm.grams(), estimate());
        assert!(!farm.exports().join("printer-uploads").join("q1").exists());

        assert_eq!(worker.run_next().await.unwrap(), None);
        assert_eq!(sends(&printer).len(), 1, "nothing is sent again");
        println!(
            "GRE_1240_RECEIPT {}",
            json!({"case": "confirmed", "start": start,
            "request_lines": printer.request_lines(), "job_state": job,
            "remote_path": result["remote_path"]})
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1240_a_refused_prusalink_upload_fails_the_send_and_frees_the_printer() {
    let disk_full = br#"{"title":"Disk full"}"#;
    for (status, body, reason) in [
        (500, &disk_full[..], "Upload failed (HTTP 500)"),
        (
            401,
            &b"{}"[..],
            "Upload failed (HTTP 401): PrusaLink rejected the username or password",
        ),
    ] {
        let printer = prusalink(status, body);
        let farm = Farm::open(printer.url(), &[("q1", true, true), ("next", false, true)]);
        farm.as_prusalink();
        let worker = farm.worker();
        farm.send("q1");
        let Some(Sent::Failed(failed)) = worker.run_next().await.unwrap() else {
            panic!("HTTP {status}: refusal expected")
        };
        assert!(failed.starts_with(reason), "{failed}");
        assert!(!failed.contains("not-a-real-secret"), "{failed}");
        let (state, error, job, _) = farm.outcome("q1");
        assert_eq!(
            (state.as_str(), error.as_deref(), job.as_str()),
            ("error", Some(failed.as_str()), "failed"),
            "HTTP {status}"
        );
        let heads = sends(&printer);
        assert_eq!(
            heads.len(),
            1,
            "HTTP {status}: one PUT, no resend: {heads:?}"
        );
        assert!(put_head(&heads[0]));
        assert_eq!(worker.run_next().await.unwrap(), None);
        assert_eq!(sends(&printer).len(), 1);
        // Nothing printed, so the printer is free for the next explicit send.
        assert!(matches!(farm.send("next"), Reply::Dispatched { .. }));
        println!(
            "GRE_1240_RECEIPT {}",
            json!({"case": "refused", "status": status, "error": failed,
            "request_lines": printer.request_lines(), "job_state": job})
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1240_an_unknown_prusalink_upload_outcome_is_never_resent_even_after_restart() {
    let printer = prusalink(0, b"{}");
    let farm = Farm::open(printer.url(), &[("q1", true, true)]);
    farm.as_prusalink();
    let worker = farm.worker();
    farm.send("q1");
    let Some(Sent::Unknown(_)) = worker.run_next().await.unwrap() else {
        panic!("unknown outcome expected")
    };
    let (state, error, job, _) = farm.outcome("q1");
    assert_eq!(state, "error");
    assert_eq!(
        error.as_deref(),
        Some(pp_storage::send_queue::UNKNOWN_OUTCOME_ERROR)
    );
    assert_eq!(job, "reconciliation_required");
    assert_eq!(worker.run_next().await.unwrap(), None);
    assert!(matches!(
        farm.send("q1"),
        Reply::Refused {
            code: Some("send_outcome_unknown"),
            ..
        }
    ));

    drop(worker);
    let Farm { root, owner, token } = farm;
    owner.shutdown().unwrap();
    let owner = WriterOwner::open_with_job_feature_admission(root.path(), Limits::default())
        .unwrap()
        .0;
    let farm = Farm { root, owner, token };
    assert_eq!(farm.worker().run_next().await.unwrap(), None);
    assert!(matches!(
        farm.send("q1"),
        Reply::Refused {
            code: Some("send_outcome_unknown"),
            ..
        }
    ));
    assert_eq!(farm.outcome("q1").2, "reconciliation_required");
    let heads = sends(&printer);
    assert_eq!(
        heads.len(),
        1,
        "an unknown upload is never resent: {heads:?}"
    );
    assert!(put_head(&heads[0]));
    println!(
        "GRE_1240_RECEIPT {}",
        json!({"case": "unknown", "restarted": true,
        "request_lines": printer.request_lines(), "job_state": farm.outcome("q1").2})
    );
}
