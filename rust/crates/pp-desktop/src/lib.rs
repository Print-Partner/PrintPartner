use anyhow::{Context, Result, ensure};
use pp_core::{DesktopLaunch, VerifiedBundle};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tauri::Url;

pub mod autostart;
pub use autostart::LaunchArgs;
pub mod browser;
pub mod downloads;
pub mod focus;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux as platform;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos as platform;
#[cfg(feature = "desktop_rebuild")]
pub mod native_actions;
#[cfg(not(target_os = "linux"))]
mod rfd_dialogs;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows as platform;
#[cfg(feature = "desktop_rebuild")]
pub mod origin_change;
pub mod tray;

pub fn desktop_data_dir(
    explicit_data: Option<PathBuf>,
    home: impl FnOnce() -> Result<PathBuf>,
) -> Result<PathBuf> {
    if let Some(data) = explicit_data {
        return Ok(data);
    }
    #[cfg(feature = "desktop_rebuild")]
    if pp_core::DesktopRebuild::from_env().enabled() {
        use pp_core::layout::{DirectoryLayout, LayoutEnvironment};

        let environment = LayoutEnvironment::from_env();
        let home = if environment.portable_root.is_some() {
            PathBuf::new()
        } else {
            home()?
        };
        return Ok(DirectoryLayout::resolve(platform::HOST, &home, &environment)?.store);
    }
    let home = home()?;
    ensure!(home.is_absolute(), "Desktop home directory unavailable");
    Ok(home.join(".print-partner"))
}

/// Only emit known diagnostic categories and OS error numbers. Arbitrary error
/// messages can include private paths, bootstrap URLs or backend payloads.
pub fn startup_diagnostic(error: &anyhow::Error) -> String {
    let mut codes = Vec::new();
    for cause in error.chain() {
        let code = match cause.to_string().as_str() {
            "Desktop resources unavailable" => Some("resources_unavailable"),
            "Desktop resource verification failed" => Some("resource_verification"),
            "Desktop core startup failed" => Some("core_startup"),
            "Desktop home directory unavailable" => Some("home_unavailable"),
            "Native shell initialization failed" => Some("shell_initialization"),
            "Native window initialization failed" => Some("window_initialization"),
            "Native tray initialization failed" => Some("tray_initialization"),
            "Desktop shutdown incomplete" => Some("shutdown_incomplete"),
            "Data directory already owned"
            | "Data directory already owned; stop the other Print Partner process before starting another instance" => {
                Some("data_directory_owned")
            }
            "Unsafe data directory permissions" => Some("data_directory_permissions"),
            "Unsafe data directory ancestor" => Some("data_directory_ancestor"),
            "storage_lease_marker_invalid" => Some("storage_lease_marker_invalid"),
            "Persisted desktop origin is unavailable" => Some("persisted_origin_unavailable"),
            "Release artifact missing" | "Release root missing" => Some("release_missing"),
            "Release artifact content mismatch" | "Node executable content mismatch" => {
                Some("release_content_mismatch")
            }
            "Release file inventory mismatch" => Some("release_inventory_mismatch"),
            "Desktop architecture mismatch" => Some("architecture_mismatch"),
            "Desktop Node identity mismatch" => Some("node_identity_mismatch"),
            "Desktop commit identity missing" => Some("commit_identity_missing"),
            "Unsupported desktop launch argument" => Some("unsupported_argument"),
            _ => None,
        };
        if let Some(code) = code {
            codes.push(code.to_owned());
        }
        if let Some(errno) = cause
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::raw_os_error)
        {
            codes.push(format!("os_error={errno}"));
        }
    }
    if codes.is_empty() {
        codes.push("unclassified".to_owned());
    }
    format!("desktop_startup_failed: {}", codes.join(" -> "))
}

/// Classify a non-fatal browser-preferences read. The launch continues without
/// portable settings. The line keeps known categories and OS error numbers.
pub fn browser_preferences_diagnostic(error: &anyhow::Error) -> String {
    let mut codes = Vec::new();
    for cause in error.chain() {
        let code = match cause.to_string().as_str() {
            "Invalid browser preferences" => Some("invalid_browser_preferences"),
            "Unsupported browser preferences version" => Some("unsupported_browser_preferences"),
            "Invalid browser-preferences.json" => Some("invalid_browser_preferences_file"),
            "Core owner stopped" => Some("core_owner_stopped"),
            "Desktop configuration unavailable" => Some("configuration_unavailable"),
            "Desktop configuration changed" => Some("configuration_changed"),
            _ => None,
        };
        if let Some(code) = code {
            codes.push(code.to_owned());
        }
        if let Some(errno) = cause
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::raw_os_error)
        {
            codes.push(format!("os_error={errno}"));
        }
    }
    if codes.is_empty() {
        codes.push("unclassified".to_owned());
    }
    format!(
        "desktop_browser_preferences_unavailable: {}",
        codes.join(" -> ")
    )
}

#[derive(Clone)]
pub struct AllowedOrigin(Url);

impl AllowedOrigin {
    pub fn parse(value: &str) -> Result<Self> {
        let url = Url::parse(value)?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none(),
            "Invalid desktop origin"
        );
        Ok(Self(url))
    }
    pub fn admits(&self, url: &Url) -> bool {
        url.scheme() == "http"
            && url.origin() == self.0.origin()
            && url.username().is_empty()
            && url.password().is_none()
    }
}

#[derive(Deserialize)]
struct ReleaseIdentity {
    runtime_version: String,
    commit: String,
    node: PathBuf,
    web: PathBuf,
    os: String,
    arch: String,
    node_version: String,
    node_abi: String,
}

pub struct ResourceLayout {
    root: PathBuf,
    release: ReleaseIdentity,
}

impl ResourceLayout {
    pub fn read(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .context("Desktop resources unavailable")?;
        let identity = if root.join("release.json").is_file() {
            root.join("release.json")
        } else {
            root.join(native("Resources/desktop-runtime/release.json"))
        };
        let release: ReleaseIdentity = serde_json::from_slice(&std::fs::read(identity)?)?;
        ensure!(
            release.os == std::env::consts::OS && release.arch == std::env::consts::ARCH,
            "Desktop architecture mismatch"
        );
        ensure!(
            release.node_version == "v24.21.0" && release.node_abi == "137",
            "Desktop Node identity mismatch"
        );
        ensure!(
            release.commit.len() == 40
                && release
                    .commit
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "Desktop commit identity missing"
        );
        contained(&root, &release.node)?;
        contained(&root, &release.web)?;
        Ok(Self { root, release })
    }
    pub fn into_launch(self, data_dir: PathBuf) -> Result<DesktopLaunch> {
        let web = contained(&self.root, &self.release.web)?;
        let node = contained(&self.root, &self.release.node)?;
        Ok(DesktopLaunch {
            data_dir,
            assets: web.join(native("apps/web/dist")),
            bundle: VerifiedBundle {
                node,
                entry: web.join(native("apps/server/dist/current/desktop.js")),
                web_root: web,
                runtime_version: self.release.runtime_version,
                commit: self.release.commit,
            },
        })
    }
}

fn contained(root: &Path, relative: &Path) -> Result<PathBuf> {
    ensure!(
        !relative.is_absolute()
            && relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        "Invalid desktop resource path"
    );
    let path = root.join(native(relative)).canonicalize()?;
    ensure!(path.starts_with(root), "Desktop resource escaped package");
    Ok(path)
}

/// `relative` with the host's separator. Windows canonical roots are `\\?\`
/// paths, which take `/` as part of a name rather than a separator.
fn native(relative: impl AsRef<Path>) -> PathBuf {
    relative.as_ref().components().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::{symlink as symlink_dir, symlink as symlink_file};
    #[cfg(windows)]
    use std::os::windows::fs::{symlink_dir, symlink_file};

    /// An absolute path on the host: a drive is prefixed on Windows.
    #[cfg(unix)]
    macro_rules! abs {
        ($path:literal) => {
            $path
        };
    }
    #[cfg(windows)]
    macro_rules! abs {
        ($path:literal) => {
            concat!("C:", $path)
        };
    }

    /// A path component that is not valid Unicode on the host.
    fn nonunicode() -> std::ffi::OsString {
        #[cfg(unix)]
        let name = std::os::unix::ffi::OsStringExt::from_vec(vec![0xff]);
        #[cfg(windows)]
        let name = std::os::windows::ffi::OsStringExt::from_wide(&[0xd800]);
        name
    }

    fn selector_child(test: &str) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--ignored",
            "--exact",
            test,
            "--nocapture",
            "--test-threads=1",
        ]);
        for name in [
            "PP_DESKTOP_REBUILD",
            "PP_PORTABLE_DIR",
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn assert_selector_child(mut command: std::process::Command) {
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("running 1 test\n"), "{stdout}");
        assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");
    }

    #[cfg(feature = "desktop_rebuild")]
    #[test]
    fn ticket_t_12_portable_launcher_store() {
        let mut command = selector_child("tests::portable_launcher_store_in_child");
        command.env("PP_DESKTOP_REBUILD", "1");
        command.env("PP_PORTABLE_DIR", abs!("/portable/Print Partner"));
        assert_selector_child(command);
    }

    #[cfg(feature = "desktop_rebuild")]
    #[test]
    #[ignore = "runs in an isolated subprocess with explicit environment"]
    fn portable_launcher_store_in_child() {
        let calls = std::cell::Cell::new(0);
        let selected = desktop_data_dir(None, || {
            calls.set(calls.get() + 1);
            Ok(abs!("/users/existing-owner").into())
        })
        .unwrap();
        assert_eq!(
            selected,
            PathBuf::from(abs!("/portable/Print Partner/data"))
        );
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn ticket_t_5_upgrades_reuse_the_legacy_desktop_store() {
        let mut command = selector_child("tests::launcher_selection_in_child");
        command.env("PP_SELECTOR_CASE", "legacy");
        assert_selector_child(command);
    }

    #[test]
    fn ticket_t_12_launcher_selection_gates_and_overrides() {
        for runtime in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("TRUE"),
            Some(" true "),
            Some("yes"),
        ] {
            let mut command = selector_child("tests::launcher_selection_in_child");
            command.env("PP_SELECTOR_CASE", "legacy");
            command.env("PP_PORTABLE_DIR", abs!("/portable/Print Partner"));
            command.env("XDG_DATA_HOME", "invalid-relative");
            if let Some(runtime) = runtime {
                command.env("PP_DESKTOP_REBUILD", runtime);
            }
            assert_selector_child(command);
        }
        for runtime in ["1", "true"] {
            for (case, portable) in [
                ("portable", Some(abs!("/portable/Print Partner"))),
                (
                    "portable-unavailable",
                    Some(abs!("/portable/Print Partner")),
                ),
                ("invalid-portable", Some("")),
                ("invalid-portable", Some("relative")),
                ("invalid-portable", Some(abs!("/safe/../other"))),
                ("invalid-portable-unavailable", Some("relative")),
                ("legacy", None),
                ("unavailable", None),
                ("relative-home-enabled", None),
                ("parent-home-enabled", None),
                ("absolute-override", Some("relative")),
                ("relative-override", Some("")),
            ] {
                let mut command = selector_child("tests::launcher_selection_in_child");
                command.env("PP_SELECTOR_CASE", case);
                command.env("PP_DESKTOP_REBUILD", runtime);
                if let Some(portable) = portable {
                    command.env("PP_PORTABLE_DIR", portable);
                }
                assert_selector_child(command);
            }
        }
        for case in [
            "unavailable",
            "relative-home",
            "parent-home",
            "absolute-override",
            "relative-override",
        ] {
            let mut command = selector_child("tests::launcher_selection_in_child");
            command.env("PP_SELECTOR_CASE", case);
            command.env("PP_PORTABLE_DIR", "invalid-relative");
            assert_selector_child(command);
        }
        for case in [
            "nonunicode-portable",
            "nonunicode-home",
            "nonunicode-override",
        ] {
            let mut command = selector_child("tests::launcher_selection_in_child");
            command.env("PP_SELECTOR_CASE", case);
            command.env("PP_DESKTOP_REBUILD", "1");
            if case == "nonunicode-portable" {
                command.env(
                    "PP_PORTABLE_DIR",
                    Path::new(abs!("/portable")).join(nonunicode()),
                );
            }
            assert_selector_child(command);
        }
    }

    #[test]
    #[ignore = "runs in an isolated subprocess with explicit environment"]
    fn launcher_selection_in_child() {
        let case = std::env::var("PP_SELECTOR_CASE").unwrap();
        let compiled = cfg!(feature = "desktop_rebuild");
        let mut home = PathBuf::from(abs!("/users/existing-owner"));
        let mut explicit = None;
        let mut expected: Result<PathBuf, &str> =
            Ok(abs!("/users/existing-owner/.print-partner").into());
        let mut expected_calls = 1;
        match case.as_str() {
            "portable" | "portable-unavailable" if compiled => {
                expected = Ok(abs!("/portable/Print Partner/data").into());
                expected_calls = 0;
            }
            "invalid-portable-unavailable" | "invalid-portable" if compiled => {
                expected = Err("Invalid PP_PORTABLE_DIR directory");
                expected_calls = 0;
            }
            "legacy" | "portable" | "invalid-portable" => {}
            "unavailable" | "portable-unavailable" | "invalid-portable-unavailable" => {
                expected = Err("Desktop home directory unavailable");
            }
            "relative-home" | "relative-home-enabled" => {
                home = "relative-home".into();
                expected = Err(if compiled && case == "relative-home-enabled" {
                    "Invalid home directory"
                } else {
                    "Desktop home directory unavailable"
                });
            }
            "parent-home" | "parent-home-enabled" => {
                home = abs!("/users/../existing-owner").into();
                expected = if compiled && case == "parent-home-enabled" {
                    Err("Invalid home directory")
                } else {
                    Ok(abs!("/users/../existing-owner/.print-partner").into())
                };
            }
            "absolute-override" | "relative-override" => {
                let data = PathBuf::from(if case == "absolute-override" {
                    "/explicit/data"
                } else {
                    "relative/data"
                });
                explicit = Some(data.clone());
                expected = Ok(data);
                expected_calls = 0;
            }
            "nonunicode-portable" | "nonunicode-home" | "nonunicode-override" => {
                match case.as_str() {
                    "nonunicode-portable" if compiled => {
                        expected = Ok(Path::new(abs!("/portable")).join(nonunicode()).join("data"));
                        expected_calls = 0;
                    }
                    "nonunicode-home" => {
                        home = Path::new(abs!("/users")).join(nonunicode());
                        expected = Ok(home.join(".print-partner"));
                    }
                    "nonunicode-override" => {
                        explicit = Some(Path::new("relative").join(nonunicode()));
                        expected = Ok(Path::new("relative").join(nonunicode()));
                        expected_calls = 0;
                    }
                    _ => {}
                }
            }
            _ => panic!("unknown selector case {case}"),
        }
        let calls = std::cell::Cell::new(0);
        let actual = desktop_data_dir(explicit, || {
            calls.set(calls.get() + 1);
            assert!(
                !case.ends_with("override"),
                "explicit data must not request home"
            );
            if case == "unavailable" || case.ends_with("-unavailable") {
                anyhow::bail!("Desktop home directory unavailable");
            }
            Ok(home)
        });
        assert_eq!(
            actual.map_err(|error| error.to_string()),
            expected.map_err(str::to_owned),
            "{case}"
        );
        assert_eq!(calls.get(), expected_calls, "{case}");
    }

    // macOS embeds Info.plist under one global symbol, so the test binary may expand the macro only once.
    fn generated_context() -> tauri::Context<tauri::Wry> {
        tauri::generate_context!()
    }

    #[test]
    fn ticket_t_5_generated_context_keeps_the_installed_product_identity() {
        let context = generated_context();
        assert_eq!(context.config().identifier, "com.poitee.printpartner");
        assert_eq!(
            context.config().product_name.as_deref(),
            Some("Print Partner")
        );
        assert_eq!(context.package_info().name, "Print Partner");
    }

    #[test]
    fn ticket_t_5_release_layout_retains_the_product_launch_identity() {
        let scratch = crate::scratch::Scratch::new("pp-identity-");
        let root = scratch.to_path_buf();
        std::fs::create_dir_all(root.join("real")).unwrap();
        symlink_dir(root.join("real"), root.join("linked")).unwrap();
        let fixture = root.join(native("linked/fixture"));
        std::fs::create_dir_all(&fixture).unwrap();
        let fixture = fixture.canonicalize().unwrap();
        std::fs::create_dir_all(fixture.join("bin")).unwrap();
        std::fs::create_dir_all(fixture.join("web")).unwrap();
        std::fs::create_dir_all(fixture.join(native("home/.print-partner"))).unwrap();
        std::fs::write(fixture.join(native("bin/node")), b"node").unwrap();
        let data = fixture.join(native("home/.print-partner"));
        let release = serde_json::json!({
            "runtime_version": "3.3.0-web",
            "commit": "f00471f13d2f0e69631d3130e594b9244f93e01d",
            "node": "bin/node",
            "web": "web",
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "node_version": "v24.21.0",
            "node_abi": "137"
        });
        std::fs::write(fixture.join("release.json"), release.to_string()).unwrap();
        let launch = ResourceLayout::read(&fixture)
            .unwrap()
            .into_launch(data.clone())
            .unwrap();
        assert_eq!(launch.data_dir, data);
        assert_eq!(
            launch.bundle.commit,
            "f00471f13d2f0e69631d3130e594b9244f93e01d"
        );
        assert_eq!(launch.bundle.runtime_version, "3.3.0-web");
        assert_eq!(launch.bundle.node, fixture.join(native("bin/node")));
        assert_eq!(
            launch.bundle.entry,
            fixture.join(native("web/apps/server/dist/current/desktop.js"))
        );
        let mut invalid = release;
        invalid["commit"] = serde_json::json!("Print Partner");
        std::fs::write(fixture.join("release.json"), invalid.to_string()).unwrap();
        assert_eq!(
            ResourceLayout::read(&fixture).err().unwrap().to_string(),
            "Desktop commit identity missing"
        );
    }

    #[test]
    fn release_layout_accepts_macos_resource_paths_and_current_version() {
        let scratch = crate::scratch::Scratch::new("pp-macos-layout-");
        let root = scratch.to_path_buf();
        std::fs::create_dir_all(root.join("MacOS")).unwrap();
        std::fs::create_dir_all(root.join(native("Resources/desktop-runtime/web"))).unwrap();
        std::fs::write(root.join(native("MacOS/printpartner-node")), b"fixture").unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(
            root.join(native("Resources/desktop-runtime/release.json")),
            serde_json::json!({
                "runtime_version": "4.2.1-web",
                "commit": "f00471f13d2f0e69631d3130e594b9244f93e01d",
                "node": "MacOS/printpartner-node",
                "web": "Resources/desktop-runtime/web",
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "node_version": "v24.21.0",
                "node_abi": "137"
            })
            .to_string(),
        )
        .unwrap();
        let launch = ResourceLayout::read(&root)
            .unwrap()
            .into_launch(root.join("data"))
            .unwrap();
        assert_eq!(
            launch.bundle.node,
            root.join(native("MacOS/printpartner-node"))
        );
        assert_eq!(
            launch.bundle.web_root,
            root.join(native("Resources/desktop-runtime/web"))
        );
        assert_eq!(launch.bundle.runtime_version, "4.2.1-web");
    }

    #[test]
    fn startup_diagnostics_keep_causes_without_private_values() {
        let secret = "private/path?bootstrap=secret-token";
        let error = anyhow::anyhow!(secret)
            .context("Data directory already owned")
            .context("Desktop core startup failed");
        assert_eq!(
            startup_diagnostic(&error),
            "desktop_startup_failed: core_startup -> data_directory_owned"
        );
        let error = anyhow::anyhow!("Data directory already owned; stop the other Print Partner process before starting another instance")
            .context("Desktop core startup failed");
        assert_eq!(
            startup_diagnostic(&error),
            "desktop_startup_failed: core_startup -> data_directory_owned"
        );
        assert!(!startup_diagnostic(&anyhow::anyhow!(secret)).contains(secret));
        let error = anyhow::Error::from(std::io::Error::from_raw_os_error(13))
            .context("Desktop resources unavailable");
        assert_eq!(
            startup_diagnostic(&error),
            "desktop_startup_failed: resources_unavailable -> os_error=13"
        );
        let preference_error = anyhow::anyhow!(secret).context("Invalid browser preferences");
        let preference = browser_preferences_diagnostic(&preference_error);
        assert_eq!(
            preference,
            "desktop_browser_preferences_unavailable: invalid_browser_preferences"
        );
        assert!(!preference.contains(secret));
        let preference_io =
            anyhow::Error::from(std::io::Error::from_raw_os_error(13)).context(secret);
        let preference_io = browser_preferences_diagnostic(&preference_io);
        assert_eq!(
            preference_io,
            "desktop_browser_preferences_unavailable: os_error=13"
        );
        assert!(!preference_io.contains(secret));
        let error = anyhow::anyhow!(secret)
            .context("Unsafe data directory permissions")
            .context("Desktop core startup failed");
        assert_eq!(
            startup_diagnostic(&error),
            "desktop_startup_failed: core_startup -> data_directory_permissions"
        );
        assert!(!startup_diagnostic(&error).contains(secret));
        for (cause, code) in [
            ("Release artifact missing", "release_missing"),
            (
                "Release artifact content mismatch",
                "release_content_mismatch",
            ),
            (
                "Persisted desktop origin is unavailable",
                "persisted_origin_unavailable",
            ),
        ] {
            assert!(startup_diagnostic(&anyhow::anyhow!(cause)).ends_with(code));
        }
    }

    #[test]
    fn ticket_t_12_startup_diagnostic_redacts_unsafe_ancestor_paths() {
        let secret = "private/path?bootstrap=secret-token";
        let error = anyhow::anyhow!(secret)
            .context("Unsafe data directory ancestor")
            .context("Desktop core startup failed");
        assert_eq!(
            startup_diagnostic(&error),
            "desktop_startup_failed: core_startup -> data_directory_ancestor"
        );
        assert!(!startup_diagnostic(&error).contains(secret));
    }

    #[test]
    fn startup_diagnostics_classify_invalid_storage_markers_without_private_causes() {
        let secret = "private/path?bootstrap=secret-token";
        let error = anyhow::anyhow!(secret)
            .context("storage_lease_marker_invalid")
            .context("Desktop core startup failed");
        assert_eq!(
            startup_diagnostic(&error),
            "desktop_startup_failed: core_startup -> storage_lease_marker_invalid"
        );
        assert!(!startup_diagnostic(&error).contains(secret));
        let error = anyhow::Error::from(std::io::Error::from_raw_os_error(13))
            .context(secret)
            .context("storage_lease_marker_invalid")
            .context("Desktop core startup failed");
        assert_eq!(
            startup_diagnostic(&error),
            "desktop_startup_failed: core_startup -> storage_lease_marker_invalid -> os_error=13"
        );
        assert!(!startup_diagnostic(&error).contains(secret));
    }

    #[test]
    fn generated_authority_denies_native_commands_for_every_origin() {
        let mut context = generated_context();
        let build_config: tauri::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            build_config.bundle.macos.minimum_system_version.as_deref(),
            Some("13.5")
        );
        let authority = context.runtime_authority_mut();
        for origin in [
            tauri::ipc::Origin::Local,
            tauri::ipc::Origin::Remote {
                url: Url::parse("http://127.0.0.1:46687/").unwrap(),
            },
            tauri::ipc::Origin::Remote {
                url: Url::parse("https://example.com/").unwrap(),
            },
        ] {
            for command in [
                "open_sponsor",
                "sync_portable_settings",
                "plugin:app|exit",
                "plugin:window|close",
                "plugin:webview|create_webview_window",
                "plugin:fs|read_file",
                "plugin:shell|execute",
            ] {
                assert!(
                    authority
                        .resolve_access(command, "main", "main", &origin)
                        .is_none(),
                    "{command}"
                );
            }
        }
    }

    #[cfg(feature = "desktop_rebuild")]
    #[test]
    fn ticket_t_68_sponsor_capability_has_only_the_bound_remote_action() {
        let mut context = generated_context();
        crate::origin_change::verify_authority(&mut context);
        crate::native_actions::verify_authority(context);
    }

    #[test]
    fn resource_paths_must_remain_inside_the_package() {
        let scratch = crate::scratch::Scratch::new("pp-layout-");
        let root = scratch.to_path_buf();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join(native("bin/node")), b"node").unwrap();
        let root = root.canonicalize().unwrap();
        assert_eq!(
            contained(&root, Path::new("bin/node")).unwrap(),
            root.join(native("bin/node"))
        );
        for path in ["../outside", "/usr/bin/node", "bin/missing"] {
            assert!(contained(&root, Path::new(path)).is_err());
        }
        symlink_file(std::env::current_exe().unwrap(), root.join("escape")).unwrap();
        assert!(contained(&root, Path::new("escape")).is_err());
    }

    #[test]
    fn ticket_t_68_rejects_same_origin_blob_documents() {
        let policy = AllowedOrigin::parse("http://127.0.0.1:46687").unwrap();
        assert!(policy.admits(&Url::parse("http://127.0.0.1:46687/plans?x=1#part").unwrap()));
        let url =
            Url::parse("blob:http://127.0.0.1:46687/00000000-0000-4000-8000-000000000001").unwrap();
        assert!(!policy.admits(&url), "{url}");
    }

    #[test]
    fn ticket_t_68_rejects_nested_same_origin_blob_documents() {
        let policy = AllowedOrigin::parse("http://127.0.0.1:46687").unwrap();
        assert!(policy.admits(&Url::parse("http://127.0.0.1:46687/plans?x=1#part").unwrap()));
        let url =
            Url::parse("blob:blob:http://127.0.0.1:46687/00000000-0000-4000-8000-000000000001")
                .unwrap();
        assert!(!policy.admits(&url), "{url}");
    }

    #[test]
    fn admits_only_the_exact_loopback_origin() {
        let policy = AllowedOrigin::parse("http://127.0.0.1:46687").unwrap();
        for url in [
            "http://127.0.0.1:46687/",
            "http://127.0.0.1:46687/plans?x=1#part",
        ] {
            assert!(policy.admits(&Url::parse(url).unwrap()));
        }
        for url in [
            "http://127.0.0.1:46688/",
            "http://localhost:46687/",
            "https://127.0.0.1:46687/",
            "http://owner@127.0.0.1:46687/",
            "http://:secret@127.0.0.1:46687/",
            "http://127.0.0.1.evil:46687/",
            "file:///tmp/index.html",
            "data:text/html,hello",
            "about:blank",
            "javascript:void(0)",
            "ws://127.0.0.1:46687/",
        ] {
            assert!(!policy.admits(&Url::parse(url).unwrap()), "{url}");
        }
        assert!(AllowedOrigin::parse("http://localhost:46687").is_err());
        assert!(AllowedOrigin::parse("http://127.0.0.1:46687/path").is_err());
    }
}

#[cfg(test)]
#[path = "../../pp-storage/tests/common/cleanup.rs"]
mod cleanup;
/// Auto-removing test scratch, shared with pp-storage's tests.
#[cfg(test)]
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

#[cfg(test)]
#[test]
fn scratch_root_is_empty_after_lib_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_lib_tests"]);
}
