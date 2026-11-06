use pp_gateway::ownership::{
    AdmissionError, Owner, SOURCE_SYNC_PARTICIPANTS, admit_bundled_source_sync, admit_observed,
    admit_planted_sections, boot_source_sync, fingerprint_tree,
};
#[cfg(unix)]
use std::os::unix::fs::symlink as symlink_file;
#[cfg(windows)]
use std::os::windows::fs::symlink_file;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn scratch(name: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dir = base.join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn bundled_text() -> &'static str {
    include_str!("../../../../web/apps/server/src/data/source-sync-ownership.json")
}

fn bundled_value() -> serde_json::Value {
    serde_json::from_str(bundled_text()).unwrap()
}

fn set_participant_owner(value: &mut serde_json::Value, id: &str, owner: &str) {
    let participants = value["participants"].as_array_mut().unwrap();
    let participant = participants
        .iter_mut()
        .find(|item| item["id"] == id)
        .unwrap_or_else(|| panic!("missing participant {id}"));
    participant["owner"] = serde_json::json!(owner);
}

fn set_participant_resources(value: &mut serde_json::Value, id: &str, resources: &[&str]) {
    let participants = value["participants"].as_array_mut().unwrap();
    let participant = participants
        .iter_mut()
        .find(|item| item["id"] == id)
        .unwrap();
    participant["resources"] = serde_json::json!(resources);
}

fn remove_participant(value: &mut serde_json::Value, id: &str) {
    let participants = value["participants"].as_array_mut().unwrap();
    participants.retain(|item| item["id"] != id);
}

fn plant_tree(dir: &Path) {
    fs::write(dir.join("state.db"), b"db-bytes").unwrap();
    fs::write(dir.join("state.db-wal"), b"wal-bytes").unwrap();
    fs::create_dir_all(dir.join("sources")).unwrap();
    fs::write(dir.join("sources/fixture.txt"), b"source-bytes").unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn ticket_t_7_ownership_bundled_manifest_admits_node() {
    let admitted = admit_bundled_source_sync().unwrap();
    assert_eq!(admitted.operation, "source.sync");
    assert_eq!(admitted.owner, Owner::Node);
    assert_eq!(
        admitted.participant_ids.len(),
        SOURCE_SYNC_PARTICIPANTS.len()
    );
    for id in SOURCE_SYNC_PARTICIPANTS {
        assert!(admitted.participant_ids.contains(*id), "{id}");
    }
    let value = bundled_value();
    let worker = value["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "worker.startSourceWatcher")
        .unwrap();
    for resource in ["table:app_settings", "table:app_events"] {
        assert!(
            worker["resources"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(resource)),
            "{resource}"
        );
    }
}

#[test]
fn ticket_t_7_ownership_participant_kind_mismatch_preserves_tree() {
    let dir = scratch("ticket_t_7_ownership_wrong_kind");
    plant_tree(&dir);
    let before = fingerprint_tree(&dir).unwrap();
    for id in SOURCE_SYNC_PARTICIPANTS {
        let mut value = bundled_value();
        let participant = value["participants"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item["id"] == *id)
            .unwrap();
        participant["kind"] = serde_json::json!(if participant["kind"] == "job" {
            "http"
        } else {
            "job"
        });
        let error = boot_source_sync(&dir, &serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(
            matches!(error, AdmissionError::ParticipantKindMismatch { .. }),
            "{id}: {error}"
        );
        assert_eq!(fingerprint_tree(&dir).unwrap(), before, "{id}");
    }
    assert!(!dir.join("source-sync-admission.json").exists());
}

#[test]
fn ticket_t_7_ownership_conflicting_worker_startup_rejected() {
    let mut value = bundled_value();
    set_participant_owner(&mut value, "worker.startSourceWatcher", "rust");
    let error = admit_observed(
        &serde_json::to_string(&value).unwrap(),
        SOURCE_SYNC_PARTICIPANTS,
    )
    .unwrap_err();
    assert!(
        matches!(error, AdmissionError::ConflictingParticipant { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("conflicting ownership"),
        "{error}"
    );
}

#[test]
fn ticket_t_7_ownership_unknown_owner_fail_closed() {
    let mut value = bundled_value();
    set_participant_owner(&mut value, "job.sync", "postgres");
    let error = admit_observed(
        &serde_json::to_string(&value).unwrap(),
        SOURCE_SYNC_PARTICIPANTS,
    )
    .unwrap_err();
    assert!(
        matches!(error, AdmissionError::UnknownOwner { ref value } if value == "postgres"),
        "{error}"
    );
}

#[test]
fn ticket_t_7_ownership_unknown_operation_rejected() {
    let mut value = bundled_value();
    value["operation"] = serde_json::json!("source.publish");
    let error = admit_observed(&serde_json::to_string(&value).unwrap(), &[]).unwrap_err();
    assert!(
        matches!(error, AdmissionError::UnknownOperation { .. }),
        "{error}"
    );
}

#[test]
fn ticket_t_7_ownership_unknown_kind_rejected() {
    let mut value = bundled_value();
    value["participants"][0]["kind"] = serde_json::json!("cron");
    let error = admit_observed(&serde_json::to_string(&value).unwrap(), &[]).unwrap_err();
    assert!(
        matches!(error, AdmissionError::UnknownParticipantKind { .. }),
        "{error}"
    );
}

#[test]
fn ticket_t_7_ownership_unknown_rollback_fail_closed() {
    let mut value = bundled_value();
    value["live_format"]["rollback"] = serde_json::json!("best-effort");
    let error = admit_observed(&serde_json::to_string(&value).unwrap(), &[]).unwrap_err();
    assert!(
        matches!(error, AdmissionError::UnknownLiveFormat { .. }),
        "{error}"
    );
}

#[test]
fn ticket_t_7_ownership_unknown_format_preserves_tree() {
    let dir = scratch("ticket_t_7_ownership_unknown_format");
    plant_tree(&dir);
    let before = fingerprint_tree(&dir).unwrap();
    let mut value = bundled_value();
    value["live_format"]["id"] = serde_json::json!("source-revision-workspace-v2");
    let error = boot_source_sync(&dir, &serde_json::to_string(&value).unwrap()).unwrap_err();
    assert!(matches!(error, AdmissionError::UnknownLiveFormat { .. }));
    assert_eq!(fingerprint_tree(&dir).unwrap(), before);
    assert!(!dir.join("source-sync-admission.json").exists());
}

#[test]
fn ticket_t_7_ownership_all_rust_unsupported() {
    let mut value = bundled_value();
    value["owner"] = serde_json::json!("rust");
    for participant in value["participants"].as_array_mut().unwrap() {
        participant["owner"] = serde_json::json!("rust");
    }
    let error = admit_observed(
        &serde_json::to_string(&value).unwrap(),
        SOURCE_SYNC_PARTICIPANTS,
    )
    .unwrap_err();
    assert!(
        matches!(error, AdmissionError::UnsupportedRustOwner { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("Node remains"), "{error}");
}

#[test]
fn ticket_t_7_ownership_single_rust_participant_conflicts() {
    let mut value = bundled_value();
    value["participants"].as_array_mut().unwrap().push(serde_json::json!({
        "id": "job.disjoint-claim", "kind": "job", "owner": "rust", "resources": ["file:mcp-only"]
    }));
    let error = admit_observed(
        &serde_json::to_string(&value).unwrap(),
        SOURCE_SYNC_PARTICIPANTS,
    )
    .unwrap_err();
    assert!(
        matches!(error, AdmissionError::ConflictingParticipant { .. }),
        "{error}"
    );
}

#[test]
fn ticket_t_7_ownership_import_profile_claims_are_required() {
    let dir = scratch("ticket_t_7_ownership_profile_claims");
    plant_tree(&dir);
    let before = fingerprint_tree(&dir).unwrap();
    for id in SOURCE_SYNC_PARTICIPANTS.iter().filter(|id| {
        id.ends_with("/upload-zip")
            || id.ends_with("/upload-files")
            || **id == "assistant.propose_import_source_files"
    }) {
        let mut value = bundled_value();
        let participant = value["participants"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item["id"] == *id)
            .unwrap();
        participant["resources"]
            .as_array_mut()
            .unwrap()
            .retain(|resource| resource != "table:build_profiles");
        let error = boot_source_sync(&dir, &serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(
            matches!(error, AdmissionError::ResourceSetMismatch { .. }),
            "{id}: {error}"
        );
        assert_eq!(fingerprint_tree(&dir).unwrap(), before);
    }
    let mut value = bundled_value();
    value["participants"].as_array_mut().unwrap().push(serde_json::json!({
        "id": "job.other-profile-owner", "kind": "job", "owner": "rust", "resources": ["table:build_profiles"]
    }));
    let error = boot_source_sync(&dir, &serde_json::to_string(&value).unwrap()).unwrap_err();
    assert!(
        matches!(error, AdmissionError::ConflictingResource { ref resource } if resource == "table:build_profiles"),
        "{error}"
    );
    assert_eq!(fingerprint_tree(&dir).unwrap(), before);
}

#[test]
fn ticket_t_7_ownership_resource_mismatch_preserves_tree() {
    let dir = scratch("ticket_t_7_ownership_resources");
    plant_tree(&dir);
    let before = fingerprint_tree(&dir).unwrap();
    for id in SOURCE_SYNC_PARTICIPANTS {
        let mut value = bundled_value();
        set_participant_resources(&mut value, id, &["file:placeholder"]);
        let error = boot_source_sync(&dir, &serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(
            matches!(error, AdmissionError::ResourceSetMismatch { .. }),
            "{id}: {error}"
        );
        assert_eq!(fingerprint_tree(&dir).unwrap(), before);
    }
}

#[test]
fn ticket_t_7_ownership_planted_temporaries_do_not_overwrite_targets() {
    for kind in ["symlink", "hardlink"] {
        let dir = scratch(&format!("ticket_t_7_ownership_temporary_{kind}"));
        let target = dir.join("unrelated.txt");
        fs::write(&target, "untouched data").unwrap();
        let planted = dir.join(".source-sync-admission.json.tmp");
        if kind == "symlink" {
            symlink_file(&target, &planted).unwrap();
        } else {
            fs::hard_link(&target, &planted).unwrap();
        }
        boot_source_sync(&dir, bundled_text()).unwrap();
        assert_eq!(read(&target), "untouched data");
        let metadata = fs::symlink_metadata(dir.join("source-sync-admission.json")).unwrap();
        assert!(metadata.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        let temporary_count = fs::read_dir(&dir)
            .unwrap()
            .filter(|item| {
                item.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(temporary_count, 1);
    }
}

/// Unix also plants a socket and a FIFO, which Windows directories cannot hold.
#[test]
fn ticket_t_7_ownership_non_regular_receipts_are_preserved() {
    #[cfg(unix)]
    let platform_kinds = ["socket", "fifo"];
    #[cfg(windows)]
    let platform_kinds = [];
    for kind in ["dangling-symlink", "valid-symlink", "directory"]
        .into_iter()
        .chain(platform_kinds)
    {
        let dir = if kind == "socket" {
            let dir = PathBuf::from("/tmp").join(format!(
                "pp-receipt-{}",
                hex::encode(rand::random::<[u8; 8]>())
            ));
            fs::create_dir(&dir).unwrap();
            dir
        } else {
            scratch(&format!("ticket_t_7_ownership_receipt_{kind}"))
        };
        let outside = scratch(&format!("ticket_t_7_ownership_outside_{kind}"));
        boot_source_sync(&outside, bundled_text()).unwrap();
        let target = outside.join("source-sync-admission.json");
        let target_bytes = fs::read(&target).unwrap();
        let receipt_path = dir.join("source-sync-admission.json");
        match kind {
            "dangling-symlink" => symlink_file(outside.join("missing"), &receipt_path).unwrap(),
            "valid-symlink" => symlink_file(&target, &receipt_path).unwrap(),
            "directory" => fs::create_dir(&receipt_path).unwrap(),
            #[cfg(unix)]
            "fifo" => {
                use std::os::unix::ffi::OsStrExt;
                let path = std::ffi::CString::new(receipt_path.as_os_str().as_bytes()).unwrap();
                // SAFETY: path is a live, nul-terminated string for this syscall.
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            _ => {}
        }
        #[cfg(unix)]
        let _socket = (kind == "socket")
            .then(|| std::os::unix::net::UnixListener::bind(&receipt_path).unwrap());
        let entry = fs::symlink_metadata(&receipt_path).unwrap();
        let error = boot_source_sync(&dir, bundled_text()).unwrap_err();
        assert!(
            matches!(error, AdmissionError::Malformed(ref detail) if detail.contains("admission receipt is not a regular file")),
            "{kind}: {error}"
        );
        let after = fs::symlink_metadata(&receipt_path).unwrap();
        assert_eq!(after.file_type(), entry.file_type());
        assert_eq!(after.modified().unwrap(), entry.modified().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(after.ino(), entry.ino());
            assert_eq!(after.mode(), entry.mode());
        }
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(fs::read(&target).unwrap(), target_bytes);
        #[cfg(unix)]
        if kind == "socket" {
            drop(_socket);
            fs::remove_dir_all(&dir).unwrap();
        }
    }
}

#[test]
fn ticket_t_7_ownership_conflicting_resource_rejected() {
    let mut value = bundled_value();
    set_participant_owner(&mut value, "mcp.start_sync", "rust");
    let error = admit_observed(
        &serde_json::to_string(&value).unwrap(),
        SOURCE_SYNC_PARTICIPANTS,
    )
    .unwrap_err();
    assert!(
        matches!(error, AdmissionError::ConflictingResource { .. }),
        "{error}"
    );
}

#[test]
fn ticket_t_7_ownership_docs_and_covers_conflicts_rejected() {
    for resource in [
        "table:source_docs",
        "file:source-covers",
        "table:app_settings",
        "table:app_events",
    ] {
        let mut value = bundled_value();
        value["participants"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "job.external-claim", "kind": "job", "owner": "rust", "resources": [resource]
            }));
        let error = admit_observed(
            &serde_json::to_string(&value).unwrap(),
            SOURCE_SYNC_PARTICIPANTS,
        )
        .unwrap_err();
        assert!(
            matches!(error, AdmissionError::ConflictingResource { .. }),
            "{resource}: {error}"
        );
    }
}

#[test]
fn ticket_t_7_ownership_incompatible_rollback_preserves_tree() {
    let mut value = bundled_value();
    value["live_format"]["rollback"] = serde_json::json!("incompatible");
    let dir = scratch("ticket_t_7_ownership_incompatible");
    plant_tree(&dir);
    let before = fingerprint_tree(&dir).unwrap();
    let error = boot_source_sync(&dir, &serde_json::to_string(&value).unwrap()).unwrap_err();
    assert!(
        matches!(error, AdmissionError::IncompatibleRollback { .. }),
        "{error}"
    );
    assert_eq!(fingerprint_tree(&dir).unwrap(), before);
    assert!(!dir.join("source-sync-admission.json").exists());
    assert!(!dir.join(".source-sync-admission.json.tmp").exists());
    assert_eq!(fs::read(dir.join("state.db")).unwrap(), b"db-bytes");
    assert_eq!(fs::read(dir.join("state.db-wal")).unwrap(), b"wal-bytes");
    assert_eq!(
        fs::read(dir.join("sources/fixture.txt")).unwrap(),
        b"source-bytes"
    );
}

#[test]
fn ticket_t_7_ownership_missing_participant_rejected() {
    for id in ["startup.startSourceWatcher", "mcp.start_sync"] {
        let mut value = bundled_value();
        remove_participant(&mut value, id);
        let error = admit_observed(
            &serde_json::to_string(&value).unwrap(),
            SOURCE_SYNC_PARTICIPANTS,
        )
        .unwrap_err();
        assert!(
            matches!(error, AdmissionError::ParticipantSetMismatch { .. }),
            "{id}: {error}"
        );
    }
}

#[test]
fn ticket_t_7_ownership_restart_reuses_receipt() {
    let dir = scratch("ticket_t_7_ownership_restart");
    let first = boot_source_sync(&dir, bundled_text()).unwrap();
    let second = boot_source_sync(&dir, bundled_text()).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.operation, "source.sync");
    assert_eq!(first.owner, "node");
    assert_eq!(first.admission_count, 1);
    assert!(first.participant_count > 0);
    assert_eq!(first.worker_pid, std::process::id());
    let persisted: pp_gateway::ownership::AdmissionReceipt =
        serde_json::from_str(&read(&dir.join("source-sync-admission.json"))).unwrap();
    assert_eq!(persisted, first);
}

#[test]
fn ticket_t_7_ownership_failure_then_valid_boot() {
    let dir = scratch("ticket_t_7_ownership_failure_then_valid");
    plant_tree(&dir);
    let before = fingerprint_tree(&dir).unwrap();
    let mut value = bundled_value();
    value["owner"] = serde_json::json!("oracle");
    let error = boot_source_sync(&dir, &serde_json::to_string(&value).unwrap()).unwrap_err();
    assert!(
        matches!(error, AdmissionError::UnknownOwner { .. }),
        "{error}"
    );
    assert_eq!(fingerprint_tree(&dir).unwrap(), before);
    assert!(!dir.join("source-sync-admission.json").exists());
    let receipt = boot_source_sync(&dir, bundled_text()).unwrap();
    assert_eq!(receipt.admission_count, 1);
    assert_eq!(receipt.owner, "node");
    assert!(receipt.participant_count > 0);
}

#[test]
fn ticket_t_7_ownership_restart_manifest_changed() {
    let dir = scratch("ticket_t_7_ownership_manifest_changed");
    let first = boot_source_sync(&dir, bundled_text()).unwrap();
    let persisted = read(&dir.join("source-sync-admission.json"));
    let error = boot_source_sync(&dir, &format!("{}\n", bundled_text())).unwrap_err();
    assert!(
        matches!(error, AdmissionError::RestartManifestChanged),
        "{error}"
    );
    assert_eq!(read(&dir.join("source-sync-admission.json")), persisted);
    assert!(persisted.contains(&first.manifest_sha256));
}

#[test]
fn ticket_t_7_ownership_corrupt_receipt_preserves_bytes() {
    let admitted = admit_bundled_source_sync().unwrap();
    let valid = serde_json::json!({
        "operation": "source.sync",
        "owner": "node",
        "worker_pid": 123,
        "admission_count": 1,
        "participant_count": admitted.participant_ids.len(),
        "manifest_sha256": admitted.manifest_sha256,
    });
    let mut wrong_count = valid.clone();
    wrong_count["participant_count"] = serde_json::json!(999);
    let mut missing_pid = valid.clone();
    missing_pid.as_object_mut().unwrap().remove("worker_pid");
    let mut unknown = valid.clone();
    unknown["unexpected"] = serde_json::json!(true);
    let cases = [
        ("count", serde_json::to_string(&wrong_count).unwrap()),
        ("pid", serde_json::to_string(&missing_pid).unwrap()),
        ("null", "null".to_string()),
        ("unknown", serde_json::to_string(&unknown).unwrap()),
    ];
    for (name, body) in cases {
        let dir = scratch(&format!("ticket_t_7_ownership_corrupt_{name}"));
        plant_tree(&dir);
        let path = dir.join("source-sync-admission.json");
        fs::write(&path, &body).unwrap();
        let before = fingerprint_tree(&dir).unwrap();
        let error = boot_source_sync(&dir, bundled_text()).unwrap_err();
        assert!(
            matches!(error, AdmissionError::Malformed(ref detail) if detail.contains("admission receipt")),
            "{name}: {error}"
        );
        assert_eq!(read(&path), body, "{name}");
        assert!(!dir.join(".source-sync-admission.json.tmp").exists());
        assert_eq!(fingerprint_tree(&dir).unwrap(), before, "{name}");
    }
}

#[test]
fn ticket_t_7_ownership_planted_sections_conflict() {
    let document = serde_json::json!({
        "routes": [],
        "workers": [{"operation": "source.sync", "id": "startSourceWatcher", "owner": "rust"}],
        "startup": [{"operation": "source.sync", "id": "startSourceWatcher", "owner": "compat"}],
        "mcp": []
    });
    let error = admit_planted_sections(&document).unwrap_err();
    assert!(
        matches!(error, AdmissionError::ConflictingParticipant { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("conflicting ownership"),
        "{error}"
    );
}

#[test]
fn ticket_t_7_ownership_call_sites_match_manifest() {
    let root = repo_root();
    let jobs = read(&root.join("web/apps/server/src/routes/jobs.ts"));
    let sources = read(&root.join("web/apps/server/src/routes/sources.ts"));
    let tools = read(&root.join("web/apps/server/src/assistant/tools.ts"));
    let runner = read(&root.join("web/apps/server/src/services/job-runner.ts"));
    let watcher = read(&root.join("web/apps/server/src/services/source-watcher.ts"));
    let schema = read(&root.join("web/apps/server/src/db/schema.ts"));
    let workspace = read(&root.join("web/apps/server/src/services/source-workspace.ts"));
    let aliases = read(&root.join("web/apps/server/src/routes/api-v1.ts"));
    let gateway = read(&root.join("rust/crates/pp-gateway/src/lib.rs"));
    let app = read(&root.join("web/apps/server/src/app.ts"));
    let admission = read(&root.join("web/apps/server/src/ownership/source-sync-admission.ts"));

    let jobs_route = &jobs[jobs.find("app.post(\"/jobs/sync\"").unwrap()..];
    let start_job = jobs_route.find("jobs.start(\"sync\"").unwrap();
    assert!(
        jobs_route[..start_job].contains("requireSourceSyncParticipant(\"http.POST /jobs/sync\")")
    );
    assert!(
        jobs_route[..start_job]
            .contains("requireSourceSyncParticipant(\"http.POST /api/v1/jobs/sync\")")
    );
    assert!(aliases.contains("registerCoreRoutes"));

    let sources_route = &sources[sources.find("app.post(\"/sources\"").unwrap()..];
    let create = sources_route.find("createSource(").unwrap();
    assert!(
        create
            > sources_route
                .find("requireSourceSyncParticipant(\"http.POST /sources\")")
                .unwrap()
    );
    assert!(
        create
            > sources_route
                .find("requireSourceSyncParticipant(\"http.POST /api/v1/sources\")")
                .unwrap()
    );

    let assistant = read(&root.join("web/apps/server/src/routes/assistant.ts"));
    let apply_route = &assistant[assistant.find("\"/assistant/actions/apply\"").unwrap()..];
    let apply_call = apply_route.find("applyAssistantAction(").unwrap();
    assert!(
        apply_route[..apply_call]
            .contains("requireSourceSyncParticipant(\"http.POST /assistant/actions/apply\")")
    );
    assert!(
        apply_route[..apply_call].contains(
            "requireSourceSyncParticipant(\"http.POST /api/v1/assistant/actions/apply\")"
        )
    );

    let sync_case = &tools[tools.find("case \"start_sync\"").unwrap()..];
    let dispatch = sync_case.find("jobs.start(\"sync\"").unwrap();
    assert!(sync_case[..dispatch].contains("requireSourceSyncParticipant(\"mcp.start_sync\")"));

    let start_fn = runner.find("async start(").unwrap();
    let queued = runner[start_fn..].find("randomUUID").unwrap();
    assert!(
        runner[start_fn..start_fn + queued].contains("requireSourceSyncParticipant(\"job.sync\")")
    );
    let run_sync = runner.find("this.runSync").unwrap();
    assert!(runner[..run_sync].contains("requireSourceSyncParticipant(\"job.sync\")"));

    let watcher_fn = watcher.find("export function startSourceWatcher").unwrap();
    let body = &watcher[watcher_fn..];
    let startup = body
        .find("requireSourceSyncParticipant(\"startup.startSourceWatcher\")")
        .unwrap();
    let worker = body
        .find("requireSourceSyncParticipant(\"worker.startSourceWatcher\")")
        .unwrap();
    let timeout = body.find("setTimeout").unwrap();
    let interval = body.find("setInterval").unwrap();
    assert!(startup < timeout && worker < timeout);
    assert!(startup < interval && worker < interval);

    assert!(schema.contains("\"projects\""));
    assert!(schema.contains("\"source_revisions\""));
    assert!(workspace.contains("function sourceWorkspaceRoot"));
    assert!(gateway.contains("ownership::admit_bundled_source_sync()"));
    assert!(!gateway.contains("boot_source_sync("));
    let core = read(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../pp-core/src/lib.rs"));
    let admitted = core.find("boot_bundled_source_sync(").unwrap();
    let bound = core
        .find("let (listener, origin_change) = bind_origin(")
        .expect("core startup must bind the origin and capture its change disclosure");
    let supervised = core.find("Supervisor::start_with_config(").unwrap();
    assert!(admitted < bound && bound < supervised);
    let admit_at = app.find("beginSourceSyncAdmission(").unwrap();
    let watcher_at = app.find("startSourceWatcher(").unwrap();
    assert!(admit_at < watcher_at);
    let start_server = &app[app.find("export async function startServer").unwrap()..];
    let startup_admit = start_server.find("beginSourceSyncAdmission(").unwrap();
    let upgrade = start_server.find("prepareSqliteUpgrade(").unwrap();
    let connect = start_server.find("ports.db.connect(").unwrap();
    assert!(startup_admit < upgrade && upgrade < connect);
    let stdio = read(&root.join("web/apps/server/src/mcp/stdio-server.ts"));
    let stdio_admit = stdio.find("beginSourceSyncAdmission(").unwrap();
    let stdio_connect = stdio.find("ports.db.connect(").unwrap();
    let stdio_runner = stdio.find("createJobRunner(").unwrap();
    assert!(stdio_admit < stdio_connect && stdio_connect < stdio_runner);
    assert!(admission.contains("../data/source-sync-ownership.json"));
    assert!(!admission.contains("rust/crates/pp-gateway/source-sync-ownership.json"));

    let manifest = bundled_value();
    let mut tables = Vec::new();
    for participant in manifest["participants"].as_array().unwrap() {
        for resource in participant["resources"].as_array().unwrap() {
            let resource = resource.as_str().unwrap();
            if let Some(name) = resource.strip_prefix("table:") {
                tables.push(name.to_string());
            } else {
                assert!(resource.starts_with("file:"), "{resource}");
            }
        }
    }
    assert!(tables.contains(&"projects".to_string()));
    assert!(tables.contains(&"source_revisions".to_string()));
}
