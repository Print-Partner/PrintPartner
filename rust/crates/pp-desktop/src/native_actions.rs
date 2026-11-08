use crate::AllowedOrigin;
use std::{process::Stdio, time::Duration};
use tauri::{Url, ipc::CapabilityBuilder};
use tokio::process::Command;

const SPONSOR_URL: &str = "https://github.com/sponsors/poitee";
const DENIED: &str = "Sponsor action is unavailable for this page";

pub enum SponsorAction {
    Disabled,
    Enabled(AllowedOrigin),
}

impl SponsorAction {
    pub fn from_startup(gate: pp_core::DesktopRebuild, origin: AllowedOrigin) -> Self {
        Self::new(
            gate.enabled() && crate::platform::sponsor_opener().is_some(),
            origin,
        )
    }

    fn new(enabled: bool, origin: AllowedOrigin) -> Self {
        if enabled {
            Self::Enabled(origin)
        } else {
            Self::Disabled
        }
    }

    pub fn capability(&self) -> Option<CapabilityBuilder> {
        let Self::Enabled(origin) = self else {
            return None;
        };
        Some(
            CapabilityBuilder::new("main-sponsor-action")
                .local(false)
                .webview("main")
                .remote(format!("{}*", origin.0))
                .permission("allow-open-sponsor"),
        )
    }

    pub fn initialization_script(&self) -> Option<String> {
        let Self::Enabled(origin) = self else {
            return None;
        };
        let origin = serde_json::Value::String(origin.0.origin().ascii_serialization()).to_string();
        Some(format!(
            r#"(() => {{
  if (window !== window.top || location.origin !== {origin}) return;
  Object.defineProperty(window, '__PRINT_PARTNER_NATIVE_SPONSOR__', {{
    value: Object.freeze(() => window.__TAURI_INTERNALS__.invoke('open_sponsor')),
    writable: false, configurable: false
  }});
}})();"#
        ))
    }

    fn authorize(
        &self,
        window: &str,
        webview: &str,
        current_url: &Url,
    ) -> Result<(), &'static str> {
        match self {
            Self::Enabled(origin)
                if window == "main" && webview == "main" && origin.admits(current_url) =>
            {
                Ok(())
            }
            _ => Err(DENIED),
        }
    }
}

#[tauri::command]
pub async fn open_sponsor(
    webview: tauri::Webview,
    action: tauri::State<'_, SponsorAction>,
) -> Result<(), String> {
    let url = webview.url().map_err(|_| DENIED.to_owned())?;
    action
        .authorize(webview.window().label(), webview.label(), &url)
        .map_err(str::to_owned)?;
    open_fixed_sponsor().await.map_err(str::to_owned)
}

async fn open_fixed_sponsor() -> Result<(), &'static str> {
    let Some(command) = crate::platform::sponsor_opener() else {
        return Err("Sponsor action is unavailable on this platform");
    };
    let mut command = Command::from(command);
    command.arg(SPONSOR_URL);
    run_browser(command, Duration::from_secs(5)).await
}

async fn run_browser(mut command: Command, timeout: Duration) -> Result<(), &'static str> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child =
        pp_core::spawn_child(&mut command).map_err(|_| "Could not start the browser")?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(_)) => Err("Could not open the browser"),
        _ => {
            let _ = child.kill().await;
            Err("Could not confirm the browser opened")
        }
    }
}

#[cfg(test)]
pub(crate) fn verify_authority(mut context: tauri::Context<tauri::Wry>) {
    use tauri::ipc::Origin;
    let origin = AllowedOrigin::parse("http://127.0.0.1:46687").unwrap();
    let authority = context.runtime_authority_mut();
    let allowed = Origin::Remote {
        url: Url::parse("http://127.0.0.1:46687/plans?x=1#part").unwrap(),
    };
    assert!(
        authority
            .resolve_access("open_sponsor", "main", "main", &allowed)
            .is_none()
    );
    assert!(
        SponsorAction::new(false, origin.clone())
            .capability()
            .is_none()
    );
    authority
        .add_capability(SponsorAction::new(true, origin).capability().unwrap())
        .unwrap();
    assert!(
        authority
            .resolve_access("open_sponsor", "main", "main", &allowed)
            .is_some()
    );
    assert!(
        authority
            .resolve_access("open_sponsor", "main", "other", &allowed)
            .is_none()
    );
    // A webview-only capability cannot constrain its enclosing window. The command does.
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
        Origin::Remote {
            url: Url::parse("blob:http://127.0.0.1:46687/opaque").unwrap(),
        },
        Origin::Remote {
            url: Url::parse("data:text/html,hello").unwrap(),
        },
        Origin::Remote {
            url: Url::parse("file:///tmp/index.html").unwrap(),
        },
        Origin::Remote {
            url: Url::parse("about:blank").unwrap(),
        },
    ] {
        assert!(
            authority
                .resolve_access("open_sponsor", "main", "main", &origin)
                .is_none(),
            "unexpected Sponsor grant for an untrusted source"
        );
    }
    for command in [
        "plugin:app|exit",
        "plugin:window|close",
        "plugin:webview|create_webview_window",
        "plugin:fs|read_file",
        "plugin:shell|execute",
        "plugin:opener|open_url",
        "open_url",
    ] {
        assert!(
            authority
                .resolve_access(command, "main", "main", &allowed)
                .is_none(),
            "{command}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_t_68_sponsor_requires_enabled_gate_main_labels_and_current_origin() {
        let origin = AllowedOrigin::parse("http://127.0.0.1:46687").unwrap();
        let url = Url::parse("http://127.0.0.1:46687/plans").unwrap();
        let disabled = SponsorAction::new(false, origin.clone());
        assert!(disabled.authorize("main", "main", &url).is_err());
        assert!(disabled.initialization_script().is_none());
        let enabled = SponsorAction::new(true, origin);
        assert!(enabled.authorize("main", "main", &url).is_ok());
        assert!(enabled.authorize("other", "main", &url).is_err());
        assert!(enabled.authorize("main", "other", &url).is_err());
        for value in [
            "http://127.0.0.1:46688/",
            "http://localhost:46687/",
            "https://example.com/",
            "http://owner@127.0.0.1:46687/",
            "http://:secret@127.0.0.1:46687/",
            "blob:http://127.0.0.1:46687/opaque",
            "data:text/html,hello",
            "file:///tmp/index.html",
            "about:blank",
        ] {
            assert!(
                enabled
                    .authorize("main", "main", &Url::parse(value).unwrap())
                    .is_err(),
                "{value}"
            );
        }
    }

    // Every case re-invokes this test binary, which exists on every OS: an
    // unknown flag fails, --list succeeds, and the helper test below outlives
    // the 50ms timeout and is killed by run_browser.
    fn test_binary(args: &[&str]) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("test binary path"));
        command.args(args);
        command
    }

    #[tokio::test]
    async fn ticket_t_68_browser_process_reports_failure_and_times_out() {
        assert!(
            run_browser(test_binary(&["--no-such-flag"]), Duration::from_secs(10))
                .await
                .is_err()
        );
        assert_eq!(
            run_browser(
                test_binary(&[
                    "--ignored",
                    "--exact",
                    "native_actions::tests::ticket_t_68_browser_timeout_helper",
                ]),
                Duration::from_millis(50)
            )
            .await,
            Err("Could not confirm the browser opened")
        );
        assert!(
            run_browser(test_binary(&["--list"]), Duration::from_secs(10))
                .await
                .is_ok()
        );
    }

    #[test]
    #[ignore = "spawned as a long-lived helper by ticket_t_68_browser_process_reports_failure_and_times_out"]
    fn ticket_t_68_browser_timeout_helper() {
        std::thread::sleep(Duration::from_secs(30));
    }
}
