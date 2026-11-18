//! `pp-server mcp-stdio`: an MCP server on stdin and stdout for clients that
//! launch one, forwarding every tool call to the running core's HTTP MCP with
//! an API key (GRE-1332).
//!
//! The bridge takes no data directory and opens no database: the core owns
//! its storage. It connects to the core before it reads stdin, and when no
//! core answers, or the core refuses the key, it exits 1 without serving.

use anyhow::{Context, Result, bail, ensure};
use reqwest::Url;
use rmcp::{
    ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, ClientCapabilities, ClientConfig, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    },
    service::{Peer, RequestContext, ServiceError},
    transport::{
        StreamableHttpClientTransport, stdio_pipes,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use std::{net::IpAddr, time::Duration};

pub(crate) const USAGE: &str = "Usage: pp-server mcp-stdio --url <url>

Serves MCP on stdin and stdout and forwards each request to the Print Partner
core already running at <url>, for example http://127.0.0.1:8080/api/v1/mcp.
A URL without a path uses /api/v1/mcp. Plain http is allowed only to a
loopback address; use https for any other host.

The API key comes from the PRINT_PARTNER_API_KEY environment variable, never
from the command line. With no core answering, or a key the core refuses, it
exits 1 before reading stdin. It never opens the data directory.";

/// The variable that carries the API key, as for the Cursor plugin.
pub(crate) const KEY_VARIABLE: &str = "PRINT_PARTNER_API_KEY";
const DEFAULT_PATH: &str = "/api/v1/mcp";
/// How long the core may take to answer `initialize` before the bridge gives up.
const HANDSHAKE: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq)]
pub(crate) enum Command {
    Help,
    Bridge { url: Url },
}

pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Result<Command> {
    let mut url = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--url" => url = Some(args.next().context("Argument value missing")?),
            _ => bail!("Unknown argument"),
        }
    }
    let url = core_url(&url.context("--url is required")?)?;
    Ok(Command::Bridge { url })
}

/// The core's MCP URL. A key may cross the network only over https.
fn core_url(value: &str) -> Result<Url> {
    let mut url = Url::parse(value).context("Invalid --url")?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "--url must not carry credentials; set {KEY_VARIABLE}"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "--url must not have a query or fragment"
    );
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&url) => {}
        "http" => bail!("--url must use https unless the host is a loopback address"),
        _ => bail!("--url must be an http or https URL"),
    }
    if url.path() == "/" {
        url.set_path(DEFAULT_PATH);
    }
    Ok(url)
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let address = host.trim_start_matches('[').trim_end_matches(']');
    match address.parse::<IpAddr>() {
        Ok(ip) => ip.to_canonical().is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// The key from the environment, refused when unset or empty.
pub(crate) fn api_key(value: Option<std::ffi::OsString>) -> Result<String> {
    let key = value
        .with_context(|| format!("{KEY_VARIABLE} is not set; MCP always needs an API key"))?
        .into_string()
        .map_err(|_| anyhow::anyhow!("{KEY_VARIABLE} must be UTF-8"))?;
    ensure!(
        !key.trim().is_empty(),
        "{KEY_VARIABLE} is empty; MCP always needs an API key"
    );
    Ok(key)
}

/// Connects to the core, then serves stdio until the client closes stdin.
pub(crate) async fn run(url: Url, key: String) -> Result<()> {
    // `core_url` checks only this URL, so a redirect, which would carry the
    // request body and session header to any other origin, is never followed.
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    // `core_url` allows plain http only to loopback. An inherited HTTP proxy
    // would carry that request, key included, off the machine in the clear,
    // so loopback goes direct. https may still use a proxy: it tunnels.
    let http = match url.scheme() {
        "http" => http.no_proxy(),
        _ => http,
    }
    .build()
    .context("Build the HTTP client")?;
    // One session per process: a session the core forgot (a restart) is an
    // error, not a silent new session without the pending proposals.
    let config = StreamableHttpClientTransportConfig::with_uri(url.as_str())
        .auth_header(key)
        .reinit_on_expired_session(false);
    let client = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("pp-server-mcp-stdio", env!("CARGO_PKG_VERSION")),
    );
    let refused = |reason: String| {
        anyhow::anyhow!(
            "No Print Partner core accepted the connection at {url}: {reason}. \
             Start Print Partner and check {KEY_VARIABLE}; mcp-stdio never opens the database."
        )
    };
    let handshake = client.serve(StreamableHttpClientTransport::with_client(http, config));
    let upstream = match tokio::time::timeout(HANDSHAKE, handshake).await {
        Ok(Ok(upstream)) => upstream,
        Ok(Err(error)) => return Err(refused(error.to_string())),
        Err(_) => {
            return Err(refused(format!(
                "no answer within {} seconds",
                HANDSHAKE.as_secs()
            )));
        }
    };
    let bridge = Bridge::new(upstream.peer().clone());
    let stdio = bridge
        .serve(stdio_pipes())
        .await
        .context("Start MCP on stdio")?;
    let quit = stdio.waiting().await;
    // Ends the core's MCP session; its pending proposals go with it.
    let _ = upstream.cancel().await;
    quit.context("MCP on stdio stopped")?;
    Ok(())
}

/// The stdio server: each request goes to the core on one MCP session, the
/// one this process opened, so proposals and `confirm_apply` meet there.
struct Bridge {
    core: Peer<RoleClient>,
    info: ServerConfig,
}

impl Bridge {
    fn new(core: Peer<RoleClient>) -> Self {
        // Only tools are forwarded, so only tools are advertised.
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        if let Some(peer) = core.peer_info() {
            if let Some(server) = peer.server_info.clone() {
                info = info.with_server_info(server);
            }
            if let Some(instructions) = peer.instructions.clone() {
                info = info.with_instructions(instructions);
            }
        }
        Self { core, info }
    }
}

/// The core's own JSON-RPC error passes through unchanged; a broken link to
/// the core becomes an internal error that says so.
fn forwarded(error: ServiceError) -> ErrorData {
    match error {
        ServiceError::McpError(error) => error,
        other => ErrorData::internal_error(format!("Print Partner core: {other}"), None),
    }
}

impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        self.info.clone()
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.core.list_tools(request).await.map_err(forwarded)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.core.call_tool_once(request).await.map_err(forwarded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Command> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    fn url_of(args: &[&str]) -> String {
        match parse_args(args).unwrap() {
            Command::Bridge { url } => url.to_string(),
            Command::Help => panic!("help"),
        }
    }

    #[test]
    fn loopback_http_and_any_https_are_accepted() {
        for (given, expected) in [
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080/api/v1/mcp"),
            ("http://127.0.0.1:8080/mcp", "http://127.0.0.1:8080/mcp"),
            (
                "http://localhost:9/api/v1/mcp",
                "http://localhost:9/api/v1/mcp",
            ),
            ("http://[::1]:9/mcp", "http://[::1]:9/mcp"),
            ("http://127.0.0.2:9/mcp", "http://127.0.0.2:9/mcp"),
            (
                "https://pp.example/api/v1/mcp",
                "https://pp.example/api/v1/mcp",
            ),
        ] {
            assert_eq!(url_of(&["--url", given]), expected, "{given}");
        }
    }

    #[test]
    fn a_key_never_crosses_the_network_in_the_clear() {
        for (given, message) in [
            (
                "http://192.168.1.5:8080/api/v1/mcp",
                "--url must use https unless the host is a loopback address",
            ),
            (
                "http://[::ffff:192.168.1.5]:8080/mcp",
                "--url must use https unless the host is a loopback address",
            ),
            (
                "http://pp.example/mcp",
                "--url must use https unless the host is a loopback address",
            ),
            ("ftp://127.0.0.1/mcp", "--url must be an http or https URL"),
            (
                "http://user:secret@127.0.0.1/mcp",
                "--url must not carry credentials; set PRINT_PARTNER_API_KEY",
            ),
            (
                "http://127.0.0.1/mcp?key=x",
                "--url must not have a query or fragment",
            ),
        ] {
            let error = parse_args(&["--url", given]).expect_err(given);
            assert_eq!(error.to_string(), message, "{given}");
        }
    }

    #[test]
    fn arguments_are_strict() {
        assert_eq!(parse_args(&["--help"]).unwrap(), Command::Help);
        for (args, message) in [
            (&[][..], "--url is required"),
            (&["--url"][..], "Argument value missing"),
            (&["--data", "/data"][..], "Unknown argument"),
            (&["--url", "not a url"][..], "Invalid --url"),
        ] {
            let error = parse_args(args).expect_err(message);
            assert_eq!(error.to_string(), message, "{args:?}");
        }
    }

    #[test]
    fn the_key_must_be_set_and_non_empty() {
        assert_eq!(api_key(Some("ppk_x".into())).unwrap(), "ppk_x");
        for value in [None, Some(""), Some("  ")] {
            let error = api_key(value.map(Into::into)).expect_err("refused");
            assert!(
                error.to_string().contains("MCP always needs an API key"),
                "{error}"
            );
        }
    }
}
