//! GRE-1296: a later Test connection never runs first-use while an earlier pin
//! write for the same integration is still queued, however long the writer
//! takes. Real storage and a loopback broker; the stalled writer is a pin
//! write whose reply has not come back yet, which is all a later Test
//! connection can observe of it.
use super::*;
use pp_storage::{Limits, Setting, SettingCommand, WriterOwner};
use pp_testkit::bambu::{Identity, idle_printer};
use std::sync::mpsc;

const ID: &str = "bambu-stalled";
const CODE: &str = "synthetic-code";
const SERIAL: &str = "FIXTURE01";

fn seed(settings: &SettingsClient, key: &str, value: Value) {
    let reply = settings
        .submit(
            SettingCommand::Set(Setting {
                tenant: "default".into(),
                key: key.into(),
                value: value.to_string(),
            }),
            &AtomicBool::new(false),
            WRITE_WAIT,
        )
        .unwrap();
    assert!(reply.recv().unwrap().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_23_a_stalled_pin_write_keeps_later_tests_off_first_use() {
    let root = tempfile::tempdir().unwrap();
    let (owner, _) =
        WriterOwner::open_with_job_feature_admission(root.path(), Limits::default()).unwrap();
    let settings = owner.client();
    // The printer the second Test connection reaches presents certificate Y.
    let (printer, _) = idle_printer(CODE, SERIAL).await.unwrap();
    let port = printer.addr.port();
    seed(&settings, "feature_toggles", json!({"bambu": true}));
    seed(
        &settings,
        "integrations",
        json!([{"id": ID, "type": "bambu", "config": {
            "host": "127.0.0.1", "port": port, "serial": SERIAL, "access_code": CODE,
        }}]),
    );
    let hosts = StoredPrinterHosts::new(settings.clone(), HostReads::new().unwrap());

    // Test A read no pin, saw certificate X and queued its pin; the writer has
    // not answered, and A's request is gone.
    let x = Identity::generate().unwrap();
    let (writer, reply) = mpsc::channel();
    let guard = PIN_LOCKS.lock(ID).await.unwrap();
    let first = under_lock(guard, move || Ok(Some(reply)), Ok);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), first)
            .await
            .is_err()
    );

    // Test B waits for A's pin instead of reading "no pin".
    let mut second = tokio::spawn(hosts.test(ID.into()));
    assert!(
        tokio::time::timeout(Duration::from_secs(2), &mut second)
            .await
            .is_err(),
        "a Test connection ran while a pin write was queued"
    );
    assert!(printer.log().is_empty(), "{:?}", printer.log());

    // The writer applies A's pin, then answers.
    let pin = Pin {
        host: "127.0.0.1".into(),
        port,
        sha256: x.fingerprint,
        pinned_at: String::new(),
    };
    let applied = settings
        .pin_tls(ID, pin, &AtomicBool::new(false), WRITE_WAIT)
        .unwrap();
    assert!(applied.recv().unwrap().unwrap());
    writer.send(Ok(true)).unwrap();

    // B reads X's pin and refuses Y before MQTT CONNECT.
    let answer = tokio::time::timeout(Duration::from_secs(20), second)
        .await
        .unwrap()
        .unwrap();
    let HostAnswer::Ok(answer) = answer else {
        panic!("Test connection answered {answer:?}")
    };
    assert_eq!(
        answer,
        json!({"ok": false, "message": pp_gateway::printer_host::BAMBU_CERTIFICATE_CHANGED})
    );
    let log = printer.log();
    assert!(
        log.iter().all(|line| !line.starts_with("CONNECT")),
        "{log:?}"
    );
    let stored = tls_pins::read(&settings, READER_WAIT, ID).unwrap().unwrap();
    assert_eq!(stored.sha256, x.fingerprint, "no other pin landed");
    drop(hosts);
    owner.shutdown().unwrap();
}
