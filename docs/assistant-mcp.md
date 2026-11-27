# Connect an MCP client

Print Partner exposes its source, Build, Plan, and Checkoff operations over streamable HTTP MCP. Read tools return current product state. Write tools create a pending proposal that must be accepted with `confirm_apply`.

## Before you connect

Set an API key on the Print Partner host:

```yaml
services:
  print-partner:
    environment:
      PRINT_PARTNER_API_KEY: replace-with-a-long-random-value
```

Restart the container:

```bash
docker compose up -d
```

Use one of these endpoints:

| Connection | URL |
|------------|-----|
| On the Docker host | `http://127.0.0.1:8080/api/v1/mcp` |
| Remote through HTTPS | `https://<your-host>/api/v1/mcp` |

Every MCP request needs a valid API key, from the desktop app, another process on the same computer, or a LAN peer. A loopback bind no longer skips the key. A request with no key gets `503` when the host has no deployment key, and `401 Valid API key required` when it has one. An invalid or revoked key gets `401`. Use HTTPS or an authenticated tunnel when the client is on another machine.

The desktop rebuild (`PP_DESKTOP_REBUILD=1`) serves `/mcp` and `/api/v1/mcp` from Rust through rmcp on its loopback listener. It accepts an API key only. The gateway passes these two paths to Rust without the desktop session cookie that every other path needs, so an assistant connects with its key alone and sends no cookie or `Origin`. The gateway still requires the listener's exact `Host`, and an `Origin` that is present must be the listener's. Neither cookie, the desktop one or the Rust session, counts as a key: a request with no key gets `401 Valid API key required`, except a POST or DELETE that also has no `Origin`, which gets `403`. A shutdown waits for an MCP tool call that is still running; only the GET notification stream ends when shutdown begins. LAN peers are not served there yet. With `PP_DESKTOP_REBUILD` off, nothing in the gateway serves MCP: with the listener's `Origin`, `/mcp` and `/api/v1/mcp` get `401` without the desktop cookie and `403` with it. The Rust-only Docker image serves the same key-only MCP on its explicit listener, LAN peers included; see [Connect an MCP client](docker.md#connect-an-mcp-client).

On Rust, an MCP session belongs to the API key that opened it. A POST, GET or DELETE that names the session with any other valid key, including the replacement of a rotated key, gets `404 Not Found: Session not found` on either MCP path. A missing, invalid, expired or revoked key is refused before the session is checked. A session opened on one path continues on the other with the same key.

After a key rotation or core restart, open a fresh session. Sessions do not survive a restart, but [proposals](mcp.md#decide-proposals) do. Any authenticated session for the installation can list, confirm or dismiss them without using the original session's ID.

`scripts/evidence/run.sh --step mcp-receipt:<build> --shot /builds` proves this on synthetic data. It connects the official MCP TypeScript SDK to a Rust-only core on both paths with a fresh API key, checks that missing, wrong and revoked keys get `401`, and runs `add_addon` and `propose_update_build_brief` through `confirm_apply`, checking the Build's stored state after each call.

Rust MCP has its own budget of 1000 requests a minute, the limit of Node's application bucket. Other Rust routes do not count against it. The window starts at the first request. Requests with a valid key share this budget. A request without a valid key (missing, wrong, revoked, or one the key check could not answer, `401` or `503`) counts instead against a bucket of its peer address with the same limit and its own window; IPv6 peers share one bucket per /64. So a LAN peer that floods keyless requests at the [explicit listener](docker.md) uses up only its own bucket, and the owner's key keeps working. Once a peer's bucket is spent, every request from that peer gets `429`, even one with a valid key, without a key check, until that peer's window resets. The server tracks up to 1024 peers. While all of them have live windows, a new peer's failures are not counted and it keeps getting `401`; its key checks are then bounded only by the 64 that can run at once. A request refused at admission gets `403` and does not count. Admission refuses an `Origin` other than the listener's and a `Sec-Fetch-Site: cross-site` request, for every method. It also refuses any method other than GET, HEAD and OPTIONS (for MCP, POST and DELETE) that has neither the listener's `Origin` nor an API key. A GET, HEAD or OPTIONS without `Origin` is admitted, counts against its peer's bucket, and gets `401` when it has no valid key; that includes a keyless GET that opens the SSE stream. Past the shared limit every request with a valid key gets `429` with Node's body (for example `Rate limit exceeded, retry in 12 seconds`) and a `retry-after` header until the window resets. Every counted response carries `x-ratelimit-limit`, `x-ratelimit-remaining` and `x-ratelimit-reset` for the bucket it counted against.

Cancelling `confirm_apply` with `notifications/cancelled` or disconnecting does not stop a decision already in progress. Reconnect and list proposals to see whether the action is still pending, applying or unknown. Confirming the same action again never applies a settled or uncertain decision twice.

## Choose the credential

On open self-host, or with `SINGLE_USER_AUTH=1`, connect to `/api/v1/mcp` with `PRINT_PARTNER_API_KEY` or with a Settings API key. `SINGLE_USER_AUTH=1` accepts either key on that MCP route without a browser session. Other API routes still require the administrator session.

With `MULTI_USER=1` on self-host, sign in and send the session cookie. A bearer key on its own gets `401` and `{"detail":"Authentication required"}`. The administrator creates Settings keys while signed in. The key matches only that tenant. Hosted planning disables Settings API keys and HTTP MCP on `/api/v1/mcp`; those MCP requests get `403` and `This feature is off on the hosted planning site.`.

## Generic client configuration

Clients that support streamable HTTP MCP can use:

```json
{
  "mcpServers": {
    "print-partner": {
      "url": "https://print-partner.example/api/v1/mcp",
      "headers": {
        "Authorization": "Bearer YOUR_API_KEY"
      }
    }
  }
}
```

Replace the URL and key with values for your host. Keep the key out of source control.

## Move the production connector to apps VM 111

Print Partner production runs on apps VM 111 at `192.168.200.81:8080`. The server-side MCP route is `/api/v1/mcp`. The old `.80:8080` address is a temporary forwarder, not the production target. Mac Docker is not the live server.

1. Open the saved `print-partner` connector in the client that owns it.
2. Retarget its protected connection to apps VM 111, preserving `/api/v1/mcp`. Terminate the HTTPS proxy or authenticated tunnel on VM 111 and forward to `127.0.0.1:8080`. If the proxy runs on another host, encrypt and authenticate its upstream connection to VM 111 with TLS or a tunnel. Do not forward credentials from a remote proxy to `http://192.168.200.81:8080`.
3. Save the connector URL for that protected connection. Keep its existing API key unchanged. Do not send the key over raw LAN HTTP.
4. Reconnect the client and run the read-only `list_sources` tool. Confirm a successful tool result, including an empty list if no sources exist.
5. Record the target host, tool name, and outcome without the API key, authorization headers, or source data.

For the Cursor plugin, set `PRINT_PARTNER_MCP_URL` to that protected connection's full MCP URL. Keep `PRINT_PARTNER_API_KEY` unchanged. No production HTTPS hostname or tunnel configuration is established by this guide.

A successful `/health` request proves reachability only. A `401 Valid API key required` response from MCP does not verify the saved connector. If the owning client's registration or existing credential is unavailable, record that blocker and leave the connector migration open. Do not create a replacement key to complete this check.

## Cursor plugin

The repository includes a plugin in [`cursor-plugin/print-partner`](../cursor-plugin/print-partner).

Configure these plugin variables:

| Variable | Value |
|----------|-------|
| `PRINT_PARTNER_MCP_URL` | The full `/api/v1/mcp` URL |
| `PRINT_PARTNER_API_KEY` | The key configured on the host |

Enable the `print-partner` MCP server after setting both values.

## Stdio bridge to the running core

For a client that only launches stdio MCP servers, `pp-server mcp-stdio` serves MCP on stdin and stdout and forwards each `tools/list` and `tools/call` to the HTTP MCP of the Print Partner core that is already running. It sends the API key with every request, so the core checks the key and applies its rate limit exactly as it does for an HTTP client.

The bridge takes no data directory and never opens the database: the running core owns it. It connects to the core before it reads stdin. When no core answers at the URL within 10 seconds, or the core refuses the key, it writes `mcp-stdio: No Print Partner core accepted the connection at <url>: <reason>` to stderr and exits 1 without answering. A name lookup that is still running does not hold that exit. Start Print Partner first.

The reason is one of a fixed set, such as `it could not connect there`, `it refused the API key (HTTP 401)` or `it answered HTTP 500`. The bridge never prints the core's response body, headers or error text, because a reverse proxy or a misbehaving server can repeat the request and its key there. For the same reason, after startup every failure from upstream reaches the client without upstream text. A JSON-RPC error keeps its numeric code and gets a fixed message chosen by that code. A tool result with `isError` keeps `isError` and becomes `{"error": "The Print Partner core refused the tool call.", "code": ...}`, where `code` is the core's refusal code when it is one of the refusal codes in [MCP tools](mcp.md), and `null` otherwise. The core's `error` and `detail` text does not reach a stdio client; an assistant that needs it can use the HTTP MCP directly. Results that are not errors pass unchanged. The bridge writes no log.

| Input | Value |
|-------|-------|
| `--url` | The core's MCP URL, for example `http://127.0.0.1:8080/api/v1/mcp`. A URL with no path uses `/api/v1/mcp`. Plain `http` is accepted only for a loopback host (`127.0.0.0/8`, `::1`, `localhost`). The name `localhost` is connected to `127.0.0.1` and `::1` only, not to whatever the system resolver returns, and the request still uses that name as its Host. The core checks the Host header against its own address, so use the numeric address the core prints, such as `http://127.0.0.1:8080`. Plain `http` connects directly, ignoring `HTTP_PROXY` and `ALL_PROXY`, so the key never leaves the machine in the clear. Any other host needs `https`, which may go through a proxy. The bridge never follows a redirect, so every request goes to this URL and nowhere else. A URL with a user, password, query or fragment is refused. |
| `PRINT_PARTNER_API_KEY` | The API key. It is read from the environment only, never from the arguments, so it stays out of process listings. Unset or empty exits 1. |

All requests from one bridge process share one MCP session on the core. The bridge never opens a second session: if the core restarts, each later request gets a JSON-RPC error until the client restarts the bridge. The new bridge session can list proposals left by the previous session. Closing stdin ends the bridge and its session; the core still finishes a decision already in progress. The bridge forwards tools only and reports the core's server name and instructions.

Example stdio configuration for the Rust-only Docker image, which ships `pp-server`:

```json
{
  "mcpServers": {
    "print-partner": {
      "command": "docker",
      "args": ["exec", "-i", "-e", "PRINT_PARTNER_API_KEY", "print-partner",
               "pp-server", "mcp-stdio", "--url", "http://127.0.0.1:8080/api/v1/mcp"],
      "env": { "PRINT_PARTNER_API_KEY": "YOUR_API_KEY" }
    }
  }
}
```

`docker exec -e PRINT_PARTNER_API_KEY` without a value passes the variable from the client's environment, so the key is not in the arguments. Inside the container the listener answers on `127.0.0.1:8080`. On the same machine as a running `pp-server`, use `pp-server` as the command and its origin as the URL.

`rust/crates/pp-server/tests/mcp_stdio.rs` proves this against a real Rust-only `pp-server`, on its saved loopback port and on an explicit `0.0.0.0` listener as in Docker: `initialize`, `tools/list`, `propose_create_build`, `list_pending_actions`, `confirm_apply` and `list_plans` through the bridge on `/mcp` and `/api/v1/mcp`. It also checks the bridge exits 1 with no core running, after the core stops (with the data directory left unchanged), with an unknown key, with no key, and within the 10-second deadline when a listener accepts but never answers. With `HTTP_PROXY` and `ALL_PROXY` set to a listening proxy, nothing reaches the proxy. After a core restart, a request gets an error instead of opening a new session.

### Node stdio server for an offline data copy

The Node server package still contains its own stdio transport, which opens a data directory itself. Use it only with a stopped Node installation or a copy of its data directory, never a live one. It refuses a Rust-owned data directory ([Storage](storage.md)).

```bash
cd web
export PRINT_PARTNER_DATA_DIR=/path/to/data-copy
export PRINT_PARTNER_MCP_PLAN_ID=1
npm run mcp -w @print-partner/server
```

`PRINT_PARTNER_MCP_PLAN_ID` is optional. It selects a default Build for tools that accept a Plan id.

## Tool behavior

Common tools include:

| Tool | Purpose |
|------|---------|
| `list_sources` | List synced Library sources. |
| `list_plans` | List Builds. |
| `get_plan_snapshot` | Read layers and kit selections. |
| `get_plan_review` | Read quantities and warnings. |
| `get_remaining` | Read printed and remaining units. |
| `duplicate_plan` | Propose a Build copy. |
| `archive_plan` | Propose archiving a finished Build. |
| `list_pending_actions` | List the installation's open proposals in Rust. |
| `confirm_apply` | Apply one pending proposal. |
| `dismiss_proposed_action` | Discard one pending proposal. |

In Rust, proposals belong to the installation, not the MCP session. Another authenticated MCP session or the replacement UI's **Apply** panel can list, apply, or dismiss them. An MCP session itself still belongs to the API key that opened it.

Print Partner does not expose a tool that starts a print without a separate product confirmation flow.

## Test the endpoint

Check the application first:

```bash
curl http://127.0.0.1:8080/health
```

If a remote MCP connection fails, verify:

1. the URL ends with `/api/v1/mcp`
2. the header uses the same key configured on the host
3. the reverse proxy allows `POST`, `GET`, and `DELETE`
4. the proxy does not buffer or time out streaming responses
5. the host certificate is valid for the client
