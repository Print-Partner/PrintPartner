use super::*;
use crate::rebuild_api::desktop_policy;
use pp_storage::{
    Limits,
    auth::{Outcome as AuthOutcome, Request as AuthRequest, Secret},
    read_model::Credential,
    send_queue::writes::{Reply, Request},
};
use pp_testkit::moonraker::RoutedPrinter;
use serde_json::{Value, json};
use std::io::Read;

const UPLOAD: &str = "/server/files/upload";

struct Farm {
    root: tempfile::TempDir,
    owner: WriterOwner,
    token: String,
}

impl Farm {
    /// A storage with one signed-in account, one Moonraker printer at `url`
    /// and the queue items `(id, start, staged)` for it.
    fn open(url: &str, items: &[(&str, bool, bool)]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (owner, _) =
            WriterOwner::open_with_job_feature_admission(root.path(), Limits::default()).unwrap();
        let AuthOutcome::Session { token, .. } = owner
            .auth_with_policy(desktop_policy())
            .unwrap()
            .submit(
                AuthRequest::Register {
                    email: "sender@example.com".into(),
                    display_name: "Sender".into(),
                    password: Secret::new("ordinary-password".into()),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
        else {
            panic!("session expected")
        };
        let farm = Self {
            root,
            owner,
            token: token.expose().into(),
        };
        let database = farm.database();
        for (key, value) in [
            ("feature_toggles", json!({"farm": true})),
            (
                "printer.fleet",
                json!([{"id": "p-moon", "name": "Micron", "integration_id": "moon"}]),
            ),
            (
                "integrations",
                json!([{"id": "moon", "name": "Micron host", "type": "moonraker",
                        "config": {"base_url": url}}]),
            ),
        ] {
            database
                .execute(
                    "INSERT INTO app_settings(tenant_id,key,value) VALUES('default',?1,?2)",
                    rusqlite::params![key, value.to_string()],
                )
                .unwrap();
        }
        for (position, (id, start, staged)) in items.iter().enumerate() {
            let path = farm
                .exports()
                .join("printer-uploads")
                .join(id)
                .join("a.gcode");
            if *staged {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(
                    &path,
                    b"G28\nG1 X10\n; filament used [mm] = 5000\n; filament used [g] = 12.5\n",
                )
                .unwrap();
            }
            database
                .execute(
                    "INSERT INTO printer_send_queue(tenant_id,position,id,filename,artifact_path,printer_id,profile_id,match,wait_for_idle,start,state,checkoff_units,upload_job_id,error,host_name,created_at,updated_at)
                     VALUES('default',?1,?2,'a.gcode',?3,'p-moon',7,'pinned',1,?4,'queued','[{\"part_id\":4,\"unit_index\":0}]',NULL,NULL,NULL,'2026-11-06T00:00:00.000Z','2026-11-06T00:00:00.000Z')",
                    rusqlite::params![position as i64, id, path.to_string_lossy(), start],
                )
                .unwrap();
        }
        farm
    }

    fn exports(&self) -> PathBuf {
        self.root.path().join("exports")
    }

    fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.path().join("print-partner.db")).unwrap()
    }

    fn send(&self, id: &str) -> Reply {
        self.owner
            .send_queue(desktop_policy())
            .unwrap()
            .apply(
                Credential::Session(Secret::new(self.token.clone())),
                Request::Dispatch {
                    id: id.into(),
                    host_name: "Micron host".into(),
                },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
    }

    fn worker(&self) -> SendWorker {
        SendWorker::new(&self.owner, desktop_policy(), HostSends::new().unwrap()).unwrap()
    }

    /// The item's state and error, and its job's state and public result.
    fn outcome(&self, id: &str) -> (String, Option<String>, String, Value) {
        self.database()
            .query_row(
                "SELECT q.state,q.error,j.state,j.document FROM printer_send_queue q JOIN durable_jobs j ON j.id=q.upload_job_id WHERE q.id=?1",
                [id],
                |row| {
                    let document: String = row.get(3)?;
                    let document: Value = serde_json::from_str(&document).unwrap();
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, document["public_result"].clone()))
                },
            )
            .unwrap()
    }

    fn links(&self) -> Value {
        self.database()
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id='default' AND key='printer.checkoff_links'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|value| serde_json::from_str(&value).unwrap())
            .unwrap_or(Value::Null)
    }
}

fn printer(routes: &[(&str, u16)]) -> RoutedPrinter {
    RoutedPrinter::start(
        routes
            .iter()
            .map(|(target, status)| ((*target).to_owned(), *status, b"{}".to_vec()))
            .collect(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_26_upload_only_send_uploads_once_and_never_calls_print_start() {
    let printer = printer(&[(UPLOAD, 201)]);
    let farm = Farm::open(printer.url(), &[("q1", false, true)]);
    let worker = farm.worker();
    assert_eq!(
        worker.run_next().await.unwrap(),
        None,
        "nothing is sent before a dispatch"
    );
    assert!(matches!(farm.send("q1"), Reply::Dispatched { .. }));
    assert_eq!(worker.run_next().await.unwrap(), Some(Sent::Done));
    let lines = printer.request_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with(&format!("POST {UPLOAD}")));
    let (state, error, job, result) = farm.outcome("q1");
    assert_eq!(
        (state.as_str(), error, job.as_str()),
        ("done", None, "succeeded")
    );
    assert_eq!(result["printer_name"], "Micron");
    assert_eq!(result["host_name"], "Micron host");
    assert_eq!(result["integration_type"], "moonraker");
    assert_eq!(result["started"], false);
    let link = &farm.links()[0];
    assert_eq!(result["checkoff_link_id"], link["id"]);
    assert_eq!(result["checkoff_units"], 1);
    assert_eq!(result["filament_estimate_g"], 12.5);
    assert_eq!(result["filament_grams_per_mm"], 0.0025);
    let grams: (f64, i64, String, f64) = farm
        .database()
        .query_row(
            "SELECT grams,units,basis,grams_per_mm FROM print_link_grams WHERE tenant_id='default' AND link_id=?1",
            [link["id"].as_str().unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(grams, (12.5, 1, "estimated".into(), 0.0025));
    assert_eq!(
        (
            link["profile_id"].clone(),
            link["state"].clone(),
            link["started"].clone()
        ),
        (json!(7), json!("watching"), json!(false))
    );
    assert_eq!(worker.run_next().await.unwrap(), None);
    assert_eq!(printer.request_lines().len(), 1);
    // As Node does after a queued send, the staged directory is deleted.
    assert!(!farm.exports().join("printer-uploads").join("q1").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_26_upload_and_start_sends_both_requests_once() {
    let start = "/printer/print/start?filename=a.gcode";
    let printer = printer(&[(UPLOAD, 201), (start, 200)]);
    let farm = Farm::open(printer.url(), &[("q1", true, true)]);
    farm.send("q1");
    assert_eq!(farm.worker().run_next().await.unwrap(), Some(Sent::Done));
    let lines = printer.request_lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].starts_with(&format!("POST {start}")));
    let (state, _, job, result) = farm.outcome("q1");
    assert_eq!(
        (state.as_str(), job.as_str(), result["started"].clone()),
        ("done", "succeeded", json!(true))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_26_a_refused_upload_or_start_fails_the_send_and_frees_the_printer() {
    let start = "/printer/print/start?filename=a.gcode";
    let printer = printer(&[(UPLOAD, 201), (start, 503)]);
    let farm = Farm::open(
        printer.url(),
        &[("started", true, true), ("next", false, true)],
    );
    let worker = farm.worker();
    farm.send("started");
    let Some(Sent::Failed(reason)) = worker.run_next().await.unwrap() else {
        panic!("start refusal expected")
    };
    assert!(
        reason.starts_with("Uploaded, but start failed (HTTP 503)"),
        "{reason}"
    );
    let (state, error, job, _) = farm.outcome("started");
    assert_eq!(
        (state.as_str(), error.as_deref(), job.as_str()),
        ("error", Some(reason.as_str()), "failed")
    );
    // Nothing printed, so the printer is free for the next explicit send.
    assert!(matches!(farm.send("next"), Reply::Dispatched { .. }));

    let refused = self::printer(&[(UPLOAD, 500)]);
    let farm = Farm::open(refused.url(), &[("q1", false, true)]);
    farm.send("q1");
    let Some(Sent::Failed(reason)) = farm.worker().run_next().await.unwrap() else {
        panic!("upload refusal expected")
    };
    assert!(reason.starts_with("Upload failed (HTTP 500)"), "{reason}");
    assert_eq!(farm.outcome("q1").2, "failed");
    assert_eq!(refused.request_lines().len(), 1);
}

/// A loopback host that reads each request and closes without answering.
/// Joining the thread after setting the flag returns the connection count.
fn silent_printer() -> (String, Arc<AtomicBool>, std::thread::JoinHandle<usize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        let mut connections = 0;
        while !stopped.load(std::sync::atomic::Ordering::SeqCst) {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            };
            connections += 1;
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut bytes = [0; 8192];
            while matches!(socket.read(&mut bytes), Ok(read) if read > 0) {}
        }
        connections
    });
    (url, stop, thread)
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_26_an_unknown_upload_outcome_is_never_resent() {
    let (url, stop, host) = silent_printer();
    let farm = Farm::open(&url, &[("q1", false, true)]);
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
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        host.join().unwrap(),
        1,
        "the host saw exactly one connection"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_26_a_missing_or_foreign_file_fails_without_contacting_the_printer() {
    let printer = printer(&[(UPLOAD, 201)]);
    let farm = Farm::open(
        printer.url(),
        &[
            ("gone", false, false),
            ("foreign", false, false),
            ("model", false, true),
        ],
    );
    let outside = farm.root.path().join("elsewhere.gcode");
    std::fs::write(&outside, b"G28\n").unwrap();
    farm.database()
        .execute(
            "UPDATE printer_send_queue SET artifact_path=?1 WHERE id='foreign'",
            [outside.to_string_lossy()],
        )
        .unwrap();
    farm.database()
        .execute(
            "UPDATE printer_send_queue SET filename='a.stl' WHERE id='model'",
            [],
        )
        .unwrap();
    let worker = farm.worker();
    for (id, reason) in [
        ("gone", "Artifact file missing on disk"),
        ("foreign", "Invalid artifact path"),
    ] {
        farm.send(id);
        assert_eq!(
            worker.run_next().await.unwrap(),
            Some(Sent::Failed(reason.into())),
            "{id}"
        );
        let (state, error, job, _) = farm.outcome(id);
        assert_eq!(
            (state.as_str(), error.as_deref(), job.as_str()),
            ("error", Some(reason), "failed")
        );
    }
    // Storage never enqueues an upload of a file that is not sliced.
    assert!(matches!(
        farm.send("model"),
        Reply::Refused { status: 400, .. }
    ));
    assert_eq!(worker.run_next().await.unwrap(), None);
    assert!(printer.request_lines().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_26_farm_turned_off_after_send_now_fails_the_send_without_contacting_the_printer()
{
    let printer = printer(&[(UPLOAD, 201)]);
    let farm = Farm::open(printer.url(), &[("q1", false, true)]);
    assert!(matches!(farm.send("q1"), Reply::Dispatched { .. }));
    farm.database()
        .execute(
            "UPDATE app_settings SET value='{\"farm\":false}' WHERE tenant_id='default' AND key='feature_toggles'",
            [],
        )
        .unwrap();
    assert_eq!(
        farm.worker().run_next().await.unwrap(),
        Some(Sent::Failed("Feature farm is disabled.".into()))
    );
    let (state, error, job, _) = farm.outcome("q1");
    assert_eq!(
        (state.as_str(), error.as_deref(), job.as_str()),
        ("error", Some("Feature farm is disabled."), "failed")
    );
    assert!(printer.request_lines().is_empty());
}

const PRINT_STATS: &str = "/printer/objects/query?print_stats";

impl Farm {
    /// A farm whose item `q1` was uploaded to `printer` with a 12.5 g estimate.
    async fn sent(printer: &RoutedPrinter) -> Self {
        let farm = Self::open(printer.url(), &[("q1", false, true)]);
        farm.send("q1");
        assert_eq!(farm.worker().run_next().await.unwrap(), Some(Sent::Done));
        assert_eq!(farm.grams(), (12.5, "estimated".into(), None));
        farm
    }

    /// The grams row of the newest Checkoff link: grams, basis, measured length.
    fn grams(&self) -> (f64, String, Option<f64>) {
        let links = self.links();
        let link = links.as_array().unwrap().last().unwrap();
        self.database()
            .query_row(
                "SELECT grams,basis,measured_mm FROM print_link_grams WHERE tenant_id='default' AND link_id=?1",
                [link["id"].as_str().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    async fn finish(&self, filename: &str) -> Option<pp_storage::print_grams::Measurement> {
        crate::measured_grams::record(
            &self.owner.client(),
            &pp_gateway::printer_host::HostReads::new().unwrap(),
            &self.owner.print_grams("default"),
            "moon",
            filename,
        )
        .await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1165_a_finished_print_replaces_the_estimate_with_measured_grams() {
    use pp_storage::print_grams::Measurement;
    let stats = br#"{"result":{"status":{"print_stats":{"filament_used":4000.0}}}}"#;
    let printer = RoutedPrinter::start(vec![
        (UPLOAD.into(), 201, b"{}".to_vec()),
        (PRINT_STATS.into(), 200, stats.to_vec()),
    ])
    .unwrap();
    let farm = Farm::sent(&printer).await;
    assert_eq!(farm.finish("other.gcode").await, None, "no link prints it");
    assert_eq!(farm.grams(), (12.5, "estimated".into(), None));
    assert_eq!(
        farm.finish("  gcodes\\A.GCODE ").await,
        Some(Measurement::Recorded),
        "matched like Node: trimmed base name, any case"
    );
    assert_eq!(farm.grams(), (10.0, "measured".into(), Some(4000.0)));
    let reads = printer
        .request_lines()
        .iter()
        .filter(|line| line.contains(PRINT_STATS))
        .count();
    assert_eq!(reads, 2, "one read per finish, never retried");
    let link = &farm.links()[0];
    assert_eq!(link["state"], "watching", "Checkoff is untouched");
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1165_no_measured_length_keeps_the_estimate() {
    let printer = printer(&[(UPLOAD, 201), (PRINT_STATS, 500)]);
    let farm = Farm::sent(&printer).await;
    assert_eq!(farm.finish("a.gcode").await, None);
    assert_eq!(farm.grams(), (12.5, "estimated".into(), None));
}
