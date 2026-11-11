use super::*;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/settings/github-pat", get(get_pat).put(put_pat))
        .route(
            "/settings/discord-notify",
            get(get_discord).put(put_discord),
        )
}

async fn get_pat(State(app): State<App>, Extension(p): Extension<PresentedCredential>) -> Response {
    match read(&app, p, Key::GithubPat).await {
        Ok(token) => ok(&json!({"configured": token.is_some(), "masked": null})),
        Err(error) => *error,
    }
}
async fn put_pat(
    State(app): State<App>,
    Extension(p): Extension<PresentedCredential>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token = match field(&headers, &body, "token") {
        Ok(Value::String(token)) => js_trim(&token).to_owned(),
        Ok(_) => return failure(StatusCode::BAD_REQUEST, "Token must be a string"),
        Err(error) => return *error,
    };
    let configured = !token.is_empty();
    match write(&app, p, Write::GithubPat(token)).await {
        Ok(_) => ok(&json!({"configured": configured, "masked": configured.then_some("****")})),
        Err(error) => *error,
    }
}

fn discord_body(values: [Option<String>; 4]) -> Value {
    json!({"webhook_url": values[0].as_deref().map(redacted),
        "notify_on_update": values[1].as_deref().unwrap_or("1") != "0",
        "notify_on_sync": values[2].as_deref().unwrap_or("0") != "0",
        "auto_sync_updates": values[3].as_deref().unwrap_or("1") != "0"})
}
fn redacted(value: &str) -> String {
    // Legacy rows are untrusted: never echo credentials, query, fragment or path.
    reqwest::Url::parse(value)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?;
            matches!(url.scheme(), "https" | "http")
                .then(|| format!("{}://{host}/[REDACTED]", url.scheme()))
        })
        .unwrap_or_else(|| "[REDACTED]".into())
}
async fn get_discord(
    State(app): State<App>,
    Extension(p): Extension<PresentedCredential>,
) -> Response {
    match read_outcome(&app, p, Key::DiscordNotify).await {
        Ok(Outcome::DiscordNotify(values)) => ok(&discord_body(values)),
        Ok(_) => unreadable(),
        Err(error) => *error,
    }
}

const WEBHOOK_ERROR: &str =
    "Webhook URL must look like https://discord.com/api/webhooks/<id>/<token>";
fn valid_webhook(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = rest.split_once("/api/webhooks/") else {
        return false;
    };
    if ![
        "discord.com",
        "discordapp.com",
        "ptb.discord.com",
        "canary.discord.com",
        "ptb.discordapp.com",
        "canary.discordapp.com",
    ]
    .contains(&host)
    {
        return false;
    }
    let Some((id, token)) = path.strip_suffix('/').unwrap_or(path).split_once('/') else {
        return false;
    };
    !id.is_empty()
        && id.bytes().all(|c| c.is_ascii_digit())
        && !js_trim(token).is_empty()
        && !token.contains(['/', '?', '#', '\\'])
        && !token.chars().any(char::is_control)
}
async fn put_discord(
    State(app): State<App>,
    Extension(p): Extension<PresentedCredential>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Parse once and retain absent fields (Node's partial update semantics).
    if let Err(error) = field(&headers, &body, "webhook_url") {
        return *error;
    }
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut values: [Option<String>; 4] = [None, None, None, None];
    if let Some(value) = parsed.get("webhook_url") {
        let text = match value {
            Value::Null => "",
            Value::String(s) => js_trim(s),
            _ => return unreadable(),
        };
        if !text.is_empty() && !valid_webhook(text) {
            return failure(StatusCode::BAD_REQUEST, WEBHOOK_ERROR);
        }
        values[0] = Some(text.into());
    }
    for (index, name) in ["notify_on_update", "notify_on_sync", "auto_sync_updates"]
        .iter()
        .enumerate()
    {
        if let Some(value) = parsed.get(name) {
            values[index + 1] = Some(if truthy(value) { "1" } else { "0" }.into());
        }
    }
    match write(&app, p, Write::DiscordNotify(values)).await {
        Ok(Outcome::DiscordNotify(values)) => ok(&discord_body(values)),
        Ok(_) => unreadable(),
        Err(error) => *error,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webhook_validation_and_redaction_never_echo_legacy_secrets() {
        for host in [
            "discord.com",
            "discordapp.com",
            "ptb.discord.com",
            "canary.discord.com",
        ] {
            let url = format!("https://{host}/api/webhooks/123/obviously-fake-token/");
            assert!(valid_webhook(&url));
            assert_eq!(redacted(&url), format!("https://{host}/[REDACTED]"));
        }
        for url in [
            "https://discord.com@evil.invalid/api/webhooks/1/fake",
            "https://discord.com/api/webhooks/1/fake?x=secret",
            "https://discord.com/api/webhooks/1/fake/extra",
            "http://discord.com/api/webhooks/1/fake",
            "https://127.0.0.1/api/webhooks/1/fake",
            "https://discord.com/[REDACTED]",
        ] {
            assert!(!valid_webhook(url));
        }
        assert_eq!(redacted("not-a-url-obviously-fake"), "[REDACTED]");
        assert_eq!(
            redacted("https://fake-user:fake-pass@discord.com/fake?fake#fake"),
            "https://discord.com/[REDACTED]"
        );
    }
}
