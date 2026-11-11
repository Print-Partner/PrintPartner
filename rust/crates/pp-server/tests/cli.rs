#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;

fn rejected(args: &[&str], message: &str) {
    let binary = if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    };
    let output = child_process::command(binary)
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
    let binary = if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    };
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
