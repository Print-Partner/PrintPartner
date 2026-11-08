//! Open in browser: hand a fresh bootstrap URL to the default browser. The
//! tray uses it; a Linux WebGL fallback can call the same two functions.
use crate::AllowedOrigin;
use anyhow::{Result, anyhow, ensure};
use tauri::Url;

/// Accept only a bootstrap URL on the desktop origin and port, so the opener
/// never receives another address. Errors never repeat the URL.
pub fn bootstrap_url(origin: &AllowedOrigin, url: &str) -> Result<Url> {
    let url = Url::parse(url).map_err(|_| anyhow!("Invalid browser bootstrap"))?;
    let token_ok = url.query().is_some_and(|query| {
        query.strip_prefix("token=").is_some_and(|token| {
            token.len() == 64
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    });
    ensure!(
        origin.admits(&url)
            && url.path() == "/__desktop/bootstrap"
            && token_ok
            && url.fragment().is_none(),
        "Invalid browser bootstrap"
    );
    Ok(url)
}

/// The platform command that opens `url` in the default browser, passed as
/// one argument with no shell.
pub fn opener(url: &Url) -> std::process::Command {
    let mut command = crate::platform::opener();
    command.arg(url.as_str());
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn ticket_t_19_bootstrap_url_requires_the_desktop_origin_and_port() {
        let origin = AllowedOrigin::parse("http://127.0.0.1:46222/").unwrap();
        let good = format!("http://127.0.0.1:46222/__desktop/bootstrap?token={TOKEN}");
        assert_eq!(bootstrap_url(&origin, &good).unwrap().as_str(), good);
        for rejected in [
            format!("http://127.0.0.1:46223/__desktop/bootstrap?token={TOKEN}"),
            format!("http://localhost:46222/__desktop/bootstrap?token={TOKEN}"),
            format!("https://127.0.0.1:46222/__desktop/bootstrap?token={TOKEN}"),
            format!("http://user@127.0.0.1:46222/__desktop/bootstrap?token={TOKEN}"),
            format!("http://127.0.0.1:46222/__desktop/other?token={TOKEN}"),
            format!("http://127.0.0.1:46222/__desktop/bootstrap?token={TOKEN}#x"),
            format!(
                "http://127.0.0.1:46222/__desktop/bootstrap?token={}",
                TOKEN.to_uppercase()
            ),
            "http://127.0.0.1:46222/__desktop/bootstrap?token=short".to_owned(),
            "http://127.0.0.1:46222/__desktop/bootstrap".to_owned(),
            "not a url".to_owned(),
        ] {
            let error = bootstrap_url(&origin, &rejected).unwrap_err().to_string();
            assert_eq!(error, "Invalid browser bootstrap", "{rejected}");
        }
    }

    #[test]
    fn ticket_t_19_tray_offers_open_in_browser() {
        assert_eq!(crate::tray::menu_ids().get(1), Some(&"browser"));
    }

    #[test]
    fn ticket_t_19_log_scrub_removes_the_bootstrap_token() {
        let line = format!("open http://127.0.0.1:46222/__desktop/bootstrap?token={TOKEN}");
        let scrubbed = pp_core::redact::scrub(&line);
        assert!(!scrubbed.contains(TOKEN), "{scrubbed}");
        assert!(scrubbed.contains("/__desktop/bootstrap?token=[REDACTED]"));
    }

    #[test]
    fn ticket_t_19_opener_gets_the_url_as_its_last_argument() {
        let url = Url::parse(&format!(
            "http://127.0.0.1:46222/__desktop/bootstrap?token={TOKEN}"
        ))
        .unwrap();
        let command = opener(&url);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args.last().unwrap().to_str(), Some(url.as_str()));
        assert!(args.len() <= 2);
    }
}
