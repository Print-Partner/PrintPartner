//! T-31 (GRE-369): the per-user LaunchAgent is created and removed
//! idempotently, starts the app with an argument array, and disabling removes
//! only this app's agent. Runs on any OS against a temporary home directory;
//! the real login/reboot check needs a Mac.

use pp_desktop::autostart::{Change, LABEL, LaunchAgent, MINIMIZED_ARG};
use std::fs;
use std::path::Path;

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

const EXECUTABLE: &str = abs!("/Applications/Print Partner & Co.app/Contents/MacOS/pp-desktop");

fn agents(home: &Path) -> std::path::PathBuf {
    home.join("Library/LaunchAgents")
}

#[test]
fn ticket_t_31_label_is_the_bundle_identifier() {
    let config: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(config["identifier"], LABEL);
}

#[test]
fn ticket_t_31_enable_is_idempotent_and_uses_an_argument_array() {
    let home = tempfile::tempdir().unwrap();
    let agent = LaunchAgent::new(home.path(), Path::new(EXECUTABLE)).unwrap();
    assert_eq!(
        agent.path(),
        agents(home.path()).join(format!("{LABEL}.plist"))
    );
    assert!(!agent.is_enabled());

    assert_eq!(agent.enable().unwrap(), Change::Created);
    let first = fs::read(agent.path()).unwrap();
    assert!(agent.is_enabled());
    assert_eq!(agent.enable().unwrap(), Change::Unchanged);
    assert_eq!(fs::read(agent.path()).unwrap(), first);

    let text = String::from_utf8(first).unwrap();
    // One <string> per argument, escaped, with no shell wrapper.
    let arguments = text
        .split("<key>ProgramArguments</key>")
        .nth(1)
        .unwrap()
        .split("</array>")
        .next()
        .unwrap();
    let strings: Vec<&str> = arguments
        .split("<string>")
        .skip(1)
        .map(|part| part.split("</string>").next().unwrap())
        .collect();
    assert_eq!(
        strings,
        [
            abs!("/Applications/Print Partner &amp; Co.app/Contents/MacOS/pp-desktop"),
            MINIMIZED_ARG,
        ]
    );
    assert!(!text.contains("/bin/sh"));
    assert!(text.contains(&format!("<key>Label</key>\n\t<string>{LABEL}</string>")));
    assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
    // No temp file is left behind.
    let names: Vec<_> = fs::read_dir(agents(home.path()))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [format!("{LABEL}.plist")]);
}

#[test]
fn ticket_t_31_enable_rewrites_a_stale_agent_for_a_moved_app() {
    let home = tempfile::tempdir().unwrap();
    let old = LaunchAgent::new(home.path(), Path::new(abs!("/Volumes/Old/pp-desktop"))).unwrap();
    assert_eq!(old.enable().unwrap(), Change::Created);
    let current = LaunchAgent::new(home.path(), Path::new(EXECUTABLE)).unwrap();
    assert!(!current.is_enabled());
    assert_eq!(current.enable().unwrap(), Change::Updated);
    assert!(current.is_enabled());
    assert!(
        !fs::read_to_string(current.path())
            .unwrap()
            .contains("/Volumes/Old")
    );
}

#[test]
fn ticket_t_31_disable_removes_only_this_apps_agent() {
    let home = tempfile::tempdir().unwrap();
    let agent = LaunchAgent::new(home.path(), Path::new(EXECUTABLE)).unwrap();
    assert_eq!(agent.disable().unwrap(), Change::Absent);

    agent.enable().unwrap();
    let other = agents(home.path()).join("com.example.other.plist");
    fs::write(
        &other,
        "<plist><dict><key>Label</key><string>com.example.other</string></dict></plist>",
    )
    .unwrap();

    assert_eq!(agent.disable().unwrap(), Change::Removed);
    assert!(!agent.path().exists());
    assert!(other.exists(), "another app's agent must stay");
    assert_eq!(agent.disable().unwrap(), Change::Absent);
}

#[test]
fn ticket_t_31_disable_keeps_a_file_that_is_not_ours() {
    let home = tempfile::tempdir().unwrap();
    let agent = LaunchAgent::new(home.path(), Path::new(EXECUTABLE)).unwrap();
    fs::create_dir_all(agents(home.path())).unwrap();
    fs::write(
        agent.path(),
        "<plist><dict><key>Label</key><string>com.example.squatter</string></dict></plist>",
    )
    .unwrap();
    assert!(agent.disable().is_err());
    assert!(agent.path().exists());
}

#[test]
fn ticket_t_31_refuses_a_symlinked_agent_path() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_file;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file;
    let home = tempfile::tempdir().unwrap();
    let agent = LaunchAgent::new(home.path(), Path::new(EXECUTABLE)).unwrap();
    fs::create_dir_all(agents(home.path())).unwrap();
    let target = home.path().join("elsewhere.plist");
    fs::write(&target, agent.plist()).unwrap();
    symlink_file(&target, agent.path()).unwrap();
    assert!(agent.enable().is_err());
    assert!(agent.disable().is_err());
    assert!(target.exists());
}

#[test]
fn ticket_t_31_requires_absolute_paths() {
    assert!(LaunchAgent::new(Path::new("relative-home"), Path::new(EXECUTABLE)).is_err());
    assert!(LaunchAgent::new(Path::new(abs!("/Users/me")), Path::new("pp-desktop")).is_err());
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| arg.to_string()).collect()
}

#[test]
fn ticket_t_31_release_launch_accepts_minimized_and_starts_hidden() {
    use pp_desktop::LaunchArgs;
    // The exact arguments the LaunchAgent passes after the binary.
    let agent = LaunchAgent::new(Path::new(abs!("/Users/me")), Path::new(EXECUTABLE)).unwrap();
    assert!(
        agent
            .plist()
            .contains(&format!("<string>{MINIMIZED_ARG}</string>"))
    );

    let release = LaunchArgs::parse(args(&[MINIMIZED_ARG]), false).unwrap();
    assert!(release.minimized);
    assert!(!release.window_visible());

    let plain = LaunchArgs::parse(args(&[]), false).unwrap();
    assert!(plain.window_visible());
    assert_eq!(plain, LaunchArgs::default());
}

#[test]
fn ticket_t_31_release_launch_still_refuses_other_arguments() {
    use pp_desktop::LaunchArgs;
    for refused in [
        &["--unknown"][..],
        &["--data", "/tmp/x"],
        &["--dev-stage", "/tmp/x"],
        &["--test-exit-seconds", "3"],
        &["--test-recovery-logs"],
        &[MINIMIZED_ARG, "--unknown"],
    ] {
        assert!(
            LaunchArgs::parse(args(refused), false).is_err(),
            "{refused:?}"
        );
    }
    let debug = LaunchArgs::parse(
        args(&[
            MINIMIZED_ARG,
            "--data",
            "/tmp/data",
            "--test-exit-seconds",
            "3",
        ]),
        true,
    )
    .unwrap();
    assert!(debug.minimized && !debug.window_visible());
    assert_eq!(debug.data.as_deref(), Some(Path::new("/tmp/data")));
    assert_eq!(debug.test_exit, Some(3));
}

#[test]
fn ticket_t_31_open_at_login_toggle_turns_the_agent_on_and_off() {
    use pp_desktop::tray::toggle_open_at_login;
    let home = tempfile::tempdir().unwrap();
    let agent = LaunchAgent::new(home.path(), Path::new(EXECUTABLE)).unwrap();
    assert!(
        toggle_open_at_login(&agent).unwrap(),
        "first toggle turns it on"
    );
    assert!(agent.path().exists());
    assert!(
        !toggle_open_at_login(&agent).unwrap(),
        "second toggle turns it off"
    );
    assert!(!agent.path().exists());

    // An agent left by a moved app counts as off; toggling rewrites it for this app.
    let moved = LaunchAgent::new(
        home.path(),
        Path::new(abs!("/Applications/Old.app/Contents/MacOS/pp-desktop")),
    )
    .unwrap();
    moved.enable().unwrap();
    assert!(!agent.is_enabled());
    assert!(toggle_open_at_login(&agent).unwrap());
    assert!(agent.is_enabled());
}

#[test]
fn ticket_t_31_open_at_login_toggle_reports_a_failure() {
    use pp_desktop::tray::toggle_open_at_login;
    let home = tempfile::tempdir().unwrap();
    // A directory where the agent file belongs makes enable fail.
    let agent = LaunchAgent::new(home.path(), Path::new(EXECUTABLE)).unwrap();
    fs::create_dir_all(agent.path()).unwrap();
    assert!(toggle_open_at_login(&agent).is_err());
    assert!(!agent.is_enabled());
}

#[test]
fn ticket_t_31_open_at_login_sits_before_quit_on_macos_only() {
    use pp_desktop::tray::{OPEN_AT_LOGIN, menu_ids, menu_ids_with_open_at_login};
    let ids = menu_ids_with_open_at_login();
    assert_eq!(ids.len(), menu_ids().len() + 1);
    assert_eq!(&ids[ids.len() - 2..], [OPEN_AT_LOGIN, "quit"]);
    let without: Vec<_> = ids
        .iter()
        .copied()
        .filter(|id| *id != OPEN_AT_LOGIN)
        .collect();
    assert_eq!(without, menu_ids(), "the default order is unchanged");
    assert!(!menu_ids().contains(&OPEN_AT_LOGIN));
}
