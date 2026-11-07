use super::*;
use crate::tests::{abs, platform};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-desktop-settings-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        pp_os::create_private_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn set(value: &str) -> DesktopSetting {
    DesktopSetting::ExchangeDir(Some(value.to_owned()))
}

#[test]
fn ticket_t_33_desktop_settings_roundtrip_clear_and_reach_the_child_env() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("desktop.toml"), b"port = 45678\n").unwrap();
    let owner = StorageLease::acquire(&fixture.0).unwrap();
    assert_eq!(read(&owner).unwrap(), DesktopSettings::default());
    assert!(read(&owner).unwrap().env().is_empty());
    update(&owner, set(&abs("/srv/exchange"))).unwrap();
    update(
        &owner,
        DesktopSetting::SlicerOrcaDir(Some(abs("/home/a/.config/OrcaSlicer"))),
    )
    .unwrap();
    let settings = update(
        &owner,
        DesktopSetting::DockerSocket(Some(abs("/run/docker.sock"))),
    )
    .unwrap();
    assert_eq!(read(&owner).unwrap(), settings);
    assert_eq!(
        settings.env(),
        [
            ("PP_EXCHANGE_DIR", abs("/srv/exchange")),
            ("DOCKER_SOCKET", abs("/run/docker.sock")),
            ("SLICER_ORCA_DIR", abs("/home/a/.config/OrcaSlicer")),
        ]
    );
    let cleared = update(&owner, DesktopSetting::DockerSocket(None)).unwrap();
    assert_eq!(cleared.docker_socket, None);
    assert_eq!(
        cleared.exchange_dir.as_deref(),
        Some(abs("/srv/exchange").as_str())
    );
    assert!(
        !cleared
            .env()
            .iter()
            .any(|(name, _)| *name == "DOCKER_SOCKET")
    );
    assert_eq!(
        std::fs::read(fixture.0.join("desktop.toml")).unwrap(),
        b"port = 45678\n"
    );
    platform::assert_private_file(&fixture.0.join(FILE));
}

#[test]
fn ticket_t_33_desktop_settings_reject_unsafe_paths_without_writing() {
    let fixture = Fixture::new();
    let owner = StorageLease::acquire(&fixture.0).unwrap();
    update(&owner, set(&abs("/srv/exchange"))).unwrap();
    let before = std::fs::read(fixture.0.join(FILE)).unwrap();
    for (value, reason) in [
        (String::new(), "Desktop setting path is empty or too long"),
        (
            abs(&format!("/{}", "a".repeat(MAX_VALUE_BYTES))),
            "Desktop setting path is empty or too long",
        ),
        (
            abs("/srv/exchange\n"),
            "Desktop setting path contains a control character",
        ),
        (
            abs("/srv/\u{0}x"),
            "Desktop setting path contains a control character",
        ),
        (
            "relative/dir".to_owned(),
            "Desktop setting path must be absolute",
        ),
        (
            abs("/srv/../etc"),
            "Desktop setting path must not contain ..",
        ),
    ] {
        let error = update(&owner, set(&value)).unwrap_err();
        assert_eq!(error.to_string(), reason, "{value:?}");
        assert_eq!(std::fs::read(fixture.0.join(FILE)).unwrap(), before);
    }
}

#[test]
fn ticket_t_33_desktop_settings_refuse_bad_files_without_overwrite() {
    let fixture = Fixture::new();
    let owner = StorageLease::acquire(&fixture.0).unwrap();
    for bytes in [
        &b"not json"[..],
        br#"{"version":2,"settings":{}}"#,
        br#"{"version":1,"settings":{"session_secret":"x"}}"#,
        br#"{"version":1,"settings":{"exchange_dir":"relative"}}"#,
    ] {
        std::fs::write(fixture.0.join(FILE), bytes).unwrap();
        assert!(read(&owner).is_err());
        assert!(update(&owner, set(&abs("/srv/exchange"))).is_err());
        assert_eq!(std::fs::read(fixture.0.join(FILE)).unwrap(), bytes);
    }
}

#[test]
fn ticket_t_33_desktop_settings_cover_exactly_the_compat_desktop_settings() {
    let all = DesktopSettings {
        exchange_dir: Some("/a".to_owned()),
        docker_socket: Some("/a".to_owned()),
        compose_file: Some("/a".to_owned()),
        slicer_bambu_dir: Some("/a".to_owned()),
        slicer_orca_dir: Some("/a".to_owned()),
        slicer_prusa_dir: Some("/a".to_owned()),
    };
    let stored: std::collections::BTreeSet<_> =
        all.env().into_iter().map(|(name, _)| name).collect();
    let owned: std::collections::BTreeSet<_> = pp_compat::settings::ENVIRONMENT
        .iter()
        .filter(|(_, owner)| *owner == pp_compat::settings::Owner::DesktopSettings)
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(stored, owned);
    assert!(
        stored
            .iter()
            .all(|name| pp_compat::settings::is_desktop_setting(name))
    );
}
