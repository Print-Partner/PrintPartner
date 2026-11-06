//! MCP tool, prompt and proposal dispatch on Rust domain ports (GRE-441).
//!
//! Ports `web/apps/server/src/mcp/product-mcp.ts`. Mutating tools only
//! propose; `confirm_apply` and `dismiss_proposed_action` settle a proposal.
//! Each [`McpSession`] owns its pending actions, like one stdio process or one
//! streamable-HTTP session in Node.
//!
//! Nothing routes here yet. The gateway answers `/api/v1/mcp` as disabled
//! until every tool has a Rust port, then all MCP routes switch at once.
//! Tools without a Rust port go through [`DomainPort`].
//!
//! The catalog (`mcp_domain/catalog.json`) is exported from the live Node
//! server by `web/apps/server/scripts/export-mcp-catalog.ts`; the Node test
//! `src/mcp/rust-catalog.test.ts` fails when the two differ.

mod custom_filaments;

pub use custom_filaments::list as list_custom_filaments;

use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

const CATALOG_JSON: &str = include_str!("mcp_domain/catalog.json");
const CUSTOMER_REQUEST_PLACEHOLDER: &str = "{{customer_request}}";

/// Session tools handled here; every other catalog tool is a product tool.
pub const META_TOOLS: [&str; 3] = [
    "list_pending_actions",
    "confirm_apply",
    "dismiss_proposed_action",
];
/// Listed and callable only while farm access is allowed.
pub const FARM_TOOLS: [&str; 2] = ["get_farm_status", "get_print_stats"];
/// `ASSISTANT_UI_ACTION_TYPES`: client-only actions that never enter the pending map.
pub const UI_ACTION_TYPES: [&str; 6] = [
    "ui_navigate",
    "ui_open_source",
    "ui_open_docs",
    "ui_highlight_part",
    "ui_focus_stl_search",
    "ui_focus_kit_option",
];

const ACCESS_OFF: &str = "MCP access is turned off in Print Partner Settings.";
const UNKNOWN_ACTION: &str = "Unknown, in-flight, or already applied/dismissed action_id";
const PROPOSED_NOTE: &str = "Not applied yet — user must confirm via Apply in the UI.";

fn catalog() -> &'static Map<String, Value> {
    static CATALOG: OnceLock<Map<String, Value>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        // A malformed catalog lists nothing; the ticket tests fail on that.
        serde_json::from_str(CATALOG_JSON).unwrap_or_default()
    })
}

fn catalog_array(key: &str) -> &'static [Value] {
    catalog()
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or_default()
}

/// True for tools the Node server answers past the unknown-tool check.
pub fn is_product_tool(name: &str) -> bool {
    !META_TOOLS.contains(&name)
        && catalog_array("tools")
            .iter()
            .any(|tool| tool_name(tool) == name)
}

/// `initialize` result fields: server name/version and capabilities.
pub fn server_info() -> Value {
    json!({ "serverInfo": catalog()["server"], "capabilities": catalog()["capabilities"] })
}

/// A proposed mutation waiting for `confirm_apply` (`AssistantProposedAction`).
#[derive(Clone, Debug, PartialEq)]
pub struct ProposedAction {
    pub id: String,
    pub action_type: String,
    pub plan_id: i64,
    pub label: String,
    pub summary: String,
    pub params: Map<String, Value>,
}

impl ProposedAction {
    /// New action with a random v4 UUID, like `proposeAssistantAction`.
    pub fn new(
        action_type: &str,
        plan_id: i64,
        label: String,
        summary: &str,
        params: Map<String, Value>,
    ) -> Self {
        Self {
            id: random_uuid_v4(),
            action_type: action_type.into(),
            plan_id,
            label,
            summary: summary.into(),
            params,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "type": self.action_type,
            "plan_id": self.plan_id,
            "label": self.label,
            "summary": self.summary,
            "params": self.params,
        })
    }

    /// Tool output for a fresh proposal: `{status, note, action}`.
    pub fn proposed_content(&self) -> String {
        json!({ "status": "proposed", "note": PROPOSED_NOTE, "action": self.to_json() }).to_string()
    }
}

fn random_uuid_v4() -> String {
    let mut bytes: [u8; 16] = rand::random();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Result of a product tool: its JSON text and an optional proposal.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolInvocation {
    pub content: String,
    pub proposed_action: Option<ProposedAction>,
}

/// `readFarmAccess`: whether farm tools are listed and callable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FarmAccess {
    Allowed,
    Denied { code: String, error: String },
}

/// Domain operations not yet ported into this module.
pub trait DomainPort: Send + Sync {
    /// The live MCP access switch (`isEnabled`); off fails every request.
    fn enabled(&self) -> bool {
        true
    }
    fn farm_access(&self) -> FarmAccess;
    /// `invokeAssistantTool` for a product tool. `Err` becomes `{"error": msg}`.
    fn invoke_tool(&self, name: &str, args: &Map<String, Value>) -> Result<ToolInvocation, String>;
    /// `applyAssistantAction`: a JSON object with an `ok` boolean. `Err` is a thrown error.
    fn apply_action(&self, action: &ProposedAction) -> Result<Value, String>;
    /// The settled result of an action this session no longer holds (applied
    /// already, or proposed before a restart), when the domain recorded it
    /// under the action id. `None` answers Node's unknown-action error.
    fn reconcile(&self, _action_id: &str) -> Option<Value> {
        None
    }
}

/// A port with no domain tools. Every product tool reports that it has no Rust port.
pub struct UnportedDomain;

impl DomainPort for UnportedDomain {
    fn farm_access(&self) -> FarmAccess {
        FarmAccess::Allowed
    }
    fn invoke_tool(
        &self,
        name: &str,
        _args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        Err(format!("{name} has no Rust port yet"))
    }
    fn apply_action(&self, action: &ProposedAction) -> Result<Value, String> {
        Ok(json!({ "ok": false, "detail": format!("Unknown action type: {}", action.action_type) }))
    }
}

/// A protocol-level failure (the Node handler threw).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpError(pub String);

/// A `tools/call` result with one text content block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

impl ToolResult {
    fn ok(text: String) -> Self {
        Self {
            text,
            is_error: false,
        }
    }
    fn error(value: Value) -> Self {
        Self {
            text: value.to_string(),
            is_error: true,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut out = json!({ "content": [{ "type": "text", "text": self.text }] });
        if self.is_error {
            out["isError"] = Value::Bool(true);
        }
        out
    }
}

/// One MCP client's state: its pending proposals and the ports it calls.
pub struct McpSession<P: DomainPort> {
    port: P,
    /// Where `custom_filaments.json` lives; `None` makes that apply fail.
    data_dir: Option<PathBuf>,
    /// Insertion-ordered like a JS `Map`; a restored entry moves to the end.
    pending: Mutex<Vec<ProposedAction>>,
}

impl<P: DomainPort> McpSession<P> {
    pub fn new(port: P, data_dir: Option<PathBuf>) -> Self {
        Self {
            port,
            data_dir,
            pending: Mutex::new(Vec::new()),
        }
    }

    pub fn port(&self) -> &P {
        &self.port
    }

    pub fn pending_ids(&self) -> Vec<String> {
        self.lock().iter().map(|a| a.id.clone()).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<ProposedAction>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_pending(&self, action: ProposedAction) {
        let mut pending = self.lock();
        match pending.iter_mut().find(|a| a.id == action.id) {
            Some(slot) => *slot = action,
            None => pending.push(action),
        }
    }

    fn take_pending(&self, id: &str) -> Option<ProposedAction> {
        let mut pending = self.lock();
        let index = pending.iter().position(|a| a.id == id)?;
        Some(pending.remove(index))
    }

    fn assert_enabled(&self) -> Result<(), McpError> {
        if self.port.enabled() {
            Ok(())
        } else {
            Err(McpError(ACCESS_OFF.into()))
        }
    }

    pub fn list_tools(&self) -> Result<Value, McpError> {
        self.assert_enabled()?;
        let farm_listed = self.port.farm_access() == FarmAccess::Allowed;
        let tools: Vec<&Value> = catalog_array("tools")
            .iter()
            .filter(|tool| farm_listed || !FARM_TOOLS.contains(&tool_name(tool)))
            .collect();
        Ok(json!({ "tools": tools }))
    }

    pub fn list_prompts(&self) -> Result<Value, McpError> {
        self.assert_enabled()?;
        Ok(json!({ "prompts": catalog()["prompts"] }))
    }

    pub fn get_prompt(&self, name: &str, args: &Map<String, Value>) -> Result<Value, McpError> {
        self.assert_enabled()?;
        if name != "plan_build" {
            return Err(McpError(format!("Unknown prompt: {name}")));
        }
        let request = args
            .get("customer_request")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut prompt = catalog()["plan_build_prompt"].clone();
        for message in prompt["messages"].as_array_mut().into_iter().flatten() {
            if let Some(text) = message["content"]["text"].as_str() {
                message["content"]["text"] =
                    Value::String(text.replacen(CUSTOMER_REQUEST_PLACEHOLDER, request, 1));
            }
        }
        Ok(prompt)
    }

    pub fn list_resources(&self) -> Result<Value, McpError> {
        self.assert_enabled()?;
        Ok(json!({ "resources": catalog()["resources"] }))
    }

    pub fn list_resource_templates(&self) -> Result<Value, McpError> {
        self.assert_enabled()?;
        Ok(json!({ "resourceTemplates": catalog()["resource_templates"] }))
    }

    /// `tools/call`. Non-object arguments count as `{}`.
    pub fn call_tool(&self, name: &str, arguments: &Value) -> Result<ToolResult, McpError> {
        self.assert_enabled()?;
        let empty = Map::new();
        let args = arguments.as_object().unwrap_or(&empty);
        Ok(match self.dispatch(name, args) {
            Ok(result) => result,
            Err(message) => ToolResult::error(json!({ "error": message })),
        })
    }

    fn dispatch(&self, name: &str, args: &Map<String, Value>) -> Result<ToolResult, String> {
        match name {
            "list_pending_actions" => {
                let actions: Vec<Value> = self.lock().iter().map(ProposedAction::to_json).collect();
                Ok(ToolResult::ok(pretty(&json!({ "actions": actions }))))
            }
            "dismiss_proposed_action" => {
                let id = js_string_or_empty(args.get("action_id"));
                let existed = self.take_pending(&id).is_some();
                Ok(ToolResult::ok(
                    json!({ "ok": existed, "action_id": id }).to_string(),
                ))
            }
            "confirm_apply" => self.confirm_apply(args),
            _ => self.invoke_product_tool(name, args),
        }
    }

    fn confirm_apply(&self, args: &Map<String, Value>) -> Result<ToolResult, String> {
        let id = js_string_or_empty(args.get("action_id"));
        // Taken before apply so a concurrent confirm cannot apply it twice.
        let Some(action) = self.take_pending(&id) else {
            if let Some(result) = self.port.reconcile(&id) {
                let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
                return Ok(ToolResult {
                    text: pretty(&result),
                    is_error: !ok,
                });
            }
            return Ok(ToolResult::error(
                json!({ "ok": false, "error": UNKNOWN_ACTION, "action_id": id }),
            ));
        };
        if UI_ACTION_TYPES.contains(&action.action_type.as_str()) {
            self.set_pending(action);
            return Ok(ToolResult::error(
                json!({ "ok": false, "error": "UI actions cannot be applied via MCP" }),
            ));
        }
        let mut to_apply = action.clone();
        if let Some(Value::Array(excludes)) = args.get("suggested_excludes") {
            let excludes: Vec<Value> = excludes
                .iter()
                .map(|x| js_trim(&js_string(x)).to_owned())
                .filter(|x| !x.is_empty())
                .map(Value::String)
                .collect();
            to_apply
                .params
                .insert("suggested_excludes".into(), Value::Array(excludes));
        }
        let applied = match to_apply.action_type.as_str() {
            "propose_add_custom_filament" => Ok(custom_filaments::apply(
                self.data_dir.as_deref(),
                &to_apply.params,
            )),
            _ => self.port.apply_action(&to_apply),
        };
        match applied {
            Ok(result) => {
                let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
                if !ok {
                    // Restored so the client can retry or dismiss.
                    self.set_pending(action);
                }
                Ok(ToolResult {
                    text: pretty(&result),
                    is_error: !ok,
                })
            }
            Err(message) => {
                self.set_pending(action);
                Err(message)
            }
        }
    }

    fn invoke_product_tool(
        &self,
        name: &str,
        args: &Map<String, Value>,
    ) -> Result<ToolResult, String> {
        if !is_product_tool(name) {
            return Ok(ToolResult::error(
                json!({ "error": format!("Unknown tool: {name}") }),
            ));
        }
        if FARM_TOOLS.contains(&name)
            && let FarmAccess::Denied { code, error } = self.port.farm_access()
        {
            return Ok(ToolResult::error(json!({ "error": error, "code": code })));
        }
        let invocation = match name {
            "propose_add_custom_filament" => custom_filaments::propose(args),
            _ => self.port.invoke_tool(name, args)?,
        };
        if let Some(action) = invocation.proposed_action
            && !UI_ACTION_TYPES.contains(&action.action_type.as_str())
        {
            self.set_pending(action);
        }
        let returned_error = serde_json::from_str::<Value>(&invocation.content)
            .ok()
            .and_then(|v| v.as_object().map(|o| o.contains_key("error")))
            .unwrap_or(false);
        Ok(ToolResult {
            text: invocation.content,
            is_error: returned_error,
        })
    }
}

/// `JSON.stringify(value, null, 2)`.
fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// JS `String(value)` for JSON values.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 1e21 => format!("{f:.0}"),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// JS `String(value ?? "")`.
fn js_string_or_empty(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(v) => js_string(v),
    }
}

/// JS `String.prototype.trim`: Unicode white space plus BOM and line terminators.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}
