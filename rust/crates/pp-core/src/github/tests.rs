//! The GitHub transport against a local plain-HTTP stand-in for
//! `api.github.com` and `raw.githubusercontent.com`. No test calls GitHub.

use super::*;
use axum::{
    Json, Router,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use pp_services::sync::{self, SyncError, SyncLimits};
use serde_json::json;
use std::{collections::HashMap, net::SocketAddr};

/// A body with no length, so it arrives chunked without `Content-Length`.
struct Unsized(Option<hyper::body::Bytes>);
impl hyper::body::Body for Unsized {
    type Data = hyper::body::Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        std::task::Poll::Ready(
            self.0
                .take()
                .map(|bytes| Ok(hyper::body::Frame::data(bytes))),
        )
    }
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const TOKEN: &str = "stand-in-token";

/// GitHub answers 404 for a private repository the caller cannot read.
fn hidden(owner: &str, headers: &HeaderMap) -> bool {
    owner == "private"
        && headers.get("authorization").and_then(|v| v.to_str().ok())
            != Some(&format!("Bearer {TOKEN}"))
}

fn branch(n: usize) -> String {
    format!("branch-{n:03}")
}

fn page(names: impl Iterator<Item = String>, query: &HashMap<String, String>) -> Response {
    let per_page: usize = query["per_page"].parse().unwrap();
    let page: usize = query["page"].parse().unwrap();
    let rows: Vec<_> = names
        .skip((page - 1) * per_page)
        .take(per_page)
        .map(|name| json!({ "name": name, "commit": { "sha": SHA } }))
        .collect();
    Json(rows).into_response()
}

async fn stand_in() -> SocketAddr {
    let repos = Router::new()
        .route(
            "/repos/{owner}/{repo}",
            get(|Path((owner, _)): Path<(String, String)>, headers: HeaderMap| async move {
                if owner == "slow" {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                if owner == "gone" || hidden(&owner, &headers) {
                    return StatusCode::NOT_FOUND.into_response();
                }
                if owner == "huge" {
                    return "x".repeat(MAX_API_BYTES + 1).into_response();
                }
                if owner == "renamed" {
                    return (StatusCode::MOVED_PERMANENTLY, [("location", "/repos/maker/kit")])
                        .into_response();
                }
                if owner == "elsewhere" {
                    return (
                        StatusCode::MOVED_PERMANENTLY,
                        [("location", "http://localhost:9/repos/maker/kit")],
                    )
                        .into_response();
                }
                Json(json!({ "default_branch": "trunk" })).into_response()
            }),
        )
        .route(
            "/repos/{owner}/{repo}/{list}",
            get(
                |Path((owner, _, list)): Path<(String, String, String)>,
                 Query(query): Query<HashMap<String, String>>,
                 headers: HeaderMap| async move {
                    if hidden(&owner, &headers) {
                        return StatusCode::NOT_FOUND.into_response();
                    }
                    let names: Vec<String> = match (list.as_str(), owner.as_str()) {
                        ("tags", _) => vec!["v1.0".into(), "v1.1".into()],
                        ("branches", "even") => (0..200).map(branch).collect(),
                        // Every page is full, so only the page cap stops it.
                        ("branches", "endless") => (0..MAX_PAGES * 100 + 1).map(branch).collect(),
                        ("branches", _) => (0..250).map(branch).collect(),
                        _ => return StatusCode::NOT_FOUND.into_response(),
                    };
                    page(names.into_iter(), &query)
                },
            ),
        )
        .route(
            "/repos/{owner}/{repo}/commits/{git_ref}",
            get(
                |Path((_, _, git_ref)): Path<(String, String, String)>,
                 headers: HeaderMap| async move {
                    let accept = headers.get("accept").and_then(|v| v.to_str().ok());
                    if accept != Some("application/vnd.github.sha") || git_ref != "feature/x" {
                        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
                    }
                    SHA.to_uppercase().into_response()
                },
            ),
        )
        .route(
            "/repos/{owner}/{repo}/git/trees/{sha}",
            get(
                |Query(query): Query<HashMap<String, String>>| async move {
                    assert_eq!(query.get("recursive").map(String::as_str), Some("1"));
                    Json(json!({
                        "sha": SHA,
                        "truncated": false,
                        "tree": [
                            { "path": "parts/a b.stl", "type": "blob", "mode": "100644", "size": 5, "sha": SHA },
                            { "path": "README.md", "type": "blob", "mode": "100644", "size": 3, "sha": SHA }
                        ]
                    }))
                },
            ),
        )
        .route(
            "/raw/{owner}/{repo}/{commit}/{*path}",
            get(|Path((owner, _, _, path)): Path<(String, String, String, String)>, headers: HeaderMap| async move {
                if hidden(&owner, &headers) {
                    return StatusCode::NOT_FOUND.into_response();
                }
                match path.as_str() {
                    "parts/a b.stl" => "solid".into_response(),
                    "README.md" => "# hi".into_response(),
                    "streamed.stl" => {
                        axum::body::Body::new(Unsized(Some("solid!".into()))).into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, repos).await.unwrap() });
    address
}

fn transport(address: SocketAddr, timeout: Duration) -> GithubTransport {
    GithubTransport::stand_in(
        Url::parse(&format!("http://{address}/")).unwrap(),
        Url::parse(&format!("http://{address}/raw/")).unwrap(),
        timeout,
    )
}

#[tokio::test]
async fn branch_listing_reads_every_page() {
    let github = transport(stand_in().await, Duration::from_secs(5));
    let port = github.port(None);
    let listing = sync::list_branches(&port, "https://github.com/maker/kit")
        .await
        .unwrap();
    assert_eq!(
        (listing.owner.as_str(), listing.repo.as_str()),
        ("maker", "kit")
    );
    assert_eq!(listing.default_branch, "trunk");
    assert_eq!(listing.branches.len(), 250);
    assert_eq!(listing.branches[249], "branch-249");
    let tags = sync::list_tags(&port, "https://github.com/maker/kit")
        .await
        .unwrap();
    assert_eq!(tags.tags, ["v1.0", "v1.1"]);
    let even = port.list_branches("even", "kit").await.unwrap();
    assert_eq!(even.len(), 200);
    let endless = port.list_branches("endless", "kit").await.unwrap_err();
    assert_eq!(endless.status, None);
}

#[tokio::test]
async fn redirects_are_followed_only_within_the_origin() {
    let github = transport(stand_in().await, Duration::from_secs(5));
    let port = github.port(None);
    let renamed = port.default_branch("renamed", "kit").await.unwrap();
    assert_eq!(renamed.as_deref(), Some("trunk"));
    let elsewhere = port.default_branch("elsewhere", "kit").await.unwrap_err();
    assert_eq!(elsewhere.status, Some(301));
}

#[tokio::test]
async fn a_private_repository_needs_the_stored_token() {
    let github = transport(stand_in().await, Duration::from_secs(5));
    let url = "https://github.com/private/kit";
    let without = sync::list_branches(&github.port(None), url)
        .await
        .unwrap_err();
    assert!(matches!(
        without,
        SyncError::Port(PortError {
            status: Some(404),
            ..
        })
    ));
    let blank = sync::list_tags(&github.port(Some("  ".into())), url)
        .await
        .unwrap_err();
    assert!(matches!(
        blank,
        SyncError::Port(PortError {
            status: Some(404),
            ..
        })
    ));
    let with = github.port(Some(format!(" {TOKEN} ")));
    assert_eq!(
        sync::list_branches(&with, url)
            .await
            .unwrap()
            .branches
            .len(),
        250
    );
    assert_eq!(sync::list_tags(&with, url).await.unwrap().tags.len(), 2);
}

#[tokio::test]
async fn failures_carry_github_status_or_none() {
    let address = stand_in().await;
    let github = transport(address, Duration::from_millis(300));
    let gone = github
        .port(None)
        .default_branch("gone", "kit")
        .await
        .unwrap_err();
    assert_eq!(gone.status, Some(404));
    let slow = github
        .port(None)
        .default_branch("slow", "kit")
        .await
        .unwrap_err();
    assert_eq!(slow.status, None);
    let huge = github
        .port(None)
        .default_branch("huge", "kit")
        .await
        .unwrap_err();
    assert_eq!(huge.status, None);

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unreachable = transport(closed.local_addr().unwrap(), Duration::from_secs(5));
    drop(closed);
    let error = unreachable
        .port(None)
        .list_tags("maker", "kit")
        .await
        .unwrap_err();
    assert_eq!(error.status, None);
}

#[tokio::test]
async fn plan_and_download_read_the_commit_tree_and_files() {
    let github = transport(stand_in().await, Duration::from_secs(5));
    let port = github.port(None);
    let plan = sync::plan_github_sync(
        &port,
        "https://github.com/maker/kit",
        "feature/x",
        None,
        SyncLimits::default(),
    )
    .await
    .unwrap();
    assert_eq!(plan.commit_sha, SHA);
    let paths: Vec<_> = plan.files.iter().map(|file| file.path.as_str()).collect();
    assert!(paths.contains(&"parts/a b.stl") && paths.contains(&"README.md"));

    let mut body = Vec::new();
    let length = port
        .download("maker", "kit", SHA, "parts/a b.stl", 5, &mut body)
        .await
        .unwrap();
    assert_eq!((length, body.as_slice()), (5, b"solid".as_slice()));

    let mut short = Vec::new();
    let error = port
        .download("maker", "kit", SHA, "parts/a b.stl", 4, &mut short)
        .await
        .unwrap_err();
    assert_eq!(error.status, None);
    let missing = port
        .download("maker", "kit", SHA, "nope.stl", 4, &mut Vec::new())
        .await
        .unwrap_err();
    assert_eq!(missing.status, Some(404));
    let mut streamed = Vec::new();
    let error = port
        .download("maker", "kit", SHA, "streamed.stl", 5, &mut streamed)
        .await
        .unwrap_err();
    assert_eq!(error.status, None);
    let private = github.port(Some(TOKEN.into()));
    let mut readme = Vec::new();
    private
        .download("private", "kit", SHA, "README.md", 4, &mut readme)
        .await
        .unwrap();
    assert_eq!(readme, b"# hi");
    let hidden = port
        .download("private", "kit", SHA, "README.md", 4, &mut Vec::new())
        .await
        .unwrap_err();
    assert_eq!(hidden.status, Some(404));
    let escape = port
        .download("maker", "kit", SHA, "../kit/README.md", 4, &mut Vec::new())
        .await
        .unwrap_err();
    assert_eq!(escape.status, None);
}

#[test]
fn only_public_addresses_pass() {
    for ip in ["140.82.112.6", "2606:50c0:8000::153", "::ffff:140.82.112.6"] {
        assert!(public(ip.parse().unwrap()), "{ip}");
    }
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "192.168.1.2",
        "169.254.169.254",
        "0.0.0.0",
        "::1",
        "fd00::1",
        "fe80::1",
        "::ffff:10.0.0.1",
    ] {
        assert!(!public(ip.parse().unwrap()), "{ip}");
    }
}

struct Fixed(Vec<IpAddr>);
impl Resolver for Fixed {
    fn resolve(&self, _host: String) -> pp_net::outbound::Resolution {
        let answer = self.0.clone();
        Box::pin(async move { Ok(answer) })
    }
}

#[tokio::test]
async fn a_dns_answer_with_any_private_address_is_refused() {
    use reqwest::dns::Resolve;
    let name = || "api.github.com".parse::<reqwest::dns::Name>().unwrap();
    let mixed = github_dns(Arc::new(Fixed(vec![
        "140.82.112.6".parse().unwrap(),
        "10.0.0.1".parse().unwrap(),
    ])));
    assert!(mixed.resolve(name()).await.is_err());
    let empty = github_dns(Arc::new(Fixed(vec![])));
    assert!(empty.resolve(name()).await.is_err());
    let good = github_dns(Arc::new(Fixed(vec!["140.82.112.6".parse().unwrap()])));
    assert_eq!(good.resolve(name()).await.unwrap().count(), 1);
}

#[test]
fn the_production_transport_builds() {
    let github = GithubTransport::new().unwrap();
    assert_eq!(github.0.api.as_str(), "https://api.github.com/");
    assert_eq!(github.0.raw.as_str(), "https://raw.githubusercontent.com/");
}
