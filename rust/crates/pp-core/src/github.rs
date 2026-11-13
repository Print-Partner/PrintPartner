//! The GitHub transport (GRE-429 PR 1): [`GithubPort`] reads from
//! `api.github.com` and raw file downloads from `raw.githubusercontent.com`.
//!
//! Every request goes over HTTPS to one of those two hosts, with no proxy.
//! Redirects are followed only within the same origin. Each DNS answer must
//! be a public address. Each request has one deadline for its answer and body,
//! and a size cap; list reads stop at [`MAX_PAGES`]. The stored token, when
//! set, goes only to these two hosts.

use pp_api::sources_github::GithubConnector;
use pp_net::outbound::{Resolver, SystemResolver, permitted};
use pp_services::sync::{GithubPort, PortError, RawTree};
use reqwest::{RequestBuilder, StatusCode, Url, header};
use serde::Deserialize;
use std::{net::IpAddr, sync::Arc, time::Duration};
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// The largest API response read: Node's `MAX_GITHUB_API_RESPONSE_BYTES`.
pub const MAX_API_BYTES: usize = 16 * 1024 * 1024;
/// Pages of 100 a branch or tag listing reads before it gives up.
pub const MAX_PAGES: usize = 100;
const PER_PAGE: usize = 100;
const MAX_REDIRECTS: usize = 3;
const API_TIMEOUT: Duration = Duration::from_secs(30);
/// `SyncLimits::file_timeout_ms`'s default.
const FILE_TIMEOUT: Duration = Duration::from_secs(120);

/// The shared client. [`GithubConnector::port`] hands out a [`GithubSession`]
/// that carries one request's token.
#[derive(Clone)]
pub struct GithubTransport(Arc<Inner>);

struct Inner {
    client: reqwest::Client,
    api: Url,
    raw: Url,
    api_timeout: Duration,
    file_timeout: Duration,
}

impl GithubTransport {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self(Arc::new(Inner {
            client: client(Some(Arc::new(SystemResolver)))?,
            api: Url::parse("https://api.github.com/")?,
            raw: Url::parse("https://raw.githubusercontent.com/")?,
            api_timeout: API_TIMEOUT,
            file_timeout: FILE_TIMEOUT,
        })))
    }

    /// A transport aimed at a local plain-HTTP stand-in for both hosts. It
    /// differs from [`Self::new`] only in allowing plain HTTP and skipping DNS.
    #[cfg(test)]
    pub(crate) fn stand_in(api: Url, raw: Url, timeout: Duration) -> Self {
        Self(Arc::new(Inner {
            client: client(None).expect("stand-in client"),
            api,
            raw,
            api_timeout: timeout,
            file_timeout: timeout,
        }))
    }
}

/// The one client builder. `resolver` is the checked DNS for production;
/// `None` (tests only) also allows plain HTTP to a stand-in.
fn client(resolver: Option<Arc<dyn Resolver>>) -> reqwest::Result<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(redirects())
        .user_agent("print-partner")
        .connect_timeout(API_TIMEOUT);
    match resolver {
        Some(resolver) => builder
            .https_only(true)
            .dns_resolver(Arc::new(PublicDns(resolver)))
            .build(),
        None => builder.build(),
    }
}

/// Follows up to [`MAX_REDIRECTS`] redirects that stay on the first
/// request's origin, as GitHub answers for a renamed repository. A redirect
/// anywhere else stops, and its 3xx fails the read.
fn redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|first| first.origin() == attempt.url().origin());
        if same_origin && attempt.previous().len() <= MAX_REDIRECTS {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

impl GithubConnector for GithubTransport {
    type Port = GithubSession;

    fn port(&self, token: Option<String>) -> GithubSession {
        GithubSession {
            inner: Arc::clone(&self.0),
            token: token.filter(|token| !token.trim().is_empty()),
        }
    }
}

/// GitHub reads with one token, or none.
pub struct GithubSession {
    inner: Arc<Inner>,
    token: Option<String>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct Repository {
    default_branch: Option<String>,
}

impl GithubSession {
    fn api_url(&self, segments: &[&str]) -> Result<Url, PortError> {
        url_with(&self.inner.api, segments)
    }

    fn authorized(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.token {
            Some(token) => request.bearer_auth(token.trim()),
            None => request,
        }
    }

    async fn api_bytes(&self, url: Url, accept: &str) -> Result<Vec<u8>, PortError> {
        let request = self
            .inner
            .client
            .get(url)
            .header(header::ACCEPT, accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        let mut body = Vec::new();
        let request = self.authorized(request);
        fetch(
            request,
            MAX_API_BYTES as u64,
            self.inner.api_timeout,
            &mut body,
        )
        .await?;
        Ok(body)
    }

    async fn api_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, PortError> {
        let body = self.api_bytes(url, "application/vnd.github+json").await?;
        serde_json::from_slice(&body).map_err(|_| failure("GitHub answered unreadable JSON"))
    }

    async fn names(&self, owner: &str, repo: &str, list: &str) -> Result<Vec<String>, PortError> {
        let mut names = Vec::new();
        for page in 1..=MAX_PAGES {
            let mut url = self.api_url(&["repos", owner, repo, list])?;
            url.query_pairs_mut()
                .append_pair("per_page", &PER_PAGE.to_string())
                .append_pair("page", &page.to_string());
            let rows: Vec<Named> = self.api_json(url).await?;
            let last = rows.len() < PER_PAGE;
            names.extend(rows.into_iter().map(|row| row.name));
            if last {
                return Ok(names);
            }
        }
        Err(failure("GitHub listing has too many pages"))
    }

    /// Streams `path` at `commit` into `out` and returns its length. Refuses
    /// a body longer than `size`, the blob size the tree reported.
    pub async fn download(
        &self,
        owner: &str,
        repo: &str,
        commit: &str,
        path: &str,
        size: u64,
        out: &mut (impl AsyncWrite + Unpin),
    ) -> Result<u64, PortError> {
        let mut segments = vec![owner, repo, commit];
        segments.extend(path.split('/'));
        let url = url_with(&self.inner.raw, &segments)?;
        let request = self.authorized(self.inner.client.get(url));
        fetch(request, size, self.inner.file_timeout, out).await
    }
}

impl GithubPort for GithubSession {
    async fn default_branch(&self, owner: &str, repo: &str) -> Result<Option<String>, PortError> {
        let url = self.api_url(&["repos", owner, repo])?;
        Ok(self.api_json::<Repository>(url).await?.default_branch)
    }

    async fn list_branches(&self, owner: &str, repo: &str) -> Result<Vec<String>, PortError> {
        self.names(owner, repo, "branches").await
    }

    async fn list_tags(&self, owner: &str, repo: &str) -> Result<Vec<String>, PortError> {
        self.names(owner, repo, "tags").await
    }

    /// Asks for the SHA media type, so GitHub answers the bare SHA rather
    /// than the commit with its diff.
    async fn commit_sha(
        &self,
        owner: &str,
        repo: &str,
        git_ref: &str,
    ) -> Result<String, PortError> {
        let url = self.api_url(&["repos", owner, repo, "commits", git_ref])?;
        let body = self.api_bytes(url, "application/vnd.github.sha").await?;
        let sha = std::str::from_utf8(&body).unwrap_or("").trim();
        if sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(sha.to_ascii_lowercase())
        } else {
            Err(failure("GitHub answered an unreadable commit"))
        }
    }

    async fn tree(&self, owner: &str, repo: &str, tree_sha: &str) -> Result<RawTree, PortError> {
        let mut url = self.api_url(&["repos", owner, repo, "git", "trees", tree_sha])?;
        url.query_pairs_mut().append_pair("recursive", "1");
        self.api_json(url).await
    }
}

fn failure(message: &str) -> PortError {
    PortError {
        status: None,
        message: message.into(),
    }
}

/// `base` with each segment appended and percent-encoded. A segment that
/// could change the path (empty, `.` or `..`) is refused.
fn url_with(base: &Url, segments: &[&str]) -> Result<Url, PortError> {
    if segments
        .iter()
        .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return Err(failure("Invalid GitHub path"));
    }
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| failure("Invalid GitHub path"))?
        .pop_if_empty()
        .extend(segments);
    Ok(url)
}

/// Sends `request` and copies a 2xx body into `out`, at most `cap` bytes,
/// all within `timeout`. Returns the body length.
async fn fetch(
    request: RequestBuilder,
    cap: u64,
    timeout: Duration,
    out: &mut (impl AsyncWrite + Unpin),
) -> Result<u64, PortError> {
    let work = async {
        let mut response = request
            .send()
            .await
            .map_err(|_| failure("GitHub could not be reached"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(PortError {
                status: Some(status.as_u16()),
                message: match status {
                    StatusCode::NOT_FOUND => "GitHub repository or file not found".into(),
                    _ => format!("GitHub answered HTTP {}", status.as_u16()),
                },
            });
        }
        if response.content_length().is_some_and(|length| length > cap) {
            return Err(failure("GitHub response is too large"));
        }
        let mut total = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| failure("GitHub response was cut off"))?
        {
            total = total.saturating_add(chunk.len() as u64);
            if total > cap {
                return Err(failure("GitHub response is too large"));
            }
            out.write_all(&chunk)
                .await
                .map_err(|_| failure("Could not write the GitHub file"))?;
        }
        out.flush()
            .await
            .map_err(|_| failure("Could not write the GitHub file"))?;
        Ok(total)
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| failure("GitHub timed out"))?
}

/// An address GitHub can have: one `pp_net` permits that is not loopback,
/// private or unique-local.
fn public(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    permitted(ip)
        && match ip {
            IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_private(),
            IpAddr::V6(v6) => !v6.is_loopback() && v6.segments()[0] & 0xfe00 != 0xfc00,
        }
}

/// Refuses a DNS answer unless every address is [`public`].
struct PublicDns(Arc<dyn Resolver>);

impl reqwest::dns::Resolve for PublicDns {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let future = self.0.resolve(name.as_str().into());
        Box::pin(async move {
            let addresses = future.await?;
            if addresses.is_empty()
                || addresses.len() > 32
                || !addresses.iter().all(|ip| public(*ip))
            {
                return Err("Prohibited GitHub DNS answer".into());
            }
            Ok(Box::new(
                addresses
                    .into_iter()
                    .map(|ip| std::net::SocketAddr::new(ip, 0)),
            ) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests;
