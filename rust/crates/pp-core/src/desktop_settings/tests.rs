use super::*;
use crate::tests::platform;
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
    update(&owner, set("/srv/exchange")).unwrap();
    update(
        &owner,
        DesktopSetting::SlicerOrcaDir(Some("/home/a/.config/OrcaSlicer".to_owned())),
    )
    .unwrap();
    let settings = update(
        &owner,
        DesktopSetting::DockerSocket(Some("/run/docker.sock".to_owned())),
    )
    .unwrap();
    assert_eq!(read(&owner).unwrap(), settings);
    assert_eq!(
        settings.env(),
        [
            ("PP_EXCHANGE_DIR", "/srv/exchange".to_owned()),
            ("DOCKER_SOCKET", "/run/docker.sock".to_owned()),
            ("SLICER_ORCA_DIR", "/home/a/.config/OrcaSlicer".to_owned()),
        ]
    );
    let cleared = update(&owner, DesktopSetting::DockerSocket(None)).unwrap();
    assert_eq!(cleared.docker_socket, None);
    assert_eq!(cleared.exchange_dir.as_deref(), Some("/srv/exchange"));
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
    update(&owner, set("/srv/exchange")).unwrap();
    let before = std::fs::read(fixture.0.join(FILE)).unwrap();
    for value in [
        "",
        "relative/dir",
        "/srv/../etc",
        "/srv/exchange\n",
        "/srv/\u{0}x",
        &format!("/{}", "a".repeat(MAX_VALUE_BYTES)),
    ] {
        assert!(
            update(&owner, set(value)).is_err(),
            "{value:?} was accepted"
        );
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
        assert!(update(&owner, set("/srv/exchange")).is_err());
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
