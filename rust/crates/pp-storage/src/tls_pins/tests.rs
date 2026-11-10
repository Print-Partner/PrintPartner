use super::*;
use crate::{Limits, WriterOwner};

const WAIT: Duration = Duration::from_secs(5);
static IDLE: AtomicBool = AtomicBool::new(false);

fn opened() -> (tempfile::TempDir, WriterOwner) {
    let directory = tempfile::tempdir().unwrap();
    let (owner, _) = WriterOwner::open(directory.path(), Limits::default()).unwrap();
    (directory, owner)
}

fn written(reply: Result<WriteReply>) -> bool {
    reply.unwrap().recv().unwrap().unwrap()
}

fn pin(host: &str, byte: u8) -> Pin {
    Pin {
        host: host.into(),
        port: 8883,
        sha256: [byte; 32],
        pinned_at: String::new(),
    }
}

#[test]
fn ticket_t_23_a_pin_is_written_once_and_only_a_reset_removes_it() {
    let (_directory, owner) = opened();
    let client = owner.client();
    assert_eq!(read(&client, WAIT, "bambu-1").unwrap(), None);
    assert!(written(client.pin_tls(
        "bambu-1",
        pin("192.168.1.20", 1),
        &IDLE,
        WAIT
    )));
    assert!(
        !written(client.pin_tls("bambu-1", pin("192.168.1.20", 2), &IDLE, WAIT)),
        "the first pin stays"
    );
    let stored = read(&client, WAIT, "bambu-1").unwrap().unwrap();
    assert_eq!(
        (stored.host.as_str(), stored.port, stored.sha256),
        ("192.168.1.20", 8883, [1; 32])
    );
    assert!(
        stored.pinned_at.ends_with('Z') && stored.pinned_at.len() == 24,
        "{}",
        stored.pinned_at
    );
    assert_eq!(read(&client, WAIT, "bambu-2").unwrap(), None);
    assert!(written(client.reset_tls_pin("bambu-1", &IDLE, WAIT)));
    assert!(!written(client.reset_tls_pin("bambu-1", &IDLE, WAIT)));
    assert_eq!(read(&client, WAIT, "bambu-1").unwrap(), None);
    assert!(written(client.pin_tls(
        "bambu-1",
        pin("192.168.1.21", 3),
        &IDLE,
        WAIT
    )));
    assert_eq!(
        read(&client, WAIT, "bambu-1").unwrap().unwrap().sha256,
        [3; 32]
    );
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_25_watched_bambu_rows_carry_their_pins() {
    let (directory, owner) = opened();
    let client = owner.client();
    assert!(written(client.pin_tls(
        "x1c",
        pin("192.168.1.20", 1),
        &IDLE,
        WAIT
    )));
    let stored = serde_json::json!([
        {"id": "moon", "type": "moonraker", "config": {"base_url": "http://m.test"}},
        {"id": "moon", "type": "bambu", "config": {"host": "192.168.1.9"}},
        {"id": "x1c", "type": "bambu", "config": {"host": "192.168.1.20", "serial": "FIXTURE01"}},
        {"id": "p1s", "type": "bambu", "config": {"ip": " 192.168.1.21 "}},
        {"id": "blank", "type": "bambu", "config": {"host": "  "}},
        {"id": "off", "type": "bambu", "config": {"hostname": "a1.lan", "enabled": false}},
    ])
    .to_string();
    rusqlite::Connection::open(directory.path().join("print-partner.db"))
        .unwrap()
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','integrations',?1)",
            [&stored],
        )
        .unwrap();

    let hosts = crate::printer_hosts::read(&client, WAIT).unwrap().unwrap();
    let rows: Vec<_> = hosts
        .bambu
        .iter()
        .map(|row| {
            (
                row.id.as_str(),
                row.address(),
                row.enabled,
                row.pin
                    .as_ref()
                    .map(|pin| pin.as_ref().map(|pin| pin.sha256[0]).ok()),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("x1c", Some("192.168.1.20"), true, Some(Some(1))),
            ("p1s", Some("192.168.1.21"), true, None),
            ("blank", None, true, None),
            ("off", Some("a1.lan"), false, None),
        ],
        "a Bambu row on a Moonraker id is skipped, and each row carries its own pin"
    );
    assert_eq!(
        crate::printer_hosts::polled_ids(Some(&stored)).unwrap(),
        ["moon", "x1c", "p1s", "off"],
        "a Bambu row without an address is never watched"
    );
    assert_eq!(hosts.bambu_feature, Some(false), "Bambu is off by default");

    // The read carries the tenant's Bambu feature with the rows.
    let database = rusqlite::Connection::open(directory.path().join("print-partner.db")).unwrap();
    let feature = |toggles: &str| {
        database
            .execute(
                "INSERT OR REPLACE INTO app_settings(tenant_id,key,value) VALUES('default','feature_toggles',?1)",
                [toggles],
            )
            .unwrap();
        crate::printer_hosts::read(&client, WAIT)
            .unwrap()
            .unwrap()
            .bambu_feature
    };
    assert_eq!(feature(r#"{"bambu":true}"#), Some(true));
    assert_eq!(feature("not json"), None, "invalid toggles");

    // One unreadable pin refuses only its own row; the rest still load.
    database
        .execute_batch(
            "PRAGMA ignore_check_constraints=ON;
             INSERT INTO printer_tls_pins(tenant_id,integration_id,host,port,sha256,pinned_at)
             VALUES('default','p1s','192.168.1.21',8883,x'0102','2026-11-10T00:00:00.000Z');",
        )
        .unwrap();
    let hosts = crate::printer_hosts::read(&client, WAIT).unwrap().unwrap();
    let pins: Vec<_> = hosts
        .bambu
        .iter()
        .map(|row| (row.id.as_str(), row.pin.as_ref().map(Result::is_ok)))
        .collect();
    assert_eq!(
        pins,
        [
            ("x1c", Some(true)),
            ("p1s", Some(false)),
            ("blank", None),
            ("off", None),
        ]
    );
    owner.shutdown().unwrap();
}
