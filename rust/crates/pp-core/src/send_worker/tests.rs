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

    /// The newest Checkoff link's `print_link_grams` row as
    /// `[grams, units, basis, grams_per_mm, measured_mm]`.
    fn grams(&self) -> Value {
        let links = self.links();
        self.grams_of(
            links.as_array().unwrap().last().unwrap()["id"]
                .as_str()
                .unwrap(),
        )
    }

    fn grams_of(&self, link: &str) -> Value {
        let row: String = self
            .database()
            .query_row(
                "SELECT json_array(grams,units,basis,grams_per_mm,measured_mm) FROM print_link_grams WHERE tenant_id='default' AND link_id=?1",
                [link],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&row).unwrap()
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
    assert_eq!(farm.grams(), json!([12.5, 1, "estimated", 0.0025, null]));
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

fn estimate() -> Value {
    json!([12.5, 1, "estimated", 0.0025, null])
}

impl Farm {
    /// A farm whose item `q1` was uploaded to `printer` with a 12.5 g estimate.
    async fn sent(printer: &RoutedPrinter) -> Self {
        let farm = Self::open(printer.url(), &[("q1", false, true)]);
        farm.send("q1");
        assert_eq!(farm.worker().run_next().await.unwrap(), Some(Sent::Done));
        assert_eq!(farm.grams(), estimate());
        farm
    }

    async fn finish(&self, filename: &str) -> Option<pp_storage::print_grams::Measurement> {
        self.finish_at(Some(filename), std::time::SystemTime::now())
            .await
    }

    async fn finish_at(
        &self,
        filename: Option<&str>,
        at: std::time::SystemTime,
    ) -> Option<pp_storage::print_grams::Measurement> {
        crate::measured_grams::record(
            &self.owner.client(),
            &pp_gateway::printer_host::HostReads::new().unwrap(),
            &self.owner.print_grams("default"),
            "moon",
            filename,
            at,
        )
        .await
    }
}

fn measured() -> Value {
    json!([10.0, 1, "measured", 0.0025, 4000.0])
}

impl Farm {
    /// A farm whose item `q1` was sent to a Moonraker printer that now
    /// reports `a.gcode` complete with 4000 mm used.
    async fn finished_on_moonraker() -> (RoutedPrinter, Self) {
        let stats = br#"{"result":{"status":{"print_stats":{"state":"complete","filename":"a.gcode","filament_used":4000.0}}}}"#;
        let printer = RoutedPrinter::start(vec![
            (UPLOAD.into(), 201, b"{}".to_vec()),
            (PRINT_STATS.into(), 200, stats.to_vec()),
        ])
        .unwrap();
        let farm = Self::sent(&printer).await;
        (printer, farm)
    }

    /// Moves the sent link to `state`, as Node's reconcile or a verify would.
    fn set_link_state(&self, state: &str) {
        self.database()
            .execute(
                "UPDATE app_settings SET value=json_set(value,'$[0].state',?1) WHERE tenant_id='default' AND key='printer.checkoff_links'",
                [state],
            )
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1165_a_finished_print_replaces_the_estimate_with_measured_grams() {
    use pp_storage::print_grams::Measurement;
    let (printer, farm) = Farm::finished_on_moonraker().await;
    assert_eq!(farm.finish("other.gcode").await, None, "no link prints it");
    assert_eq!(farm.grams(), estimate());
    let before_send = std::time::SystemTime::UNIX_EPOCH;
    assert_eq!(
        farm.finish_at(Some("a.gcode"), before_send).await,
        None,
        "a link made after the finish is a newer send"
    );
    assert_eq!(farm.grams(), estimate());
    assert_eq!(
        farm.finish("  gcodes\\A.GCODE ").await,
        Some(Measurement::Recorded),
        "matched like Node: trimmed base name, any case"
    );
    assert_eq!(farm.grams(), measured());
    assert_eq!(
        farm.finish("a.gcode").await,
        Some(Measurement::AlreadyMeasured),
        "a reprint of the same file name never replaces the measurement"
    );
    assert_eq!(farm.grams(), measured());
    let reads = printer
        .request_lines()
        .iter()
        .filter(|line| line.contains(PRINT_STATS))
        .count();
    assert_eq!(reads, 4, "one read per finish, never retried");
    let link = &farm.links()[0];
    assert_eq!(link["state"], "watching", "Checkoff is untouched");
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1165_a_link_reconciled_or_verified_before_the_write_is_still_measured() {
    use pp_storage::print_grams::Measurement;
    let (_printer, farm) = Farm::finished_on_moonraker().await;
    farm.set_link_state("dismissed");
    assert_eq!(farm.finish("a.gcode").await, None, "a dismissed link");
    assert_eq!(farm.grams(), estimate());
    for state in ["awaiting_verify", "verified"] {
        let (_printer, farm) = Farm::finished_on_moonraker().await;
        farm.set_link_state(state);
        let before_send = std::time::SystemTime::UNIX_EPOCH;
        assert_eq!(
            farm.finish_at(Some("a.gcode"), before_send).await,
            None,
            "{state}: a link created after the finish never takes its length"
        );
        assert_eq!(farm.grams(), estimate(), "{state}");
        let recorded = farm.finish("a.gcode").await;
        assert_eq!(recorded, Some(Measurement::Recorded), "{state}");
        assert_eq!(farm.grams(), measured(), "{state}");
        assert_eq!(farm.links()[0]["state"], state, "Checkoff is untouched");
        let again = farm.finish("a.gcode").await;
        assert_eq!(again, Some(Measurement::AlreadyMeasured), "{state}");
    }
}

/// Two sends of `a.gcode`: an older `verified` L1 stored after a newer
/// `watching` L2, as a save leaves terminal links after active ones.
async fn two_sends_of_one_file(printer: &RoutedPrinter) -> (Farm, String, String) {
    let farm = Farm::open(printer.url(), &[("q1", false, true), ("q2", false, true)]);
    for id in ["q1", "q2"] {
        farm.send(id);
        assert_eq!(farm.worker().run_next().await.unwrap(), Some(Sent::Done));
    }
    let mut links = farm.links();
    let list = links.as_array_mut().unwrap();
    let id = |link: &Value| link["id"].as_str().unwrap().to_owned();
    let (l1, l2) = (id(&list[0]), id(&list[1]));
    list[0]["state"] = json!("verified");
    list[0]["created_at"] = json!("2026-11-01T00:00:00.000Z");
    list[1]["created_at"] = json!("2026-11-02T00:00:00.000Z");
    list.reverse();
    farm.database()
        .execute(
            "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='printer.checkoff_links'",
            [links.to_string()],
        )
        .unwrap();
    (farm, l1, l2)
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1165_the_newest_send_of_a_file_takes_the_length_whatever_the_list_order() {
    use pp_storage::print_grams::Measurement;
    let stats = br#"{"result":{"status":{"print_stats":{"state":"complete","filename":"a.gcode","filament_used":4000.0}}}}"#;
    let routes = || {
        vec![
            (UPLOAD.to_owned(), 201, b"{}".to_vec()),
            (PRINT_STATS.to_owned(), 200, stats.to_vec()),
        ]
    };
    for l1_measured in [true, false] {
        let printer = RoutedPrinter::start(routes()).unwrap();
        let (farm, l1, l2) = two_sends_of_one_file(&printer).await;
        if l1_measured {
            farm.database()
                .execute(
                    "UPDATE print_link_grams SET grams=9.0,basis='measured',measured_mm=3600.0 WHERE link_id=?1",
                    [&l1],
                )
                .unwrap();
        }
        let l1_before = farm.grams_of(&l1);
        let before_l2 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_793_534_400);
        assert_eq!(
            farm.finish_at(Some("a.gcode"), before_l2).await,
            Some(if l1_measured {
                Measurement::AlreadyMeasured
            } else {
                Measurement::Recorded
            }),
            "only L1 existed at a finish on 2026-11-01T12:00Z"
        );
        let l1_after_old_finish = farm.grams_of(&l1);
        if l1_measured {
            assert_eq!(l1_after_old_finish, l1_before);
        }
        assert_eq!(
            farm.grams_of(&l2),
            estimate(),
            "L2 was created after that finish"
        );
        if !l1_measured {
            farm.database()
                .execute(
                    "UPDATE print_link_grams SET grams=12.5,basis='estimated',measured_mm=NULL WHERE link_id=?1",
                    [&l1],
                )
                .unwrap();
        }
        assert_eq!(
            farm.finish("a.gcode").await,
            Some(Measurement::Recorded),
            "L1 measured: {l1_measured}"
        );
        assert_eq!(farm.grams_of(&l2), measured(), "L1 measured: {l1_measured}");
        let l1_expected = if l1_measured { l1_before } else { estimate() };
        assert_eq!(farm.grams_of(&l1), l1_expected, "L1 is unchanged");
        assert_eq!(
            farm.links()[0]["id"],
            l2.as_str(),
            "L2 is still listed first"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1165_a_reprint_after_the_newest_send_is_measured_never_fills_an_older_send() {
    use pp_storage::print_grams::Measurement;
    let (printer, _) = Farm::finished_on_moonraker().await;
    let (farm, l1, l2) = two_sends_of_one_file(&printer).await;
    // L1 was cancelled at the printer, so it stays `watching` with its estimate.
    let mut links = farm.links();
    for link in links.as_array_mut().unwrap() {
        if link["id"] == l1.as_str() {
            link["state"] = json!("watching");
        }
    }
    farm.database()
        .execute(
            "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='printer.checkoff_links'",
            [links.to_string()],
        )
        .unwrap();
    assert_eq!(farm.finish("a.gcode").await, Some(Measurement::Recorded));
    assert_eq!(farm.grams_of(&l2), measured(), "L2's own finish");
    assert_eq!(
        farm.finish("a.gcode").await,
        Some(Measurement::AlreadyMeasured),
        "a reprint from the printer's history"
    );
    assert_eq!(farm.grams_of(&l1), estimate(), "L1 keeps its estimate");
    assert_eq!(farm.grams_of(&l2), measured(), "L2 keeps its measurement");
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1165_no_finished_length_for_the_file_keeps_the_estimate() {
    let stats = |state: &str, filename: &str, used: f64| {
        format!(
            r#"{{"result":{{"status":{{"print_stats":{{"state":"{state}","filename":"{filename}","filament_used":{used}}}}}}}}}"#
        )
    };
    for (status, body, case) in [
        (500, String::from("{}"), "read failed"),
        (
            200,
            stats("printing", "b.gcode", 50.0),
            "the next print started",
        ),
        (
            200,
            stats("complete", "b.gcode", 50.0),
            "another file finished",
        ),
        (200, stats("complete", "a.gcode", 0.0), "no length"),
    ] {
        let printer = RoutedPrinter::start(vec![
            (UPLOAD.into(), 201, b"{}".to_vec()),
            (PRINT_STATS.into(), status, body.into_bytes()),
        ])
        .unwrap();
        let farm = Farm::sent(&printer).await;
        assert_eq!(farm.finish("a.gcode").await, None, "{case}");
        assert_eq!(farm.grams(), estimate(), "{case}");
    }
}

impl Farm {
    /// Re-points host `moon` at PrusaLink on the same loopback printer.
    fn as_prusalink(&self) {
        self.database()
            .execute(
                "UPDATE app_settings SET value=json_set(value,'$[0].type','prusalink','$[0].config.username','maker','$[0].config.password','not-a-real-secret') WHERE key='integrations'",
                [],
            )
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1165_a_prusalink_finish_without_a_status_file_name_records_from_the_finished_job() {
    use pp_storage::print_grams::Measurement;
    let job = |state: &str| {
        let body = json!({"state": state, "file": {"display_name": "A.GCODE"}, "consumed_material": 4000.0});
        ("/api/v1/job".to_owned(), 200, body.to_string().into_bytes())
    };
    let printer =
        RoutedPrinter::start(vec![(UPLOAD.into(), 201, b"{}".to_vec()), job("PRINTING")]).unwrap();
    let farm = Farm::sent(&printer).await;
    farm.as_prusalink();
    let now = std::time::SystemTime::now();
    assert_eq!(
        farm.finish_at(None, now).await,
        None,
        "a job that is not FINISHED is the next print"
    );
    assert_eq!(farm.grams(), estimate());

    let printer =
        RoutedPrinter::start(vec![(UPLOAD.into(), 201, b"{}".to_vec()), job("FINISHED")]).unwrap();
    let farm = Farm::sent(&printer).await;
    farm.as_prusalink();
    assert_eq!(
        farm.finish_at(Some("b.gcode"), std::time::SystemTime::now())
            .await,
        None,
        "a status that names another file keeps the estimate"
    );
    assert_eq!(farm.grams(), estimate());
    assert_eq!(
        farm.finish_at(None, std::time::SystemTime::now()).await,
        Some(Measurement::Recorded),
        "the finished job names the file when the status did not"
    );
    assert_eq!(farm.grams(), measured());
    assert_eq!(
        farm.finish_at(None, std::time::SystemTime::now()).await,
        Some(Measurement::AlreadyMeasured)
    );
    let reads = printer
        .request_lines()
        .iter()
        .filter(|line| line.contains("/api/v1/job"))
        .count();
    assert_eq!(reads, 3, "one read per finish, never retried");
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_26_a_claim_that_meets_shutdown_never_contacts_the_printer() {
    let printer = printer(&[(UPLOAD, 201)]);
    let farm = Farm::open(printer.url(), &[("q1", false, true)]);
    farm.send("q1");
    let worker = farm.worker();
    let claimed = worker.worker.claim().unwrap().unwrap();
    let sent = worker.run(claimed, &AtomicBool::new(true)).await.unwrap();
    assert_eq!(sent, Sent::Failed(STOPPING.into()));
    assert!(printer.request_lines().is_empty());
    let (state, error, job, _) = farm.outcome("q1");
    assert_eq!(
        (state.as_str(), error.as_deref(), job.as_str()),
        ("error", Some(STOPPING), "failed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_26_a_host_that_is_not_read_is_refused_without_contacting_the_printer() {
    for kind in ["bambu", "octoprint", "unknown-host"] {
        let printer = printer(&[(UPLOAD, 201)]);
        let farm = Farm::open(printer.url(), &[("q1", false, true)]);
        farm.database()
            .execute(
                "UPDATE app_settings SET value=json_set(value,'$[0].type',?1) WHERE key='integrations'",
                [kind],
            )
            .unwrap();
        farm.send("q1");
        let expected = format!("Upload is not supported for {kind}");
        let sent = farm.worker().run_next().await.unwrap();
        assert_eq!(sent, Some(Sent::Failed(expected.clone())), "{kind}");
        assert!(printer.request_lines().is_empty(), "{kind}");
        let (state, error, job, _) = farm.outcome("q1");
        assert_eq!(
            (state.as_str(), error.as_deref(), job.as_str()),
            ("error", Some(expected.as_str()), "failed"),
            "{kind}"
        );
    }
}

#[test]
fn ticket_t_26_receipt_ids_fit_128_bytes_on_a_character_boundary() {
    let name = format!("{}.gcode", "日".repeat(50));
    let id = receipt_id(&name);
    assert!(id.len() <= 128 && name.starts_with(&id), "{id}");
    assert_eq!(id.chars().count(), 42);
    assert_eq!(receipt_id("a.gcode"), "a.gcode");
}

#[tokio::test(flavor = "multi_thread")]
async fn add_to_queue_takes_only_a_read_host_and_stages_inside_printer_uploads() {
    let farm = Farm::open("http://127.0.0.1:9", &[]);
    let settings = farm.owner.client();
    let host = |printer: &str| {
        let settings = settings.clone();
        let printer = printer.to_owned();
        std::thread::spawn(move || queue_host(&settings, &printer))
            .join()
            .unwrap()
    };
    assert_eq!(host("p-moon"), Ok("Micron host".into()));
    assert_eq!(
        host("p-gone"),
        Err((StatusCode::NOT_FOUND, "Fleet printer not found".into()))
    );
    farm.database()
        .execute(
            "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='printer.fleet'",
            [json!([
                {"id": "p-moon", "integration_id": "moon"},
                {"id": "p-bambu", "integration_id": "bam"},
                {"id": "p-odd", "integration_id": "odd"},
                {"id": "p-loose"}
            ])
            .to_string()],
        )
        .unwrap();
    farm.database()
        .execute(
            "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='integrations'",
            [json!([
                {"id": "moon", "name": "Micron host", "type": "moonraker", "config": {"base_url": "http://127.0.0.1:9"}},
                {"id": "bam", "name": "X1", "type": "bambu", "config": {}},
                {"id": "odd", "name": "Odd", "type": "octoprint", "config": {}}
            ])
            .to_string()],
        )
        .unwrap();
    for (printer, detail) in [
        ("p-bambu", "Queue is not supported for bambu"),
        ("p-odd", "Queue is not supported for octoprint"),
        (
            "p-loose",
            "Printer is not linked to a host. Link a Moonraker or PrusaLink host in Settings.",
        ),
    ] {
        assert_eq!(
            host(printer),
            Err((StatusCode::BAD_REQUEST, detail.into())),
            "{printer}"
        );
    }

    let path = PathBuf::from(stage(&farm.exports(), "Plate 1.gcode", b"G28\n").unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"G28\n");
    assert_eq!(
        staged(&farm.exports(), &path),
        Ok(path.canonicalize().unwrap())
    );
    for hostile in ["../x.gcode", "a/b.gcode", "..", "/x.gcode", ""] {
        assert!(stage(&farm.exports(), hostile, b"").is_err(), "{hostile}");
    }
    discard(&farm.exports(), &path.to_string_lossy());
    assert!(
        !path.parent().unwrap().exists(),
        "Remove deletes the staged directory"
    );
}

/// Under the shared worker admission, `run_next` claims printer uploads only:
/// a cancelled Source import waits queued for its owner's cleanup claim
/// (GRE-428).
#[tokio::test]
async fn run_next_never_claims_a_source_import() {
    use crate::uploads::{SourceImports, Through};
    use pp_storage::{
        catalog::CreateSource,
        jobs::{Credential as JobCredential, UserOperation},
        uploads::{Admission, Input, State, Target},
    };
    let farm = Farm::open("http://127.0.0.1:9", &[]);
    let supplied = farm.root.path().join("supplied");
    std::fs::create_dir(&supplied).unwrap();
    std::fs::write(supplied.join("part.stl"), b"solid part\nendsolid part\n").unwrap();
    let session = || JobCredential::Session(Secret::new(farm.token.clone()));
    let imports = SourceImports::new(&farm.owner, desktop_policy(), 64 * 1024 * 1024).unwrap();
    let admitted = imports
        .admit(
            session(),
            Admission {
                input: Input::Files {
                    paths: vec!["part.stl".into()],
                },
                max_input_bytes: 1024,
                max_prepared_bytes: 2048,
                reserved_bytes: 20 * 1024 * 1024,
                key: "send-worker-kind".into(),
                target: Target::Create {
                    metadata: Box::new(CreateSource {
                        source_type: Some("local".into()),
                        source_kind: Some("local".into()),
                        name: "Kind check".into(),
                        ..Default::default()
                    }),
                },
            },
            &supplied,
            &AtomicBool::new(false),
        )
        .unwrap();
    let owned = imports
        .work_operation(
            &admitted.job_id,
            Some(&supplied),
            Through::OwnedInput,
            &AtomicBool::new(false),
        )
        .unwrap()
        .unwrap();
    assert_eq!(owned.state, State::OwnedInputReady);
    // Cancelling requeues the import past admission for its owner.
    farm.owner
        .jobs(desktop_policy())
        .unwrap()
        .submit(
            session(),
            UserOperation::Cancel {
                job_id: admitted.job_id.clone(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap();

    assert_eq!(farm.worker().run_next().await.unwrap(), None);
    let state: String = farm
        .database()
        .query_row(
            "SELECT state FROM durable_jobs WHERE id=?1",
            [&admitted.job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "queued");
}
