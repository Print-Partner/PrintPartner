use super::*;
fn rows(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect()
}

#[tokio::test]
async fn ticket_t_13_compat_capture_writes_scrubbed_pino_and_text_to_compat_log() {
    let scratch = Scratch::new();
    let logs = Logs::open(scratch.0.join("logs"))
        .unwrap()
        .with_secret("PLANTED_RUNTIME_KEY_ab12cd34");
    let output = concat!(
        "{\"level\":30,\"time\":1,\"msg\":\"request completed\",\"req\":{\"url\":\"/desktop?bootstrap=PLANTED_BOOTSTRAP_e1\",",
        "\"headers\":{\"authorization\":\"Bearer PLANTED_BEARER_1f9c\",\"cookie\":\"pp_session=PLANTED_COOKIE_77aa\",",
        "\"x-pp-principal\":\"PLANTED_PRINCIPAL_5d\",\"accept\":\"text/html\"}}}\n",
        "{\"level\":50,\"time\":2,\"msg\":\"spoolman sync failed\",\"err\":{\"type\":\"Error\",",
        "\"message\":\"GET http://spoolman.local/api?api_key=PLANTED_API_KEY_beef\",",
        "\"stack\":\"Error: key PLANTED_RUNTIME_KEY_ab12cd34\\n    at sync.js:4\"}}\n",
        "plain stderr Authorization: Bearer PLANTED_BEARER_1f9c\n",
        "{\"level\":40,\"time\":3,\"msg\":\"unterminated cookie: PLANTED_COOKIE_77aa\"}",
    );
    logs.capture(output.as_bytes()).await;
    let compat = std::fs::read_to_string(scratch.0.join("logs/compat.log")).unwrap();
    assert_clean(&compat);
    let rows = rows(&scratch.0.join("logs/compat.log"));
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["msg"], "request completed");
    assert_eq!(rows[0]["req"]["url"], "/desktop?bootstrap=[REDACTED]");
    assert_eq!(rows[0]["req"]["headers"]["authorization"], redact::REDACTED);
    assert_eq!(rows[0]["req"]["headers"]["accept"], "text/html");
    assert_eq!(rows[1]["level"], 50);
    assert_eq!(
        rows[1]["err"]["message"],
        "GET http://spoolman.local/api?api_key=[REDACTED]"
    );
    assert_eq!(rows[2]["level"], 30);
    assert_eq!(rows[2]["msg"], "plain stderr Authorization: [REDACTED]");
    assert_eq!(rows[3]["level"], 40);
    assert!(private(&scratch.0.join("logs/compat.log")));
    let events = std::fs::read_to_string(scratch.0.join("logs/desktop.jsonl")).unwrap();
    assert_eq!(events.lines().count(), 4);
    assert_clean(&events);
    assert_clean(&recent_diagnostics(&scratch.0).unwrap());
}

#[tokio::test]
async fn ticket_t_13_capture_bounds_oversized_lines() {
    let scratch = Scratch::new();
    let logs = Logs::open(scratch.0.join("logs")).unwrap();
    let mut output = vec![b'a'; 1024 * 1024];
    output.extend_from_slice(b"\n{\"level\":30,\"msg\":\"after\"}\n");
    logs.capture(&output[..]).await;
    let rows = rows(&scratch.0.join("logs/compat.log"));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["msg"].as_str().unwrap().len(), LINE_BYTES);
    assert_eq!(rows[1]["msg"], "after");
}

#[test]
fn ticket_t_13_core_log_records_scrubbed_tracing_events_and_error_chains() {
    let scratch = Scratch::new();
    let core = CoreLog::open(&scratch.0).unwrap();
    let error = anyhow::anyhow!("cookie: pp_session=PLANTED_COOKIE_77aa")
        .context("GET /desktop?bootstrap=PLANTED_BOOTSTRAP_e1")
        .context("Compatibility request failed");
    tracing::subscriber::with_default(core, || {
        tracing::debug!("not recorded below info");
        tracing::error!(
            error = AsRef::<dyn std::error::Error + 'static>::as_ref(&error),
            api_key = "PLANTED_API_KEY_beef",
            attempt = 3u64,
            "proxy rejected Authorization: Bearer PLANTED_BEARER_1f9c"
        );
        tracing::info!(target: "pp_core::startup", "core ready");
    });
    let text = std::fs::read_to_string(scratch.0.join("logs/core.log")).unwrap();
    assert_clean(&text);
    let rows = rows(&scratch.0.join("logs/core.log"));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["level"], 50);
    assert_eq!(rows[0]["target"], module_path!());
    assert_eq!(
        rows[0]["message"],
        "proxy rejected Authorization: [REDACTED]"
    );
    assert_eq!(
        rows[0]["error"],
        "Compatibility request failed: GET /desktop?bootstrap=[REDACTED]: cookie: [REDACTED]"
    );
    assert_eq!(rows[0]["api_key"], redact::REDACTED);
    assert_eq!(rows[0]["attempt"], 3);
    assert_eq!(rows[1]["level"], 30);
    assert_eq!(rows[1]["target"], "pp_core::startup");
    assert!(private(&scratch.0.join("logs/core.log")));
}

#[test]
fn gre_1006_core_log_redacts_each_sensitive_field_kind() {
    let scratch = Scratch::new();
    let core = CoreLog::open(&scratch.0).unwrap();
    let headers = std::collections::BTreeMap::from([
        ("authorization", "token PLANTED_HDR_AUTH_01"),
        ("cookie", "pp_session=PLANTED_HDR_COOKIE_02"),
        ("x-api-key", "PLANTED_HDR_APIKEY_03"),
    ]);
    tracing::subscriber::with_default(core, || {
        tracing::warn!(
            password = "PLANTED_FIELD_PASSWORD",
            token = "PLANTED_FIELD_TOKEN",
            cookie = "PLANTED_FIELD_COOKIE",
            authorization = "PLANTED_FIELD_AUTHZ",
            webhook = "PLANTED_FIELD_WEBHOOK",
            api_key = "PLANTED_FIELD_API_KEY",
            github_pat = "PLANTED_FIELD_GITHUB_PAT",
            api_keys_v1 = "PLANTED_FIELD_API_KEYS_V1",
            discord_webhook_url = "PLANTED_FIELD_DISCORD_URL",
            "fields"
        );
        tracing::warn!(
            url = "https://discord.com/api/webhooks/1234/PLANTED_URL_WEBHOOK",
            "POST https://u:PLANTED_URL_PASSWORD@host/x?token=PLANTED_URL_TOKEN&api_key=PLANTED_URL_API_KEY"
        );
        tracing::warn!(?headers, "upstream request");
        tracing::warn!("Cookie: a=PLANTED_LINE_COOKIE; Authorization: Bearer PLANTED_LINE_BEARER");
    });
    let text = std::fs::read_to_string(scratch.0.join("logs/core.log")).unwrap();
    assert_eq!(text.matches("PLANTED_").count(), 0, "secret leaked: {text}");
    let rows = rows(&scratch.0.join("logs/core.log"));
    assert_eq!(rows.len(), 4);
    for name in [
        "password",
        "token",
        "cookie",
        "authorization",
        "webhook",
        "api_key",
        "github_pat",
        "api_keys_v1",
        "discord_webhook_url",
    ] {
        assert_eq!(rows[0][name], redact::REDACTED, "{name}");
    }
    assert_eq!(rows[0]["message"], "fields");
    assert_eq!(
        rows[1]["url"],
        "https://discord.com/api/webhooks/1234/[REDACTED]"
    );
    assert_eq!(
        rows[1]["message"],
        "POST https://[REDACTED]@host/x?token=[REDACTED]&api_key=[REDACTED]"
    );
    assert_eq!(
        rows[2]["headers"],
        r#"{"authorization": "[REDACTED]", "cookie": "[REDACTED]", "x-api-key": "[REDACTED]"}"#
    );
    assert_eq!(rows[3]["message"], "Cookie: [REDACTED]");
}

#[test]
fn ticket_t_13_core_log_truncates_oversized_rows() {
    let scratch = Scratch::new();
    let core = CoreLog::open(&scratch.0).unwrap();
    let huge = "x".repeat(1024 * 1024);
    tracing::subscriber::with_default(core, || tracing::warn!("{huge}"));
    let rows = rows(&scratch.0.join("logs/core.log"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["truncated"], true);
    assert_eq!(rows[0]["message"].as_str().unwrap().len(), LINE_BYTES / 2);
    let scratch = Scratch::new();
    let core = CoreLog::open(&scratch.0).unwrap();
    let wide = "\u{20ac}".repeat(1024 * 1024);
    tracing::subscriber::with_default(core, || tracing::warn!("{wide}"));
    let text = std::fs::read_to_string(scratch.0.join("logs/core.log")).unwrap();
    assert!(text.len() <= LINE_BYTES);
    let row: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(row["truncated"], true);
}

fn stream_files(directory: &Path, stream: Stream) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(&format!("{}.", stream.stem)))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn ticket_t_13_forced_rotation_keeps_seven_files_for_each_source() {
    let scratch = Scratch::new();
    let directory = scratch.0.join("logs");
    let logs = Logs::open(directory.clone()).unwrap();
    let core = CoreLog::open(&scratch.0).unwrap();
    for round in 0..10u64 {
        let size = round % 2 == 0;
        {
            let mut compat = logs.compat.lock().unwrap();
            let mut sink = core.sink.lock().unwrap();
            let Sink::File(core_file) = &mut *sink else {
                panic!("core log not attached")
            };
            for log in [&mut *compat, core_file] {
                if size {
                    log.bytes = FILE_BYTES;
                } else {
                    log.day = 0;
                }
            }
        }
        logs.capture(format!("{{\"level\":30,\"msg\":\"round {round}\"}}\n").as_bytes())
            .await;
        tracing::subscriber::with_default(core.clone(), || tracing::info!("round {round}"));
    }
    for (stream, field) in [(COMPAT, "msg"), (CORE, "message")] {
        let expected: Vec<String> = std::iter::once(format!("{}.log", stream.stem))
            .chain((1..=ARCHIVES).map(|index| format!("{}.{index}.log", stream.stem)))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(stream_files(&directory, stream), expected);
        assert_eq!(
            rows(&directory.join(format!("{}.log", stream.stem)))[0][field],
            "round 9"
        );
        for index in 1..=ARCHIVES {
            let archive = directory.join(format!("{}.{index}.log", stream.stem));
            assert_eq!(rows(&archive)[0][field], format!("round {}", 9 - index));
            assert!(private(&archive));
        }
    }
}

#[test]
fn ticket_t_13_panic_writes_local_crash_record_without_telemetry() {
    let scratch = Scratch::new();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "logs::tests::core::core_log_panic_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("PP_T13_PANIC_DATA", &scratch.0)
        .env_remove("SENTRY_DSN")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = std::fs::read_to_string(scratch.0.join("logs/core.log")).unwrap();
    assert_clean(&text);
    let rows = rows(&scratch.0.join("logs/core.log"));
    let panic = rows.iter().find(|row| row["target"] == "panic").unwrap();
    assert_eq!(panic["level"], 60);
    assert_eq!(panic["message"], "renderer bridge lost token=[REDACTED]");
    assert!(
        panic["location"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}:", file!()))
    );
    assert_eq!(rows[0]["message"], "before admission session_id=[REDACTED]");
    assert_eq!(rows[0]["level"], 40);
    assert_eq!(rows[1]["message"], "before crash");
}

#[test]
fn ticket_t_13_core_log_holds_early_rows_until_attach_and_bounds_them() {
    let scratch = Scratch::new();
    let core = CoreLog::buffered();
    tracing::subscriber::with_default(core.clone(), || {
        tracing::warn!("cutover_evidence_unsaved");
        for index in 0..PENDING_ROWS + 10 {
            tracing::info!("early {index}");
        }
    });
    assert!(!scratch.0.join("logs/core.log").exists());
    core.attach(&scratch.0).unwrap();
    tracing::subscriber::with_default(core, || tracing::info!("attached"));
    let rows = rows(&scratch.0.join("logs/core.log"));
    assert_eq!(rows.len(), PENDING_ROWS + 2);
    assert_eq!(rows[0]["message"], "core log dropped 11 early rows");
    assert_eq!(rows[0]["level"], 40);
    assert_eq!(rows[1]["message"], "early 10");
    assert_eq!(
        rows[PENDING_ROWS]["message"],
        format!("early {}", PENDING_ROWS + 9)
    );
    assert_eq!(rows[PENDING_ROWS + 1]["message"], "attached");
}

#[test]
#[ignore = "runs in an isolated subprocess and panics on purpose"]
fn core_log_panic_child() {
    let data = PathBuf::from(std::env::var_os("PP_T13_PANIC_DATA").unwrap());
    let log = CoreLog::buffered();
    assert!(log.install());
    tracing::warn!("before admission session_id={}", "PLANTED_SESSION_ID_77");
    log.attach(&data).unwrap();
    tracing::info!("before crash");
    panic!("renderer bridge lost token={}", "PLANTED_PANIC_TOKEN");
}

#[tokio::test]
async fn ticket_t_13_log_view_shows_compat_and_core_sources_scrubbed() {
    let scratch = Scratch::new();
    let logs = Logs::open(scratch.0.join("logs")).unwrap();
    // A row written before slice 1 scrubbed compat.log: the view scrubs again.
    std::fs::write(
        scratch.0.join("logs/compat.1.log"),
        "{\"level\":30,\"time\":1000,\"msg\":\"legacy Authorization: Bearer PLANTED_BEARER_1f9c\"}\n",
    )
    .unwrap();
    logs.capture(
        &b"{\"level\":40,\"time\":2000,\"msg\":\"sync slow\",\"cookie\":\"PLANTED_COOKIE_77aa\"}\n"
            [..],
    )
    .await;
    let core = CoreLog::open(&scratch.0).unwrap();
    tracing::subscriber::with_default(core, || {
        tracing::error!(api_key = "PLANTED_API_KEY_beef", "printer bridge lost");
    });
    std::fs::OpenOptions::new()
        .append(true)
        .open(scratch.0.join("logs/core.log"))
        .unwrap()
        .write_all(b"{\"level\":99,\"message\":\"bad level\"}\nnot json\n")
        .unwrap();

    let text = recent_logs(&scratch.0).unwrap();
    assert_clean(&text);
    let events = text.find("Service events (desktop.jsonl)").unwrap();
    let compat = text.find("Compat backend (compat.log)").unwrap();
    let core = text.find("Core (core.log)").unwrap();
    assert!(events < compat && compat < core, "{text}");
    let section = |start: usize, end: usize| -> Vec<serde_json::Value> {
        text[start..end]
            .lines()
            .filter(|row| row.starts_with('{'))
            .map(|row| serde_json::from_str(row).unwrap())
            .collect()
    };
    let compat_rows = section(compat, core);
    assert_eq!(compat_rows.len(), 2);
    assert_eq!(compat_rows[0]["msg"], "legacy Authorization: [REDACTED]");
    assert_eq!(compat_rows[1]["msg"], "sync slow");
    assert_eq!(compat_rows[1]["cookie"], redact::REDACTED);
    let core_rows = section(core, text.len());
    assert_eq!(core_rows.len(), 1);
    assert_eq!(core_rows[0]["message"], "printer bridge lost");
    assert_eq!(core_rows[0]["api_key"], redact::REDACTED);
    assert!(text[core..].contains("Older or invalid records were omitted."));
    assert_eq!(section(events, compat).len(), 1);
}

#[test]
fn ticket_t_13_log_view_skips_symlinked_sources_and_reports_empty_ones() {
    let scratch = Scratch::new();
    let outside = scratch.0.join("outside.log");
    std::fs::write(&outside, "{\"level\":30,\"msg\":\"SECRET_SYMLINK\"}\n").unwrap();
    symlink_file(&outside, &scratch.0.join("logs/compat.log"));
    let text = recent_logs(&scratch.0).unwrap();
    assert!(!text.contains("SECRET_SYMLINK"), "{text}");
    let compat = text.find("Compat backend (compat.log)").unwrap();
    let core = text.find("Core (core.log)").unwrap();
    assert!(text[compat..core].contains("Older or invalid records were omitted."));
    assert!(text[core..].contains("No records yet."));
    let linked = scratch.0.join("linked-data");
    std::fs::create_dir(&linked).unwrap();
    symlink_dir(&scratch.0.join("logs"), &linked.join("logs"));
    assert_eq!(
        recent_logs(&linked).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[test]
fn ticket_t_13_log_folder_admits_only_the_owned_logs_directory() {
    let scratch = Scratch::new();
    restrict(&scratch.0.join("logs"));
    assert_eq!(log_folder(&scratch.0).unwrap(), scratch.0.join("logs"));

    let linked = scratch.0.join("linked-data");
    std::fs::create_dir(&linked).unwrap();
    symlink_dir(&scratch.0.join("logs"), &linked.join("logs"));
    assert_eq!(
        log_folder(&linked).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );

    let file = scratch.0.join("file-data");
    std::fs::create_dir(&file).unwrap();
    std::fs::write(file.join("logs"), "").unwrap();
    assert_eq!(
        log_folder(&file).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );

    let missing = scratch.0.join("missing-data");
    assert_eq!(
        log_folder(&missing).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    // A relative path could reach the opener as an option such as `-R`.
    assert_eq!(
        log_folder(Path::new("-data")).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    // Group or world access means another account could plant files.
    widen(&scratch.0.join("logs"));
    assert_eq!(
        log_folder(&scratch.0).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}
