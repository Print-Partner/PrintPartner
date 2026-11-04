#[cfg(feature = "desktop_rebuild")]
use super::request;
use super::{launch, temporary};
#[cfg(feature = "desktop_rebuild")]
use axum::http::StatusCode;
use pp_core::CoreRuntime;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

async fn isolated(name: &str, enabled: bool) -> Option<std::path::PathBuf> {
    if std::env::var("PP_ACCOUNT_CASE").ok().as_deref() == Some(name) {
        return Some(std::env::var_os("PP_ACCOUNT_DATA").unwrap().into());
    }
    let data = temporary("ticket-t-68-account");
    let output = tokio::time::timeout(
        Duration::from_secs(600),
        tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("ticket_t_68_desktop_account_mapping::{name}"),
                "--nocapture",
            ])
            .env("PP_ACCOUNT_CASE", name)
            .env("PP_ACCOUNT_DATA", &data)
            .env("PRINT_PARTNER_DESKTOP_ACCOUNT_MODE", "existing-account-v1")
            .env("PP_DESKTOP_REBUILD", if enabled { "1" } else { "0" })
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("mapping subprocess timed out")
    .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    println!("{}", json!({"case":name,"artifact":data}));
    None
}

async fn node(data: &Path, body: &str) -> Value {
    let bundle = launch(data.to_owned()).bundle;
    let database = bundle
        .web_root
        .join("node_modules/better-sqlite3/lib/index.js");
    let client = bundle.entry.parent().unwrap().join("db/client.js");
    let schema = bundle.entry.parent().unwrap().join("db/schema.js");
    let script = format!(
        "import Database from {}; import {{SqliteDatabase}} from {}; import {{schemaVersionKey,currentSchemaVersion}} from {}; const data = {}; {body}",
        json!(format!("file://{}", database.display())),
        json!(format!("file://{}", client.display())),
        json!(format!("file://{}", schema.display())),
        json!(data),
    );
    let output = tokio::process::Command::new(bundle.node)
        .args(["--input-type=module", "-e", &script])
        .current_dir(bundle.web_root)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("fixture emits exactly one JSON result")
}

async fn fixture(data: &Path) {
    fs::create_dir_all(data).unwrap();
    node(data, r#"
        const store = new SqliteDatabase(data); store.connect(); store.close();
        const db = new Database(data + '/print-partner.db');
        db.exec(`INSERT INTO users (id,email,display_name,password_hash,is_admin,created_at) VALUES
          ('account-b','b@example.test','Foreign account',NULL,1,'2026-10-14'),
          ('account-a','a@example.test','Selected account','hash',0,'2026-10-14');
          INSERT INTO build_profiles (id,tenant_id,name) VALUES
          (101,'account-a','Owned Plan'), (202,'account-b','Foreign Plan'), (303,'default','Legacy Plan');
          INSERT INTO projects (id,tenant_id,name,url) VALUES (77,'account-b','Foreign Source','https://example.test');
          INSERT INTO source_revisions (id,tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at)
          VALUES (77,'default',77,'proof','digest','snapshot','2026-10-14');
          UPDATE projects SET current_source_revision_id = 77 WHERE id = 77;
          DELETE FROM printer_profiles WHERE tenant_id = 'default' AND name = 'Generic Klipper Machine';
          INSERT INTO app_settings (tenant_id,key,value) VALUES ('default','print_outcomes','{"sentinel":true}');`);
        for (const tenant of ['default','account-b']) db.prepare('INSERT INTO app_settings (tenant_id,key,value) VALUES (?,?,?)').run(tenant, 'printer.send_queue', JSON.stringify([{id:'foreign-send',filename:'foreign.gcode',artifact_path:data+'/exports/foreign.gcode',printer_id:'foreign-printer',state:'sending',created_at:'2026-10-14',updated_at:'2026-10-14'}]));
        db.close(); console.log('{}');
    "#).await;
    let transfer = data.join("exports/tenant-account-b/printer-uploads/foreign-transfer");
    fs::create_dir_all(&transfer).unwrap();
    fs::write(transfer.join("sentinel.txt"), "foreign artifact").unwrap();
    fs::File::open(&transfer)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    fs::write(data.join("exports/legacy-sentinel.txt"), "legacy artifact").unwrap();
}

fn mapping(data: &Path, value: Value) {
    let path = data.join("desktop-account.json");
    fs::write(&path, value.to_string()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn selected() -> Value {
    json!({"version":1,"kind":"existing-account","user_id":"account-a","tenant_id":"account-a"})
}
#[cfg(feature = "desktop_rebuild")]
async fn foreign_snapshot(data: &Path) -> Value {
    snapshot_outside(data, "account-a").await
}
#[cfg(feature = "desktop_rebuild")]
async fn snapshot_outside(data: &Path, owner: &str) -> Value {
    node(data, &format!(r#"
        const owner = {};
        const db = new Database(data + '/print-partner.db', {{readonly:true}});
        const result = {{}};
        for (const {{name}} of db.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").all()) {{
          if (!db.prepare(`PRAGMA table_info("${{name}}")`).all().some(c => c.name === 'tenant_id')) continue;
          result[name] = db.prepare(`SELECT * FROM "${{name}}" WHERE tenant_id <> ? ORDER BY rowid`).all(owner);
        }}
        for (const table of ['sessions','auth_identities','password_reset_tokens']) result[table] = db.prepare(`SELECT * FROM "${{table}}" ORDER BY rowid`).all();
        result.users = db.prepare("SELECT * FROM users WHERE id <> ? ORDER BY id").all(owner);
        db.close(); console.log(JSON.stringify(result));
    "#, json!(owner))).await
}
#[cfg(feature = "desktop_rebuild")]
async fn bootstrap(runtime: &mut CoreRuntime) -> String {
    let target = runtime.take_launch_target().unwrap().into_url();
    let path = target.strip_prefix(runtime.origin()).unwrap();
    let (status, headers, _) = request(runtime.origin(), path, "GET", None, None, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}
#[cfg(feature = "desktop_rebuild")]
async fn api(
    runtime: &CoreRuntime,
    cookie: &str,
    path: &str,
    method: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, _, bytes) = request(
        runtime.origin(),
        path,
        method,
        Some(cookie),
        None,
        None,
        body,
    )
    .await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))),
    )
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn mapped_account_reads_writes_and_restarts_without_foreign_changes() {
    let Some(data) = isolated(
        "mapped_account_reads_writes_and_restarts_without_foreign_changes",
        true,
    )
    .await
    else {
        return;
    };
    fixture(&data).await;
    mapping(&data, selected());
    let before = foreign_snapshot(&data).await;
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    assert!(runtime.desktop_rebuild().enabled());
    let cookie = bootstrap(&mut runtime).await;
    let (status, me) = api(&runtime, &cookie, "/auth/me", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        me["user"],
        json!({"user_id":"account-a","login":"a@example.test","display_name":"Selected account","email":"a@example.test","provider":"desktop","hasPassword":true,"is_admin":false})
    );
    let mut denied = Vec::new();
    for path in [
        "/auth/dev-login",
        "/auth/login",
        "/auth/register",
        "/auth/forgot-password",
        "/auth/reset-password",
        "/auth/change-password",
    ] {
        let (status, _) = api(&runtime, &cookie, path, "POST", Some(json!({"login":"fixture","email":"b@example.test","password":"fixture-password","token":"fixture","current_password":"fixture","new_password":"fixture-password"}))).await;
        denied.push(status.as_u16());
    }
    for path in [
        "/auth/github",
        "/auth/callback",
        "/auth/discord",
        "/auth/discord/callback",
    ] {
        denied.push(api(&runtime, &cookie, path, "GET", None).await.0.as_u16());
    }
    let (status, health) = api(&runtime, &cookie, "/health", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    let capabilities = health["capabilities"].as_array().unwrap();
    let advertised = [
        "backups",
        "multi_user_auth",
        "password_reset_email",
        "github_oauth",
        "discord_oauth",
    ]
    .map(|name| capabilities.contains(&json!(name)));
    assert_eq!(denied, vec![501; 10]);
    assert_eq!(advertised, [false; 5]);
    assert_eq!(foreign_snapshot(&data).await, before);
    let (status, own) = api(&runtime, &cookie, "/plans/101", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(own["name"], "Owned Plan");
    for path in ["/api/v1/plans/101", "/api/v2/plans/101"] {
        let (status, own) = api(&runtime, &cookie, path, "GET", None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(own["name"], "Owned Plan");
    }

    let (status, _) = api(
        &runtime,
        &cookie,
        "/plans/101",
        "PATCH",
        Some(json!({"name":"Owned edited"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for id in [202, 303] {
        for path in [
            format!("/plans/{id}"),
            format!("/api/v1/plans/{id}"),
            format!("/api/v2/plans/{id}"),
        ] {
            assert_eq!(
                api(&runtime, &cookie, &path, "GET", None).await.0,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        assert_eq!(
            api(
                &runtime,
                &cookie,
                &format!("/plans/{id}"),
                "PATCH",
                Some(json!({"name":"Stolen"}))
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            api(&runtime, &cookie, &format!("/plans/{id}"), "DELETE", None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    let (status, listing) = api(&runtime, &cookie, "/plans?tenant_id=account-b", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(listing["profiles"][0]["id"], 101);
    let forged = node(&data, &format!(
        "const response = await fetch({} + '/plans?tenant_id=account-b', {{headers:{{cookie:{},origin:{},'x-tenant-id':'account-b','x-pp-principal':'forged','x-pp-signature':'forged'}}}}); console.log(JSON.stringify({{status:response.status,body:await response.json()}}));",
        json!(runtime.origin()), json!(cookie), json!(runtime.origin())
    )).await;
    assert_eq!(forged["status"], 200);
    assert_eq!(forged["body"]["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(forged["body"]["profiles"][0]["id"], 101);

    for path in [
        "/settings/api-keys",
        "/settings/logging/config",
        "/slicer-instances",
        "/api/v1/integrations",
        "/api/v1/webhooks",
        "/backups/restore",
    ] {
        assert_eq!(
            api(&runtime, &cookie, path, "POST", Some(json!({})))
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    let (status, refused_admin) = api(
        &runtime,
        &cookie,
        "/admin/import-kit-bundle",
        "POST",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(refused_admin["code"], "desktop_feature_unavailable");
    let (status, created) = api(
        &runtime,
        &cookie,
        "/plans",
        "POST",
        Some(json!({"name":"Mapped new Plan"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let created_id = created["id"].as_i64().unwrap();
    assert!(runtime.shutdown().await.complete());
    assert_eq!(foreign_snapshot(&data).await, before);
    assert_eq!(
        fs::read_to_string(data.join("exports/legacy-sentinel.txt")).unwrap(),
        "legacy artifact"
    );
    assert_eq!(
        fs::read_to_string(
            data.join("exports/tenant-account-b/printer-uploads/foreign-transfer/sentinel.txt")
        )
        .unwrap(),
        "foreign artifact"
    );
    let own = node(&data, "const db = new Database(data + '/print-partner.db', {readonly:true}); console.log(JSON.stringify(db.prepare(\"SELECT id,tenant_id,name FROM build_profiles WHERE tenant_id = 'account-a' ORDER BY id\").all())); db.close();").await;
    assert_eq!(
        own,
        json!([{"id":101,"tenant_id":"account-a","name":"Owned edited"},{"id":created_id,"tenant_id":"account-a","name":"Mapped new Plan"}])
    );
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    assert_eq!(
        api(&runtime, &cookie, "/plans/101", "GET", None).await.1["name"],
        "Owned edited"
    );
    assert_eq!(
        api(
            &runtime,
            &cookie,
            &format!("/plans/{created_id}"),
            "GET",
            None
        )
        .await
        .1["name"],
        "Mapped new Plan"
    );
    assert!(runtime.shutdown().await.complete());
    assert_eq!(foreign_snapshot(&data).await, before);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn launch_credentials_are_revoked_on_same_origin_account_switch() {
    let Some(data) = isolated(
        "launch_credentials_are_revoked_on_same_origin_account_switch",
        true,
    )
    .await
    else {
        return;
    };
    fixture(&data).await;
    mapping(&data, selected());
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    assert!(runtime.desktop_rebuild().enabled());
    let old_launch_origin = runtime.origin().to_owned();
    let old_bootstrap_target = runtime.take_launch_target().unwrap().into_url();
    let (status, headers, _) = request(
        &old_launch_origin,
        old_bootstrap_target
            .strip_prefix(&old_launch_origin)
            .unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let old_session_cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let (status, me) = api(&runtime, &old_session_cookie, "/auth/me", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["user"]["user_id"], "account-a");
    let (status, own) = api(&runtime, &old_session_cookie, "/plans/101", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(own["name"], "Owned Plan");
    assert!(runtime.shutdown().await.complete());

    mapping(
        &data,
        json!({"version":1,"kind":"existing-account","user_id":"account-b","tenant_id":"account-b"}),
    );
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let new_launch_origin = runtime.origin().to_owned();
    let new_bootstrap_target = runtime.take_launch_target().unwrap().into_url();
    assert_eq!(new_launch_origin, old_launch_origin);
    assert!(new_bootstrap_target != old_bootstrap_target);
    assert_eq!(
        api(&runtime, &old_session_cookie, "/auth/me", "GET", None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api(
            &runtime,
            &old_session_cookie,
            "/plans/202",
            "PATCH",
            Some(json!({"name":"Stale before redemption"})),
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, headers, _) = request(
        &new_launch_origin,
        old_bootstrap_target
            .strip_prefix(&old_launch_origin)
            .unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!headers.contains_key("set-cookie"));
    assert!(!headers.contains_key("location"));
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(
        api(&runtime, &old_session_cookie, "/auth/logout", "POST", None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), runtime.shutdown_requested())
            .await
            .is_err(),
        "stale logout before redemption requested shutdown"
    );

    let (status, headers, body) = request(
        &new_launch_origin,
        new_bootstrap_target
            .strip_prefix(&new_launch_origin)
            .unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], "/");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert!(body.is_empty());
    let set_cookie = headers["set-cookie"].to_str().unwrap();
    assert!(set_cookie.split(';').any(|part| part.trim() == "HttpOnly"));
    assert!(
        set_cookie
            .split(';')
            .any(|part| part.trim() == "SameSite=Strict")
    );
    assert!(set_cookie.split(';').any(|part| part.trim() == "Path=/"));
    let new_session_cookie = set_cookie.split(';').next().unwrap().to_owned();
    assert!(new_session_cookie != old_session_cookie);
    assert_eq!(
        new_session_cookie.split_once('=').unwrap().0,
        old_session_cookie.split_once('=').unwrap().0
    );
    let (status, me) = api(&runtime, &new_session_cookie, "/auth/me", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["user"]["user_id"], "account-b");
    let (status, own) = api(&runtime, &new_session_cookie, "/plans/202", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(own["name"], "Foreign Plan");
    assert_eq!(
        api(&runtime, &new_session_cookie, "/plans/101", "GET", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        api(&runtime, &old_session_cookie, "/auth/me", "GET", None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api(
            &runtime,
            &old_session_cookie,
            "/plans/202",
            "PATCH",
            Some(json!({"name":"Stale after redemption"})),
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api(&runtime, &old_session_cookie, "/auth/logout", "POST", None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, headers, _) = request(
        &new_launch_origin,
        old_bootstrap_target
            .strip_prefix(&old_launch_origin)
            .unwrap(),
        "GET",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!headers.contains_key("set-cookie"));
    assert!(!headers.contains_key("location"));
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), runtime.shutdown_requested())
            .await
            .is_err(),
        "stale credentials after redemption requested shutdown"
    );
    let (status, me) = api(&runtime, &new_session_cookie, "/auth/me", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["user"]["user_id"], "account-b");
    let (status, own) = api(&runtime, &new_session_cookie, "/plans/202", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(own["name"], "Foreign Plan");
    assert_eq!(
        api(
            &runtime,
            &new_session_cookie,
            "/plans/202",
            "PATCH",
            Some(json!({"name":"Fresh account-b edit"})),
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, own) = api(&runtime, &new_session_cookie, "/plans/202", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(own["name"], "Fresh account-b edit");
    assert!(runtime.shutdown().await.complete());
    println!(
        "{}",
        json!({
            "case":"launch_credentials_are_revoked_on_same_origin_account_switch",
            "proof_class":"headless_unsigned",
            "origin":new_launch_origin,
            "same_origin":true,
            "old_account_authenticated":true,
            "old_cookie_denied_before_and_after_redemption":true,
            "old_bootstrap_denied_before_and_after_redemption":true,
            "bootstrap_denial_has_no_credentials_or_redirect":true,
            "stale_logout_did_not_request_shutdown":true,
            "fresh_bootstrap_clean_redirect_and_cookie_attributes":true,
            "same_cookie_name_with_new_value":true,
            "new_account_authenticated_and_own_plan_readable":true,
            "old_account_plan_unavailable":true,
            "stale_mutations_did_not_take_effect":true,
            "fresh_cookie_remained_valid":true,
            "fresh_mutation_accepted":true,
            "both_shutdown_receipts_complete":true
        })
    );
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn claimed_default_account_maps_legacy_default_data_without_foreign_changes() {
    let Some(data) = isolated(
        "claimed_default_account_maps_legacy_default_data_without_foreign_changes",
        true,
    )
    .await
    else {
        return;
    };
    fixture(&data).await;
    node(&data, "const db = new Database(data + '/print-partner.db'); db.prepare(\"INSERT INTO users (id,email,display_name,password_hash,is_admin,created_at) VALUES ('default','first@example.test','First account','hash',1,'2026-10-14')\").run(); db.close(); console.log('{}');").await;
    mapping(
        &data,
        json!({"version":1,"kind":"existing-account","user_id":"default","tenant_id":"default"}),
    );
    let before = snapshot_outside(&data, "default").await;
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    let (status, me) = api(&runtime, &cookie, "/auth/me", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        me["user"],
        json!({"user_id":"default","login":"first@example.test","display_name":"First account","email":"first@example.test","provider":"desktop","hasPassword":true,"is_admin":true})
    );
    let (status, legacy) = api(&runtime, &cookie, "/plans/303", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(legacy["name"], "Legacy Plan");
    for id in [101, 202] {
        assert_eq!(
            api(&runtime, &cookie, &format!("/plans/{id}"), "GET", None)
                .await
                .0,
            StatusCode::NOT_FOUND,
            "{id}"
        );
        assert_eq!(
            api(
                &runtime,
                &cookie,
                &format!("/plans/{id}"),
                "PATCH",
                Some(json!({"name":"Stolen"}))
            )
            .await
            .0,
            StatusCode::NOT_FOUND,
            "{id}"
        );
    }
    let (status, listing) = api(&runtime, &cookie, "/plans?tenant_id=account-a", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    let listed: Vec<_> = listing["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|profile| profile["id"].as_i64().unwrap())
        .collect();
    assert!(listed.contains(&303), "{listed:?}");
    assert!(
        !listed.contains(&101) && !listed.contains(&202),
        "{listed:?}"
    );
    let (status, _) = api(
        &runtime,
        &cookie,
        "/plans/303",
        "PATCH",
        Some(json!({"name":"Legacy edited"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, created) = api(
        &runtime,
        &cookie,
        "/plans",
        "POST",
        Some(json!({"name":"Default new Plan"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let created_id = created["id"].as_i64().unwrap();
    assert_eq!(
        api(
            &runtime,
            &cookie,
            "/backups/restore",
            "POST",
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(runtime.shutdown().await.complete());
    assert_eq!(snapshot_outside(&data, "default").await, before);
    let owned = node(&data, &format!("const db = new Database(data + '/print-partner.db', {{readonly:true}}); console.log(JSON.stringify(db.prepare(\"SELECT id,tenant_id,name FROM build_profiles WHERE id IN (303, ?) ORDER BY id\").all({created_id}))); db.close();")).await;
    assert_eq!(
        owned,
        json!([{"id":303,"tenant_id":"default","name":"Legacy edited"},{"id":created_id,"tenant_id":"default","name":"Default new Plan"}])
    );
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    assert_eq!(
        api(&runtime, &cookie, "/plans/303", "GET", None).await.1["name"],
        "Legacy edited"
    );
    assert!(runtime.shutdown().await.complete());
    assert_eq!(snapshot_outside(&data, "default").await, before);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn mapping_refusals_happen_before_product_writes() {
    let Some(root) = isolated("mapping_refusals_happen_before_product_writes", true).await else {
        return;
    };
    let mut cases = tokio::task::JoinSet::new();
    for case in [
        "no-map",
        "corrupt",
        "unknown-field",
        "unknown-user",
        "mismatch",
        "default",
        "unclaimed-default",
        "symlink",
        "public-mode",
        "old-schema",
    ] {
        let data = root.join(case);
        cases.spawn(async move {
            fixture(&data).await;
            let path = data.join("desktop-account.json");
            match case {
                "no-map" => {}
                "corrupt" => {
                    fs::write(&path, "{").unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                }
                "unknown-field" => {
                    let mut map = selected();
                    map["extra"] = json!(true);
                    mapping(&data, map);
                }
                "unknown-user" => mapping(
                    &data,
                    json!({"version":1,"kind":"existing-account","user_id":"unknown","tenant_id":"unknown"}),
                ),
                "mismatch" => {
                    let mut map = selected();
                    map["tenant_id"] = json!("account-b");
                    mapping(&data, map);
                }
                "default" => mapping(
                    &data,
                    json!({"version":1,"kind":"existing-account","user_id":"default","tenant_id":"default"}),
                ),
                "unclaimed-default" => mapping(
                    &data,
                    json!({"version":1,"kind":"existing-account","user_id":"account-a","tenant_id":"default"}),
                ),
                "symlink" => {
                    mapping(&data, selected());
                    fs::rename(&path, data.join("private-map.json")).unwrap();
                    std::os::unix::fs::symlink("private-map.json", &path).unwrap();
                }
                "public-mode" => {
                    mapping(&data, selected());
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                }
                "old-schema" => {
                    mapping(&data, selected());
                    node(&data, "const db = new Database(data + '/print-partner.db'); db.prepare(\"UPDATE app_settings SET value = ? WHERE tenant_id = 'default' AND key = ?\").run(String(currentSchemaVersion - 1), schemaVersionKey); db.close(); console.log('{}');").await;
                }
                _ => unreachable!(),
            }
            let rows = foreign_snapshot(&data).await;
            let bytes = fs::read(data.join("print-partner.db")).unwrap();
            assert!(
                CoreRuntime::start(launch(data.clone())).await.is_err(),
                "{case}"
            );
            assert_eq!(
                fs::read(data.join("print-partner.db")).unwrap(),
                bytes,
                "{case} wrote product DB"
            );
            assert_eq!(foreign_snapshot(&data).await, rows, "{case}");
            println!("{}", json!({"refused":case,"data":data}));
        });
        if cases.len() == 3 {
            while let Some(result) = cases.join_next().await {
                result.unwrap();
            }
        }
    }
    while let Some(result) = cases.join_next().await {
        result.unwrap();
    }
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn mapped_metadata_demotion_deletion_and_restore_refusal() {
    let Some(data) = isolated(
        "mapped_metadata_demotion_deletion_and_restore_refusal",
        true,
    )
    .await
    else {
        return;
    };
    fixture(&data).await;
    mapping(&data, selected());
    node(&data, "const db = new Database(data + '/print-partner.db'); db.prepare(\"UPDATE users SET is_admin = 1 WHERE id = 'account-a'\").run(); db.close(); console.log('{}');").await;
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    assert_eq!(
        api(&runtime, &cookie, "/auth/me", "GET", None).await.1["user"]["is_admin"],
        true
    );
    assert_eq!(
        api(
            &runtime,
            &cookie,
            "/backups/restore",
            "POST",
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for path in ["/api/v1/backups/restore", "/api/v2/backups/restore"] {
        let (status, body) = api(&runtime, &cookie, path, "POST", Some(json!({}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["detail"], "Operation not registered");
    }
    mapping(
        &data,
        json!({"version":1,"kind":"existing-account","user_id":"account-b","tenant_id":"account-b"}),
    );
    node(&data, "const db = new Database(data + '/print-partner.db'); db.prepare(\"UPDATE users SET is_admin = 0, display_name = 'Demoted' WHERE id = 'account-a'\").run(); db.close(); console.log('{}');").await;
    let me = api(&runtime, &cookie, "/auth/me", "GET", None).await.1;
    assert_eq!(me["user"]["user_id"], "account-a");
    assert_eq!(me["user"]["display_name"], "Demoted");
    assert_eq!(me["user"]["is_admin"], false);
    assert_eq!(
        api(&runtime, &cookie, "/settings/logging/config", "GET", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    node(&data, "const db = new Database(data + '/print-partner.db'); db.prepare(\"DELETE FROM users WHERE id = 'account-a'\").run(); db.close(); console.log('{}');").await;
    assert_eq!(
        api(&runtime, &cookie, "/auth/me", "GET", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api(
            &runtime,
            &cookie,
            "/plans/101",
            "PATCH",
            Some(json!({"name":"Deleted user"}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(runtime.shutdown().await.complete());
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn database_replacement_including_alias_fails_closed() {
    let Some(root) = isolated("database_replacement_including_alias_fails_closed", true).await
    else {
        return;
    };
    for alias in [false, true] {
        let data = root.join(if alias { "alias" } else { "direct" });
        fixture(&data).await;
        mapping(&data, selected());
        let path = data.join("print-partner.db");
        let actual = if alias {
            let actual = data.join("actual.db");
            fs::rename(&path, &actual).unwrap();
            std::os::unix::fs::symlink("actual.db", &path).unwrap();
            actual
        } else {
            path.clone()
        };
        let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
        let cookie = bootstrap(&mut runtime).await;
        assert_eq!(
            api(&runtime, &cookie, "/auth/me", "GET", None).await.0,
            StatusCode::OK
        );
        fs::rename(&actual, data.join("prior.db")).unwrap();
        fs::copy(data.join("prior.db"), &actual).unwrap();
        assert_eq!(
            api(&runtime, &cookie, "/auth/me", "GET", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            api(
                &runtime,
                &cookie,
                "/plans",
                "POST",
                Some(json!({"name":"Replacement"}))
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        assert!(runtime.shutdown().await.complete());
    }
}

#[tokio::test]
async fn gates_off_refuse_existing_accounts_even_with_child_env() {
    let Some(data) = isolated(
        "gates_off_refuse_existing_accounts_even_with_child_env",
        false,
    )
    .await
    else {
        return;
    };
    fixture(&data).await;
    mapping(&data, selected());
    let before = fs::read(data.join("print-partner.db")).unwrap();
    assert!(!pp_core::DesktopRebuild::from_env().enabled());
    // The refusing child never becomes Ready; the error says how it failed (GRE-989).
    let error = CoreRuntime::start(launch(data.clone()))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.starts_with("Compatibility readiness timed out after 32s; last status ")
            || error.starts_with("Compatibility child stopped before readiness: "),
        "{error}"
    );
    assert_eq!(fs::read(data.join("print-partner.db")).unwrap(), before);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn compile_gate_off_refuses_mapping_despite_runtime_opt_in() {
    let Some(data) = isolated(
        "compile_gate_off_refuses_mapping_despite_runtime_opt_in",
        true,
    )
    .await
    else {
        return;
    };
    fixture(&data).await;
    mapping(&data, selected());
    let before = fs::read(data.join("print-partner.db")).unwrap();
    assert!(!pp_core::DesktopRebuild::from_env().enabled());
    assert!(CoreRuntime::start(launch(data.clone())).await.is_err());
    assert_eq!(fs::read(data.join("print-partner.db")).unwrap(), before);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn fresh_store_stays_synthetic_when_enabled() {
    let Some(data) = isolated("fresh_store_stays_synthetic_when_enabled", true).await else {
        return;
    };
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    let (status, me) = api(&runtime, &cookie, "/auth/me", "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["user"]["user_id"], "desktop-owner");
    assert_eq!(me["user"]["is_admin"], true);
    assert!(runtime.shutdown().await.complete());
}

#[cfg(feature = "desktop_rebuild")]
async fn crash_child(runtime: &CoreRuntime) {
    let state = runtime.handle().compat_status().unwrap().borrow().clone();
    let pp_core::CoreStatus::Ready { pid, .. } = state else {
        panic!("not ready")
    };
    let mut states = runtime.handle().compat_status().unwrap();
    assert_eq!(
        unsafe { libc::kill(i32::try_from(pid).unwrap(), libc::SIGKILL) },
        0
    );
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match *states.borrow() {
                pp_core::CoreStatus::Ready { pid: next, .. } if next != pid => break,
                pp_core::CoreStatus::Guarded
                | pp_core::CoreStatus::Failed
                | pp_core::CoreStatus::Stopped => break,
                _ => {}
            }
            states.changed().await.unwrap();
        }
    })
    .await
    .expect("compat restart did not settle");
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn mapping_pin_survives_compat_crash_and_full_restart() {
    let Some(data) = isolated("mapping_pin_survives_compat_crash_and_full_restart", true).await
    else {
        return;
    };
    fixture(&data).await;
    node(&data, "const db = new Database(data + '/print-partner.db'); db.prepare('UPDATE projects SET last_synced_at = ?').run('2026-10-14'); db.close(); console.log('{}');").await;
    mapping(&data, selected());
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    assert_eq!(
        api(&runtime, &cookie, "/auth/me", "GET", None).await.1["user"]["user_id"],
        "account-a"
    );
    mapping(
        &data,
        json!({"version":1,"kind":"existing-account","user_id":"account-b","tenant_id":"account-b"}),
    );
    crash_child(&runtime).await;
    let (status, me) = api(&runtime, &cookie, "/auth/me", "GET", None).await;
    println!(
        "{}",
        json!({"case":"mapping_pin_after_crash","data":data,"status":status.as_u16(),"body":me})
    );
    assert_ne!(
        me["user"]["user_id"], "account-b",
        "an existing gateway session must never switch account"
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        api(&runtime, &cookie, "/plans/202", "GET", None).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(runtime.shutdown().await.complete());
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    assert_eq!(
        api(&runtime, &cookie, "/auth/me", "GET", None).await.1["user"]["user_id"],
        "account-b"
    );
    assert_eq!(
        api(&runtime, &cookie, "/plans/202", "GET", None).await.1["name"],
        "Foreign Plan"
    );
    assert!(runtime.shutdown().await.complete());
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn missing_mapping_pin_rejects_later_account_after_crash() {
    let Some(data) = isolated(
        "missing_mapping_pin_rejects_later_account_after_crash",
        true,
    )
    .await
    else {
        return;
    };
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    assert_eq!(
        api(&runtime, &cookie, "/auth/me", "GET", None).await.1["user"]["user_id"],
        "desktop-owner"
    );
    node(&data, "const db = new Database(data + '/print-partner.db'); db.exec(\"INSERT INTO users (id,display_name,is_admin,created_at) VALUES ('account-a','Selected account',0,'2026-10-14')\"); db.close(); console.log('{}');").await;
    mapping(&data, selected());
    crash_child(&runtime).await;
    let (status, me) = api(&runtime, &cookie, "/auth/me", "GET", None).await;
    println!(
        "{}",
        json!({"case":"missing_pin_after_crash","data":data,"status":status.as_u16(),"body":me})
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(runtime.shutdown().await.complete());
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    assert_eq!(
        api(&runtime, &cookie, "/auth/me", "GET", None).await.1["user"]["user_id"],
        "account-a"
    );
    assert!(runtime.shutdown().await.complete());
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn present_mapping_pin_rejects_removal_after_crash() {
    let Some(data) = isolated("present_mapping_pin_rejects_removal_after_crash", true).await else {
        return;
    };
    fixture(&data).await;
    mapping(&data, selected());
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let cookie = bootstrap(&mut runtime).await;
    fs::remove_file(data.join("desktop-account.json")).unwrap();
    crash_child(&runtime).await;
    let (status, me) = api(&runtime, &cookie, "/auth/me", "GET", None).await;
    println!(
        "{}",
        json!({"case":"removed_pin_after_crash","data":data,"status":status.as_u16(),"body":me})
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(runtime.shutdown().await.complete());
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn migrated_shared_default_plans_launch_with_explicit_owners() {
    let Some(root) = isolated(
        "migrated_shared_default_plans_launch_with_explicit_owners",
        true,
    )
    .await
    else {
        return;
    };
    let source = root.join("legacy");
    let destination = root.join("claimed");
    fs::create_dir_all(&source).unwrap();
    node(&source, r#"
        const store = new SqliteDatabase(data); store.connect(); store.close();
        const db = new Database(data + '/print-partner.db');
        db.exec(`DELETE FROM app_settings WHERE key <> 'schema_version';
          INSERT INTO users (id,email,display_name,password_hash,is_admin,created_at) VALUES
          ('account-a','a@example.test','Selected owner','hash',0,'2026-10-15'),
          ('account-b','b@example.test','Other owner',NULL,1,'2026-10-15');
          INSERT INTO build_profiles (id,tenant_id,name) VALUES
          (303,'default','Shared A'),(404,'default','Shared B'),(505,'account-b','Foreign Plan');
          INSERT INTO projects (id,tenant_id,name,url) VALUES
          (77,'default','Legacy Source','https://example.test/legacy');
          INSERT INTO profile_layers (tenant_id,profile_id,project_id,layer_type) VALUES ('default',303,77,'project');`);
        db.close(); console.log('{}');
    "#).await;
    fs::create_dir_all(source.join("repos/77")).unwrap();
    fs::write(source.join("repos/77/proof.stl"), "preserved source bytes").unwrap();
    let before = snapshot_outside(&source, "__all_tenants__").await;
    let control = root.join("unclaimed-control");
    node(&source, &format!(r#"const {{cpSync}} = await import('node:fs'); cpSync(data, {}, {{recursive:true}}); console.log('{{}}');"#,json!(control))).await;
    mapping(&control, selected());
    let mut unclaimed = CoreRuntime::start(launch(control)).await.unwrap();
    let cookie = bootstrap(&mut unclaimed).await;
    assert_eq!(
        api(&unclaimed, &cookie, "/plans/303", "GET", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(unclaimed.shutdown().await.complete());
    println!("unmigrated explicit-account control refuses default Plan with 404");
    let module = launch(source.clone())
        .bundle
        .entry
        .parent()
        .unwrap()
        .join("ownership-migration.js");
    let migration = node(&source, &format!(r#"
        const {{inspectOwnershipCopy,executeOwnershipCopy}} = await import({});
        const inventory = await inspectOwnershipCopy({{sourceDir:data}});
        const manifest = {{version:1,source_digest:inventory.source_digest,selected_user_id:'account-a',
          claims:inventory.claims.map(row => ({{table:row.table,rowid:row.rowid,user_id:row.table === 'build_profiles' && row.rowid === 404 ? 'account-b' : 'account-a'}}))}};
        console.log(JSON.stringify(await executeOwnershipCopy({{sourceDir:data,destinationDir:{},manifest}})));
    "#, json!(format!("file://{}", module.display())), json!(destination))).await;
    assert_eq!(
        migration,
        json!({"destinationDir":destination,"user_id":"account-a"})
    );
    assert_eq!(snapshot_outside(&source, "__all_tenants__").await, before);
    assert_eq!(
        fs::read(source.join("repos/77/proof.stl")).unwrap(),
        b"preserved source bytes"
    );
    assert_eq!(
        fs::read(destination.join("repos/77/proof.stl")).unwrap(),
        b"preserved source bytes"
    );
    let foreign_before = snapshot_outside(&destination, "account-a").await;
    for round in 0..2 {
        let mut runtime = CoreRuntime::start(launch(destination.clone()))
            .await
            .unwrap();
        let cookie = bootstrap(&mut runtime).await;
        let (status, me) = api(&runtime, &cookie, "/auth/me", "GET", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(me["user"]["user_id"], "account-a");
        assert_eq!(me["user"]["is_admin"], false);
        let (status, plan) = api(&runtime, &cookie, "/plans/303", "GET", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            plan["name"],
            if round == 0 {
                "Shared A"
            } else {
                "Claimed edit"
            }
        );
        for id in [404, 505] {
            for method in ["GET", "PATCH"] {
                assert_eq!(
                    api(
                        &runtime,
                        &cookie,
                        &format!("/plans/{id}"),
                        method,
                        if method == "PATCH" {
                            Some(json!({"name":"Stolen"}))
                        } else {
                            None
                        }
                    )
                    .await
                    .0,
                    StatusCode::NOT_FOUND
                );
            }
        }
        assert_eq!(
            api(
                &runtime,
                &cookie,
                "/plans/303",
                "PATCH",
                Some(json!({"name":"Claimed edit"}))
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(runtime.shutdown().await.complete());
        assert_eq!(
            snapshot_outside(&destination, "account-a").await,
            foreign_before
        );
    }
    assert_eq!(snapshot_outside(&source, "__all_tenants__").await, before);
}
