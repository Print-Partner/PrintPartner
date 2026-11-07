//! GRE-441 (T-62-mcp-domain): MCP dispatch parity with the Node product MCP server.
//!
//! The transcript tests replay the requests recorded in
//! `fixtures/parity/mcp-proposals/*.json` (GRE-577, recorded from the real Node
//! server) against [`McpSession`], normalize ids and paths the same way, and
//! compare the result to the golden byte for byte. The catalog test diffs
//! `tools/list` against the CI-regenerated Node inventory (GRE-542).

use pp_services::mcp_domain::{
    DomainPort, FarmAccess, McpError, McpSession, ProposedAction, ToolInvocation, UnportedDomain,
    list_custom_filaments, server_info,
};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const UNKNOWN_ACTION_ID: &str = "00000000-0000-4000-8000-000000000000";

fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

/// Port of the Node transcript `Normalizer`: placeholders by first appearance.
struct Normalizer {
    data_dir: String,
    seen: HashMap<String, String>,
    raw_by_placeholder: HashMap<String, String>,
    counters: HashMap<&'static str, usize>,
    uuid: Regex,
    filament: Regex,
    timestamp: Regex,
}

impl Normalizer {
    fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_str().unwrap().to_owned(),
            seen: HashMap::new(),
            raw_by_placeholder: HashMap::new(),
            counters: HashMap::new(),
            uuid: Regex::new(
                r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
            )
            .unwrap(),
            filament: Regex::new(r"\bcustom-[0-9a-f]{8}\b").unwrap(),
            timestamp: Regex::new(
                r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})\b",
            )
            .unwrap(),
        }
    }

    fn token(&mut self, kind: &'static str, raw: &str) -> String {
        if let Some(known) = self.seen.get(raw) {
            return known.clone();
        }
        let n = self.counters.entry(kind).or_default();
        *n += 1;
        let placeholder = format!("<{kind}-{n}>");
        self.seen.insert(raw.to_owned(), placeholder.clone());
        self.raw_by_placeholder
            .insert(placeholder.clone(), raw.to_owned());
        placeholder
    }

    fn replace_all(
        &mut self,
        input: &str,
        re: &Regex,
        kind: &'static str,
        keep: Option<&str>,
    ) -> String {
        let mut out = String::new();
        let mut last = 0;
        let matches: Vec<(usize, usize)> =
            re.find_iter(input).map(|m| (m.start(), m.end())).collect();
        for (start, end) in matches {
            let raw = &input[start..end];
            out.push_str(&input[last..start]);
            if Some(raw) == keep {
                out.push_str(raw);
            } else {
                out.push_str(&self.token(kind, raw));
            }
            last = end;
        }
        out.push_str(&input[last..]);
        out
    }

    fn string(&mut self, value: &str) -> String {
        let value = value.replace(&self.data_dir, "<data-dir>");
        let uuid = self.uuid.clone();
        let value = self.replace_all(&value, &uuid, "action-id", Some(UNKNOWN_ACTION_ID));
        let filament = self.filament.clone();
        let value = self.replace_all(&value, &filament, "custom-filament-id", None);
        self.timestamp
            .replace_all(&value, "<timestamp>")
            .into_owned()
    }

    fn value(&mut self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.string(s)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.value(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (self.string(k), self.value(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Request arguments in a golden hold placeholders; map them back to raw ids.
    fn denormalize(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(
                self.raw_by_placeholder
                    .get(s)
                    .cloned()
                    .unwrap_or_else(|| s.clone()),
            ),
            Value::Array(items) => {
                Value::Array(items.iter().map(|v| self.denormalize(v)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.denormalize(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

/// Replays one golden through Rust and returns (expected, actual) file text.
fn replay(golden_path: &Path) -> (String, String) {
    // A Windows checkout may hold the golden with CRLF line endings.
    let expected = fs::read_to_string(golden_path)
        .unwrap()
        .replace("\r\n", "\n");
    let golden: Value = serde_json::from_str(&expected).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let session = McpSession::new(UnportedDomain, Some(data_dir.clone()));
    let mut normalizer = Normalizer::new(&data_dir);

    let mut steps = Vec::new();
    for step in golden["steps"].as_array().unwrap() {
        let params = &step["request"]["params"];
        let name = params["name"].as_str().unwrap();
        let args = normalizer.denormalize(&params["arguments"]);
        let result = session.call_tool(name, &args).unwrap();
        let content = match serde_json::from_str::<Value>(&result.text) {
            Ok(parsed) => json!({ "type": "text", "json": parsed }),
            Err(_) => json!({ "type": "text", "text": result.text }),
        };
        // Normalized per step, in transcript order, so later requests can map
        // placeholders back to the raw ids this run produced.
        steps.push(normalizer.value(&json!({
            "request": { "method": "tools/call", "params": { "name": name, "arguments": args } },
            "response": { "isError": result.is_error, "content": [content] },
        })));
    }
    let final_state = normalizer.value(&json!({
        "pending_action_ids": session.pending_ids(),
        "custom_filaments": list_custom_filaments(&data_dir),
    }));
    let normalized = json!({
        "scenario": golden["scenario"],
        "description": golden["description"],
        "steps": steps,
        "final_state": final_state,
    });
    let actual = format!("{}\n", serde_json::to_string_pretty(&normalized).unwrap());
    assert!(
        !actual.contains(data_dir.to_str().unwrap()),
        "temp path leaked into transcript"
    );
    (expected, actual)
}

fn golden_transcripts() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(repo_path("fixtures/parity/mcp-proposals"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn ticket_t_62_mcp_domain_replays_every_node_proposal_transcript_byte_for_byte() {
    let paths = golden_transcripts();
    let names: Vec<_> = paths
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "confirm-twice.json",
            "propose-dismiss.json",
            "propose-list-confirm.json",
            "unknown-key-rejected.json"
        ]
    );
    for path in &paths {
        let (expected, actual) = replay(path);
        assert_eq!(
            actual,
            expected,
            "MCP transcript drift in {}",
            path.display()
        );
    }
}

#[test]
fn ticket_t_62_mcp_domain_tools_list_matches_node_inventory_golden() {
    let golden: Value = serde_json::from_str(
        &fs::read_to_string(repo_path(
            "web/apps/server/fixtures/mcp/tools-inventory.json",
        ))
        .unwrap(),
    )
    .unwrap();
    let session = McpSession::new(UnportedDomain, None);
    let listed = session.list_tools().unwrap();
    let mut tools: Vec<Value> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| json!({ "name": t["name"], "inputSchema": t["inputSchema"] }))
        .collect();
    tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    assert_eq!(golden["toolCount"], json!(tools.len()));
    assert_eq!(golden["server"], server_info()["serverInfo"]);
    let golden_tools = golden["tools"].as_array().unwrap();
    for (rust, node) in tools.iter().zip(golden_tools) {
        assert_eq!(
            rust, node,
            "tool {} differs from the Node inventory",
            node["name"]
        );
    }
    for tool in listed["tools"].as_array().unwrap() {
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "{} has no description",
            tool["name"]
        );
    }
}

/// A scripted port that records what reaches it.
struct FakePort {
    enabled: bool,
    farm: FarmAccess,
    apply_result: Result<Value, String>,
    reconcile_result: Result<Option<Value>, String>,
    applied: Mutex<Vec<ProposedAction>>,
}

impl FakePort {
    fn new() -> Self {
        Self {
            enabled: true,
            farm: FarmAccess::Allowed,
            apply_result: Ok(json!({ "ok": true })),
            reconcile_result: Ok(None),
            applied: Mutex::default(),
        }
    }
}

impl DomainPort for FakePort {
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn farm_access(&self) -> FarmAccess {
        self.farm.clone()
    }
    fn invoke_tool(&self, name: &str, args: &Map<String, Value>) -> Result<ToolInvocation, String> {
        match name {
            "list_plans" => Ok(ToolInvocation {
                content: json!({ "plans": [] }).to_string(),
                proposed_action: None,
            }),
            "get_plan_review" => Ok(ToolInvocation {
                content: json!({ "error": "plan_id required" }).to_string(),
                proposed_action: None,
            }),
            "archive_plan" | "get_workflow_help" => {
                let action_type = if name == "archive_plan" {
                    "archive_plan"
                } else {
                    "ui_navigate"
                };
                let action = ProposedAction::new(action_type, 7, "l".into(), "s", args.clone());
                Ok(ToolInvocation {
                    content: action.proposed_content(),
                    proposed_action: Some(action),
                })
            }
            _ => Err(format!("boom in {name}")),
        }
    }
    fn apply_action(&self, action: &ProposedAction) -> Result<Value, String> {
        self.applied.lock().unwrap().push(action.clone());
        self.apply_result.clone()
    }
    fn reconcile(&self, _action_id: &str) -> Result<Option<Value>, String> {
        self.reconcile_result.clone()
    }
}

#[test]
fn ticket_t_62_mcp_domain_confirm_of_an_unheld_action_reconciles_or_fails_but_never_guesses() {
    let unknown = |port: FakePort| {
        McpSession::new(port, None).call_tool("confirm_apply", &json!({ "action_id": "gone" }))
    };
    let (body, is_error) = call(
        &McpSession::new(FakePort::new(), None),
        "confirm_apply",
        json!({ "action_id": "gone" }),
    );
    assert!(is_error);
    assert_eq!(body["action_id"], "gone");

    let mut settled = FakePort::new();
    settled.reconcile_result = Ok(Some(json!({ "ok": true, "reconciled": true })));
    let result = unknown(settled).unwrap();
    assert!(!result.is_error);
    assert_eq!(
        serde_json::from_str::<Value>(&result.text).unwrap()["reconciled"],
        true
    );

    let mut busy = FakePort::new();
    busy.reconcile_result = Err("Checkoff is unavailable".into());
    let failed = unknown(busy).unwrap();
    assert!(failed.is_error);
    assert_eq!(
        serde_json::from_str::<Value>(&failed.text).unwrap(),
        json!({ "error": "Checkoff is unavailable" })
    );
}

fn call(session: &McpSession<FakePort>, name: &str, args: Value) -> (Value, bool) {
    let result = session.call_tool(name, &args).unwrap();
    (serde_json::from_str(&result.text).unwrap(), result.is_error)
}

#[test]
fn ticket_t_62_mcp_domain_farm_tools_hide_and_refuse_when_farm_access_is_denied() {
    let mut port = FakePort::new();
    port.farm = FarmAccess::Denied {
        code: "FEATURE_DISABLED".into(),
        error: "Feature farm is disabled.".into(),
    };
    let session = McpSession::new(port, None);
    let listed = session.list_tools().unwrap();
    let names: Vec<&str> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 84);
    assert!(!names.contains(&"get_farm_status") && !names.contains(&"get_print_stats"));
    for tool in ["get_farm_status", "get_print_stats"] {
        assert_eq!(
            call(&session, tool, json!({})),
            (
                json!({ "error": "Feature farm is disabled.", "code": "FEATURE_DISABLED" }),
                true
            )
        );
    }
    assert_eq!(
        McpSession::new(FakePort::new(), None).list_tools().unwrap()["tools"]
            .as_array()
            .unwrap()
            .len(),
        86
    );
}

#[test]
fn ticket_t_62_mcp_domain_access_switch_off_fails_every_request() {
    let mut port = FakePort::new();
    port.enabled = false;
    let session = McpSession::new(port, None);
    let off = McpError("MCP access is turned off in Print Partner Settings.".into());
    assert_eq!(session.list_tools().unwrap_err(), off);
    assert_eq!(
        session
            .call_tool("list_pending_actions", &json!({}))
            .unwrap_err(),
        off
    );
    assert_eq!(session.list_prompts().unwrap_err(), off);
    assert_eq!(session.list_resources().unwrap_err(), off);
    assert_eq!(session.list_resource_templates().unwrap_err(), off);
}

#[test]
fn ticket_t_62_mcp_domain_unknown_failing_and_erroring_tools_match_node_results() {
    let session = McpSession::new(FakePort::new(), None);
    assert_eq!(
        call(&session, "nope", json!({})),
        (json!({ "error": "Unknown tool: nope" }), true)
    );
    // SPA-only ui_* tools are not product MCP tools.
    assert_eq!(
        call(&session, "ui_navigate", json!({})),
        (json!({ "error": "Unknown tool: ui_navigate" }), true)
    );
    assert_eq!(
        call(&session, "list_plans", json!({})),
        (json!({ "plans": [] }), false)
    );
    // Any tool output with an "error" key is an MCP error result.
    assert_eq!(
        call(&session, "get_plan_review", json!({})),
        (json!({ "error": "plan_id required" }), true)
    );
    // A thrown port error becomes {"error": message}.
    assert_eq!(
        call(&session, "get_remaining", json!({})),
        (json!({ "error": "boom in get_remaining" }), true)
    );
    // Non-object arguments are treated as {}.
    assert_eq!(
        session
            .call_tool("list_pending_actions", &json!("x"))
            .unwrap()
            .text,
        "{\n  \"actions\": []\n}"
    );
    let unported = McpSession::new(UnportedDomain, None);
    let result = unported.call_tool("list_plans", &json!({})).unwrap();
    assert!(result.is_error);
    assert_eq!(
        serde_json::from_str::<Value>(&result.text).unwrap(),
        json!({ "error": "list_plans has no Rust port yet" })
    );
}

#[test]
fn ticket_t_62_mcp_domain_failed_apply_restores_the_original_action_at_the_end() {
    let mut port = FakePort::new();
    port.apply_result = Ok(json!({ "ok": false, "detail": "Plan not found" }));
    let session = McpSession::new(port, None);
    let (first, _) = call(&session, "archive_plan", json!({ "plan_id": 7 }));
    let (second, _) = call(&session, "archive_plan", json!({ "plan_id": 8 }));
    let first_id = first["action"]["id"].as_str().unwrap().to_owned();
    let second_id = second["action"]["id"].as_str().unwrap().to_owned();

    let result = session
        .call_tool(
            "confirm_apply",
            &json!({ "action_id": first_id, "suggested_excludes": [" a ", "", 3, null] }),
        )
        .unwrap();
    assert!(result.is_error);
    assert_eq!(
        result.text,
        "{\n  \"ok\": false,\n  \"detail\": \"Plan not found\"\n}"
    );
    // The port saw the suggested_excludes override (String(x).trim(), empties dropped)...
    let applied = session.port().applied.lock().unwrap().clone();
    assert_eq!(
        applied[0].params["suggested_excludes"],
        json!(["a", "3", "null"])
    );
    // ...but the restored pending action is the original, moved to the end like Map.set after delete.
    assert_eq!(session.pending_ids(), [second_id.clone(), first_id.clone()]);
    let (listed, _) = call(&session, "list_pending_actions", json!({}));
    assert_eq!(listed["actions"][1]["params"], json!({ "plan_id": 7 }));
}

#[test]
fn ticket_t_62_mcp_domain_thrown_apply_restores_and_reports_error() {
    let mut port = FakePort::new();
    port.apply_result = Err("disk full".into());
    let session = McpSession::new(port, None);
    let (proposed, _) = call(&session, "archive_plan", json!({}));
    let id = proposed["action"]["id"].as_str().unwrap();
    assert_eq!(
        call(&session, "confirm_apply", json!({ "action_id": id })),
        (json!({ "error": "disk full" }), true)
    );
    assert_eq!(session.pending_ids(), [id]);
}

#[test]
fn ticket_t_62_mcp_domain_ui_actions_never_enter_the_pending_map() {
    let session = McpSession::new(FakePort::new(), None);
    let (proposed, is_error) = call(&session, "get_workflow_help", json!({}));
    assert!(!is_error);
    assert_eq!(proposed["action"]["type"], "ui_navigate");
    assert!(session.pending_ids().is_empty());
    let id = proposed["action"]["id"].as_str().unwrap();
    assert!(call(&session, "confirm_apply", json!({ "action_id": id })).1);
}

#[test]
fn ticket_t_62_mcp_domain_custom_filament_propose_validates_and_apply_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let session = McpSession::new(UnportedDomain, Some(dir.path().to_path_buf()));
    for bad in [
        json!({ "display_name": " ", "hex": "#123456" }),
        json!({ "display_name": "X", "hex": "12345" }),
        json!({ "display_name": "X", "hex": "#12345g" }),
    ] {
        let result = session
            .call_tool("propose_add_custom_filament", &bad)
            .unwrap();
        assert!(result.is_error);
        assert_eq!(
            result.text,
            r#"{"error":"display_name and a six-digit hex color are required"}"#
        );
    }
    assert!(session.pending_ids().is_empty());

    let apply = |args: Value| {
        let proposed: Value = serde_json::from_str(
            &session
                .call_tool("propose_add_custom_filament", &args)
                .unwrap()
                .text,
        )
        .unwrap();
        let id = proposed["action"]["id"].as_str().unwrap().to_owned();
        assert!(
            Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
                .unwrap()
                .is_match(&id)
        );
        let result = session
            .call_tool("confirm_apply", &json!({ "action_id": id }))
            .unwrap();
        assert!(!result.is_error, "{}", result.text);
        serde_json::from_str::<Value>(&result.text).unwrap()["result"].clone()
    };
    let first = apply(json!({ "display_name": "  Orange ", "hex": "FF6600" }));
    assert_eq!(first["filament"]["hex"], "#ff6600");
    assert_eq!(first["filament"]["product_line"], "Custom");
    assert_eq!(first["idempotent_replay"], false);
    let replay =
        apply(json!({ "display_name": "orange", "hex": "#ff6600", "product_line": " custom " }));
    assert_eq!(replay["idempotent_replay"], true);
    assert_eq!(replay["filament"], first["filament"]);
    // A blank product line misses the "Custom" entry, then saves as "Custom" (Node does the same).
    let blank = apply(json!({ "display_name": "Orange", "hex": "#FF6600", "product_line": "  " }));
    assert_eq!(
        (
            blank["idempotent_replay"].clone(),
            blank["filament"]["product_line"].clone()
        ),
        (json!(false), json!("Custom"))
    );

    // Node file format: two-space pretty JSON, no trailing newline, entry key order kept.
    let text = fs::read_to_string(dir.path().join("custom_filaments.json")).unwrap();
    assert!(
        text.starts_with("{\n  \"filaments\": [\n    {\n      \"id\": \"custom-")
            && !text.ends_with('\n')
    );
    assert_eq!(list_custom_filaments(dir.path()).len(), 2);
}

#[test]
fn ticket_t_62_mcp_domain_custom_filament_apply_fails_on_a_malformed_store_like_node() {
    let dir = tempfile::tempdir().unwrap();
    let session = McpSession::new(UnportedDomain, Some(dir.path().to_path_buf()));
    let confirm = |args: Value| {
        let proposed: Value = serde_json::from_str(
            &session
                .call_tool("propose_add_custom_filament", &args)
                .unwrap()
                .text,
        )
        .unwrap();
        let id = proposed["action"]["id"].as_str().unwrap().to_owned();
        session
            .call_tool("confirm_apply", &json!({ "action_id": id }))
            .unwrap()
    };
    let store = dir.path().join("custom_filaments.json");
    // Node: listCustomFilaments(...).find on undefined throws; nothing is written.
    fs::write(&store, r#"{"other":1}"#).unwrap();
    assert!(confirm(json!({ "display_name": "A", "hex": "#000000" })).is_error);
    assert_eq!(fs::read_to_string(&store).unwrap(), r#"{"other":1}"#);
    // Node: entry.display_name.toLowerCase() throws on a number.
    fs::write(&store, r#"{"filaments":[{"display_name":5}]}"#).unwrap();
    assert!(confirm(json!({ "display_name": "A", "hex": "#000000" })).is_error);
    // An entry whose name differs is never read past display_name, as in Node.
    fs::write(&store, r#"{"filaments":[{"display_name":"B","hex":7}]}"#).unwrap();
    let added = confirm(json!({ "display_name": "A", "hex": "#000000" }));
    assert!(!added.is_error, "{}", added.text);
    // Unparsable JSON reads as an empty store.
    fs::write(&store, "{").unwrap();
    assert!(!confirm(json!({ "display_name": "A", "hex": "#000000" })).is_error);
    assert_eq!(list_custom_filaments(dir.path()).len(), 1);
}

#[test]
fn ticket_t_62_mcp_domain_custom_filament_apply_without_data_dir_keeps_the_proposal() {
    let session = McpSession::new(UnportedDomain, None);
    let proposed: Value = serde_json::from_str(
        &session
            .call_tool(
                "propose_add_custom_filament",
                &json!({ "display_name": "A", "hex": "#000000" }),
            )
            .unwrap()
            .text,
    )
    .unwrap();
    let id = proposed["action"]["id"].as_str().unwrap();
    let result = session
        .call_tool("confirm_apply", &json!({ "action_id": id }))
        .unwrap();
    assert!(result.is_error);
    assert_eq!(
        serde_json::from_str::<Value>(&result.text).unwrap(),
        json!({ "ok": false, "detail": "Custom filament storage is unavailable" })
    );
    assert_eq!(session.pending_ids(), [id]);
}

#[test]
fn ticket_t_62_mcp_domain_prompts_and_resources_match_the_node_catalog() {
    let session = McpSession::new(UnportedDomain, None);
    let prompts = session.list_prompts().unwrap();
    assert_eq!(prompts["prompts"][0]["name"], "plan_build");
    let mut args = Map::new();
    args.insert(
        "customer_request".into(),
        json!("Build a $1 {{customer_request}} widget"),
    );
    let prompt = session.get_prompt("plan_build", &args).unwrap();
    let text = prompt["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.starts_with("Plan this Build without choosing between conflicting sources on my behalf.\n\nBuild a $1 {{customer_request}} widget\n\nAnalyze the request"));
    assert!(text.ends_with("Do not start printing, exporting, or queueing."));
    let empty = session.get_prompt("plan_build", &Map::new()).unwrap();
    assert!(
        empty["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("behalf.\n\n\n\nAnalyze")
    );
    assert_eq!(
        session.get_prompt("other", &Map::new()).unwrap_err(),
        McpError("Unknown prompt: other".into())
    );
    assert_eq!(
        session.list_resources().unwrap()["resources"][0]["uri"],
        "print-partner://source-categories"
    );
    let listed = session.list_resource_templates().unwrap();
    let templates: Vec<&str> = listed["resourceTemplates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["uriTemplate"].as_str().unwrap())
        .collect();
    assert_eq!(
        templates,
        [
            "print-partner://build-workflow/{build_id}",
            "print-partner://build-planning/{build_id}"
        ]
    );
    assert_eq!(
        server_info()["capabilities"],
        json!({ "prompts": {}, "resources": {}, "tools": {} })
    );
}
