#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;

fn server() -> std::process::Command {
    child_process::command(if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    })
}

fn rejected(args: &[&str], message: &str) {
    let output = server()
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

#[cfg(not(feature = "desktop_rebuild"))]
#[test]
fn import_needs_a_rebuild_build() {
    rejected(
        &["import", "--from", "unused", "--dry-run"],
        "needs a build with the desktop_rebuild feature",
    );
}

/// `pp-server import --from <folder> --dry-run` with the runtime switch set
/// to `switch`: the exit code, stdout and stderr.
#[cfg(feature = "desktop_rebuild")]
fn import_dry_run(switch: &str, folder: &std::path::Path) -> (Option<i32>, String, String) {
    let output = server()
        .env("PP_DESKTOP_REBUILD", switch)
        .args(["import", "--dry-run", "--from"])
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

    let (code, report, error) = import_dry_run("0", node.path());
    assert_eq!((code, report.as_str()), (Some(1), ""));
    assert!(error.contains("needs PP_DESKTOP_REBUILD=1"), "{error}");

    let rust = Installation::create(Fixture::RustMarked).unwrap();
    let (code, report, error) = import_dry_run("1", rust.path());
    assert_eq!((code, report.as_str()), (Some(1), ""));
    assert!(
        error.starts_with("import refused: The folder has a .pp-rust-owned.json entry"),
        "{error}"
    );
    let (code, _, error) = import_dry_run("1", &rust.path().join("missing"));
    assert_eq!(code, Some(1));
    assert!(error.starts_with("import failed: Read "), "{error}");
}
