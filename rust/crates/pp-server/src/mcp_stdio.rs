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
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig,
        ContentBlock, ErrorCode, Implementation, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerConfig,
    },
    service::{ClientInitializeError, Peer, RequestContext, ServiceError},
    transport::{
        DynamicTransportError, StreamableHttpClientTransport, stdio_pipes,
        streamable_http_client::{StreamableHttpClientTransportConfig, StreamableHttpError},
    },
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

pub(crate) const USAGE: &str = "Usage: pp-server mcp-stdio --url <url>

Serves MCP on stdin and stdout and forwards each request to the Print Partner
core already running at <url>, for example http://127.0.0.1:8080/api/v1/mcp.
A URL without a path uses /api/v1/mcp. Plain http is allowed only to a
loopback address. The name localhost is connected to 127.0.0.1 and ::1
only, never to another address from the resolver. Use https for any
other host.

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

/// The HTTP client for one core URL.
///
/// Redirects are off: `core_url` checked only this URL, and a redirect would
/// carry the body and session header to another origin. Plain http goes
/// direct, because an inherited proxy would take the key off the machine in
/// the clear. https may still use a proxy: it tunnels. The name `localhost`
/// is pinned to loopback; the system resolver is not asked, and the request
/// still sends this URL's Host.
fn http_client(url: &Url) -> Result<reqwest::Client> {
    let mut http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if url.scheme() == "http" {
        http = http.no_proxy();
        if url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
        {
            http = http.resolve_to_addrs(
                "localhost",
                &[
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
                ],
            );
        }
    }
    http.build().context("Build the HTTP client")
}

/// Connects to the core, then serves stdio until the client closes stdin.
pub(crate) async fn run(url: Url, key: String) -> Result<()> {
    let http = http_client(&url)?;
    // One session per process: a session the core forgot (a restart) is an
    // error, not a silent new session without the pending proposals.
    let config = StreamableHttpClientTransportConfig::with_uri(url.as_str())
        .auth_header(key)
        .reinit_on_expired_session(false);
    let client = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("pp-server-mcp-stdio", env!("CARGO_PKG_VERSION")),
    );
    let refused = |reason: &str| {
        anyhow::anyhow!(
            "No Print Partner core accepted the connection at {url}: {reason}. \
             Start Print Partner and check {KEY_VARIABLE}; mcp-stdio never opens the database."
        )
    };
    let handshake = client.serve(StreamableHttpClientTransport::with_client(http, config));
    let upstream = match tokio::time::timeout(HANDSHAKE, handshake).await {
        Ok(Ok(upstream)) => upstream,
        Ok(Err(error)) => return Err(refused(&handshake_failure(&error))),
        Err(_) => {
            return Err(refused(&format!(
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

// Every failure from upstream is untrusted: a reverse proxy or a faulty
// server can repeat the request, and its key, in any encoding. So a failure
// reaches the client as its numeric code, or one of the core's refusal
// codes, and a fixed message. Results that are not errors pass unchanged.

/// The core's refusal codes, as `pp-api/src/mcp/write_result.rs` lists them.
const REFUSAL_CODES: &str = "unauthorized unavailable outcome_unknown internal blank_name \
    name_too_long blank_request invalid_idempotency_key name_taken orders_module_off \
    module_settings_invalid idempotency_record_invalid arguments_required not_found not_open \
    parts_required invalid_edit part_not_found edit_empty invalid_digest draft_changed \
    base_changed accepted_baseline_required edit_refused invalid_plan_id plan_not_found \
    no_planning_brief invalid_roles planning_brief_invalid invalid_role_assignment \
    invalid_source_role evidence_not_found multiple_structural_bases farm_module_off \
    invalid_filament_assignment substitution_unconfirmed catalog_filament_not_found \
    catalog_color_mismatch catalog_name_mismatch catalog_brand_mismatch spoolman_module_off \
    spoolman_unavailable invalid_inputs invalid_input invalid_evidence_kind invalid_url \
    source_not_found derived_evidence_not_found invalid_hours no_structural_base \
    unresolved_source_choices layer_conflict no_stls would_wipe idempotency_conflict";

/// A failed tool call: `isError` and the refusal code, when it is one.
fn tool_error(result: &CallToolResult) -> CallToolResult {
    let code = result
        .content
        .iter()
        .filter_map(|block| serde_json::from_str::<serde_json::Value>(&block.as_text()?.text).ok())
        .find_map(|text| {
            let code = text.get("code")?.as_str()?;
            REFUSAL_CODES.split(' ').find(|known| *known == code)
        });
    let text = serde_json::json!({ "error": "The Print Partner core refused the tool call.", "code": code });
    CallToolResult::error(vec![ContentBlock::text(text.to_string())])
}

/// A failed request: the JSON-RPC code with a message chosen by that code.
fn forwarded(error: ServiceError) -> ErrorData {
    let (code, reason) = match error {
        ServiceError::McpError(error) => (
            error.code,
            match error.code.0 {
                -32700 => "it could not parse the request".to_owned(),
                -32600 => "the request was not valid".to_owned(),
                -32601 => "it has no such method".to_owned(),
                -32602 => "the arguments were not valid".to_owned(),
                _ => format!("it answered JSON-RPC error {}", error.code.0),
            },
        ),
        ServiceError::TransportSend(error) => {
            (ErrorCode::INTERNAL_ERROR, transport_failure(&error))
        }
        _ => (
            ErrorCode::INTERNAL_ERROR,
            "the connection failed".to_owned(),
        ),
    };
    ErrorData::new(code, format!("Print Partner core: {reason}"), None)
}

/// Why the core did not accept the handshake, from a fixed set of messages.
/// Upstream text (a response body, a header, a JSON-RPC message, a parse
/// error quoting the body) can repeat the request and its bearer key, so
/// none of it is printed: only the error's kind and an HTTP status or
/// JSON-RPC code, which are numbers.
fn handshake_failure(error: &ClientInitializeError) -> String {
    match error {
        ClientInitializeError::TransportError { error, .. } => transport_failure(error),
        ClientInitializeError::JsonRpcError(error) => {
            format!(
                "it refused the MCP handshake (JSON-RPC error {})",
                error.code.0
            )
        }
        ClientInitializeError::ConnectionClosed(_) => {
            "it closed the connection during the MCP handshake".to_owned()
        }
        _ => "its answer was not a Print Partner MCP handshake".to_owned(),
    }
}

/// A failed HTTP exchange with the core, from a fixed set of messages.
fn transport_failure(error: &DynamicTransportError) -> String {
    let Some(error) = error
        .error
        .downcast_ref::<StreamableHttpError<reqwest::Error>>()
    else {
        return "the connection failed".to_owned();
    };
    let status = match error {
        StreamableHttpError::Client(error) if error.is_connect() => {
            return "it could not connect there".to_owned();
        }
        StreamableHttpError::Client(error) => error.status().map(|status| status.as_u16()),
        StreamableHttpError::AuthRequired(_) => Some(401),
        StreamableHttpError::InsufficientScope(_) => Some(403),
        StreamableHttpError::HttpStatus(error) => Some(error.status),
        StreamableHttpError::SessionExpired => {
            return "it no longer knows this MCP session; restart mcp-stdio".to_owned();
        }
        _ => None,
    };
    match status {
        Some(status @ (401 | 403)) => format!("it refused the API key (HTTP {status})"),
        Some(status) => format!("it answered HTTP {status}"),
        None => "its answer was not Print Partner MCP".to_owned(),
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
        match self.core.call_tool_once(request).await.map_err(forwarded)? {
            CallToolResponse::Complete(result) if result.is_error == Some(true) => {
                Ok(CallToolResponse::Complete(tool_error(&result)))
            }
            response => Ok(response),
        }
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

    /// A refusal code the core adds must be added here too.
    #[test]
    fn the_refusal_codes_are_the_cores() {
        let core: Vec<&str> = include_str!("../../pp-api/src/mcp/write_result.rs")
            .lines()
            .filter_map(|line| line.trim().strip_prefix("Self::")?.split_once(" => \""))
            .filter_map(|(_, code)| code.strip_suffix("\","))
            .collect();
        assert_eq!(core, REFUSAL_CODES.split(' ').collect::<Vec<_>>());
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
