//! GRE-1480: the generic setting write refuses `integrations` and
//! `printer.fleet`, whose typed shape printer setup owns, and leaves the
//! stored row as it was. The test checks the failure and the stored row,
//! never the message text.

use pp_storage::{Limits, Setting, SettingCommand, SettingSnapshot, SettingsClient, WriterOwner};
use std::{path::Path, sync::atomic::AtomicBool, time::Duration};

const WAIT: Duration = Duration::from_secs(5);
const STORED: &str = r#"[{"id":"kept","name":"Kept"}]"#;

fn snapshot(storage: &SettingsClient, key: &str) -> SettingSnapshot {
    let reader = storage.reader(WAIT).unwrap();
    reader.snapshot("default", key).unwrap()
}

/// `Set`, and a `CompareAndSet` whose expectation matches the stored row, so
/// only the refusal can stop it.
fn commands(storage: &SettingsClient, key: &str) -> [SettingCommand; 2] {
    let setting = Setting {
        tenant: "default".into(),
        key: key.into(),
        value: "[]".into(),
    };
    [
        SettingCommand::Set(setting.clone()),
        SettingCommand::CompareAndSet {
            setting,
            expected: snapshot(storage, key),
        },
    ]
}

fn submit(storage: &SettingsClient, command: SettingCommand) -> anyhow::Result<bool> {
    storage
        .submit(command, &AtomicBool::new(false), WAIT)?
        .recv()?
}

#[test]
fn the_generic_write_refuses_integrations_and_the_printer_fleet() {
    let directory = pp_testkit::private_temp_dir(None).unwrap();
    let data: &Path = directory.path();
    let (owner, _) = WriterOwner::open(data, Limits::default()).unwrap();
    let storage = owner.client();
    for key in ["integrations", "printer.fleet"] {
        // With no stored row, then with one: neither command changes it.
        for seed in [None, Some(STORED)] {
            if let Some(value) = seed {
                pp_testkit::settings::seed_setting(data, key, value).unwrap();
            }
            let before = snapshot(&storage, key);
            for command in commands(&storage, key) {
                assert!(submit(&storage, command).is_err(), "{key} was written");
            }
            assert_eq!(snapshot(&storage, key), before, "{key}");
        }
        let kept = SettingSnapshot::Stored {
            value: STORED.into(),
        };
        assert_eq!(snapshot(&storage, key), kept);
    }

    // The same two commands still write a key no module owns.
    let [set, _] = commands(&storage, "theme");
    assert!(submit(&storage, set).unwrap());
    let [_, compare_and_set] = commands(&storage, "theme");
    assert!(submit(&storage, compare_and_set).unwrap());
    let written = SettingSnapshot::Stored { value: "[]".into() };
    assert_eq!(snapshot(&storage, "theme"), written);
    owner.shutdown().unwrap();
}
