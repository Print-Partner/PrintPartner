//! `get_farm_status` and the Farm Module gate (GRE-1352 part 1) through the
//! MCP router with an API key. Printer A is printing `x.gcode` at 40%, B is
//! idle with one empty slot, C has no integration. Both integrations point
//! at a listener that counts connections, so a request to a printer shows.
use super::reads::{call, session};
use super::*;
use crate::farm::FarmObservations;
use pp_storage::read_model::farm::{HostState, Observation, PrinterIntegration};
use pp_storage::read_model::{Credential, farm::FarmPrinterList};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, SystemTime},
};

/// The watchers' view: an observation per integration, and when the watcher
/// saw each host change state.
#[derive(Default)]
struct Watchers {
    seen: HashMap<String, Observation>,
    entered: Mutex<HashMap<String, SystemTime>>,
}

impl FarmObservations for Watchers {
    fn observe(&self, integration: &PrinterIntegration) -> Observation {
        self.seen
            .get(&integration.id)
            .cloned()
            .unwrap_or(Observation::Unavailable)
    }

    fn seen_entering(&self, integration: &PrinterIntegration) -> Option<SystemTime> {
        self.entered.lock().unwrap().get(&integration.id).copied()
    }
}

fn seen(state: HostState, progress: Option<u8>, filename: Option<&str>) -> Observation {
    Observation::Seen {
        state,
        progress_percent: progress,
        filename: filename.map(str::to_owned),
        last_heard_at: Some("2026-11-12T12:00:00.000Z".into()),
        state_since: Some("2026-11-12T10:00:00.000Z".into()),
    }
}

fn seed(farm: bool, printer_host: &str) -> String {
    let integrations = json!([
        { "id": "moon-a", "type": "moonraker", "config": { "base_url": printer_host } },
        { "id": "moon-b", "type": "moonraker", "config": { "base_url": printer_host } },
    ]);
    let fleet = json!([
        { "id": "p-a", "name": "Printer A", "integration_id": "moon-a",
          "loaded_filaments": [{ "slot": 1, "filament_color_id": "pla-black", "label": "PLA Black" }] },
        { "id": "p-b", "name": "Printer B", "integration_id": "moon-b", "max_filament_slots": 2,
          "loaded_filaments": [{ "slot": 1, "filament_color_id": "petg-red", "label": "PETG Red" }] },
        { "id": "p-c", "name": "Printer C",
          "loaded_filaments": [{ "slot": 1, "filament_color_id": "pla-white", "label": "PLA White" }] },
    ]);
    let setting = |key: &str, value: &Value| {
        format!(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','{key}','{}') \
             ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value;",
            value.to_string().replace('\'', "''")
        )
    };
    let mut sql = setting("integrations", &integrations) + &setting("printer.fleet", &fleet);
    if farm {
        sql += &setting("feature_toggles", &json!({ "farm": true }));
    }
    sql
}

/// A listener the integrations name. A printer request would connect to it.
fn printer_host() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}

fn watchers(idle_entered: SystemTime) -> Arc<Watchers> {
    let watchers = Watchers {
        seen: HashMap::from([
            (
                "moon-a".into(),
                seen(HostState::Printing, Some(40), Some("x.gcode")),
            ),
            (
                "moon-b".into(),
                seen(HostState::Idle, None, Some("old.gcode")),
            ),
        ]),
        ..Default::default()
    };
    watchers
        .entered
        .lock()
        .unwrap()
        .insert("moon-b".into(), idle_entered);
    Arc::new(watchers)
}

/// What `GET /api/v2/printers` serves for the same observations.
async fn http_printers(server: &McpServer, watchers: Arc<Watchers>) -> FarmPrinterList {
    let client = server.owner.farm_reads(policy()).unwrap();
    let farm = tokio::task::spawn_blocking(move || {
        client.read(
            Credential::Session(Secret::new(FIXTURE_SESSION.into())),
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
    })
    .await
    .unwrap()
    .unwrap();
    farm.printers(|integration| watchers.observe(integration))
}

async fn tool_names(server: &McpServer, headers: &[(String, String)]) -> Vec<String> {
    let headers: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let request = json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" });
    let (status, body, _) = server.post("/api/v1/mcp", &headers, &request).await;
    assert_eq!(status, 200, "{body}");
    mcp_rpc::rpc(&body)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_farm_status_reports_what_the_watchers_saw() {
    let (host, url) = printer_host();
    let idle_entered = SystemTime::now() - Duration::from_secs(2 * 3600 + 5 * 60);
    let watched = watchers(idle_entered);
    let server = McpServer::watching(&seed(true, &url), watched.clone()).await;
    let bearer = format!("Bearer {}", server.create_key().await.1);
    let headers = session(&server, &bearer).await;
    let database = server._directory.join("print-partner.db");
    let before = pp_testkit::mcp_planning::state(&database).unwrap();

    let (text, error) = call(&server, &headers, "get_farm_status", json!({})).await;
    assert!(!error, "{text}");
    let status: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(status["printer_count"], 3);
    let printers = status["printers"].as_array().unwrap();
    let (a, b, c) = (&printers[0], &printers[1], &printers[2]);
    for printer in printers {
        for field in [
            "id",
            "name",
            "state",
            "integration_id",
            "active_job",
            "progress",
            "idle_since",
            "idle_for",
            "filament_slots",
        ] {
            assert!(printer.get(field).is_some(), "{field} in {printer}");
        }
        assert!(printer["eta_seconds"].is_null() && printer["message"].is_null());
        assert!(printer["filament_remaining_g"].is_null());
    }
    assert_eq!((&a["id"], &a["state"]), (&json!("p-a"), &json!("printing")));
    assert_eq!(
        (&a["active_job"], &a["progress"]),
        (&json!("x.gcode"), &json!(40))
    );
    assert_eq!(a["integration_id"], "moon-a");
    assert!(a["idle_since"].is_null() && a["idle_for"].is_null());
    assert_eq!(
        (&b["state"], &b["active_job"]),
        (&json!("idle"), &json!(null))
    );
    assert_eq!(
        b["idle_since"],
        pp_storage::read_model::farm::iso_time(idle_entered)
    );
    assert_eq!(b["idle_for"], "2h 5m");
    assert_eq!(b["empty_slots"], json!([2]));
    assert_eq!(
        (
            &c["state"],
            &c["not_connected_reason"],
            &c["integration_id"]
        ),
        (&json!("not_connected"), &json!("not_set_up"), &json!(null))
    );
    assert_eq!(
        status["state_counts"],
        json!({ "idle": 1, "printing": 1, "paused": 0, "complete": 0, "error": 0,
                "offline": 0, "not_connected": 1 })
    );
    assert_eq!(
        status["needs_filament_swap"],
        json!([{ "id": "p-b", "name": "Printer B", "empty_slots": [2] }])
    );

    // Each state agrees with the route's execution state for the same reading.
    let route = http_printers(&server, watched.clone()).await;
    let execution: Vec<Value> = route
        .printers
        .iter()
        .map(|printer| serde_json::to_value(printer.state.execution()).unwrap())
        .collect();
    assert_eq!(
        execution,
        json!(["printing", "idle", "not_set_up"])
            .as_array()
            .unwrap()
            .clone()
    );
    for (mcp, http) in printers.iter().zip(&route.printers) {
        assert_eq!(mcp["id"], http.id);
        assert_eq!(mcp["progress"], json!(http.progress_percent));
        assert_eq!(mcp["last_heard_at"], json!(http.last_heard_at));
    }

    // After a restart the watcher has not seen B go idle.
    watched.entered.lock().unwrap().clear();
    let (text, _) = call(&server, &headers, "get_farm_status", json!({})).await;
    let b = &serde_json::from_str::<Value>(&text).unwrap()["printers"][1];
    assert_eq!(
        (&b["idle_since"], &b["idle_for"]),
        (&json!(null), &json!("unknown"))
    );

    assert!(
        matches!(host.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "no request reached a printer"
    );
    assert_eq!(
        before,
        pp_testkit::mcp_planning::state(&database).unwrap(),
        "nothing written"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_farm_tools_are_hidden_and_refused_while_the_farm_module_is_off() {
    let (_host, url) = printer_host();
    let server = McpServer::watching(&seed(false, &url), watchers(SystemTime::now())).await;
    let bearer = format!("Bearer {}", server.create_key().await.1);
    let headers = session(&server, &bearer).await;
    let names = tool_names(&server, &headers).await;
    assert!(names.iter().any(|name| name == "list_plans"), "{names:?}");
    for tool in ["get_farm_status", "get_print_stats"] {
        assert!(!names.iter().any(|name| name == tool), "{tool} listed");
        let (text, error) = call(&server, &headers, tool, json!({})).await;
        assert!(error, "{tool}: {text}");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["code"],
            "farm_module_off"
        );
    }

    // Turned on, both are listed again.
    let connection =
        rusqlite::Connection::open(server._directory.join("print-partner.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    connection
        .execute_batch(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','feature_toggles','{\"farm\":true}');",
        )
        .unwrap();
    let names = tool_names(&server, &headers).await;
    assert!(
        names.iter().any(|name| name == "get_farm_status"),
        "{names:?}"
    );

    // Unreadable Module settings refuse with their own code.
    connection
        .execute(
            "UPDATE app_settings SET value='not json' WHERE key='feature_toggles'",
            [],
        )
        .unwrap();
    let (text, error) = call(&server, &headers, "get_farm_status", json!({})).await;
    assert!(error);
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["code"],
        "module_settings_invalid"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_farm_status_needs_a_key() {
    let (_host, url) = printer_host();
    let server = McpServer::watching(&seed(true, &url), watchers(SystemTime::now())).await;
    let bearer = format!("Bearer {}", server.create_key().await.1);
    let headers = session(&server, &bearer).await;
    let status = server
        .unkeyed_call_status(&headers, "get_farm_status", json!({}))
        .await;
    assert_eq!(status, 401);
}
