use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use pp_core::{CoreRuntime, CoreStatus, SupervisorConfig};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

// Run 38941881212: an idle macOS respawn hash took 10.7–19.7s after Starting,
// and the 30s readiness probe starts only after that hash. A 20s wait expired
// during the hash.
const RESPAWN_READY_FUSE: Duration = Duration::from_secs(50);

#[path = "runtime_m0/drain.rs"]
mod drain;

#[path = "runtime_m0/launcher_start.rs"]
mod launcher_start;

#[cfg(feature = "desktop_rebuild")]
#[path = "runtime_m0/watcher_proof.rs"]
mod watcher_proof;

#[cfg(feature = "desktop_rebuild")]
#[path = "runtime_m0/farm_reads.rs"]
mod farm_reads;

#[cfg(feature = "desktop_rebuild")]
#[path = "runtime_m0/printer_stream.rs"]
mod printer_stream;

#[cfg(feature = "desktop_rebuild")]
#[path = "runtime_m0/printer_host.rs"]
mod printer_host;

#[cfg(feature = "desktop_rebuild")]
#[path = "runtime_m0/bambu_pin.rs"]
mod bambu_pin;

#[path = "support/process.rs"]
mod process;

#[path = "support/process_tree.rs"]
mod process_tree;

#[path = "support/release.rs"]
mod release;
use release::{launch, launch_from_manifest};

#[cfg(target_os = "linux")]
#[path = "runtime_m0/desktop_account_mapping.rs"]
mod desktop_account_mapping;

/// The Node child's status channel of a Node-backed runtime.
fn compat(runtime: &CoreRuntime) -> tokio::sync::watch::Receiver<CoreStatus> {
    runtime.handle().compat_status().unwrap()
}

async fn request(
    origin: &str,
    path: &str,
    method: &str,
    cookie: Option<&str>,
    host: Option<&str>,
    foreign: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let authority = origin.strip_prefix("http://").unwrap();
    let socket = tokio::net::TcpStream::connect(authority).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut builder = Request::builder()
        .uri(path)
        .method(method)
        .header("host", host.unwrap_or(authority))
        .header("origin", foreign.unwrap_or(origin));
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    let body = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = sender
        .send_request(builder.body(body).unwrap())
        .await
        .unwrap();
    let (head, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes().to_vec();
    (head.status, head.headers, bytes)
}

#[tokio::test]
async fn protected_real_node_lifecycle() {
    let data =
        std::env::temp_dir().join(format!("pp-m0-{}", hex::encode(rand::random::<[u8; 8]>())));
    let mut runtime = CoreRuntime::start(launch(data.clone()))
        .await
        .expect("real Node reaches verified readiness");
    assert!(
        !runtime.desktop_rebuild().enabled(),
        "lifecycle proof runs with the rebuild off"
    );
    let origin = runtime.origin().to_owned();
    let target = runtime.take_launch_target().unwrap().into_url();
    assert!(runtime.take_launch_target().is_err());
    let bootstrap = target.strip_prefix(&origin).unwrap();
    assert_eq!(
        request(&origin, "/health", "GET", None, None, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &origin,
            bootstrap,
            "GET",
            None,
            Some("foreign.invalid"),
            None,
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &origin,
            bootstrap,
            "GET",
            None,
            None,
            Some("http://foreign.invalid"),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, headers, _) = request(&origin, bootstrap, "GET", None, None, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], "/");
    assert_eq!(headers["cache-control"], "no-store");
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_eq!(
        request(&origin, bootstrap, "GET", None, None, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, bytes) =
        request(&origin, "/health", "GET", Some(cookie), None, None, None).await;
    assert_eq!(status, StatusCode::OK);
    let health: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(health["authenticated"], true);
    assert_eq!(health["authentication_required"], true);
    assert_eq!(
        health["version"],
        launch(data.clone()).bundle.runtime_version
    );
    let (status, _, bytes) =
        request(&origin, "/auth/me", "GET", Some(cookie), None, None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["user"]["provider"],
        "desktop"
    );
    assert_eq!(
        request(
            &origin,
            "/plans",
            "POST",
            Some(cookie),
            None,
            Some("http://foreign.invalid"),
            Some(serde_json::json!({"name":"refused"}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &origin,
            "/api/v1/mcp",
            "POST",
            Some(cookie),
            None,
            None,
            Some(serde_json::json!({}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &origin,
            "/unknown-api",
            "GET",
            Some(cookie),
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (status, _, bytes) = request(
        &origin,
        "/plans",
        "POST",
        Some(cookie),
        None,
        None,
        Some(serde_json::json!({"name":"Rust protected Build"})),
    )
    .await;
    assert!(status.is_success(), "create Build returned {status}");
    let build: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(build["id"].as_u64().is_some());
    assert!(CoreRuntime::start(launch(data.clone())).await.is_err());
    let child_pid = match compat(&runtime).borrow().clone() {
        CoreStatus::Ready { pid, .. } => pid,
        CoreStatus::Starting
        | CoreStatus::Backoff { .. }
        | CoreStatus::Guarded
        | CoreStatus::Stopping
        | CoreStatus::Stopped
        | CoreStatus::Failed => panic!("not ready"),
    };
    let receipt = tokio::time::timeout(Duration::from_secs(16), runtime.shutdown())
        .await
        .unwrap();
    assert!(receipt.compat_reaped && receipt.storage_released);
    assert!(!data.join(".desktop-owner.json").exists());
    assert!(!pp_core::process_alive(child_pid));
    let second = started(CoreRuntime::start(launch(data.clone())).await, &data);
    assert_eq!(second.origin(), origin);
    second.shutdown().await;
    println!(
        "{}",
        serde_json::json!({"proof_class":"headless_unsigned","case":"protected_real_node_lifecycle","origin":origin,"child_reaped":true,"persisted_origin":true,"build_id":build["id"],"data_dir":data})
    );
}

#[tokio::test]
async fn authenticated_logout_requests_complete_shutdown() {
    use fs2::FileExt;

    let data = temporary("logout");
    let mut runtime = started(CoreRuntime::start(launch(data.clone())).await, &data);
    let origin = runtime.origin().to_owned();
    let target = runtime.take_launch_target().unwrap().into_url();
    let bootstrap = target.strip_prefix(&origin).unwrap();
    let (status, headers, _) = request(&origin, bootstrap, "GET", None, None, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_eq!(
        request(&origin, bootstrap, "GET", None, None, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for (cookie, host, foreign, expected) in [
        (None, None, None, StatusCode::UNAUTHORIZED),
        (
            Some(cookie),
            Some("foreign.invalid"),
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            Some(cookie),
            None,
            Some("http://foreign.invalid"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        assert_eq!(
            request(&origin, "/auth/logout", "POST", cookie, host, foreign, None,)
                .await
                .0,
            expected
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), runtime.shutdown_requested())
                .await
                .is_err(),
            "rejected logout requested shutdown"
        );
    }
    let runtime_dir = runtime.runtime_dir().to_owned();
    let child_pid = match compat(&runtime).borrow().clone() {
        CoreStatus::Ready { pid, .. } => pid,
        CoreStatus::Starting
        | CoreStatus::Backoff { .. }
        | CoreStatus::Guarded
        | CoreStatus::Stopping
        | CoreStatus::Stopped
        | CoreStatus::Failed => panic!("not ready"),
    };
    let first = runtime.shutdown_requested();
    let second = runtime.shutdown_requested();
    let (status, _, body) = request(
        &origin,
        "/auth/logout",
        "POST",
        Some(cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"ok":true})
    );
    tokio::time::timeout(Duration::from_secs(1), first)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(16), runtime.shutdown())
        .await
        .unwrap();
    assert!(receipt.complete(), "incomplete receipt: {receipt:?}");
    assert!(!pp_core::process_alive(child_pid));
    assert!(!data.join(".desktop-owner.json").exists());
    assert!(!runtime_dir.exists());
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(data.join("printpartner.lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    println!(
        "{}",
        serde_json::json!({"proof_class":"headless_unsigned","case":"authenticated_logout_requests_complete_shutdown","receipt":receipt,"child_reaped":true,"marker_removed":true,"runtime_removed":true,"lock_reacquired":true})
    );
}

#[tokio::test]
async fn ticket_t_12_hostile_lock_is_refused_before_supervised_node_startup() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as symlink;
    let data = temporary("hostile-lock");
    pp_os::create_private_dir(&data).unwrap();
    let database = data.join("print-partner.db");
    let config = data.join("desktop.toml");
    let marker = data.join(".desktop-owner.json");
    std::fs::write(&database, b"must remain unopened").unwrap();
    std::fs::write(&config, b"port = 43123\n").unwrap();
    std::fs::write(&marker, br#"{"pid":2147483647,"kind":"standalone"}"#).unwrap();
    symlink(&database, data.join("printpartner.lock")).unwrap();
    let before = [&database, &config, &marker].map(|path| std::fs::read(path).unwrap());
    let error = CoreRuntime::start(launch(data.clone()))
        .await
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "Unsafe data directory lock");
    assert_eq!(
        [&database, &config, &marker].map(|path| std::fs::read(path).unwrap()),
        before
    );
    assert_eq!(
        std::fs::read_link(data.join("printpartner.lock")).unwrap(),
        database
    );
    assert_eq!(std::fs::read_dir(&data).unwrap().count(), 4);
    assert!(!data.join(".desktop-lease").exists());
    println!(
        "{}",
        serde_json::json!({"proof_class":"headless_unsigned", "case":"hostile_lock_refused", "error":error.to_string(), "database_unchanged":true, "config_unchanged":true, "stale_marker_unchanged":true, "ownership_cleanup_not_started":true, "supervised_child_not_started":true})
    );
    std::fs::remove_dir_all(data).unwrap();
}

/// The Node-backed path refuses a Rust-marked directory, valid marker or not,
/// after taking the lease and before cutover recovery, Source Sync admission or
/// the Node child. Nothing in the directory changes.
#[tokio::test]
async fn rust_marked_directory_is_refused_before_supervised_node_startup() {
    use pp_testkit::{Corruption, Fixture, Installation, MarkerState, Spec};
    let specs = [
        Fixture::RustMarked.spec(),
        Fixture::MarkedCorrupt.spec(),
        Spec {
            marker: MarkerState::Corrupt(Corruption::UnsupportedVersion),
            ..Spec::default()
        },
        Spec {
            marker: MarkerState::Corrupt(Corruption::Directory),
            ..Spec::default()
        },
    ];
    for spec in specs {
        let installation = Installation::create(spec).unwrap();
        let data = installation.path().to_path_buf();
        let database = installation.database_path();
        std::fs::write(&database, b"must remain unopened").unwrap();
        let marker = std::fs::read(installation.marker_path()).ok();
        let before = installation.entries().unwrap();
        let error = CoreRuntime::start(launch(data.clone()))
            .await
            .err()
            .unwrap();
        assert!(
            error.to_string().contains(".pp-rust-owned.json"),
            "{spec:?}: {error:#}"
        );
        assert_eq!(std::fs::read(&database).unwrap(), b"must remain unopened");
        assert_eq!(std::fs::read(installation.marker_path()).ok(), marker);
        // The full listing, lease files included: the refused start leaves no
        // printpartner.lock or .desktop-lease behind.
        assert_eq!(installation.entries().unwrap(), before, "{spec:?}");
    }
    println!(
        "{}",
        serde_json::json!({"proof_class":"headless_unsigned", "case":"rust_marked_refused", "supervised_child_not_started":true, "entries_unchanged":true})
    );
}

/// Windows counterpart: `ticket_t_12_windows_admission_reads_no_posix_modes`.
#[cfg(unix)]
#[tokio::test]
async fn ticket_t_12_unsafe_ancestor_is_refused_before_child_or_database_open() {
    use std::os::unix::fs::PermissionsExt;
    let parent = temporary("unsafe-ancestor");
    let data = parent.join("data");
    pp_os::create_private_dir_all(&data).unwrap();
    let source = temporary("ancestor-copy-source");
    let (owner, _) = pp_storage::WriterOwner::open(&source, pp_storage::Limits::default()).unwrap();
    owner.shutdown().unwrap();
    let database = data.join("print-partner.db");
    let config = data.join("desktop.toml");
    let marker = data.join(".desktop-owner.json");
    std::fs::copy(source.join("print-partner.db"), &database).unwrap();
    std::fs::write(&config, b"port = 43123\n").unwrap();
    std::fs::write(&marker, br#"{"pid":2147483647,"kind":"standalone"}"#).unwrap();
    let before = [&database, &config, &marker].map(|path| std::fs::read(path).unwrap());
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
    let error = CoreRuntime::start(launch(data.clone()))
        .await
        .err()
        .unwrap();
    let after = [&database, &config, &marker].map(|path| std::fs::read(path).unwrap());
    let entries = std::fs::read_dir(&data).unwrap().count();
    let parent_entries = std::fs::read_dir(&parent).unwrap().count();
    std::fs::remove_dir_all(&parent).unwrap();
    std::fs::remove_dir_all(&source).unwrap();
    assert_eq!(error.to_string(), "Unsafe data directory ancestor");
    assert_eq!(after, before);
    assert_eq!(entries, 3);
    assert_eq!(parent_entries, 1);
    println!(
        "{}",
        serde_json::json!({"proof_class":"headless_unsigned", "case":"unsafe_ancestor_refused", "error":error.to_string(), "database_unchanged":true, "config_unchanged":true, "stale_marker_unchanged":true, "ownership_artifacts_absent":true, "supervised_child_not_started":true})
    );
}

/// Windows counterpart: `ticket_t_12_windows_admission_reads_no_posix_modes`.
#[cfg(unix)]
#[tokio::test]
async fn ticket_t_12_unsafe_directory_is_refused_before_child_or_database_open() {
    use std::os::unix::fs::PermissionsExt;
    let data = temporary("unsafe-directory");
    pp_os::create_private_dir_all(&data).unwrap();
    let database = data.join("print-partner.db");
    let copied = b"copied database must remain unopened";
    std::fs::write(&database, copied).unwrap();
    std::fs::write(data.join("desktop.toml"), b"port = 43123\n").unwrap();
    for permissions in [0o775, 0o300] {
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(permissions)).unwrap();
        let error = CoreRuntime::start(launch(data.clone()))
            .await
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Unsafe data directory permissions");
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(std::fs::read(&database).unwrap(), copied);
        assert_eq!(
            std::fs::read(data.join("desktop.toml")).unwrap(),
            b"port = 43123\n"
        );
        assert_eq!(std::fs::read_dir(&data).unwrap().count(), 2);
        println!(
            "{}",
            serde_json::json!({"proof_class":"headless_unsigned", "case":"unsafe_directory_refused", "permissions":permissions, "error":error.to_string(), "database_unchanged":true, "origin_unchanged":true, "ownership_artifacts_absent":true})
        );
    }
    std::fs::remove_dir_all(data).unwrap();
}

/// Windows has no POSIX modes, and pp-storage does not inspect ACLs yet (ADR
/// 0015), so a data directory and an ancestor that Everyone may change are
/// admitted. Counterpart of the two Unix refusals above; it pins this
/// behaviour so changing it is deliberate.
#[cfg(windows)]
#[test]
fn ticket_t_12_windows_admission_reads_no_posix_modes() {
    let parent = temporary("acl-ancestor");
    let data = parent.join("data");
    pp_os::create_private_dir_all(&data).unwrap();
    for path in [&parent, &data] {
        let granted = std::process::Command::new("icacls")
            .arg(path)
            .args(["/grant", "*S-1-1-0:(OI)(CI)(M)"])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(granted.success(), "icacls failed: {granted}");
        assert_eq!(pp_os::status_follow(path).unwrap().posix_access(), None);
    }
    drop(pp_storage::lease::StorageLease::acquire(&data).unwrap());
    std::fs::remove_dir_all(&parent).unwrap();
}

#[tokio::test]
async fn ticket_t_12_unsafe_config_is_refused_before_any_database_open() {
    let data = temporary("unsafe-origin-config");
    pp_os::create_private_dir_all(&data).unwrap();
    let database = data.join("print-partner.db");
    let copied = b"copied database must remain unopened";
    std::fs::write(&database, copied).unwrap();
    let target = data.join("missing-config.toml");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, data.join("desktop.toml")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&target, data.join("desktop.toml")).unwrap();
    let error = CoreRuntime::start(launch(data.clone()))
        .await
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "Desktop configuration unavailable");
    assert_eq!(std::fs::read(&database).unwrap(), copied);
    assert_eq!(
        std::fs::read_link(data.join("desktop.toml")).unwrap(),
        target
    );
    assert!(!data.join(".desktop-owner.json").exists());
    let owner = pp_storage::lease::StorageLease::acquire(&data).unwrap();
    drop(owner);
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn occupied_saved_port_recovers_before_child_start() {
    let data = std::env::temp_dir().join(format!(
        "pp-bind-m0-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    pp_os::create_private_dir_all(&data).unwrap();
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    std::fs::write(
        data.join("desktop.toml"),
        format!("port = {}\n", occupied.local_addr().unwrap().port()),
    )
    .unwrap();
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let old_port = occupied.local_addr().unwrap().port();
    let new_port: u16 = runtime
        .origin()
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(old_port, new_port);
    assert_eq!(
        runtime.origin_change(),
        Some(&pp_core::OriginChange::PersistedPortInUse {
            previous_port: std::num::NonZeroU16::new(old_port).unwrap(),
            new_port: std::num::NonZeroU16::new(new_port).unwrap(),
        })
    );
    assert_eq!(
        std::fs::read_to_string(data.join("desktop.toml")).unwrap(),
        format!("port = {new_port}\n")
    );
    let origin = runtime.origin().to_owned();
    let stale = format!("pp_desktop_{old_port}=stale");
    assert_eq!(
        request(&origin, "/health", "GET", Some(&stale), None, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let target = runtime.take_launch_target().unwrap().into_url();
    let (status, headers, _) = request(
        &origin,
        target.strip_prefix(&origin).unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let wrong_name = format!(
        "pp_desktop_{old_port}={}",
        cookie.split_once('=').unwrap().1
    );
    assert_eq!(
        request(
            &origin,
            "/auth/me",
            "GET",
            Some(&wrong_name),
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, bytes) =
        request(&origin, "/auth/me", "GET", Some(cookie), None, None, None).await;
    assert_eq!(status, StatusCode::OK);
    let principal: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(principal["user"]["user_id"], "desktop-owner");
    assert_eq!(principal["user"]["provider"], "desktop");
    assert_eq!(principal["user"]["is_admin"], true);
    let handle = runtime.handle();
    assert_eq!(
        handle.browser_preferences().await.unwrap(),
        pp_core::BrowserPreferences::default()
    );
    handle
        .set_browser_preference(pp_core::BrowserPreference::Theme(Some(
            pp_core::Theme::Light,
        )))
        .await
        .unwrap();
    handle
        .set_browser_preference(pp_core::BrowserPreference::SidebarCollapsed(Some(true)))
        .await
        .unwrap();
    assert_eq!(
        runtime.handle().browser_preferences().await.unwrap(),
        pp_core::BrowserPreferences {
            theme: Some(pp_core::Theme::Light),
            sidebar_collapsed: Some(true),
            printer_poll_interval: None,
        }
    );
    assert!(runtime.shutdown().await.complete());
    assert!(!data.join(".desktop-owner.json").exists());
    let runtime = started(CoreRuntime::start(launch(data.clone())).await, &data);
    assert_eq!(runtime.origin(), origin);
    assert_eq!(runtime.origin_change(), None);
    assert!(handle.browser_preferences().await.is_err());
    assert_eq!(
        runtime.handle().browser_preferences().await.unwrap().theme,
        Some(pp_core::Theme::Light)
    );
    let cleared = runtime
        .handle()
        .set_browser_preference(pp_core::BrowserPreference::Theme(None))
        .await
        .unwrap();
    assert_eq!(cleared.theme, None);
    assert_eq!(cleared.sidebar_collapsed, Some(true));
    assert!(runtime.shutdown().await.complete());
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn wrong_release_is_rejected_before_storage_or_child() {
    let data = std::env::temp_dir().join(format!(
        "pp-version-m0-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let mut wrong = launch(data.clone());
    wrong.bundle.runtime_version = "0.0.0-web".into();
    let error = CoreRuntime::start(wrong).await.err().unwrap();
    assert!(error.to_string().contains("Requested release differs"));
    let mut wrong = launch(data.clone());
    wrong.bundle.commit = "0".repeat(40);
    let error = CoreRuntime::start(wrong).await.err().unwrap();
    assert!(error.to_string().contains("Requested release differs"));
    assert!(!data.join(".desktop-owner.json").exists());
    let runtime = CoreRuntime::start(launch(data)).await.unwrap();
    runtime.shutdown().await;
}

#[tokio::test]
async fn expired_bootstrap_is_denied_by_real_listener() {
    let data = std::env::temp_dir().join(format!(
        "pp-expiry-m0-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let mut runtime = CoreRuntime::start(launch(data)).await.unwrap();
    let origin = runtime.origin().to_owned();
    let target = runtime.take_launch_target().unwrap().into_url();
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert_eq!(
        request(
            &origin,
            target.strip_prefix(&origin).unwrap(),
            "GET",
            None,
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    runtime.shutdown().await;
}

// One kill-to-Ready cycle: stop and reap, backoff (at most 4s here), the respawn
// hash (up to 20s on an idle macOS runner) and the child's 30s readiness probe.
// The test pinned to one CPU beside two busy loops still finishes each cycle in
// under this.
const CRASH_CYCLE_DEADLINE: Duration = Duration::from_secs(150);

/// Waits until `done` accepts the status. If the supervisor stops, or the
/// deadline passes, it panics with every status it saw.
async fn await_status(
    state: &mut tokio::sync::watch::Receiver<CoreStatus>,
    deadline: tokio::time::Instant,
    what: &str,
    done: impl Fn(&CoreStatus) -> bool,
) -> CoreStatus {
    let start = tokio::time::Instant::now();
    let mut seen = Vec::new();
    loop {
        let current = state.borrow_and_update().clone();
        seen.push(format!("{:.1}s {current:?}", start.elapsed().as_secs_f64()));
        if done(&current) {
            return current;
        }
        if matches!(current, CoreStatus::Stopped | CoreStatus::Failed) {
            panic!("supervisor stopped before {what}; saw {seen:?}");
        }
        match tokio::time::timeout_at(deadline, state.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => panic!("supervisor exited before {what}; saw {seen:?}"),
            Err(_) => panic!("timed out waiting for {what}; saw {seen:?}"),
        }
    }
}

#[tokio::test]
async fn crash_guard_stops_after_five_failures_and_recovers_explicitly() {
    let data = std::env::temp_dir().join(format!(
        "pp-guard-m0-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    // Five slow cycles on a loaded runner can span more than the 120s production
    // window, which would forget the first crash. This test counts crashes, so
    // keep all five in the window.
    let runtime = CoreRuntime::start_with_compat_config(
        launch(data),
        SupervisorConfig {
            crash_guard_window: 5 * CRASH_CYCLE_DEADLINE,
            ..SupervisorConfig::default()
        },
    )
    .await
    .unwrap();
    let handle = runtime.handle();
    assert!(handle.recover_compat().await.is_err());
    let mut state = handle.compat_status().unwrap();
    let mut prior_pid = 0;
    let mut cycle_secs = Vec::new();
    for attempt in 0..5 {
        let cycle_start = tokio::time::Instant::now();
        let deadline = cycle_start + CRASH_CYCLE_DEADLINE;
        let CoreStatus::Ready { pid, .. } = await_status(
            &mut state,
            deadline,
            &format!("respawn {attempt} Ready"),
            |current| {
                assert!(
                    !matches!(current, CoreStatus::Guarded),
                    "crash guard tripped before kill {attempt}"
                );
                matches!(current, CoreStatus::Ready { pid, .. } if *pid != prior_pid)
            },
        )
        .await
        else {
            unreachable!()
        };
        prior_pid = pid;
        assert!(process::kill(pid));
        await_status(
            &mut state,
            deadline,
            &format!("crash {attempt} handled"),
            // A stalled test task can first observe the respawn already Ready.
            |current| match current {
                CoreStatus::Guarded => attempt == 4,
                CoreStatus::Backoff { .. } | CoreStatus::Starting => attempt < 4,
                CoreStatus::Ready { pid, .. } => attempt < 4 && *pid != prior_pid,
                _ => false,
            },
        )
        .await;
        cycle_secs.push(cycle_start.elapsed().as_secs_f64());
    }
    assert!(matches!(*state.borrow(), CoreStatus::Guarded));
    handle.recover_compat().await.unwrap();
    await_status(
        &mut state,
        tokio::time::Instant::now() + CRASH_CYCLE_DEADLINE,
        "Ready after recovery",
        |current| matches!(current, CoreStatus::Ready { .. }),
    )
    .await;
    runtime.shutdown().await;
    println!(
        "{}",
        serde_json::json!({"case":"crash_guard", "cycle_secs":cycle_secs, "deadline_secs":CRASH_CYCLE_DEADLINE.as_secs()})
    );
}

/// Redeem the launch bootstrap the way the desktop window does, following no
/// redirect, and return the desktop session cookie.
async fn desktop_cookie(runtime: &mut CoreRuntime) -> String {
    let browser = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let redeemed = browser
        .get(runtime.take_launch_target().unwrap().into_url())
        .send()
        .await
        .unwrap();
    assert_eq!(redeemed.status(), 303);
    let desktop = redeemed.headers()["set-cookie"].to_str().unwrap();
    desktop.split(';').next().unwrap().to_owned()
}

/// The started runtime, or a panic that carries the redacted recent logs, so
/// a hosted failure shows why the child never became ready.
fn started<E: std::fmt::Display>(result: Result<CoreRuntime, E>, data: &Path) -> CoreRuntime {
    result.unwrap_or_else(|error| {
        panic!(
            "runtime start failed: {error:#}\n{}",
            pp_core::recent_logs(data).unwrap_or_else(|error| error.to_string())
        )
    })
}

fn temporary(case: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "pp-{case}-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ))
}

#[tokio::test]
async fn source_sync_receipt_is_admitted_before_desktop_effects() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "source-sync-receipt-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    std::fs::create_dir_all(&root).unwrap();
    let cases = [
        (
            "count",
            r#"{"operation":"source.sync","owner":"node","worker_pid":123,"admission_count":1,"participant_count":999,"manifest_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        ),
        (
            "pid",
            r#"{"operation":"source.sync","owner":"node","admission_count":1,"participant_count":7,"manifest_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        ),
        ("null", "null"),
    ];
    for (name, body) in cases {
        let data = root.join(name);
        pp_os::create_private_dir_all(&data).unwrap();
        let receipt = data.join("source-sync-admission.json");
        std::fs::write(&receipt, body).unwrap();
        let error = CoreRuntime::start(launch(data.clone()))
            .await
            .err()
            .unwrap_or_else(|| panic!("{name} start succeeded"));
        let message = format!("{error:#}");
        assert!(
            message.contains("Source Sync ownership admission failed"),
            "{name}: {message}"
        );
        assert!(message.contains("admission receipt"), "{name}: {message}");
        assert_eq!(std::fs::read(&receipt).unwrap(), body.as_bytes(), "{name}");
        assert!(!data.join("desktop.toml").exists(), "{name}");
        assert!(!data.join("print-partner.db").exists(), "{name}");
    }

    let data = root.join("reuse");
    let first = CoreRuntime::start(launch(data.clone()))
        .await
        .expect("desktop startup admits Source Sync");
    let receipt = data.join("source-sync-admission.json");
    let bytes = std::fs::read(&receipt).unwrap();
    let identity = pp_os::status_follow(&receipt).unwrap().identity();
    let shutdown = first.shutdown().await;
    assert!(shutdown.complete(), "{shutdown:?}");
    let second = started(CoreRuntime::start(launch(data.clone())).await, &data);
    assert_eq!(std::fs::read(&receipt).unwrap(), bytes);
    assert_eq!(pp_os::status_follow(&receipt).unwrap().identity(), identity);
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["operation"], "source.sync");
    assert_eq!(parsed["owner"], "node");
    assert_eq!(parsed["admission_count"], 1);
    assert!(parsed["participant_count"].as_u64().unwrap() > 0);
    assert!(parsed["worker_pid"].as_u64().unwrap() > 0);
    let shutdown = second.shutdown().await;
    assert!(shutdown.complete(), "{shutdown:?}");
    std::fs::remove_dir_all(&root).unwrap();
}

fn copy_directory(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.path().is_dir() {
            copy_directory(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).unwrap();
        }
    }
}

#[tokio::test]
async fn changed_or_missing_built_artifacts_are_rejected() {
    use std::io::Write;
    let root = temporary("integrity");
    let source = launch(root.join("data"));
    let assets = root.join("assets");
    copy_directory(&source.assets, &assets);
    let script = std::fs::read_dir(assets.join("assets"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "js"))
        .unwrap();
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&script)
            .unwrap(),
        "changed after build"
    )
    .unwrap();
    let mut changed = launch(root.join("changed-frontend"));
    changed.assets = assets.clone();
    assert!(CoreRuntime::start(changed).await.is_err());
    std::fs::remove_file(script).unwrap();
    let mut missing = launch(root.join("missing-frontend"));
    missing.assets = assets;
    assert!(CoreRuntime::start(missing).await.is_err());
    let backend = root.join("backend");
    copy_directory(source.bundle.entry.parent().unwrap(), &backend);
    let entry = backend.join("desktop.js");
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&entry)
            .unwrap(),
        "changed after build"
    )
    .unwrap();
    let mut changed = launch(root.join("changed-backend"));
    changed.bundle.entry = entry.clone();
    assert!(CoreRuntime::start(changed).await.is_err());
    std::fs::remove_file(entry).unwrap();
    let mut missing = launch(root.join("missing-backend"));
    missing.bundle.entry = backend.join("desktop.js");
    assert!(CoreRuntime::start(missing).await.is_err());
    let mut changed = launch(root.join("changed-node"));
    changed.bundle.node = root.join("node");
    std::fs::write(&changed.bundle.node, b"changed executable").unwrap();
    assert!(CoreRuntime::start(changed).await.is_err());
    for case in [
        "changed-frontend",
        "missing-frontend",
        "changed-backend",
        "missing-backend",
        "changed-node",
    ] {
        assert!(!root.join(case).exists());
    }
    println!(
        "{}",
        serde_json::json!({"case":"measured_release_negatives","changed_frontend_denied":true,"missing_frontend_denied":true,"changed_backend_denied":true,"missing_backend_denied":true,"changed_node_denied":true})
    );
}

// Unix only: it fails cleanup by planting a directory at the channel socket.
// The Windows channel is a named pipe with no file to plant over; see
// `ticket_t_12_windows_channel_leaves_nothing_to_clean`.
#[cfg(unix)]
#[tokio::test]
async fn cleanup_error_publishes_failed_and_preserves_failure() {
    let data = temporary("compat-cleanup-status");
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let handle = runtime.handle();
    let mut state = handle.compat_status().unwrap();
    let pid = match *state.borrow() {
        CoreStatus::Ready { pid, .. } => pid,
        CoreStatus::Starting
        | CoreStatus::Backoff { .. }
        | CoreStatus::Guarded
        | CoreStatus::Stopping
        | CoreStatus::Stopped
        | CoreStatus::Failed => panic!("Runtime was not ready"),
    };
    let directory = runtime.runtime_dir().to_owned();
    let socket = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sock")
        })
        .unwrap();
    std::fs::rename(&socket, directory.join("retained-socket")).unwrap();
    std::fs::create_dir(&socket).unwrap();
    assert!(process::kill(pid));
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if matches!(*state.borrow(), CoreStatus::Failed) {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(handle.recover_compat().await.is_err());
    let receipt = runtime.shutdown().await;
    assert!(!receipt.complete());
    assert!(receipt.errors.contains(&"compat_cleanup_unproved"));
    assert!(socket.is_dir());
    println!(
        "{}",
        serde_json::json!({"case":"compat_cleanup_terminal_status", "failed":true, "receipt":receipt})
    );
}

#[tokio::test]
async fn stable_child_resets_backoff_without_erasing_rapid_crash_escalation() {
    let window = Duration::from_secs(1);
    let stable_time = window + Duration::from_millis(100);
    let runtime = CoreRuntime::start_with_compat_config(
        launch(temporary("stable-backoff")),
        SupervisorConfig {
            crash_guard_window: window,
            ..SupervisorConfig::default()
        },
    )
    .await
    .unwrap();
    let mut state = compat(&runtime);
    let mut prior_pid = 0;
    for (index, expected) in [0, 1, 0].into_iter().enumerate() {
        let pid = loop {
            if let CoreStatus::Ready { pid, .. } = *state.borrow()
                && pid != prior_pid
            {
                break pid;
            }
            tokio::time::timeout(RESPAWN_READY_FUSE, state.changed())
                .await
                .unwrap()
                .unwrap();
        };
        prior_pid = pid;
        if index == 2 {
            tokio::time::sleep(stable_time).await;
        }
        assert!(process::kill(pid));
        let attempt = loop {
            if let CoreStatus::Backoff { attempt } = *state.borrow() {
                break attempt;
            }
            tokio::time::timeout(Duration::from_secs(20), state.changed())
                .await
                .unwrap()
                .unwrap();
        };
        assert_eq!(attempt, expected);
    }
    let receipt = runtime.shutdown().await;
    assert!(receipt.complete());
    println!(
        "{}",
        serde_json::json!({"case":"stable_backoff_reset", "observed_attempts":[0,1,0], "window_ms":window.as_millis(), "stable_ms":stable_time.as_millis(), "receipt":receipt})
    );
}

/// A standalone Node lease directory refuses the start, and the lock file the
/// refused start created is removed: the directory listing is unchanged.
#[tokio::test]
async fn node_lease_directory_refuses_start_without_leaving_a_lock_file() {
    let data = temporary("node-lease-no-lock-left");
    pp_os::create_private_dir_all(&data.join(".desktop-lease")).unwrap();
    std::fs::write(data.join(".desktop-lease/owner.json"), b"{}").unwrap();
    let listing = |root: &Path| {
        let mut entries = std::collections::BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let bytes = if path.is_dir() {
                    pending.push(path.clone());
                    None
                } else {
                    Some(std::fs::read(&path).unwrap())
                };
                entries.insert(path.strip_prefix(root).unwrap().to_path_buf(), bytes);
            }
        }
        entries
    };
    let before = listing(&data);
    let error = CoreRuntime::start(launch(data.clone()))
        .await
        .err()
        .unwrap();
    assert!(
        format!("{error:#}").contains("standalone Node"),
        "{error:#}"
    );
    assert_eq!(listing(&data), before);
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn unexpected_marker_directory_is_preserved_and_reported() {
    let data = temporary("cleanup-failure");
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let marker = data.join(".desktop-lease");
    std::fs::remove_file(&marker).unwrap();
    std::fs::create_dir(&marker).unwrap();
    std::fs::write(marker.join("retain"), b"must not delete").unwrap();
    let receipt = runtime.shutdown().await;
    assert!(receipt.compat_reaped);
    assert!(!receipt.storage_released);
    assert!(!receipt.marker_removed);
    assert!(!receipt.complete());
    assert!(
        receipt
            .errors
            .contains(&"compatibility_reservation_cleanup_failed")
    );
    assert!(marker.join("retain").is_file());
    println!(
        "{}",
        serde_json::json!({"case":"truthful_cleanup_failure","receipt":receipt,"unexpected_marker_preserved":true})
    );
}

#[tokio::test]
async fn same_process_restart_retries_failed_storage_release_without_double_ownership() {
    use fs2::FileExt;

    let data = temporary("restart-after-failed-release");
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let marker = data.join(".desktop-lease");
    std::fs::remove_file(&marker).unwrap();
    std::fs::create_dir(&marker).unwrap();
    let blocker = marker.join("retain");
    std::fs::write(&blocker, b"must not delete").unwrap();
    let lease = data.join(".desktop-lease");
    let lock = std::fs::File::open(data.join("printpartner.lock")).unwrap();

    let receipt = runtime.shutdown().await;
    assert!(receipt.compat_reaped && receipt.gateway_stopped && receipt.runtime_removed);
    assert!(!receipt.storage_released && !receipt.marker_removed && !receipt.complete());
    assert!(receipt.errors.contains(&"storage_ownership_retained"));
    assert!(lock.try_lock_exclusive().is_err());

    assert!(CoreRuntime::start(launch(data.clone())).await.is_err());
    assert!(lease.is_dir());
    assert_eq!(std::fs::read(&blocker).unwrap(), b"must not delete");
    assert!(lock.try_lock_exclusive().is_err());

    std::fs::remove_file(blocker).unwrap();
    std::fs::remove_dir(&marker).unwrap();
    let restarted = started(CoreRuntime::start(launch(data.clone())).await, &data);
    assert!(lease.is_file());
    assert!(lock.try_lock_exclusive().is_err());
    assert!(CoreRuntime::start(launch(data.clone())).await.is_err());
    assert!(restarted.shutdown().await.complete());
    assert!(!marker.exists() && !data.join(".desktop-lease").exists());
    lock.try_lock_exclusive().unwrap();
    FileExt::unlock(&lock).unwrap();
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn drop_after_caller_runtime_teardown_reaps_writer_and_descendant() {
    use fs2::FileExt;
    let data = temporary("drop-after-runtime");
    let caller = tokio::runtime::Runtime::new().unwrap();
    let runtime = caller
        .block_on(CoreRuntime::start(launch(data.clone())))
        .unwrap();
    let pid = match *compat(&runtime).borrow() {
        CoreStatus::Ready { pid, .. } => pid,
        CoreStatus::Starting
        | CoreStatus::Backoff { .. }
        | CoreStatus::Guarded
        | CoreStatus::Stopping
        | CoreStatus::Stopped
        | CoreStatus::Failed => panic!("not ready"),
    };
    let runtime_dir = runtime.runtime_dir().to_owned();
    // Windows keeps the group in a Job Object this test cannot join; pp-compat's
    // `ticket_t_3_drain_job_remains_until_every_member_exits` covers members.
    #[cfg(unix)]
    let mut descendant = Some({
        use std::os::unix::process::CommandExt;
        std::process::Command::new("sleep")
            .arg("60")
            .process_group(pid as i32)
            .spawn()
            .unwrap()
    });
    #[cfg(windows)]
    let mut descendant: Option<std::process::Child> = None;
    let descendant_pid = descendant.as_ref().map(std::process::Child::id);
    caller.shutdown_timeout(Duration::from_secs(2));
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(data.join("printpartner.lock"))
        .unwrap();
    assert!(lock.try_lock_exclusive().is_err());
    drop(runtime);
    let deadline = std::time::Instant::now() + Duration::from_secs(16);
    loop {
        let descendant_reaped = descendant
            .as_mut()
            .is_none_or(|descendant| descendant.try_wait().unwrap().is_some());
        let child_reaped = !pp_core::process_alive(pid);
        if descendant_reaped
            && child_reaped
            && !data.join(".desktop-owner.json").exists()
            && !runtime_dir.exists()
            && lock.try_lock_exclusive().is_ok()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Drop cleanup exceeded 16 seconds"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "{}",
        serde_json::json!({"case":"drop_after_runtime_teardown","child_pid":pid,"descendant_pid":descendant_pid,"child_reaped":true,"descendant_reaped":true,"marker_removed":true,"runtime_removed":true,"lock_reacquired":true})
    );
}

// Unix only, for the reason given on
// `cleanup_error_publishes_failed_and_preserves_failure`.
#[cfg(unix)]
#[tokio::test]
async fn compatibility_cleanup_failure_reports_failed_instead_of_ready() {
    let data = temporary("compat-cleanup-failure");
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let state = compat(&runtime);
    assert!(matches!(*state.borrow(), CoreStatus::Ready { .. }));
    let runtime_dir = runtime.runtime_dir().to_owned();
    let socket = std::fs::read_dir(&runtime_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "sock"))
        .unwrap();
    std::fs::remove_file(&socket).unwrap();
    std::fs::create_dir(&socket).unwrap();
    let receipt = runtime.shutdown().await;
    assert!(!receipt.compat_reaped);
    assert!(!receipt.complete());
    assert!(receipt.errors.contains(&"compat_cleanup_unproved"));
    assert!(matches!(*state.borrow(), CoreStatus::Failed));
    std::fs::remove_dir_all(runtime_dir).unwrap();
    let error = CoreRuntime::start(launch(data.clone()))
        .await
        .err()
        .expect("unproved child closure must prevent same-process restart");
    assert!(error.to_string().contains("Closure was not proved"));
    std::fs::remove_dir_all(data).unwrap();
}

/// Windows counterpart of the two socket cleanup failure tests: the channel
/// is a named pipe, so the runtime directory holds no entry for it, and
/// shutdown proves the child closed.
#[cfg(windows)]
#[tokio::test]
async fn ticket_t_12_windows_channel_leaves_nothing_to_clean() {
    let data = temporary("compat-pipe-cleanup");
    let runtime = started(CoreRuntime::start(launch(data.clone())).await, &data);
    let state = compat(&runtime);
    assert!(matches!(*state.borrow(), CoreStatus::Ready { .. }));
    let runtime_dir = runtime.runtime_dir().to_owned();
    assert!(std::fs::read_dir(&runtime_dir).unwrap().all(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_none_or(|ext| ext != "sock")
    }));
    let receipt = runtime.shutdown().await;
    assert!(receipt.compat_reaped);
    assert!(receipt.complete(), "{receipt:?}");
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn desktop_manifest_and_icons_are_served_as_assets() {
    let data = temporary("desktop-icons");
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let bootstrap = runtime.take_launch_target().unwrap().into_url();
    let (_, headers, _) = request(
        runtime.origin(),
        bootstrap.strip_prefix(runtime.origin()).unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    for (path, expected_type) in [
        ("/manifest.json", "application/manifest+json"),
        ("/icons/icon-192.png", "image/png"),
        ("/icons/icon-512.png", "image/png"),
        ("/icons/icon.svg", "image/svg+xml"),
    ] {
        let (status, headers, bytes) = request(
            runtime.origin(),
            path,
            "GET",
            Some(cookie),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers["content-type"], expected_type);
        assert!(!bytes.is_empty());
    }
    let (status, _, _) = request(
        runtime.origin(),
        "/icons/../../Cargo.toml",
        "GET",
        Some(cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(runtime.shutdown().await.complete());
    std::fs::remove_dir_all(data).unwrap();
}

// Run as a subprocess so SIGKILL ends the actual Rust owner, not the test runner.
#[tokio::test]
async fn core_process_fixture() {
    let Some(data) = std::env::var_os("PP_CORE_DEATH_FIXTURE") else {
        return;
    };
    let runtime = CoreRuntime::start(launch(data.into())).await.unwrap();
    let state = compat(&runtime).borrow().clone();
    let CoreStatus::Ready { pid, .. } = state else {
        panic!("not ready")
    };
    println!(
        "PP_CORE_READY {}",
        serde_json::json!({"pid":pid,"runtime_dir":runtime.runtime_dir()})
    );
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    std::future::pending::<()>().await;
}

fn pid_has_exited(pid: u32) -> bool {
    !pp_core::process_alive(pid) || matches!(linux_proc_state(pid), Some('Z' | 'X'))
}

fn peer_is_running(peer: &mut std::process::Child) -> bool {
    let state = linux_proc_state(peer.id());
    let unreaped = peer.try_wait().unwrap().is_none();
    unreaped && state.is_none_or(|state| state != 'Z' && state != 'X')
}

fn linux_proc_state(pid: u32) -> Option<char> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let close = text.rfind(')')?;
        text[close + 1..].split_whitespace().next()?.chars().next()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "linux")]
fn canonical_node(node: &Path) -> PathBuf {
    let candidate = if node.is_absolute() || node.components().count() > 1 {
        node.to_path_buf()
    } else if let Some(paths) = std::env::var_os("PATH") {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(node))
            .find(|path| path.is_file())
            .unwrap_or_else(|| node.to_path_buf())
    } else {
        node.to_path_buf()
    };
    std::fs::canonicalize(&candidate).unwrap_or(candidate)
}

#[test]
fn core_death_terminates_stopped_node_and_allows_recovery() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let data = temporary("core-death-live-child");
    let mut core = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "core_process_fixture", "--nocapture"])
        .env("PP_CORE_DEATH_FIXTURE", &data)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child_pid = None;
    let mut group_peer = None;
    let mut runtime_dir = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let output = BufReader::new(core.stdout.take().unwrap());
        let line = output
            .lines()
            .map(Result::unwrap)
            .find(|line| line.contains("PP_CORE_READY "))
            .unwrap();
        let ready: serde_json::Value =
            serde_json::from_str(line.split_once("PP_CORE_READY ").unwrap().1).unwrap();
        let pid = ready["pid"].as_u64().unwrap() as u32;
        child_pid = Some(pid);
        runtime_dir = Some(PathBuf::from(ready["runtime_dir"].as_str().unwrap()));
        // The peer only keeps the group from being orphaned. POSIX would
        // SIGHUP a stopped orphan group. Windows has no process groups; the
        // core's Job Object holds Node.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            group_peer = Some(
                Command::new("sleep")
                    .arg("600")
                    .process_group(pid as i32)
                    .spawn()
                    .unwrap(),
            );
        }
        // Freeze the child with its DB open to deterministically hold the shutdown window.
        process::stop(pid);
        // waitpid cannot observe this non-child on macOS; ps works on both
        // hosts. NtSuspendProcess has suspended every thread when it returns.
        #[cfg(unix)]
        {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let state = Command::new("ps")
                    .args(["-o", "stat=", "-p", &pid.to_string()])
                    .output()
                    .unwrap();
                if String::from_utf8_lossy(&state.stdout).contains('T') {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "child did not stop");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        core.kill().unwrap();
        core.wait().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !pid_has_exited(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            pid_has_exited(pid),
            "stopped Node outlived the force-killed owner"
        );
        while group_peer.as_mut().is_some_and(peer_is_running)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !group_peer.as_mut().is_some_and(peer_is_running),
            "Node group member outlived its owner"
        );
        let caller = tokio::runtime::Runtime::new().unwrap();
        let runtime = started(
            caller.block_on(CoreRuntime::start(launch(data.clone()))),
            &data,
        );
        let state = compat(&runtime).borrow().clone();
        let CoreStatus::Ready { pid: new_pid, .. } = state else {
            panic!("not ready")
        };
        assert_ne!(new_pid, pid);
        #[cfg(target_os = "linux")]
        {
            let node = canonical_node(&launch(data.clone()).bundle.node);
            let exe = std::fs::read_link(format!("/proc/{new_pid}/exe")).unwrap();
            assert_eq!(
                std::fs::canonicalize(&exe).unwrap_or(exe),
                node,
                "recovered child must be the staged node binary"
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            #[cfg(unix)]
            let comm = Command::new("ps")
                .args(["-o", "comm=", "-p", &new_pid.to_string()])
                .output()
                .unwrap();
            #[cfg(windows)]
            let comm = Command::new("tasklist")
                .args(["/NH", "/FO", "CSV", "/FI", &format!("PID eq {new_pid}")])
                .output()
                .unwrap();
            let comm = String::from_utf8_lossy(&comm.stdout);
            assert!(
                comm.to_ascii_lowercase().contains("node"),
                "recovered child must be node, saw {comm}"
            );
        }
        assert!(caller.block_on(runtime.shutdown()).complete());
        assert!(
            !group_peer.as_mut().is_some_and(peer_is_running),
            "old Node group member survived recovery"
        );
    }));
    let _ = core.kill();
    let _ = core.wait();
    if let Some(pid) = child_pid {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        #[cfg(windows)]
        process::kill(pid);
    }
    if let Some(mut peer) = group_peer {
        let _ = peer.kill();
        let _ = peer.wait();
    }
    if let Some(path) = runtime_dir {
        let _ = std::fs::remove_dir_all(path);
    }
    let _ = std::fs::remove_dir_all(data);
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn ticket_t_4_real_gateway_hidden_status_and_config_cancellation() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let printer_url = format!("http://{}", listener.local_addr().unwrap());
    let mode = Arc::new(AtomicUsize::new(0));
    let served_mode = mode.clone();

    let simulator = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(request.starts_with("GET /printer/objects/query?"));
            assert!(request.contains("x-api-key: recorded-test-key"));
            seen.fetch_add(1, Ordering::SeqCst);
            let fixture: &[u8] = match served_mode.load(Ordering::SeqCst) {
                1 => include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-error.json"),
                2 => {
                    include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-offline.json")
                }
                _ => {
                    include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-success.json")
                }
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                fixture.len()
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(fixture).await.unwrap();
        }
    });
    let data =
        std::env::temp_dir().join(format!("pp-t4-{}", hex::encode(rand::random::<[u8; 8]>())));
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let origin = runtime.origin().to_owned();
    let target = runtime.take_launch_target().unwrap().into_url();
    let bootstrap = request(
        &origin,
        target.strip_prefix(&origin).unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(bootstrap.0, StatusCode::SEE_OTHER);
    let cookie = bootstrap.1["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let created = request(&origin,"/api/v1/integrations","POST",Some(cookie),None,None,Some(serde_json::json!({"name":"Isolated simulator","type":"moonraker","config":{"base_url":printer_url,"api_key":"recorded-test-key"}}))).await;
    assert_eq!(
        created.0,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&created.2)
    );
    let item: serde_json::Value = serde_json::from_slice(&created.2).unwrap();
    let id = item["id"].as_str().unwrap();
    assert_eq!(item["config"]["api_key"], "****");
    // No browser/window/request traffic drives the polling loop.
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(calls.load(Ordering::SeqCst) >= 2);
    let count = calls.load(Ordering::SeqCst);
    for alias in ["/api/v1/integrations/"] {
        let response = request(
            &origin,
            &format!("{alias}{id}/status"),
            "GET",
            Some(cookie),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(response.0, StatusCode::OK);
        assert_eq!(response.1["cache-control"], "no-store");
        let body: serde_json::Value = serde_json::from_slice(&response.2).unwrap();
        assert_eq!(body["state"], "printing");
        assert_eq!(body["filename"], "bracket.gcode");
        assert!(!String::from_utf8_lossy(&response.2).contains("recorded-test-key"));
    }
    let head = request(
        &origin,
        &format!("/api/v1/integrations/{id}/status"),
        "HEAD",
        Some(cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(head.0, StatusCode::OK);
    assert_eq!(head.1["cache-control"], "no-store");
    assert!(head.2.is_empty());
    assert!(
        head.1["content-length"]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap()
            > 0
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        count,
        "Reads must not make another outbound request"
    );
    // A stalled compatibility snapshot must not stall an already-cached read.
    let pid = match compat(&runtime).borrow().clone() {
        CoreStatus::Ready { pid, .. } => pid,
        _ => panic!("backend not ready"),
    };
    process::stop(pid);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let cached_while_node_stopped = tokio::time::timeout(
        Duration::from_millis(300),
        request(
            &origin,
            &format!("/api/v1/integrations/{id}/status"),
            "GET",
            Some(cookie),
            None,
            None,
            None,
        ),
    )
    .await;
    assert!(process::resume(pid));
    assert_eq!(
        cached_while_node_stopped
            .expect("cached read blocked on Node")
            .0,
        StatusCode::OK
    );
    for (fixture_mode, expected) in [(1, "error"), (2, "offline")] {
        mode.store(fixture_mode, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let response = request(
                    &origin,
                    &format!("/api/v1/integrations/{id}/status"),
                    "GET",
                    Some(cookie),
                    None,
                    None,
                    None,
                )
                .await;
                if serde_json::from_slice::<serde_json::Value>(&response.2).unwrap()["state"]
                    == expected
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .unwrap();
    }
    mode.store(0, Ordering::SeqCst);
    let control = runtime.handle();
    tokio::time::timeout(Duration::from_millis(300), control.suspend_monitoring())
        .await
        .unwrap();
    let asleep = calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(calls.load(Ordering::SeqCst), asleep);
    control.resume_monitoring();
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) == asleep {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request(
            &origin,
            "/__desktop/moonraker-hosts",
            "GET",
            Some(cookie),
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &origin,
            &format!("/api/v1/integrations/{id}/status"),
            "GET",
            None,
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &origin,
            &format!("/api/v1/integrations/{id}"),
            "PATCH",
            Some(cookie),
            None,
            None,
            Some(serde_json::json!({"config":{"enabled":false}}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let count = calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(calls.load(Ordering::SeqCst), count);
    let response = request(
        &origin,
        &format!("/api/v1/integrations/{id}/status"),
        "GET",
        Some(cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&response.2).unwrap()["state"],
        "offline"
    );
    assert_eq!(
        request(
            &origin,
            &format!("/api/v1/integrations/{id}"),
            "DELETE",
            Some(cookie),
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(
            &origin,
            &format!("/api/v1/integrations/{id}/status"),
            "GET",
            Some(cookie),
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let receipt = runtime.shutdown().await;
    assert!(receipt.complete(), "{receipt:?}");
    assert_eq!(calls.load(Ordering::SeqCst), count);
    simulator.abort();
    std::fs::remove_dir_all(data).unwrap();
    println!(
        "ticket_t_4 gateway: hidden polling, existing v1 alias, no request-driven polls, secret isolation, suspend/resume, disable/removal/shutdown passed"
    );
}

#[tokio::test]
async fn gre_335_shared_status_route_keeps_node_path_for_other_printers() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // Records connections; `reply` is None for a socket that only reads (TLS peer).
    async fn fake(reply: Option<&'static [u8]>) -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (calls, tls) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (seen, handshakes) = (calls.clone(), tls.clone());
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut bytes = [0; 4096];
                let count = stream.read(&mut bytes).await.unwrap_or(0);
                seen.fetch_add(1, Ordering::SeqCst);
                if count > 0 && bytes[0] == 0x16 {
                    handshakes.fetch_add(1, Ordering::SeqCst);
                }
                if let Some(reply) = reply {
                    let _ = stream.write_all(reply).await;
                }
            }
        });
        (address, calls, tls)
    }
    async fn get(
        origin: &str,
        cookie: &str,
        id: &str,
    ) -> (StatusCode, serde_json::Value, Duration) {
        let started = std::time::Instant::now();
        let response = request(
            origin,
            &format!("/api/v1/integrations/{id}/status"),
            "GET",
            Some(cookie),
            None,
            None,
            None,
        )
        .await;
        let body = serde_json::from_slice(&response.2).unwrap_or(serde_json::Value::Null);
        (response.0, body, started.elapsed())
    }
    let success = include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-success.json");
    let moonraker_reply: &'static [u8] = Box::leak(
        [
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                success.len()
            )
            .as_bytes(),
            success,
        ]
        .concat()
        .into_boxed_slice(),
    );
    let (moonraker, moonraker_calls, _) = fake(Some(moonraker_reply)).await;
    let (prusa, prusa_calls, _) = fake(Some(
        b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n",
    ))
    .await;
    let (tls, tls_calls, tls_handshakes) = fake(None).await;

    let data = std::env::temp_dir().join(format!(
        "pp-gre335-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let origin = runtime.origin().to_owned();
    let target = runtime.take_launch_target().unwrap().into_url();
    let bootstrap = request(
        &origin,
        target.strip_prefix(&origin).unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    let cookie = bootstrap.1["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let create = |name: &str, kind: &str, config: serde_json::Value| {
        let (origin, cookie) = (origin.clone(), cookie.clone());
        let body = serde_json::json!({"name": name, "type": kind, "config": config});
        async move {
            let created = request(
                &origin,
                "/api/v1/integrations",
                "POST",
                Some(&cookie),
                None,
                None,
                Some(body),
            )
            .await;
            assert_eq!(
                created.0,
                StatusCode::CREATED,
                "{}",
                String::from_utf8_lossy(&created.2)
            );
            serde_json::from_slice::<serde_json::Value>(&created.2).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let moonraker_id = create(
        "Moonraker",
        "moonraker",
        serde_json::json!({"base_url": format!("http://{moonraker}"), "api_key": "k"}),
    )
    .await;
    let prusa_id = create(
        "PrusaLink",
        "prusalink",
        serde_json::json!({"base_url": format!("http://{prusa}"), "api_key": "k"}),
    )
    .await;
    let tls_id = create(
        "TLS Moonraker",
        "moonraker",
        serde_json::json!({"base_url": format!("https://{tls}"), "api_key": "k"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while get(&origin, &cookie, &moonraker_id).await.1["state"] != "printing" {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("Rust watcher serves the plain-HTTP Moonraker host");

    // (1) Healthy snapshot: a non-Moonraker id forwards straight to Node.
    let (status, body, elapsed) = get(&origin, &cookie, &prusa_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        prusa_calls.load(Ordering::SeqCst) >= 1,
        "Node queried PrusaLink"
    );
    println!(
        "gre_335 trace: healthy snapshot, prusalink status={status} via Node in {elapsed:?} body={body}"
    );

    // HTTPS Moonraker stays on Node's on-demand path until GRE-359; Rust never polls it.
    assert_eq!(tls_calls.load(Ordering::SeqCst), 0);
    let (status, body, elapsed) = get(&origin, &cookie, &tls_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        tls_handshakes.load(Ordering::SeqCst) >= 1,
        "Node attempted TLS"
    );
    println!(
        "gre_335 trace: https moonraker status={status} via Node fallback in {elapsed:?} body={body}"
    );

    // A Node stall past the 2 s snapshot deadline keeps Moonraker pollers alive.
    let pid = match compat(&runtime).borrow().clone() {
        CoreStatus::Ready { pid, .. } => pid,
        _ => panic!("backend not ready"),
    };
    process::stop(pid);
    let before = moonraker_calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let during = moonraker_calls.load(Ordering::SeqCst);
    let (status, body, _) = get(&origin, &cookie, &moonraker_id).await;
    assert!(process::resume(pid));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "printing");
    assert!(
        during > before,
        "pollers kept running through the snapshot timeout"
    );
    println!(
        "gre_335 trace: node stalled 3.5s, moonraker polls {before}->{during}, cached state={}",
        body["state"]
    );

    // An oversized Moonraker row fails only its own id closed; other printers keep routing.
    let oversized_id = create(
        "Oversized",
        "moonraker",
        serde_json::json!({"base_url": format!("http://{}", "a".repeat(5000)), "enabled": true}),
    )
    .await;
    let (status, _, _) = get(&origin, &cookie, &oversized_id).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, body, _) = get(&origin, &cookie, &prusa_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body, _) = get(&origin, &cookie, &moonraker_id).await;
    assert_eq!(
        (status, &body["state"]),
        (StatusCode::OK, &serde_json::json!("printing"))
    );
    println!("gre_335 trace: oversized row=503, prusalink and moonraker unaffected");

    // (2) Over-cap snapshot (above 128 Moonraker hosts): no new pollers, grace for old ones.
    for index in 0..125 {
        create(
            &format!("Disabled {index}"),
            "moonraker",
            serde_json::json!({"base_url": "http://127.0.0.1:9", "enabled": false}),
        )
        .await;
    }
    let (overflow, overflow_calls, _) = fake(Some(moonraker_reply)).await;
    let newest = create(
        "129th enabled HTTP host",
        "moonraker",
        serde_json::json!({"base_url": format!("http://{overflow}"), "enabled": true}),
    )
    .await;
    let calls = prusa_calls.load(Ordering::SeqCst);
    let (status, body, elapsed) = get(&origin, &cookie, &prusa_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(prusa_calls.load(Ordering::SeqCst) > calls);
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    // The 129th host is not proven non-Moonraker, so it must not fall back to Node.
    let (status, body, _) = get(&origin, &cookie, &newest).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(overflow_calls.load(Ordering::SeqCst), 0);
    let (status, body, _) = get(&origin, &cookie, &moonraker_id).await;
    assert_eq!(
        (status, &body["state"]),
        (StatusCode::OK, &serde_json::json!("printing"))
    );
    println!(
        "gre_335 trace: failing snapshot, prusalink status={status} via Node in {elapsed:?}; moonraker still cached"
    );
    // Past the grace period plain-HTTP Moonraker fails closed; other printers and the
    // HTTPS Moonraker host still reach Node.
    tokio::time::sleep(Duration::from_secs(11)).await;
    let (status, _, _) = get(&origin, &cookie, &moonraker_id).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, body, elapsed) = get(&origin, &cookie, &prusa_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let handshakes = tls_handshakes.load(Ordering::SeqCst);
    let (tls_status, tls_body, tls_elapsed) = get(&origin, &cookie, &tls_id).await;
    assert_eq!(tls_status, StatusCode::OK, "{tls_body}");
    assert!(
        tls_handshakes.load(Ordering::SeqCst) > handshakes,
        "Node attempted TLS after grace"
    );
    println!(
        "gre_335 trace: after grace, moonraker=503, prusalink status={status} via Node in {elapsed:?}, https moonraker status={tls_status} via Node in {tls_elapsed:?}"
    );

    let (status, body, _) = get(&origin, &cookie, &newest).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        overflow_calls.load(Ordering::SeqCst),
        0,
        "no Node fallback or new poller, even after grace"
    );
    println!("gre_335 trace: 129th enabled HTTP host=503, outbound calls=0 before and after grace");

    let receipt = runtime.shutdown().await;
    assert!(receipt.complete(), "{receipt:?}");
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn ticket_t_5_staged_release_reuses_the_persisted_desktop_identity() {
    let data =
        std::env::temp_dir().join(format!("pp-t-5-{}", hex::encode(rand::random::<[u8; 8]>())));
    let selected = launch(data.clone());
    let expected_version = selected.bundle.runtime_version.clone();
    let mut runtime = CoreRuntime::start(selected).await.unwrap();
    let origin = runtime.origin().to_owned();
    let target = runtime.take_launch_target().unwrap().into_url();
    let (status, headers, _) = request(
        &origin,
        target.strip_prefix(&origin).unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let (status, _, body) =
        request(&origin, "/health", "GET", Some(cookie), None, None, None).await;
    assert_eq!(status, StatusCode::OK);
    let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(health["ok"], true);
    assert_eq!(health["version"], expected_version);
    let persisted = std::fs::read(data.join("desktop.toml")).unwrap();
    assert!(runtime.shutdown().await.complete());
    let reopened = started(CoreRuntime::start(launch(data.clone())).await, &data);
    assert_eq!(reopened.origin(), origin);
    assert_eq!(std::fs::read(data.join("desktop.toml")).unwrap(), persisted);
    assert!(reopened.shutdown().await.complete());
    assert!(!data.join(".desktop-owner.json").exists());
    assert!(!data.join(".desktop-lease").exists());
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn identity_manifest_selects_staged_paths_and_current_version() {
    let root = temporary("identity-manifest");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let manifest = root.join("bundle-manifest.json");
    for (identity, node, web) in [
        ("release.json", "bin/node", "web"),
        (
            "Resources/desktop-runtime/release.json",
            "MacOS/printpartner-node",
            "Resources/desktop-runtime/web",
        ),
    ] {
        let identity = root.join(identity);
        std::fs::create_dir_all(identity.parent().unwrap()).unwrap();
        std::fs::write(
            &manifest,
            serde_json::json!({"dependencies": {"web_path": web}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            &identity,
            serde_json::json!({
                "node": node,
                "web": web,
                "runtime_version": "4.2.1-web",
                "commit": "f00471f13d2f0e69631d3130e594b9244f93e01d"
            })
            .to_string(),
        )
        .unwrap();
        let selected = launch_from_manifest(root.join("data"), &manifest);
        assert_eq!(selected.bundle.node, root.join(node));
        assert_eq!(selected.bundle.web_root, root.join(web));
        assert_eq!(selected.assets, root.join(web).join("apps/web/dist"));
        assert_eq!(
            selected.bundle.entry,
            root.join(web).join("apps/server/dist/current/desktop.js")
        );
        assert_eq!(selected.bundle.runtime_version, "4.2.1-web");
        assert_eq!(
            selected.bundle.commit,
            "f00471f13d2f0e69631d3130e594b9244f93e01d"
        );
        std::fs::write(&identity, b"invalid json").unwrap();
        assert!(
            std::panic::catch_unwind(|| launch_from_manifest(root.join("data"), &manifest))
                .is_err()
        );
        std::fs::remove_file(identity).unwrap();
        assert!(
            std::panic::catch_unwind(|| launch_from_manifest(root.join("data"), &manifest))
                .is_err()
        );
    }
    std::fs::write(
        &manifest,
        serde_json::json!({
            "dependencies": {"web_path": "."},
            "runtime_version": "4.2.1-web",
            "commit": "f00471f13d2f0e69631d3130e594b9244f93e01d"
        })
        .to_string(),
    )
    .unwrap();
    let selected = launch_from_manifest(root.join("data"), &manifest);
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../web")
        .canonicalize()
        .unwrap();
    assert_eq!(selected.bundle.web_root, source);
    assert_eq!(selected.bundle.runtime_version, "4.2.1-web");
    std::fs::write(&manifest, b"invalid json").unwrap();
    assert!(
        std::panic::catch_unwind(|| launch_from_manifest(root.join("data"), &manifest)).is_err()
    );
    std::fs::remove_file(&manifest).unwrap();
    assert!(
        std::panic::catch_unwind(|| launch_from_manifest(root.join("data"), &manifest)).is_err()
    );
    std::fs::remove_dir_all(root).unwrap();
}
