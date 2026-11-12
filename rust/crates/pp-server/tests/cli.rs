#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;

fn binary() -> std::path::PathBuf {
    if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    }
}

fn rejected(args: &[&str], message: &str) {
    let output = child_process::command(binary())
        .args(args)
        .output()
        .expect("run the gateway binary");
    assert!(!output.status.success(), "invalid arguments must fail");
    assert!(output.stdout.is_empty(), "must not announce an origin");
    let error = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(error.contains(message), "{error}");
}

#[test]
fn missing_web_argument_is_rejected_before_startup() {
    rejected(&[], "--web is required");
}

#[test]
fn missing_argument_value_is_rejected_before_startup() {
    rejected(&["--data"], "Argument value missing");
}

#[test]
fn unknown_argument_is_rejected_before_startup() {
    rejected(&["--unknown", "value"], "Unknown argument");
}

#[test]
fn malformed_listener_is_rejected_before_opening_paths() {
    let binary = binary();
    for address in [
        "",
        "localhost:8080",
        "0.0.0.0",
        "0.0.0.0:99999",
        "[::ffff:0.0.0.0]:8080",
        "[::ffff:224.0.0.1]:8080",
        "224.0.0.1:8080",
        "[ff02::1]:8080",
    ] {
        let output = child_process::command(&binary)
            .env("PP_LISTEN_ADDR", address)
            .args(["--web", "/path-that-must-not-be-opened"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid PP_LISTEN_ADDR"));
    }
}

#[cfg(not(feature = "desktop_rebuild"))]
#[test]
fn import_needs_a_rebuild_build() {
    let arguments = ["import", "--from", "unused", "--dry-run"];
    rejected(&arguments, "needs a build with the desktop_rebuild feature");
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn import_arguments_are_checked_before_the_source_is_read() {
    rejected(&["import", "--dry-run"], "--from is required");
    rejected(&["import", "--from"], "--from needs a value");
    rejected(
        &["import", "--from", "s"],
        "--data is required without --dry-run",
    );
    rejected(
        &["import", "--from", "s", "--dry-run", "--force"],
        "Unknown argument",
    );
}

/// `pp-server import --from <folder> --data unused --dry-run` with the
/// runtime switch set to `switch`: the exit code, stdout and stderr.
#[cfg(feature = "desktop_rebuild")]
fn import_dry_run(switch: &str, folder: &std::path::Path) -> (Option<i32>, String, String) {
    let output = child_process::command(binary())
        .env("PP_DESKTOP_REBUILD", switch)
        .args(["import", "--dry-run", "--data", "unused", "--from"])
        .arg(folder)
        .output()
        .expect("run pp-server import");
    let text = |bytes| String::from_utf8(bytes).expect("UTF-8 output");
    (
        output.status.code(),
        text(output.stdout),
        text(output.stderr),
    )
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn import_dry_run_exits_0_with_the_report_and_1_with_the_reason() {
    use pp_testkit::{Fixture, Installation, node_installation};
    let node = Installation::create(Fixture::NodeSchema34).unwrap();
    node_installation::populate(node.path()).unwrap();
    let (code, report, error) = import_dry_run("1", node.path());
    assert_eq!((code, error.as_str()), (Some(0), ""), "{report}");
    assert!(
        report.contains("3 Builds, 2 Sources, 2 printers, 2 unmatched prints."),
        "{report}"
    );
    assert!(report.contains("The destination is absent or empty, as the import needs."));
    assert!(
        !std::path::Path::new("unused").exists(),
        "a dry run creates no destination"
    );

    // Without --dry-run the same folder imports into a new one.
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let output = child_process::command(binary())
        .env("PP_DESKTOP_REBUILD", "1")
        .args(["import", "--from"])
        .arg(node.path())
        .arg("--data")
        .arg(&destination)
        .output()
        .expect("run pp-server import");
    let summary = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success() && summary.contains("complete: false."),
        "{summary}"
    );
    assert!(destination.join(".pp-rust-owned.json").is_file());
    assert!(destination.join("import-report.json").is_file());

    let (code, report, error) = import_dry_run("0", node.path());
    assert_eq!((code, report.as_str()), (Some(1), ""));
    assert!(error.contains("needs PP_DESKTOP_REBUILD=1"), "{error}");

    let rust = Installation::create(Fixture::RustMarked).unwrap();
    let (code, report, error) = import_dry_run("1", rust.path());
    assert_eq!((code, report.as_str()), (Some(1), ""));
    assert!(
        error.starts_with("import refused: The folder has a .pp-rust-owned.json"),
        "{error}"
    );
    let (code, _, error) = import_dry_run("1", &rust.path().join("missing"));
    assert_eq!(code, Some(1));
    assert!(
        error.starts_with("import failed: ") && error.contains("missing: "),
        "{error}"
    );
}
