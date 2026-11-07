use anyhow::{Result, ensure};
use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
};

const APP_ID: &str = "com.poitee.printpartner";

#[derive(Debug, Clone, Copy)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

#[derive(Debug, Default)]
pub struct LayoutEnvironment {
    pub portable_root: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub local_appdata: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub xdg_cache_home: Option<PathBuf>,
}

impl LayoutEnvironment {
    /// Snapshot supported variables without changing process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<OsString>) -> Self {
        Self {
            portable_root: lookup("PP_PORTABLE_DIR").map(PathBuf::from),
            appdata: lookup("APPDATA").map(PathBuf::from),
            local_appdata: lookup("LOCALAPPDATA").map(PathBuf::from),
            xdg_data_home: lookup("XDG_DATA_HOME").map(PathBuf::from),
            xdg_config_home: lookup("XDG_CONFIG_HOME").map(PathBuf::from),
            xdg_state_home: lookup("XDG_STATE_HOME").map(PathBuf::from),
            xdg_cache_home: lookup("XDG_CACHE_HOME").map(PathBuf::from),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct DirectoryLayout {
    pub store: PathBuf,
    pub app_data: PathBuf,
    pub config: PathBuf,
    pub logs: PathBuf,
    pub cache: PathBuf,
}

impl DirectoryLayout {
    /// Resolve paths without filesystem access. Outside portable mode, `store`
    /// retains ~/.print-partner and `app_data` holds auxiliary state.
    /// Portable mode places both `store` and `app_data` in <root>/data.
    /// The existing desktop.toml remains in `store`, not `config`.
    /// Portable mode ignores home and platform environment roots.
    /// Empty platform values use defaults; selected roots must be absolute
    /// and contain no parent traversal.
    pub fn resolve(
        platform: Platform,
        home: &Path,
        environment: &LayoutEnvironment,
    ) -> Result<Self> {
        if let Some(root) = &environment.portable_root {
            validate_root(root, "PP_PORTABLE_DIR")?;
            return Ok(Self {
                store: root.join("data"),
                app_data: root.join("data"),
                config: root.join("config"),
                logs: root.join("logs"),
                cache: root.join("cache"),
            });
        }
        validate_root(home, "home")?;
        match platform {
            Platform::Windows => {
                let local = environment_root(
                    &environment.local_appdata,
                    home,
                    "AppData/Local",
                    "LOCALAPPDATA",
                )?
                .join(APP_ID);
                let roaming =
                    environment_root(&environment.appdata, home, "AppData/Roaming", "APPDATA")?
                        .join(APP_ID);
                Ok(Self {
                    store: home.join(".print-partner"),
                    app_data: roaming.clone(),
                    config: roaming,
                    logs: local.join("logs"),
                    cache: local,
                })
            }
            Platform::MacOs => {
                let library = home.join("Library");
                let support = library.join("Application Support").join(APP_ID);
                Ok(Self {
                    store: home.join(".print-partner"),
                    app_data: support.clone(),
                    config: support,
                    logs: library.join("Logs").join(APP_ID),
                    cache: library.join("Caches").join(APP_ID),
                })
            }
            Platform::Linux => Ok(Self {
                store: home.join(".print-partner"),
                app_data: environment_root(
                    &environment.xdg_data_home,
                    home,
                    ".local/share",
                    "XDG_DATA_HOME",
                )?
                .join(APP_ID),
                config: environment_root(
                    &environment.xdg_config_home,
                    home,
                    ".config",
                    "XDG_CONFIG_HOME",
                )?
                .join(APP_ID),
                logs: environment_root(
                    &environment.xdg_state_home,
                    home,
                    ".local/state",
                    "XDG_STATE_HOME",
                )?
                .join(APP_ID)
                .join("logs"),
                cache: environment_root(
                    &environment.xdg_cache_home,
                    home,
                    ".cache",
                    "XDG_CACHE_HOME",
                )?
                .join(APP_ID),
            }),
        }
    }
}

fn validate_root(path: &Path, name: &str) -> Result<()> {
    ensure!(
        path.is_absolute() && !path.components().any(|part| part == Component::ParentDir),
        "Invalid {name} directory"
    );
    Ok(())
}

fn environment_root(
    value: &Option<PathBuf>,
    home: &Path,
    default: &str,
    name: &str,
) -> Result<PathBuf> {
    match value {
        Some(path) if !path.as_os_str().is_empty() => {
            validate_root(path, name)?;
            Ok(path.clone())
        }
        _ => Ok(home.join(default)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `path` made absolute on this host: Windows needs a drive.
    fn abs(path: &str) -> PathBuf {
        crate::tests::abs(path).into()
    }

    #[test]
    fn ticket_t_12_windows_layout_defaults_preserve_legacy_store() {
        let layout = DirectoryLayout::resolve(
            Platform::Windows,
            &abs("/Users/owner"),
            &LayoutEnvironment::default(),
        )
        .unwrap();
        assert_eq!(
            layout,
            DirectoryLayout {
                store: abs("/Users/owner/.print-partner"),
                app_data: abs("/Users/owner/AppData/Roaming/com.poitee.printpartner"),
                config: abs("/Users/owner/AppData/Roaming/com.poitee.printpartner"),
                logs: abs("/Users/owner/AppData/Local/com.poitee.printpartner/logs"),
                cache: abs("/Users/owner/AppData/Local/com.poitee.printpartner"),
            }
        );
    }

    #[test]
    fn ticket_t_12_windows_layout_honors_roaming_and_local_roots() {
        let environment = LayoutEnvironment {
            appdata: Some(abs("/roaming")),
            local_appdata: Some(abs("/local")),
            xdg_data_home: Some("ignored-relative".into()),
            ..Default::default()
        };
        let layout =
            DirectoryLayout::resolve(Platform::Windows, &abs("/Users/owner"), &environment)
                .unwrap();
        assert_eq!(
            layout,
            DirectoryLayout {
                store: abs("/Users/owner/.print-partner"),
                app_data: abs("/roaming/com.poitee.printpartner"),
                config: abs("/roaming/com.poitee.printpartner"),
                logs: abs("/local/com.poitee.printpartner/logs"),
                cache: abs("/local/com.poitee.printpartner"),
            }
        );
    }

    #[test]
    fn ticket_t_12_windows_layout_empty_values_use_defaults() {
        let environment = LayoutEnvironment {
            appdata: Some(PathBuf::new()),
            local_appdata: Some(PathBuf::new()),
            ..Default::default()
        };
        let layout =
            DirectoryLayout::resolve(Platform::Windows, &abs("/Users/owner"), &environment)
                .unwrap();
        assert_eq!(
            layout.config,
            abs("/Users/owner/AppData/Roaming/com.poitee.printpartner")
        );
        assert_eq!(
            layout.app_data,
            abs("/Users/owner/AppData/Roaming/com.poitee.printpartner")
        );
        assert_eq!(
            layout.cache,
            abs("/Users/owner/AppData/Local/com.poitee.printpartner")
        );
        assert_eq!(
            layout.logs,
            abs("/Users/owner/AppData/Local/com.poitee.printpartner/logs")
        );
    }

    #[test]
    fn ticket_t_12_windows_layout_rejects_invalid_selected_roots() {
        for invalid in ["relative", "/safe/../other"] {
            for (environment, message) in [
                (
                    LayoutEnvironment {
                        appdata: Some(invalid.into()),
                        ..Default::default()
                    },
                    "Invalid APPDATA directory",
                ),
                (
                    LayoutEnvironment {
                        local_appdata: Some(invalid.into()),
                        ..Default::default()
                    },
                    "Invalid LOCALAPPDATA directory",
                ),
            ] {
                assert_eq!(
                    DirectoryLayout::resolve(Platform::Windows, &abs("/Users/owner"), &environment)
                        .unwrap_err()
                        .to_string(),
                    message
                );
            }
        }
    }

    #[test]
    fn ticket_t_12_windows_environment_snapshot_uses_named_variables() {
        let environment = LayoutEnvironment::from_lookup(|name| match name {
            "APPDATA" => Some(abs("/roaming").into_os_string()),
            "LOCALAPPDATA" => Some(abs("/local").into_os_string()),
            _ => None,
        });
        assert_eq!(
            environment.appdata.as_deref(),
            Some(abs("/roaming").as_path())
        );
        assert_eq!(
            environment.local_appdata.as_deref(),
            Some(abs("/local").as_path())
        );
        let layout =
            DirectoryLayout::resolve(Platform::Windows, &abs("/Users/owner"), &environment)
                .unwrap();
        assert_eq!(layout.config, abs("/roaming/com.poitee.printpartner"));
        assert_eq!(layout.logs, abs("/local/com.poitee.printpartner/logs"));
    }

    #[cfg(unix)]
    #[test]
    fn ticket_t_12_windows_layout_preserves_non_unicode_roots() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let environment = LayoutEnvironment {
            appdata: Some(OsString::from_vec(b"/roaming/\xff".to_vec()).into()),
            local_appdata: Some(OsString::from_vec(b"/local/\xfe".to_vec()).into()),
            ..Default::default()
        };
        let layout =
            DirectoryLayout::resolve(Platform::Windows, &abs("/Users/owner"), &environment)
                .unwrap();
        assert_eq!(
            layout.config.as_os_str().as_bytes(),
            b"/roaming/\xff/com.poitee.printpartner"
        );
        assert_eq!(
            layout.logs.as_os_str().as_bytes(),
            b"/local/\xfe/com.poitee.printpartner/logs"
        );
    }

    #[test]
    fn ticket_t_12_windows_layout_falls_back_for_each_missing_root() {
        let environment = LayoutEnvironment {
            appdata: Some(abs("/roaming")),
            ..Default::default()
        };
        let layout =
            DirectoryLayout::resolve(Platform::Windows, &abs("/Users/owner"), &environment)
                .unwrap();
        assert_eq!(layout.config, abs("/roaming/com.poitee.printpartner"));
        assert_eq!(
            layout.cache,
            abs("/Users/owner/AppData/Local/com.poitee.printpartner")
        );
        let environment = LayoutEnvironment {
            local_appdata: Some(abs("/local")),
            ..Default::default()
        };
        let layout =
            DirectoryLayout::resolve(Platform::Windows, &abs("/Users/owner"), &environment)
                .unwrap();
        assert_eq!(
            layout.config,
            abs("/Users/owner/AppData/Roaming/com.poitee.printpartner")
        );
        assert_eq!(layout.cache, abs("/local/com.poitee.printpartner"));
        assert_eq!(
            layout.app_data,
            abs("/Users/owner/AppData/Roaming/com.poitee.printpartner")
        );
    }

    #[test]
    fn ticket_t_12_macos_layout_uses_bundle_identity() {
        let layout = DirectoryLayout::resolve(
            Platform::MacOs,
            &abs("/Users/owner"),
            &LayoutEnvironment::default(),
        )
        .unwrap();
        assert_eq!(
            layout,
            DirectoryLayout {
                store: abs("/Users/owner/.print-partner"),
                app_data: abs("/Users/owner/Library/Application Support/com.poitee.printpartner"),
                config: abs("/Users/owner/Library/Application Support/com.poitee.printpartner"),
                logs: abs("/Users/owner/Library/Logs/com.poitee.printpartner"),
                cache: abs("/Users/owner/Library/Caches/com.poitee.printpartner"),
            }
        );
    }

    #[test]
    fn ticket_t_12_linux_layout_uses_xdg_defaults() {
        let layout = DirectoryLayout::resolve(
            Platform::Linux,
            &abs("/home/owner"),
            &LayoutEnvironment::default(),
        )
        .unwrap();
        assert_eq!(
            layout,
            DirectoryLayout {
                store: abs("/home/owner/.print-partner"),
                app_data: abs("/home/owner/.local/share/com.poitee.printpartner"),
                config: abs("/home/owner/.config/com.poitee.printpartner"),
                logs: abs("/home/owner/.local/state/com.poitee.printpartner/logs"),
                cache: abs("/home/owner/.cache/com.poitee.printpartner"),
            }
        );
    }

    #[test]
    fn ticket_t_12_linux_layout_honors_each_xdg_override() {
        let environment = LayoutEnvironment {
            appdata: Some("ignored-relative".into()),
            local_appdata: Some("ignored-relative".into()),
            xdg_data_home: Some(abs("/data")),
            xdg_config_home: Some(abs("/config")),
            xdg_state_home: Some(abs("/state")),
            xdg_cache_home: Some(abs("/cache")),
            ..Default::default()
        };
        let layout =
            DirectoryLayout::resolve(Platform::Linux, &abs("/home/owner"), &environment).unwrap();
        assert_eq!(
            layout,
            DirectoryLayout {
                store: abs("/home/owner/.print-partner"),
                app_data: abs("/data/com.poitee.printpartner"),
                config: abs("/config/com.poitee.printpartner"),
                logs: abs("/state/com.poitee.printpartner/logs"),
                cache: abs("/cache/com.poitee.printpartner"),
            }
        );
    }

    #[test]
    fn ticket_t_12_portable_layout_overrides_home_and_xdg_without_io() {
        let environment = LayoutEnvironment {
            portable_root: Some(abs("/portable/Print Partner")),
            xdg_data_home: Some("ignored-relative".into()),
            appdata: Some("ignored-relative".into()),
            local_appdata: Some("ignored-relative".into()),
            ..Default::default()
        };
        for platform in [Platform::MacOs, Platform::Linux, Platform::Windows] {
            let layout = DirectoryLayout::resolve(platform, Path::new(""), &environment).unwrap();
            assert_eq!(
                layout,
                DirectoryLayout {
                    store: abs("/portable/Print Partner/data"),
                    app_data: abs("/portable/Print Partner/data"),
                    config: abs("/portable/Print Partner/config"),
                    logs: abs("/portable/Print Partner/logs"),
                    cache: abs("/portable/Print Partner/cache"),
                }
            );
        }
    }

    #[test]
    fn ticket_t_12_layout_rejects_relative_and_empty_roots() {
        for root in ["", "relative", "/safe/../other"] {
            let environment = LayoutEnvironment {
                portable_root: Some(root.into()),
                ..Default::default()
            };
            assert_eq!(
                DirectoryLayout::resolve(Platform::Linux, &abs("/home/owner"), &environment)
                    .unwrap_err()
                    .to_string(),
                "Invalid PP_PORTABLE_DIR directory",
                "{root}"
            );
        }
        for home in ["", "relative", "/home/../other"] {
            assert_eq!(
                DirectoryLayout::resolve(
                    Platform::MacOs,
                    Path::new(home),
                    &LayoutEnvironment::default()
                )
                .unwrap_err()
                .to_string(),
                "Invalid home directory",
                "{home}"
            );
        }
    }

    #[test]
    fn ticket_t_12_layout_rejects_each_invalid_xdg_override() {
        for (index, expected) in [
            "Invalid XDG_DATA_HOME directory",
            "Invalid XDG_CONFIG_HOME directory",
            "Invalid XDG_STATE_HOME directory",
            "Invalid XDG_CACHE_HOME directory",
        ]
        .into_iter()
        .enumerate()
        {
            let mut environment = LayoutEnvironment::default();
            let field = match index {
                0 => &mut environment.xdg_data_home,
                1 => &mut environment.xdg_config_home,
                2 => &mut environment.xdg_state_home,
                _ => &mut environment.xdg_cache_home,
            };
            *field = Some("relative".into());
            assert_eq!(
                DirectoryLayout::resolve(Platform::Linux, &abs("/home/owner"), &environment)
                    .unwrap_err()
                    .to_string(),
                expected
            );
        }
    }

    #[test]
    fn ticket_t_12_empty_xdg_values_use_defaults() {
        let environment = LayoutEnvironment::from_lookup(|_| Some(OsString::new()));
        let environment = LayoutEnvironment {
            portable_root: None,
            ..environment
        };
        let layout =
            DirectoryLayout::resolve(Platform::Linux, &abs("/home/owner"), &environment).unwrap();
        assert_eq!(
            layout.app_data,
            abs("/home/owner/.local/share/com.poitee.printpartner")
        );
        assert_eq!(
            layout.config,
            abs("/home/owner/.config/com.poitee.printpartner")
        );
        assert_eq!(
            layout.logs,
            abs("/home/owner/.local/state/com.poitee.printpartner/logs")
        );
        assert_eq!(
            layout.cache,
            abs("/home/owner/.cache/com.poitee.printpartner")
        );
    }

    #[test]
    fn ticket_t_12_environment_snapshot_uses_named_variables() {
        let environment =
            LayoutEnvironment::from_lookup(|name| Some(abs(&format!("/{name}")).into_os_string()));
        assert_eq!(
            environment.portable_root.as_deref(),
            Some(abs("/PP_PORTABLE_DIR").as_path())
        );
        assert_eq!(
            environment.xdg_data_home.as_deref(),
            Some(abs("/XDG_DATA_HOME").as_path())
        );
        assert_eq!(
            environment.xdg_config_home.as_deref(),
            Some(abs("/XDG_CONFIG_HOME").as_path())
        );
        assert_eq!(
            environment.xdg_state_home.as_deref(),
            Some(abs("/XDG_STATE_HOME").as_path())
        );
        assert_eq!(
            environment.xdg_cache_home.as_deref(),
            Some(abs("/XDG_CACHE_HOME").as_path())
        );
    }

    #[test]
    fn ticket_t_12_macos_ignores_xdg_environment() {
        let environment = LayoutEnvironment::from_lookup(|name| {
            (name != "PP_PORTABLE_DIR").then(|| OsString::from("relative"))
        });
        let layout =
            DirectoryLayout::resolve(Platform::MacOs, &abs("/Users/owner"), &environment).unwrap();
        assert_eq!(
            layout.app_data,
            abs("/Users/owner/Library/Application Support/com.poitee.printpartner")
        );
        assert_eq!(
            layout.logs,
            abs("/Users/owner/Library/Logs/com.poitee.printpartner")
        );
    }

    #[cfg(unix)]
    #[test]
    fn ticket_t_12_layout_preserves_non_unicode_roots() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let environment = LayoutEnvironment::from_lookup(|name| {
            (name == "XDG_DATA_HOME").then(|| OsString::from_vec(b"/data/\xff".to_vec()))
        });
        let layout =
            DirectoryLayout::resolve(Platform::Linux, &abs("/home/owner"), &environment).unwrap();
        assert_eq!(
            layout.app_data.as_os_str().as_bytes(),
            b"/data/\xff/com.poitee.printpartner"
        );
    }
}
