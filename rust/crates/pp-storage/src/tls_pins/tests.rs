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
