use super::*;

#[test]
fn ticket_t_27_mode_defaults_off_persists_and_is_tenant_scoped() {
    let queue = Harness::open();
    assert_eq!(
        queue.apply(Request::UploadOnly),
        Reply::Item(json!({"enabled": false}))
    );
    queue.apply(Request::SetUploadOnly { enabled: true });
    let AuthOutcome::Session { token: other, .. } = queue.auth(AuthRequest::Register {
        email: "other@example.com".into(),
        display_name: "Other".into(),
        password: Secret::new("ordinary-password".into()),
    }) else {
        panic!("session expected")
    };
    assert_eq!(
        queue.apply_as(other.expose(), Request::UploadOnly),
        Reply::Item(json!({"enabled": false}))
    );
    let queue = queue.reopen();
    assert_eq!(
        queue.apply(Request::UploadOnly),
        Reply::Item(json!({"enabled": true}))
    );
    assert!(
        queue.worker().claim().unwrap().is_none(),
        "enabling never dispatches"
    );
    queue.apply(Request::SetUploadOnly { enabled: false });
    let queue = queue.reopen();
    assert_eq!(
        queue.apply(Request::UploadOnly),
        Reply::Item(json!({"enabled": false}))
    );
}

#[test]
fn ticket_t_27_dispatch_freezes_upload_only_and_cannot_admit_start() {
    let queue = Harness::open();
    queue.seed(&[("q1", "p-a", "queued", None)]);
    queue
        .database()
        .execute("UPDATE printer_send_queue SET start=1", [])
        .unwrap();
    queue.apply(Request::SetUploadOnly { enabled: true });
    let id = queue.dispatch("q1");
    assert_eq!(queue.job(&id).1["payload"]["payload"]["start"], false);
    let Reply::Item(item) = queue.apply(Request::Get { id: "q1".into() }) else {
        panic!()
    };
    assert_eq!(item["start"], false);
    queue.apply(Request::SetUploadOnly { enabled: false });
    let worker = queue.worker();
    let mut attempt = claim(&worker);
    let mut start = intent("p-a");
    start.operation = EffectOperation::PrinterUploadAndStart;
    assert!(
        worker
            .update(&mut attempt.lease, WorkerOperation::BeginEffect(start))
            .is_err()
    );
    worker
        .update(
            &mut attempt.lease,
            WorkerOperation::BeginEffect(intent("p-a")),
        )
        .unwrap();
    worker
        .update(
            &mut attempt.lease,
            WorkerOperation::ConfirmEffect(ResultArtifact {
                receipt_id: "only-uploaded".into(),
                content_hash: "b".repeat(64),
                target: "p-a".into(),
            }),
        )
        .unwrap();
    let mut start = intent("p-a");
    start.operation = EffectOperation::PrinterStart;
    assert!(
        worker
            .update(&mut attempt.lease, WorkerOperation::BeginEffect(start))
            .is_err()
    );
}

#[test]
fn ticket_t_27_restart_preserves_mode_but_never_dispatches_a_pending_send() {
    let queue = Harness::open();
    queue.seed(&[
        ("waiting", "p-a", "queued", None),
        ("dispatched", "p-b", "queued", None),
    ]);
    queue.apply(Request::SetUploadOnly { enabled: true });
    queue.dispatch("dispatched");
    let queue = queue.reopen();
    assert_eq!(queue.item("waiting").0, "queued");
    assert_eq!(queue.item("dispatched").0, "error");
    assert_eq!(
        queue.apply(Request::UploadOnly),
        Reply::Item(json!({"enabled": true}))
    );
    assert!(queue.worker().claim().unwrap().is_none());
}

#[test]
fn ticket_t_27_invalid_persisted_mode_refuses_dispatch_without_a_job() {
    let queue = Harness::open();
    queue.seed(&[("q1", "p-a", "queued", None)]);
    queue
        .database()
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,'not-a-boolean')",
            params![queue.tenant, crate::send_queue::UPLOAD_ONLY_KEY],
        )
        .unwrap();
    let reply = queue.client.apply(
        Credential::Session(Secret::new(queue.token.clone())),
        Request::Dispatch {
            id: "q1".into(),
            host_name: "Moon".into(),
        },
        &AtomicBool::new(false),
        Duration::from_secs(5),
    );
    assert!(reply.is_err());
    assert_eq!(queue.item("q1").0, "queued");
    assert!(queue.worker().claim().unwrap().is_none());
}
