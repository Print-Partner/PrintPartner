use crate::AllowedOrigin;
use pp_core::{BrowserPreference, BrowserPreferences, OriginChange, PrinterPollInterval, Theme};
use tauri::{Url, ipc::CapabilityBuilder};

const THEME_KEY: &str = "print-partner.theme";
const SIDEBAR_KEY: &str = "print-partner.sidebar.ui.v1";
const POLL_KEY: &str = "print-partner.printer-status-poll.v1";
const UNAVAILABLE: &str = "Portable settings are unavailable";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OriginSession {
    pub fresh_gateway_bootstrap: bool,
    pub clear_user_session: bool,
}

/// The desktop gateway cookie is named `pp_desktop_{port}`, so a replacement
/// port needs the one-shot bootstrap. `pp_session` is host-only, and cookies
/// are not scoped to a port, so that user session still applies.
pub fn session_for(change: &OriginChange) -> OriginSession {
    match change {
        OriginChange::PersistedPortInUse { .. } => OriginSession {
            fresh_gateway_bootstrap: true,
            clear_user_session: false,
        },
    }
}

pub fn user_notice(enabled: bool, change: Option<&OriginChange>) -> Option<String> {
    if !enabled {
        return None;
    }
    change.map(|change| format!("The desktop address changed. {change}"))
}

pub fn require_fresh_bootstrap(change: Option<&OriginChange>, url: &str) -> anyhow::Result<()> {
    let Some(change) = change else {
        return Ok(());
    };
    let session = session_for(change);
    if session.fresh_gateway_bootstrap {
        accept_bootstrap_url(url)?;
    }
    if session.clear_user_session {
        anyhow::bail!("Fresh desktop login required");
    }
    Ok(())
}

fn accept_bootstrap_url(url: &str) -> anyhow::Result<()> {
    let parsed =
        Url::parse(url).map_err(|_| anyhow::anyhow!("Fresh desktop bootstrap required"))?;
    let token_ok = parsed.query().is_some_and(|query| {
        query.strip_prefix("token=").is_some_and(|token| {
            token.len() == 64
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    });
    if parsed.scheme() == "http"
        && parsed.host_str() == Some("127.0.0.1")
        && parsed.port().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.path() == "/__desktop/bootstrap"
        && token_ok
        && parsed.fragment().is_none()
    {
        Ok(())
    } else {
        anyhow::bail!("Fresh desktop bootstrap required")
    }
}

pub fn capture_updates(
    theme: Option<&str>,
    sidebar_collapsed: Option<bool>,
    printer_poll_seconds: Option<u8>,
) -> Result<Vec<BrowserPreference>, &'static str> {
    let mut updates = Vec::new();
    if let Some(theme) = theme {
        let theme = match theme {
            "system" => Theme::System,
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => return Err("Unsupported portable setting"),
        };
        updates.push(BrowserPreference::Theme(Some(theme)));
    }
    if let Some(collapsed) = sidebar_collapsed {
        updates.push(BrowserPreference::SidebarCollapsed(Some(collapsed)));
    }
    if let Some(seconds) = printer_poll_seconds {
        let interval =
            PrinterPollInterval::try_from(seconds).map_err(|_| "Unsupported portable setting")?;
        updates.push(BrowserPreference::PrinterPollInterval(Some(interval)));
    }
    Ok(updates)
}

fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::System => "system",
        Theme::Light => "light",
        Theme::Dark => "dark",
    }
}

fn restore_json(preferences: &BrowserPreferences, origin_changed: bool) -> String {
    if !origin_changed {
        return "null".to_owned();
    }
    let mut value = serde_json::Map::new();
    if let Some(theme) = preferences.theme {
        value.insert(
            "theme".to_owned(),
            serde_json::Value::String(theme_name(theme).to_owned()),
        );
    }
    if let Some(collapsed) = preferences.sidebar_collapsed {
        value.insert(
            "sidebar".to_owned(),
            serde_json::Value::String(if collapsed { "1" } else { "0" }.to_owned()),
        );
    }
    if let Some(interval) = preferences.printer_poll_interval {
        value.insert(
            "poll".to_owned(),
            serde_json::Value::from(u8::from(interval)),
        );
    }
    if value.is_empty() {
        return "null".to_owned();
    }
    match serde_json::to_string(&value) {
        Ok(json) => json,
        Err(_) => "null".to_owned(),
    }
}

pub fn initialization_script(
    origin: &AllowedOrigin,
    preferences: &BrowserPreferences,
    origin_changed: bool,
) -> String {
    let origin = match serde_json::to_string(&origin.0.origin().ascii_serialization()) {
        Ok(origin) => origin,
        Err(_) => "\"\"".to_owned(),
    };
    let changed = if origin_changed { "true" } else { "false" };
    let restore = restore_json(preferences, origin_changed);
    format!(
        r#"(() => {{
  const origin = {origin};
  const changed = {changed};
  const restore = {restore};
  if (window !== window.top || location.origin !== origin) return;
  const themeKey = {theme_key};
  const sidebarKey = {sidebar_key};
  const pollKey = {poll_key};
  const restoredKey = "print-partner.origin-change.restored.v1";
  let storageProto = null;
  let originalSetItem = null;
  try {{
    storageProto = Object.getPrototypeOf(localStorage);
    if (storageProto && storageProto !== Object.prototype && typeof storageProto.setItem === "function") {{
      originalSetItem = storageProto.setItem;
    }}
  }} catch {{}}
  const write = (key, value) => {{
    originalSetItem.call(localStorage, key, value);
  }};
  const publish = () => {{
    const captured = {{}};
    const theme = localStorage.getItem(themeKey);
    if (theme === "system" || theme === "light" || theme === "dark") captured.theme = theme;
    const sidebar = localStorage.getItem(sidebarKey);
    if (sidebar === "1" || sidebar === "0") captured.sidebarCollapsed = sidebar === "1";
    const poll = localStorage.getItem(pollKey);
    if (poll === "5" || poll === "10" || poll === "15" || poll === "30") captured.printerPollSeconds = Number(poll);
    if (captured.theme == null && captured.sidebarCollapsed == null && captured.printerPollSeconds == null) return;
    window.__TAURI_INTERNALS__?.invoke("sync_portable_settings", {{ captured }})?.catch(() => {{}});
  }};
  if (changed && restore && originalSetItem) {{
    let alreadyRestored = false;
    try {{
      alreadyRestored = sessionStorage.getItem(restoredKey) === "1";
    }} catch {{}}
    if (!alreadyRestored) {{
      try {{
        if (restore.theme != null) write(themeKey, restore.theme);
        if (restore.sidebar != null) write(sidebarKey, restore.sidebar);
        if (restore.poll != null) write(pollKey, String(restore.poll));
        originalSetItem.call(sessionStorage, restoredKey, "1");
      }} catch {{}}
    }}
  }}
  publish();
  if (originalSetItem) {{
    try {{
      storageProto.setItem = function (key, value) {{
        originalSetItem.call(this, key, value);
        if (this === localStorage && (key === themeKey || key === sidebarKey || key === pollKey)) publish();
      }};
    }} catch {{}}
  }}
  if (typeof window.addEventListener === "function") window.addEventListener("pagehide", publish);
}})();"#,
        theme_key = serde_json::to_string(THEME_KEY).unwrap_or_else(|_| "\"\"".to_owned()),
        sidebar_key = serde_json::to_string(SIDEBAR_KEY).unwrap_or_else(|_| "\"\"".to_owned()),
        poll_key = serde_json::to_string(POLL_KEY).unwrap_or_else(|_| "\"\"".to_owned()),
    )
}

enum Gate {
    Off,
    On {
        origin: AllowedOrigin,
        handle: Box<pp_core::CoreHandle>,
    },
}

pub struct OriginBridge {
    gate: Gate,
    notice: Option<String>,
    script: Option<String>,
}

impl OriginBridge {
    pub fn disabled() -> Self {
        Self {
            gate: Gate::Off,
            notice: None,
            script: None,
        }
    }

    pub fn enabled(
        origin: AllowedOrigin,
        handle: pp_core::CoreHandle,
        preferences: Option<&BrowserPreferences>,
        change: Option<&OriginChange>,
    ) -> Self {
        let preferences = preferences.cloned().unwrap_or_default();
        let script = Some(initialization_script(
            &origin,
            &preferences,
            change.is_some(),
        ));
        Self {
            gate: Gate::On {
                origin,
                handle: Box::new(handle),
            },
            notice: user_notice(true, change),
            script,
        }
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn initialization_script(&self) -> Option<&str> {
        self.script.as_deref()
    }

    pub fn capability(&self) -> Option<CapabilityBuilder> {
        let Gate::On { origin, .. } = &self.gate else {
            return None;
        };
        Some(capability_for(origin))
    }

    fn open_handle(
        &self,
        window: &str,
        webview: &str,
        url: &Url,
    ) -> Result<pp_core::CoreHandle, &'static str> {
        let Gate::On { origin, handle } = &self.gate else {
            return Err(UNAVAILABLE);
        };
        authorize(origin, window, webview, url)?;
        Ok(pp_core::CoreHandle::clone(handle))
    }
}

fn authorize(
    origin: &AllowedOrigin,
    window: &str,
    webview: &str,
    url: &Url,
) -> Result<(), &'static str> {
    if window == "main" && webview == "main" && origin.admits(url) {
        Ok(())
    } else {
        Err(UNAVAILABLE)
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CapturedPortableSettings {
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    sidebar_collapsed: Option<bool>,
    #[serde(default)]
    printer_poll_seconds: Option<u8>,
}

#[tauri::command]
pub async fn sync_portable_settings(
    webview: tauri::Webview,
    bridge: tauri::State<'_, OriginBridge>,
    captured: serde_json::Value,
) -> Result<(), String> {
    let captured: CapturedPortableSettings =
        serde_json::from_value(captured).map_err(|_| "Unsupported portable setting".to_owned())?;
    let url = webview.url().map_err(|_| UNAVAILABLE.to_owned())?;
    let handle = bridge
        .open_handle(webview.window().label(), webview.label(), &url)
        .map_err(str::to_owned)?;
    let updates = capture_updates(
        captured.theme.as_deref(),
        captured.sidebar_collapsed,
        captured.printer_poll_seconds,
    )
    .map_err(str::to_owned)?;
    for update in updates {
        handle
            .set_browser_preference(update)
            .await
            .map_err(|_| "Portable settings could not be saved".to_owned())?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn verify_authority(context: &mut tauri::Context<tauri::Wry>) {
    use tauri::ipc::Origin;
    let origin = AllowedOrigin::parse("http://127.0.0.1:46687").unwrap();
    let authority = context.runtime_authority_mut();
    let allowed = Origin::Remote {
        url: Url::parse("http://127.0.0.1:46687/plans?x=1#part").unwrap(),
    };
    assert!(
        authority
            .resolve_access("sync_portable_settings", "main", "main", &allowed)
            .is_none()
    );
    authority.add_capability(capability_for(&origin)).unwrap();
    assert!(
        authority
            .resolve_access("sync_portable_settings", "main", "main", &allowed)
            .is_some()
    );
    assert!(
        authority
            .resolve_access("sync_portable_settings", "main", "other", &allowed)
            .is_none()
    );
    for origin in [
        Origin::Local,
        Origin::Remote {
            url: Url::parse("http://127.0.0.1:46688/").unwrap(),
        },
        Origin::Remote {
            url: Url::parse("http://localhost:46687/").unwrap(),
        },
        Origin::Remote {
            url: Url::parse("https://127.0.0.1:46687/").unwrap(),
        },
        Origin::Remote {
            url: Url::parse("https://example.com/").unwrap(),
        },
    ] {
        assert!(
            authority
                .resolve_access("sync_portable_settings", "main", "main", &origin)
                .is_none(),
            "unexpected portable-settings grant"
        );
    }
}

fn capability_for(origin: &AllowedOrigin) -> CapabilityBuilder {
    CapabilityBuilder::new("main-origin-settings")
        .local(false)
        .webview("main")
        .remote(format!("{}*", origin.0))
        .permission("allow-sync-portable-settings")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;

    fn change(previous: u16, new_port: u16) -> OriginChange {
        OriginChange::PersistedPortInUse {
            previous_port: NonZeroU16::new(previous).unwrap(),
            new_port: NonZeroU16::new(new_port).unwrap(),
        }
    }

    fn origin() -> AllowedOrigin {
        AllowedOrigin::parse("http://127.0.0.1:46687").unwrap()
    }

    fn token() -> String {
        "ab".repeat(32)
    }

    fn bootstrap(port: u16) -> String {
        format!(
            "http://127.0.0.1:{port}/__desktop/bootstrap?token={}",
            token()
        )
    }

    #[test]
    fn ticket_t_12_origin_notice_requires_the_runtime_gate() {
        let change = change(46111, 46222);
        assert_eq!(user_notice(false, Some(&change)), None);
        assert_eq!(user_notice(true, None), None);
        let notice = user_notice(true, Some(&change)).unwrap();
        assert_eq!(
            notice,
            "The desktop address changed. The saved desktop port 46111 was in use. This launch uses port 46222."
        );
        assert!(!notice.to_ascii_lowercase().contains("token"));
        assert!(!notice.contains("bootstrap"));
        assert!(!notice.contains("pp_session"));
    }

    #[test]
    fn ticket_t_12_origin_change_keeps_the_host_session_and_requires_bootstrap() {
        let change = change(46111, 46222);
        let session = session_for(&change);
        assert!(session.fresh_gateway_bootstrap);
        assert!(!session.clear_user_session);
        let url = bootstrap(46222);
        assert!(require_fresh_bootstrap(Some(&change), &url).is_ok());
        assert!(require_fresh_bootstrap(None, "http://127.0.0.1:46222/").is_ok());
        for rejected in [
            "http://127.0.0.1:46222/",
            "http://127.0.0.1:46222/__desktop/bootstrap",
            "http://127.0.0.1:46222/__desktop/bootstrap?token=short",
            &format!(
                "http://127.0.0.1:46222/__desktop/bootstrap?token={}",
                "AB".repeat(32)
            ),
            &format!("{url}&next=1"),
            "https://127.0.0.1:46222/__desktop/bootstrap?token=ab",
            "http://localhost:46222/__desktop/bootstrap?token=ab",
        ] {
            let error = require_fresh_bootstrap(Some(&change), rejected).unwrap_err();
            let text = error.to_string();
            assert_eq!(text, "Fresh desktop bootstrap required");
            assert!(!text.contains(&token()));
            assert!(!text.contains("46111"));
        }
    }

    #[test]
    fn ticket_t_12_portable_capture_rejects_unknown_values() {
        assert_eq!(
            capture_updates(Some("dark"), Some(true), Some(10)).unwrap(),
            vec![
                BrowserPreference::Theme(Some(Theme::Dark)),
                BrowserPreference::SidebarCollapsed(Some(true)),
                BrowserPreference::PrinterPollInterval(Some(PrinterPollInterval::TenSeconds)),
            ]
        );
        assert!(capture_updates(None, None, None).unwrap().is_empty());
        let parsed: CapturedPortableSettings =
            serde_json::from_value(serde_json::json!({"theme": "light"})).unwrap();
        assert_eq!(
            capture_updates(
                parsed.theme.as_deref(),
                parsed.sidebar_collapsed,
                parsed.printer_poll_seconds
            )
            .unwrap(),
            vec![BrowserPreference::Theme(Some(Theme::Light))]
        );
        assert!(
            serde_json::from_value::<CapturedPortableSettings>(
                serde_json::json!({"theme": "dark", "token": "secret"})
            )
            .is_err()
        );
        assert!(capture_updates(Some("Dark"), None, None).is_err());
        assert!(capture_updates(Some(""), None, None).is_err());
        assert!(capture_updates(None, None, Some(7)).is_err());
        assert!(capture_updates(Some("blue"), Some(false), Some(5)).is_err());
    }

    #[test]
    fn ticket_t_12_disabled_bridge_has_no_notice_script_or_capability() {
        let bridge = OriginBridge::disabled();
        assert!(bridge.notice().is_none());
        assert!(bridge.initialization_script().is_none());
        assert!(bridge.capability().is_none());
        assert!(
            bridge
                .open_handle(
                    "main",
                    "main",
                    &Url::parse("http://127.0.0.1:46687/").unwrap()
                )
                .is_err()
        );
    }

    #[test]
    fn ticket_t_12_portable_settings_follow_a_replacement_origin() {
        let origin = origin();
        let preferences = BrowserPreferences {
            theme: Some(Theme::Dark),
            sidebar_collapsed: Some(true),
            printer_poll_interval: Some(PrinterPollInterval::TenSeconds),
        };
        let cases = [
            (
                true,
                serde_json::json!({}),
                serde_json::json!({
                    "print-partner.theme": "dark",
                    "print-partner.sidebar.ui.v1": "1",
                    "print-partner.printer-status-poll.v1": "10",
                }),
                serde_json::json!({"theme": "dark", "sidebarCollapsed": true, "printerPollSeconds": 10}),
            ),
            (
                true,
                serde_json::json!({"print-partner.theme": "light"}),
                serde_json::json!({
                    "print-partner.theme": "dark",
                    "print-partner.sidebar.ui.v1": "1",
                    "print-partner.printer-status-poll.v1": "10",
                }),
                serde_json::json!({"theme": "dark", "sidebarCollapsed": true, "printerPollSeconds": 10}),
            ),
            (
                false,
                serde_json::json!({"print-partner.theme": "system"}),
                serde_json::json!({"print-partner.theme": "system"}),
                serde_json::json!({"theme": "system"}),
            ),
            (
                true,
                serde_json::json!({"print-partner.printer-status-poll.v1": "7"}),
                serde_json::json!({
                    "print-partner.theme": "dark",
                    "print-partner.sidebar.ui.v1": "1",
                    "print-partner.printer-status-poll.v1": "10",
                }),
                serde_json::json!({"theme": "dark", "sidebarCollapsed": true, "printerPollSeconds": 10}),
            ),
        ];
        for (changed, existing, expected_store, expected_capture) in cases {
            let script = initialization_script(&origin, &preferences, changed);
            assert!(!script.contains("pp_session"), "{script}");
            assert!(!script.contains("logout"), "{script}");
            assert!(!script.contains("cookie"), "{script}");
            assert!(!script.contains("token"), "{script}");
            assert!(!script.contains("bootstrap"), "{script}");
            let observed = run_script(&script, "http://127.0.0.1:46687", &existing);
            assert_eq!(observed.store, expected_store, "changed={changed}");
            assert_eq!(observed.invoked.len(), 1, "changed={changed}");
            assert_eq!(observed.invoked[0].name, "sync_portable_settings");
            assert_eq!(observed.invoked[0].captured, expected_capture);
            let captured = &observed.invoked[0].captured;
            let updates = capture_updates(
                captured.get("theme").and_then(serde_json::Value::as_str),
                captured
                    .get("sidebarCollapsed")
                    .and_then(serde_json::Value::as_bool),
                captured
                    .get("printerPollSeconds")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u8::try_from(value).ok()),
            )
            .unwrap();
            if changed && existing.get("print-partner.theme").is_none() {
                assert!(updates.contains(&BrowserPreference::Theme(Some(Theme::Dark))));
            }
        }
        let foreign = run_script(
            &initialization_script(&origin, &preferences, true),
            "http://127.0.0.1:46688",
            &serde_json::json!({}),
        );
        assert_eq!(foreign.store, serde_json::json!({}));
        assert!(foreign.invoked.is_empty());
        let without_internals = run_script_after(
            &initialization_script(&origin, &preferences, true),
            "http://127.0.0.1:46687",
            &serde_json::json!({"print-partner.theme": "light"}),
            &[],
            true,
        );
        assert_eq!(
            without_internals.store["print-partner.theme"],
            serde_json::json!("dark")
        );
        assert_eq!(without_internals.store.get("setItem"), None);
        assert!(without_internals.invoked.is_empty());
    }

    #[test]
    fn ticket_t_12_portable_settings_follow_a_change_after_the_init_script() {
        let script = initialization_script(&origin(), &BrowserPreferences::default(), false);
        let observed = run_script_after(
            &script,
            "http://127.0.0.1:46687",
            &serde_json::json!({}),
            &[("print-partner.theme", "dark")],
            false,
        );
        assert_eq!(
            observed.store,
            serde_json::json!({"print-partner.theme": "dark"})
        );
        assert_eq!(observed.invoked.len(), 1);
        assert_eq!(observed.invoked[0].name, "sync_portable_settings");
        assert_eq!(
            observed.invoked[0].captured,
            serde_json::json!({"theme": "dark"})
        );
        assert_eq!(observed.store.get("setItem"), None);
    }

    #[test]
    fn ticket_t_12_session_storage_writes_do_not_publish_local_settings() {
        let preferences = BrowserPreferences {
            theme: Some(Theme::Dark),
            sidebar_collapsed: None,
            printer_poll_interval: None,
        };
        let observed = run_script_case(&ScriptCase {
            script: &initialization_script(&origin(), &preferences, true),
            page_origin: "http://127.0.0.1:46687",
            store: &serde_json::json!({}),
            writes: &[],
            session_writes: &[("print-partner.theme", "light")],
            omit_internals: false,
            reload: false,
        });
        assert_eq!(observed.invoked.len(), 1);
        assert_eq!(
            observed.invoked[0].captured,
            serde_json::json!({"theme": "dark"})
        );
        assert_eq!(
            observed.store,
            serde_json::json!({"print-partner.theme": "dark"})
        );
    }

    #[test]
    fn ticket_t_12_origin_change_keeps_a_later_edit_across_reload() {
        let preferences = BrowserPreferences {
            theme: Some(Theme::Dark),
            sidebar_collapsed: Some(true),
            printer_poll_interval: Some(PrinterPollInterval::TenSeconds),
        };
        let observed = run_script_case(&ScriptCase {
            script: &initialization_script(&origin(), &preferences, true),
            page_origin: "http://127.0.0.1:46687",
            store: &serde_json::json!({}),
            writes: &[("print-partner.theme", "light")],
            session_writes: &[],
            omit_internals: false,
            reload: true,
        });
        assert_eq!(
            observed.store,
            serde_json::json!({
                "print-partner.theme": "light",
                "print-partner.sidebar.ui.v1": "1",
                "print-partner.printer-status-poll.v1": "10",
            })
        );
        let last = observed.invoked.last().unwrap();
        assert_eq!(last.name, "sync_portable_settings");
        assert_eq!(last.captured["theme"], serde_json::json!("light"));
        assert_eq!(last.captured["sidebarCollapsed"], serde_json::json!(true));
        assert_eq!(last.captured["printerPollSeconds"], serde_json::json!(10));
    }

    struct Invocation {
        name: String,
        captured: serde_json::Value,
    }

    struct ScriptResult {
        store: serde_json::Value,
        invoked: Vec<Invocation>,
    }

    struct ScriptCase<'a> {
        script: &'a str,
        page_origin: &'a str,
        store: &'a serde_json::Value,
        writes: &'a [(&'a str, &'a str)],
        session_writes: &'a [(&'a str, &'a str)],
        omit_internals: bool,
        reload: bool,
    }

    fn run_script(script: &str, page_origin: &str, store: &serde_json::Value) -> ScriptResult {
        run_script_after(script, page_origin, store, &[], false)
    }

    fn run_script_after(
        script: &str,
        page_origin: &str,
        store: &serde_json::Value,
        writes: &[(&str, &str)],
        omit_internals: bool,
    ) -> ScriptResult {
        run_script_case(&ScriptCase {
            script,
            page_origin,
            store,
            writes,
            session_writes: &[],
            omit_internals,
            reload: false,
        })
    }

    fn run_script_case(case: &ScriptCase<'_>) -> ScriptResult {
        let after: Vec<_> = case
            .writes
            .iter()
            .map(|(key, value)| serde_json::json!({"key": key, "value": value}))
            .collect();
        let session_after: Vec<_> = case
            .session_writes
            .iter()
            .map(|(key, value)| serde_json::json!({"key": key, "value": value}))
            .collect();
        let input = serde_json::json!({
            "origin": case.page_origin,
            "store": case.store,
            "after": after,
            "sessionAfter": session_after,
            "omitInternals": case.omit_internals,
            "reload": case.reload,
        });
        let mut child = std::process::Command::new("node")
            .arg("-e")
            .arg(
                r#"
const fs = require("node:fs");
const script = fs.readFileSync(0, "utf8");
const input = JSON.parse(process.argv[1]);
const buckets = new WeakMap();
const members = new Set(["getItem", "setItem", "removeItem", "clear", "key", "length"]);

function Storage() {}
Storage.prototype.getItem = function (key) {
  const data = buckets.get(this);
  const name = String(key);
  return data && data.has(name) ? data.get(name) : null;
};
Storage.prototype.setItem = function (key, value) {
  const data = buckets.get(this);
  if (data) data.set(String(key), String(value));
};
const pristineGetItem = Storage.prototype.getItem;
const pristineSetItem = Storage.prototype.setItem;

function createStorage(initial) {
  const data = new Map();
  for (const [key, value] of Object.entries(initial || {})) {
    if (value != null) data.set(String(key), String(value));
  }
  const target = Object.create(Storage.prototype);
  // Assignment stores an item and leaves the prototype method in place.
  const storage = new Proxy(target, {
    get(target, prop, receiver) {
      if (typeof prop !== "string") return Reflect.get(target, prop, receiver);
      if (members.has(prop)) return Reflect.get(target, prop, receiver);
      if (data.has(prop)) return data.get(prop);
      return Reflect.get(target, prop, receiver);
    },
    set(target, prop, value, receiver) {
      if (typeof prop !== "string") return Reflect.set(target, prop, value, receiver);
      data.set(prop, String(value));
      return true;
    },
    getPrototypeOf() {
      return Storage.prototype;
    },
  });
  buckets.set(storage, data);
  return { storage, data };
}

const local = createStorage(input.store);
const session = createStorage({});
const localStorage = local.storage;
const sessionStorage = session.storage;
const invoked = [];
const location = { origin: input.origin };

function evaluate() {
  Storage.prototype.getItem = pristineGetItem;
  Storage.prototype.setItem = pristineSetItem;
  const window = {};
  if (!input.omitInternals) {
    window.__TAURI_INTERNALS__ = {
      invoke(name, args) {
        invoked.push({ name, args });
        return Promise.resolve();
      },
    };
  }
  window.top = window;
  new Function("window", "location", "localStorage", "sessionStorage", script)(
    window,
    location,
    localStorage,
    sessionStorage,
  );
}

evaluate();
for (const change of input.after) localStorage.setItem(change.key, change.value);
for (const change of input.sessionAfter) sessionStorage.setItem(change.key, change.value);
if (input.reload) evaluate();
setTimeout(() => {
  process.stdout.write(JSON.stringify({
    store: Object.fromEntries(local.data),
    invoked,
  }));
}, 0);
"#,
            )
            .arg(input.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(case.script.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let invoked = value["invoked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| Invocation {
                name: entry["name"].as_str().unwrap().to_owned(),
                captured: entry["args"]["captured"].clone(),
            })
            .collect();
        ScriptResult {
            store: value["store"].clone(),
            invoked,
        }
    }
}
