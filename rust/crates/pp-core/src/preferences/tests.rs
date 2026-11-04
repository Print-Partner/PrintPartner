use super::*;
use std::os::unix::fs::DirBuilderExt;
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-prefs-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

async fn serve(
    mut commands: mpsc::Receiver<Command>,
    owner: StorageLease,
    mut stop: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => break,
            command = commands.recv() => match command {
                Some(command) => command.execute(&owner),
                None => break,
            }
        }
    }
    settle(&mut commands, |command| command.execute(&owner)).await;
}

#[tokio::test]
async fn owner_commands_roundtrip_clear_restart_copy_and_preserve_product_bytes() {
    let fixture = Fixture::new();
    let database = b"unrelated database bytes";
    std::fs::write(fixture.0.join("print-partner.db"), database).unwrap();
    std::fs::write(fixture.0.join("other-settings"), b"opaque settings").unwrap();
    let owner = StorageLease::acquire(&fixture.0).unwrap();
    let (client, commands) = Client::channel();
    let second = client.clone();
    let (stop, stopped) = oneshot::channel();
    let worker = tokio::spawn(serve(commands, owner, stopped));
    assert_eq!(client.read().await.unwrap(), BrowserPreferences::default());
    let (theme, sidebar) = tokio::join!(
        client.update(BrowserPreference::Theme(Some(Theme::Light))),
        second.update(BrowserPreference::SidebarCollapsed(Some(true))),
    );
    theme.unwrap();
    sidebar.unwrap();
    client
        .update(BrowserPreference::PrinterPollInterval(Some(
            PrinterPollInterval::FifteenSeconds,
        )))
        .await
        .unwrap();
    let expected = BrowserPreferences {
        sidebar_collapsed: Some(true),
        theme: Some(Theme::Light),
        printer_poll_interval: Some(PrinterPollInterval::FifteenSeconds),
    };
    assert_eq!(second.read().await.unwrap(), expected);
    assert_eq!(
        client
            .update(BrowserPreference::Theme(Some(Theme::Light)))
            .await
            .unwrap(),
        expected
    );
    let cleared = second.update(BrowserPreference::Theme(None)).await.unwrap();
    assert_eq!(cleared.theme, None);
    assert_eq!(cleared.sidebar_collapsed, Some(true));
    assert_eq!(
        cleared.printer_poll_interval,
        expected.printer_poll_interval
    );
    assert_eq!(
        std::fs::metadata(fixture.0.join(FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    stop.send(()).unwrap();
    worker.await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), client.read())
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        client
            .update(BrowserPreference::Theme(Some(Theme::Dark)))
            .await
            .is_err()
    );
    let owner = StorageLease::acquire(&fixture.0).unwrap();
    let (client, mut commands) = Client::channel();
    let request = tokio::spawn(async move { client.read().await });
    commands.recv().await.unwrap().execute(&owner);
    assert_eq!(request.await.unwrap().unwrap(), cleared);
    drop(owner);
    let copy = Fixture::new();
    for name in [FILE, "print-partner.db", "other-settings"] {
        std::fs::copy(fixture.0.join(name), copy.0.join(name)).unwrap();
    }
    let owner = StorageLease::acquire(&copy.0).unwrap();
    let (client, mut commands) = Client::channel();
    let request = tokio::spawn(async move { client.read().await });
    commands.recv().await.unwrap().execute(&owner);
    assert_eq!(request.await.unwrap().unwrap(), cleared);
    for root in [&fixture.0, &copy.0] {
        assert_eq!(
            std::fs::read(root.join("print-partner.db")).unwrap(),
            database
        );
        assert_eq!(
            std::fs::read(root.join("other-settings")).unwrap(),
            b"opaque settings"
        );
        assert!(std::fs::read_dir(root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".new")
        }));
    }
}

#[tokio::test]
async fn shutdown_settles_queued_commands_and_closes_admission() {
    let fixture = Fixture::new();
    let owner = StorageLease::acquire(&fixture.0).unwrap();
    let (client, mut commands) = Client::channel();
    let mut results = Vec::new();
    for _ in 0..16 {
        let (reply, result) = oneshot::channel();
        client
            .0
            .send(Command::Update(
                BrowserPreference::Theme(Some(Theme::Dark)),
                reply,
            ))
            .await
            .unwrap();
        results.push(result);
    }
    let blocked = {
        let client = client.clone();
        tokio::spawn(async move { client.read().await })
    };
    tokio::task::yield_now().await;
    assert!(!blocked.is_finished());
    settle(&mut commands, |command| command.execute(&owner)).await;
    for result in results {
        assert_eq!(result.await.unwrap().unwrap().theme, Some(Theme::Dark));
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), blocked)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(client.read().await.is_err());
}

#[tokio::test]
async fn invalid_preferences_are_refused_by_owner_without_overwrite() {
    let fixture = Fixture::new();
    let owner = StorageLease::acquire(&fixture.0).unwrap();
    let (client, mut commands) = Client::channel();
    for bytes in [
        b"garbage".as_slice(),
        br#"{"version":2,"preferences":{}}"#,
        br#"{"version":1,"preferences":{"theme":"unknown"}}"#,
        br#"{"version":1,"preferences":{"printer_poll_interval":"60"}}"#,
        br#"{"version":1,"preferences":{"printer_poll_interval":60}}"#,
        br#"{"version":1,"preferences":{"credential":"secret"}}"#,
        br#"{"version":1,"preferences":{},"extra":true}"#,
        br#"{"version":1,"version":1,"preferences":{}}"#,
        &[b' '; 4097],
    ] {
        std::fs::write(fixture.0.join(FILE), bytes).unwrap();
        let request = {
            let client = client.clone();
            tokio::spawn(async move {
                client
                    .update(BrowserPreference::Theme(Some(Theme::Dark)))
                    .await
            })
        };
        commands.recv().await.unwrap().execute(&owner);
        assert!(request.await.unwrap().is_err());
        assert_eq!(std::fs::read(fixture.0.join(FILE)).unwrap(), bytes);
    }
}

#[tokio::test]
async fn unsafe_preferences_are_refused_by_owner() {
    for kind in ["link", "directory", "fifo"] {
        let fixture = Fixture::new();
        let path = fixture.0.join(FILE);
        match kind {
            "link" => {
                std::fs::write(fixture.0.join("target"), b"untouched").unwrap();
                symlink("target", &path).unwrap();
            }
            "directory" => std::fs::create_dir(&path).unwrap(),
            "fifo" => {
                let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            _ => unreachable!(),
        }
        let owner = StorageLease::acquire(&fixture.0).unwrap();
        let (client, mut commands) = Client::channel();
        let request = tokio::spawn(async move {
            client
                .update(BrowserPreference::Theme(Some(Theme::Dark)))
                .await
        });
        commands.recv().await.unwrap().execute(&owner);
        assert!(request.await.unwrap().is_err());
        if kind == "link" {
            assert_eq!(
                std::fs::read(fixture.0.join("target")).unwrap(),
                b"untouched"
            );
        }
    }
}

#[test]
fn supported_preference_values_roundtrip() {
    let fixture = Fixture::new();
    let owner = StorageLease::acquire(&fixture.0).unwrap();
    update(&owner, BrowserPreference::SidebarCollapsed(Some(false))).unwrap();
    update(&owner, BrowserPreference::Theme(Some(Theme::System))).unwrap();
    update(
        &owner,
        BrowserPreference::PrinterPollInterval(Some(PrinterPollInterval::FiveSeconds)),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.0.join(FILE)).unwrap(),
        r#"{"version":1,"preferences":{"sidebar_collapsed":false,"theme":"system","printer_poll_interval":5}}"#
    );
    for field in [
        BrowserPreference::SidebarCollapsed(None),
        BrowserPreference::Theme(None),
        BrowserPreference::PrinterPollInterval(None),
    ] {
        update(&owner, field).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(fixture.0.join(FILE)).unwrap(),
        r#"{"version":1,"preferences":{}}"#
    );
    assert_eq!(read(&owner).unwrap(), BrowserPreferences::default());

    for theme in [Theme::System, Theme::Light, Theme::Dark] {
        let json = serde_json::to_vec(&theme).unwrap();
        assert_eq!(serde_json::from_slice::<Theme>(&json).unwrap(), theme);
    }
    for interval in [
        PrinterPollInterval::FiveSeconds,
        PrinterPollInterval::TenSeconds,
        PrinterPollInterval::FifteenSeconds,
        PrinterPollInterval::ThirtySeconds,
    ] {
        let json = serde_json::to_vec(&interval).unwrap();
        assert_eq!(
            serde_json::from_slice::<PrinterPollInterval>(&json).unwrap(),
            interval
        );
    }
}

#[test]
fn failed_preference_publication_preserves_previous_fields_and_product_bytes() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("print-partner.db"), b"unchanged").unwrap();
    let owner = StorageLease::acquire(&fixture.0).unwrap();
    update(&owner, BrowserPreference::Theme(Some(Theme::System))).unwrap();
    let previous = std::fs::read(fixture.0.join(FILE)).unwrap();
    let result = update_with(
        &owner,
        BrowserPreference::SidebarCollapsed(Some(true)),
        |boundary| {
            if boundary == state_file::Boundary::Rename {
                anyhow::bail!("injected rename failure");
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(fixture.0.join(FILE)).unwrap(), previous);
    assert_eq!(
        read(&owner).unwrap(),
        BrowserPreferences {
            theme: Some(Theme::System),
            ..BrowserPreferences::default()
        }
    );
    assert_eq!(
        std::fs::read(fixture.0.join("print-partner.db")).unwrap(),
        b"unchanged"
    );
    assert!(std::fs::read_dir(&fixture.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".new")
    }));
}
