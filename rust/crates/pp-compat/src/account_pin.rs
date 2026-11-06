use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

#[derive(Debug, PartialEq, Eq)]
pub enum DesktopAccountPin {
    Disabled,
    Missing,
    Present { sha256: [u8; 32] },
}

impl DesktopAccountPin {
    pub fn capture(data_dir: &Path, enabled: bool) -> Result<Self> {
        if !cfg!(feature = "desktop_rebuild") || !enabled {
            return Ok(Self::Disabled);
        }
        let file = match crate::platform::open_nofollow(&data_dir.join("desktop-account.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::Missing),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        ensure!(
            crate::platform::private_file(&file)? && metadata.len() <= 4096,
            "Desktop account mapping must be a private regular file owned by the current user"
        );
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 4096, "Desktop account mapping is too large");
        Ok(Self::Present {
            sha256: Sha256::digest(&bytes).into(),
        })
    }

    pub(crate) fn apply(&self, setup: &mut serde_json::Value) {
        if !cfg!(feature = "desktop_rebuild") {
            return;
        }
        let pin = match self {
            Self::Disabled => return,
            Self::Missing => serde_json::json!({"kind":"missing"}),
            Self::Present { sha256 } => {
                serde_json::json!({"kind":"present","sha256":hex::encode(sha256)})
            }
        };
        setup["account_mode"] = serde_json::json!({"kind":"existing-account-v1","pin":pin});
    }
}

// GRE-1187 ports these tests to Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[cfg(feature = "desktop_rebuild")]
    use std::os::unix::fs::PermissionsExt;
    use std::{fs, os::unix::fs::symlink};

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("pp-account-pin-{:032x}", rand::random::<u128>()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        #[cfg(feature = "desktop_rebuild")]
        fn write(&self, bytes: &[u8]) {
            let path = self.0.join("desktop-account.json");
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn disabled_does_not_read_or_change_setup() {
        let fixture = Fixture::new();
        symlink("absent", fixture.0.join("desktop-account.json")).unwrap();
        let pin = DesktopAccountPin::capture(&fixture.0, false).unwrap();
        assert_eq!(pin, DesktopAccountPin::Disabled);
        let mut setup = serde_json::json!({"version":1});
        let bytes = setup.to_string();
        pin.apply(&mut setup);
        assert_eq!(setup.to_string(), bytes);
        #[cfg(feature = "desktop_rebuild")]
        assert!(DesktopAccountPin::capture(&fixture.0, true).is_err());
    }

    #[cfg(not(feature = "desktop_rebuild"))]
    #[test]
    fn compile_gate_off_does_not_read_or_enable_mapping() {
        let fixture = Fixture::new();
        symlink("absent", fixture.0.join("desktop-account.json")).unwrap();
        assert_eq!(
            DesktopAccountPin::capture(&fixture.0, true).unwrap(),
            DesktopAccountPin::Disabled
        );
        let mut setup = serde_json::json!({"version":1});
        let bytes = setup.to_string();
        for pin in [
            DesktopAccountPin::Missing,
            DesktopAccountPin::Present { sha256: [0; 32] },
        ] {
            pin.apply(&mut setup);
            assert_eq!(setup.to_string(), bytes);
        }
    }

    #[cfg(feature = "desktop_rebuild")]
    #[test]
    fn missing_and_present_pins_remain_immutable() {
        let fixture = Fixture::new();
        let missing = DesktopAccountPin::capture(&fixture.0, true).unwrap();
        fixture.write(b"{}");
        let present = DesktopAccountPin::capture(&fixture.0, true).unwrap();
        assert_eq!(
            present,
            DesktopAccountPin::Present {
                sha256: Sha256::digest(b"{}").into()
            }
        );
        fixture.write(b"changed");
        assert_ne!(
            DesktopAccountPin::capture(&fixture.0, true).unwrap(),
            present
        );
        fs::remove_file(fixture.0.join("desktop-account.json")).unwrap();
        let mut setup = serde_json::json!({});
        missing.apply(&mut setup);
        assert_eq!(
            setup["account_mode"],
            serde_json::json!({"kind":"existing-account-v1","pin":{"kind":"missing"}})
        );
        present.apply(&mut setup);
        assert_eq!(
            setup["account_mode"]["pin"]["sha256"],
            hex::encode(Sha256::digest(b"{}"))
        );
    }

    #[cfg(feature = "desktop_rebuild")]
    #[test]
    fn unsafe_files_fail_without_blocking() {
        let fixture = Fixture::new();
        let path = fixture.0.join("desktop-account.json");
        fixture.write(b"{}");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(DesktopAccountPin::capture(&fixture.0, true).is_err());
        fixture.write(&vec![0; 4097]);
        assert!(DesktopAccountPin::capture(&fixture.0, true).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(DesktopAccountPin::capture(&fixture.0, true).is_err());
        fs::remove_dir(&path).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(DesktopAccountPin::capture(&fixture.0, true).is_err());
    }
}
