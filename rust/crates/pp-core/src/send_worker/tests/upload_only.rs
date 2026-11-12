use super::*;

fn mode(farm: &Farm, enabled: bool) {
    assert_eq!(
        farm.owner
            .send_queue(desktop_policy())
            .unwrap()
            .apply(
                Credential::Session(Secret::new(farm.token.clone())),
                Request::SetUploadOnly { enabled },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap(),
        Reply::Item(json!({"enabled": enabled}))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_27_upload_only_never_starts_moonraker_or_prusalink() {
    for prusa in [false, true] {
        let upload = if prusa {
            "/api/v1/files/usb/a.gcode"
        } else {
            UPLOAD
        };
        let printer = printer(&[(upload, 201), ("/api/v1/status", 200)]);
        let farm = Farm::open(printer.url(), &[("q1", true, true)]);
        if prusa {
            farm.database()
                .execute(
                    "UPDATE app_settings SET value=?1 WHERE key='integrations'",
                    [
                        json!([{"id":"moon", "type":"prusalink", "name":"Prusa host", "config":{
                    "base_url":printer.url(), "username":"maker", "password":"recorded-only"}}])
                        .to_string(),
                    ],
                )
                .unwrap();
        }
        mode(&farm, true);
        let worker = farm.worker();
        for _ in 0..3 {
            assert_eq!(
                worker.run_next().await.unwrap(),
                None,
                "mode alone never dispatches"
            );
        }
        assert!(printer.request_lines().is_empty());
        assert!(matches!(farm.send("q1"), Reply::Dispatched { .. }));
        mode(&farm, false); // Cannot upgrade the confirmed durable upload to a print.
        assert_eq!(worker.run_next().await.unwrap(), Some(Sent::Done));
        assert_eq!(farm.outcome("q1").3["started"], false);
        let heads = printer.request_heads();
        assert_eq!(
            heads.len(),
            if prusa { 2 } else { 1 },
            "only Prusa's probe and the upload: {heads:?}"
        );
        assert!(heads.iter().all(|head| !head.contains("/print/start")));
        let method = if prusa { "PUT" } else { "POST" };
        let uploads: Vec<_> = heads
            .iter()
            .filter(|head| head.starts_with(&format!("{method} {upload} ")))
            .collect();
        assert_eq!(uploads.len(), 1, "no upload resend");
        if prusa {
            assert!(
                heads
                    .iter()
                    .any(|head| head.starts_with("GET /api/v1/status "))
            );
            assert!(uploads[0].to_lowercase().contains("print-after-upload: ?0"));
        }
        assert_eq!(worker.run_next().await.unwrap(), None);
        assert_eq!(printer.request_lines().len(), if prusa { 2 } else { 1 });
        println!(
            "UPLOAD_ONLY_RECEIPT {}",
            json!({"driver": if prusa {"prusalink"} else {"moonraker"},
            "request_lines": printer.request_lines(), "started": farm.outcome("q1").3["started"],
            "uploads": uploads.len(), "job_state": farm.outcome("q1").2})
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_27_interrupted_upload_is_not_retried_even_after_restart() {
    // A malformed response after accepting the request leaves the upload outcome unknown.
    let printer = printer(&[(UPLOAD, 0)]);
    let farm = Farm::open(printer.url(), &[("q1", true, true)]);
    mode(&farm, true);
    let worker = farm.worker();
    farm.send("q1");
    assert!(matches!(
        worker.run_next().await.unwrap(),
        Some(Sent::Unknown(_))
    ));
    assert_eq!(farm.outcome("q1").2, "reconciliation_required");
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
    let lines = printer.request_lines();
    assert_eq!(
        lines.len(),
        1,
        "interrupted upload must never resend: {lines:?}"
    );
    assert!(lines[0].starts_with(&format!("POST {UPLOAD} ")));
    println!(
        "UPLOAD_ONLY_RECEIPT {}",
        json!({"interrupted": true, "restarted": true,
        "request_lines": lines, "uploads": 1, "job_state": farm.outcome("q1").2})
    );
}

#[tokio::test(start_paused = true)]
async fn ticket_t_27_idle_worker_timers_do_not_dispatch_queued_items() {
    let printer = printer(&[(UPLOAD, 201)]);
    let farm = Farm::open(printer.url(), &[("q1", true, true)]);
    mode(&farm, true);
    let worker = farm.worker();
    let cancelled = Arc::new(AtomicBool::new(false));
    let task = worker.clone().spawn(Arc::clone(&cancelled));
    tokio::time::sleep(IDLE_RECHECK * 3).await;
    cancelled.store(true, Ordering::Release);
    worker.waker().notify_one();
    task.await.unwrap();
    assert_eq!(worker.run_next().await.unwrap(), None);
    let state: String = farm
        .database()
        .query_row(
            "SELECT state FROM printer_send_queue WHERE id='q1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "queued");
    assert!(printer.request_lines().is_empty());
    println!(
        "UPLOAD_ONLY_RECEIPT {}",
        json!({"idle_seconds": 90, "uploads": 0, "queue_state": state})
    );
}
