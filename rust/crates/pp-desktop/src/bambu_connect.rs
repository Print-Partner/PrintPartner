use crate::AllowedOrigin;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tauri::{Url, ipc::CapabilityBuilder};
use tokio::process::Command;

const IMPORT_URL: &str = "bambu-connect://import-file";
const DENIED: &str =
    "This file is not available for Bambu Connect. Stage it again from Production.";

pub struct BambuConnect {
    origin: AllowedOrigin,
    data: PathBuf,
}

impl BambuConnect {
    pub fn new(origin: AllowedOrigin, data: PathBuf) -> Self {
        Self { origin, data }
    }

    pub fn capability(&self) -> CapabilityBuilder {
        CapabilityBuilder::new("main-bambu-connect")
            .local(false)
            .webview("main")
            .remote(format!("{}*", self.origin.0))
            .permission("allow-open-bambu-connect")
    }

    pub fn initialization_script(&self) -> String {
        let origin =
            serde_json::Value::String(self.origin.0.origin().ascii_serialization()).to_string();
        format!(
            r#"(() => {{
  if (window !== window.top || location.origin !== {origin}) return;
  Object.defineProperty(window, '__PRINT_PARTNER_NATIVE_BAMBU_CONNECT__', {{
    value: Object.freeze((url) => window.__TAURI_INTERNALS__.invoke('open_bambu_connect', {{ url }})),
    writable: false, configurable: false
  }});
}})();"#
        )
    }

    fn admit(
        &self,
        window: &str,
        webview: &str,
        current: &Url,
        raw: &str,
    ) -> Result<Url, &'static str> {
        if window != "main" || webview != "main" || !self.origin.admits(current) {
            return Err(DENIED);
        }
        admitted_url(raw, |path| {
            pp_core::rebuild_api::is_bambu_connect_staged_file(&self.data, path)
        })
    }
}

fn admitted_url(raw: &str, staged: impl FnOnce(&Path) -> bool) -> Result<Url, &'static str> {
    let url = Url::parse(raw).map_err(|_| DENIED)?;
    let mut pairs = url.query_pairs();
    let (Some((path_key, path)), Some((name_key, name)), Some((version_key, version))) =
        (pairs.next(), pairs.next(), pairs.next())
    else {
        return Err(DENIED);
    };
    if path_key != "path"
        || name_key != "name"
        || version_key != "version"
        || version != "1.0.0"
        || pairs.next().is_some()
        || name.is_empty()
        || name.chars().count() > 120
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err(DENIED);
    }
    let mut expected = Url::parse(IMPORT_URL).map_err(|_| DENIED)?;
    expected
        .query_pairs_mut()
        .append_pair("path", &path)
        .append_pair("name", &name)
        .append_pair("version", "1.0.0");
    if url != expected || !staged(Path::new(path.as_ref())) {
        return Err(DENIED);
    }
    Ok(expected)
}

#[tauri::command]
pub async fn open_bambu_connect(
    webview: tauri::Webview,
    action: tauri::State<'_, BambuConnect>,
    url: String,
) -> Result<(), String> {
    let current = webview.url().map_err(|_| DENIED.to_owned())?;
    let url = action
        .admit(webview.window().label(), webview.label(), &current, &url)
        .map_err(str::to_owned)?;
    let mut command = Command::from(crate::platform::opener());
    command.arg(url.as_str());
    crate::native_actions::run_browser(command, Duration::from_secs(5))
        .await
        .map_err(|_| "Could not open Bambu Connect. Check that it is installed.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_canonical_import_url_reaches_the_fake_stage_validator() {
        const LINK: &str =
            "bambu-connect://import-file?path=%2Ffixture.gcode&name=fixture&version=1.0.0";
        let staged = |path: &Path| path == Path::new("/fixture.gcode");
        assert_eq!(admitted_url(LINK, staged).unwrap().as_str(), LINK);
        assert!(admitted_url(LINK, |_| false).is_err());
        for link in [
            LINK.replace("bambu-connect", "https"),
            LINK.replace("import-file", "other"),
            LINK.replace("import-file", "user@import-file"),
            LINK.replace("1.0.0", "2.0.0"),
            LINK.replace("fixture&", "other%2Ffile&"),
            format!("{LINK}&path=%2Fother.gcode"),
            format!("{LINK}#fragment"),
        ] {
            assert!(admitted_url(&link, |_| panic!("invalid URL reached staging check")).is_err());
        }
    }
}
