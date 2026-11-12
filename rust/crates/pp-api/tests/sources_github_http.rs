//! GRE-429 PR 1: `GET /sources/github-branches` and `GET /sources/github-tags`
//! over HTTP, with a stand-in [`GithubConnector`]. The stored GitHub token from
//! `PUT /settings/github-pat` reaches the port; GitHub's failures map to 404 or 502.
#![cfg(feature = "desktop_rebuild")]

use pp_api::{
    admin::AdminHttpConfig,
    serving::Serving,
    settings::settings_router,
    sources_github::{GithubConnector, github_router},
};
use pp_services::sync::{GithubPort, PortError, RawTree};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
};
use reqwest::Client;
use serde_json::{Value, json};
use std::path::Path;

const COOKIE: (&str, &str) = ("cookie", "pp_session=read-fixture-secret");
const TOKEN: &str = "stand-in-token";

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

/// Answers by owner: `gone` is GitHub's 404, `down` no answer, `limited`
/// GitHub's 403, `revoked` its 401, and `private` a 404 without the token.
#[derive(Clone)]
struct StandIn;

struct StandInPort(Option<String>);

impl GithubConnector for StandIn {
    type Port = StandInPort;
    fn port(&self, token: Option<String>) -> StandInPort {
        StandInPort(token)
    }
}

impl StandInPort {
    fn check(&self, owner: &str) -> Result<(), PortError> {
        let status = match owner {
            "gone" => Some(404),
            "down" => None,
            "limited" => Some(403),
            "revoked" => Some(401),
            "private" if self.0.as_deref() != Some(TOKEN) => Some(404),
            _ => return Ok(()),
        };
        Err(PortError {
            status,
            message: "private upstream detail".into(),
        })
    }
}

impl GithubPort for StandInPort {
    async fn default_branch(&self, owner: &str, _: &str) -> Result<Option<String>, PortError> {
        self.check(owner)?;
        Ok(Some("trunk".into()))
    }
    async fn list_branches(&self, owner: &str, _: &str) -> Result<Vec<String>, PortError> {
        self.check(owner)?;
        Ok((0..150).map(|n| format!("branch-{n:03}")).collect())
    }
    async fn list_tags(&self, owner: &str, _: &str) -> Result<Vec<String>, PortError> {
        self.check(owner)?;
        Ok(vec!["v1.0".into()])
    }
    async fn commit_sha(&self, _: &str, _: &str, _: &str) -> Result<String, PortError> {
        unreachable!("listings never read a commit")
    }
    async fn tree(&self, _: &str, _: &str, _: &str) -> Result<RawTree, PortError> {
        unreachable!("listings never read a tree")
    }
}

struct Server {
    owner: WriterOwner,
    origin: String,
    client: Client,
    serving: Serving,
}

impl Server {
    async fn start(directory: &Path, flag: Option<&str>) -> Self {
        std::fs::write(
            directory.join("print-partner.db"),
            include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        let owner = WriterOwner::open(directory, Limits::default()).unwrap().0;
        let (listener, origin) = pp_testkit::http_listener().await.unwrap();
        let config = AdminHttpConfig::from_startup(&origin, "default", flag).unwrap();
        let settings = owner.app_settings_with_policy(policy()).unwrap();
        let reads = owner.accepted_reads().unwrap();
        let router = github_router(config.clone(), reads.clone(), settings.clone(), StandIn)
            .merge(settings_router(config, reads, settings));
        Self {
            owner,
            origin,
            client: Client::builder().no_proxy().build().unwrap(),
            serving: Serving::start(listener, router),
        }
    }

    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", self.origin))
            .header("origin", &self.origin)
            .header(COOKIE.0, COOKIE.1);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let bytes = response.bytes().await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        self.send("GET", path, None).await
    }

    async fn cache_control(&self, path: &str) -> Option<String> {
        let response = self
            .client
            .get(format!("{}{path}", self.origin))
            .header(COOKIE.0, COOKIE.1)
            .send()
            .await
            .unwrap();
        let value = response.headers().get("cache-control")?;
        Some(value.to_str().unwrap().to_owned())
    }

    async fn stop(mut self) {
        self.serving.stop().await.unwrap();
        self.owner.shutdown().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn branches_and_tags_list_a_repository() {
    let directory = tempfile::tempdir().unwrap();
    let server = Server::start(directory.path(), Some("1")).await;
    for prefix in ["", "/api/v1"] {
        let (status, body) = server
            .get(&format!(
                "{prefix}/sources/github-branches?url=https://github.com/maker/kit"
            ))
            .await;
        assert_eq!(status, 200, "{prefix}");
        assert_eq!(
            (&body["owner"], &body["repo"], &body["default_branch"]),
            (&json!("maker"), &json!("kit"), &json!("trunk"))
        );
        assert_eq!(body["branches"].as_array().unwrap().len(), 150);
        let (status, body) = server
            .get(&format!(
                "{prefix}/sources/github-tags?url=https://github.com/maker/kit"
            ))
            .await;
        assert_eq!(
            (status, body),
            (
                200,
                json!({"owner": "maker", "repo": "kit", "tags": ["v1.0"]})
            )
        );
    }
    let path = "/api/v1/sources/github-tags?url=https://github.com/maker/kit";
    assert_eq!(
        server.cache_control(path).await.as_deref(),
        Some("private, no-store")
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_repository_lists_after_the_token_is_saved() {
    let directory = tempfile::tempdir().unwrap();
    let server = Server::start(directory.path(), Some("1")).await;
    let branches = "/api/v1/sources/github-branches?url=https://github.com/private/kit";
    let tags = "/api/v1/sources/github-tags?url=https://github.com/private/kit";
    assert_eq!(server.get(branches).await.0, 404);
    assert_eq!(server.get(tags).await.0, 404);
    let saved = server
        .send(
            "PUT",
            "/api/v1/settings/github-pat",
            Some(json!({"token": TOKEN})),
        )
        .await;
    assert_eq!(saved.0, 200);
    assert_eq!(server.get(branches).await.0, 200);
    assert_eq!(server.get(tags).await.0, 200);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_are_problems_without_github_text() {
    let directory = tempfile::tempdir().unwrap();
    let server = Server::start(directory.path(), Some("1")).await;
    let cases = [
        ("url=https://example.com/maker/kit", 400),
        ("url=not%20a%20url", 400),
        ("url=%20", 400),
        ("", 400),
        ("url=https://github.com/gone/kit", 404),
        ("url=https://github.com/down/kit", 502),
        ("url=https://github.com/limited/kit", 502),
        ("url=https://github.com/revoked/kit", 502),
    ];
    for (query, expected) in cases {
        for route in ["github-branches", "github-tags"] {
            let (status, body) = server
                .get(&format!("/api/v1/sources/{route}?{query}"))
                .await;
            assert_eq!(status, expected, "{route}?{query}");
            assert_eq!(body["status"], expected, "{route}?{query}");
            assert!(
                !body.to_string().contains("private upstream detail"),
                "{route}?{query}: {body}"
            );
            let code = match query {
                "url=https://github.com/limited/kit" => json!("github_refused"),
                "url=https://github.com/revoked/kit" => json!("github_token_rejected"),
                _ => Value::Null,
            };
            assert_eq!(body["code"], code, "{route}?{query}");
        }
    }
    let anonymous = server
        .client
        .get(format!(
            "{}/api/v1/sources/github-tags?url=https://github.com/maker/kit",
            server.origin
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_mounted_with_the_flag_off() {
    let directory = tempfile::tempdir().unwrap();
    let server = Server::start(directory.path(), None).await;
    let (status, _) = server
        .get("/api/v1/sources/github-branches?url=https://github.com/maker/kit")
        .await;
    assert_eq!(status, 404);
    server.stop().await;
}
