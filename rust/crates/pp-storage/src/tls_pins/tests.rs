use super::*;
use crate::{Limits, WriterOwner};

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
    let directory = tempfile::tempdir().unwrap();
    let (owner, _) = WriterOwner::open(directory.path(), Limits::default()).unwrap();
    let client = owner.client();
    let wait = Duration::from_secs(5);
    let idle = AtomicBool::new(false);
    let write = |reply: Result<WriteReply>| reply.unwrap().recv().unwrap().unwrap();
    assert_eq!(read(&client, wait, "bambu-1").unwrap(), None);
    assert!(write(client.pin_tls(
        "bambu-1",
        pin("192.168.1.20", 1),
        &idle,
        wait
    )));
    assert!(
        !write(client.pin_tls("bambu-1", pin("192.168.1.20", 2), &idle, wait)),
        "the first pin stays"
    );
    let stored = read(&client, wait, "bambu-1").unwrap().unwrap();
    assert_eq!(
        (stored.host.as_str(), stored.port, stored.sha256),
        ("192.168.1.20", 8883, [1; 32])
    );
    assert!(
        stored.pinned_at.ends_with('Z') && stored.pinned_at.len() == 24,
        "{}",
        stored.pinned_at
    );
    assert_eq!(read(&client, wait, "bambu-2").unwrap(), None);
    assert!(write(client.reset_tls_pin("bambu-1", &idle, wait)));
    assert!(!write(client.reset_tls_pin("bambu-1", &idle, wait)));
    assert_eq!(read(&client, wait, "bambu-1").unwrap(), None);
    assert!(write(client.pin_tls(
        "bambu-1",
        pin("192.168.1.21", 3),
        &idle,
        wait
    )));
    assert_eq!(
        read(&client, wait, "bambu-1").unwrap().unwrap().sha256,
        [3; 32]
    );
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_25_watched_bambu_rows_carry_their_pins() {
    let directory = tempfile::tempdir().unwrap();
    let (owner, _) = WriterOwner::open(directory.path(), Limits::default()).unwrap();
    let client = owner.client();
    let wait = Duration::from_secs(5);
    let idle = AtomicBool::new(false);
    assert!(
        client
            .pin_tls("x1c", pin("192.168.1.20", 1), &idle, wait)
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
    );
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

    let hosts = crate::printer_hosts::read(&client, wait).unwrap().unwrap();
    let rows: Vec<_> = hosts
        .bambu
        .iter()
        .map(|row| {
            (
                row.id.as_str(),
                row.address(),
                row.enabled,
                row.pin.as_ref().map(|pin| pin.sha256[0]),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("x1c", Some("192.168.1.20"), true, Some(1)),
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
    owner.shutdown().unwrap();
}
