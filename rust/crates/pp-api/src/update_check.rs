//! Node's cached, bounded release check. Failures never expose remote bodies.
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub(crate) struct UpdateCheck {
    enabled: bool,
    version: String,
    repo: String,
    latest: Option<String>,
    hours: f64,
    endpoint: String,
    client: reqwest::Client,
    cache: Mutex<Option<(Instant, Value)>>,
}
impl UpdateCheck {
    pub(crate) fn from_startup() -> Self {
        let env = |key: &str| std::env::var(key).ok();
        let repo = env("GITHUB_REPO")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "Print-Partner/PrintPartner".into())
            .trim()
            .to_owned();
        let package: Value =
            serde_json::from_str(include_str!("../../../../web/package.json")).unwrap();
        Self {
            enabled: env("PRINT_PARTNER_UPDATE_CHECK").as_deref() != Some("0"),
            version: format!("{}-web", package["version"].as_str().unwrap()),
            endpoint: format!("https://api.github.com/repos/{repo}/releases/latest"),
            repo,
            latest: env("PRINT_PARTNER_LATEST_VERSION")
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()),
            hours: cache_hours(env("PRINT_PARTNER_UPDATE_CHECK_CACHE_HOURS").as_deref()),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("release client"),
            cache: Mutex::new(None),
        }
    }
    pub(crate) async fn check(&self, refresh: bool) -> Value {
        let mut body = json!({"enabled": self.enabled, "update_available":false, "current_version":self.version, "latest_version":null, "release_url":null, "release_notes_url":null, "deploy_mode":"self-host", "checked_at":null});
        if !self.enabled {
            return body;
        }
        let mut cache = self.cache.lock().await;
        if !refresh
            && let Some((at, value)) = &*cache
            && !self.hours.is_nan()
            && at.elapsed().as_secs_f64() < self.hours.max(1.0) * 3600.0
        {
            return value.clone();
        }
        let now = time::OffsetDateTime::now_utc();
        body["checked_at"] = json!(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            now.year(),
            u8::from(now.month()),
            now.day(),
            now.hour(),
            now.minute(),
            now.second(),
            now.millisecond()
        ));
        let releases = format!("https://github.com/{}/releases", self.repo);
        body["release_notes_url"] = json!(releases);
        let latest = if let Some(version) = &self.latest {
            Some((version.clone(), releases))
        } else {
            self.fetch().await
        };
        if let Some((version, url)) = latest {
            body["update_available"] = json!(segments(&self.version) < segments(&version));
            body["latest_version"] = json!(version);
            body["release_url"] = json!(url);
            body["release_notes_url"] = body["release_url"].clone();
        }
        *cache = Some((Instant::now(), body.clone()));
        body
    }
    async fn fetch(&self) -> Option<(String, String)> {
        let mut reply = self
            .client
            .get(&self.endpoint)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "PrintPartner-UpdateCheck")
            .send()
            .await
            .ok()?;
        if !reply.status().is_success() {
            return None;
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = reply.chunk().await.ok()? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let version = value["tag_name"].as_str()?.trim();
        if version.is_empty() {
            return None;
        }
        let fallback = format!("https://github.com/{}/releases/latest", self.repo);
        let url = value["html_url"]
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(&fallback);
        Some((version.into(), url.into()))
    }
}
fn cache_hours(raw: Option<&str>) -> f64 {
    match raw.map(str::trim) {
        None => 12.0,
        Some("") => 0.0,
        Some(text) => text.parse().unwrap_or(f64::NAN),
    }
}
fn segments(raw: &str) -> Vec<u64> {
    let raw = raw.trim().trim_start_matches(['v', 'V']);
    let numeric: String = raw
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut values: Vec<_> = numeric.split('.').map(|v| v.parse().unwrap_or(0)).collect();
    while values.last() == Some(&0) {
        values.pop();
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn update_check_uses_local_fake_and_caches_refreshes_and_redacts_failures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/release", listener.local_addr().unwrap());
        let routes = axum::Router::new().route("/release", axum::routing::get(move || {
            let count = counter.fetch_add(1, Ordering::SeqCst);
            async move { axum::Json(if count == 0 { json!({"tag_name":"v99.0.0", "html_url":"https://github.com/example/releases/latest"}) } else { json!({"message":"private upstream error"}) }) }
        }));
        let mut server = crate::serving::Serving::start(listener, routes);
        let mut check = UpdateCheck::from_startup();
        check.enabled = true;
        check.latest = None;
        check.endpoint = endpoint;
        check.version = "3.3.0-web".into();
        let first = check.check(false).await;
        assert_eq!(first["latest_version"], "v99.0.0");
        assert_eq!(first["update_available"], true);
        assert_eq!(check.check(false).await, first);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let offline = check.check(true).await;
        assert!(offline["latest_version"].is_null());
        assert!(!offline.to_string().contains("private upstream"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        check.enabled = false;
        assert!(check.check(true).await["checked_at"].is_null());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        check.enabled = true;
        check.latest = Some("v100.0.0".into());
        assert_eq!(check.check(true).await["latest_version"], "v100.0.0");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.stop().await.unwrap();
    }
    #[test]
    fn update_check_version_comparison_matches_node_examples() {
        assert_eq!(cache_hours(None), 12.0);
        assert_eq!(cache_hours(Some("")), 0.0);
        assert_eq!(cache_hours(Some(" 2 ")), 2.0);
        assert!(cache_hours(Some("invalid")).is_nan());
        for (a, b, ordering) in [
            ("v1.2-web", "1.2.0", std::cmp::Ordering::Equal),
            ("1.10.0", "1.9.9", std::cmp::Ordering::Greater),
            ("unknown", "0.1.0", std::cmp::Ordering::Less),
        ] {
            assert_eq!(segments(a).cmp(&segments(b)), ordering);
        }
    }
}
