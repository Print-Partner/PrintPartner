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

`scripts/evidence/run.sh --step mcp-receipt:<build> --shot /builds` proves this on synthetic data. It connects the official MCP TypeScript SDK to a Rust-only core on both paths with a fresh API key, checks that missing, wrong and revoked keys get `401`, and runs `add_addon` and `propose_update_build_brief` through `confirm_apply`, checking the Build's stored state after each call.

Rust MCP has its own budget of 1000 requests a minute, the limit of Node's application bucket. Other Rust routes do not count against it. The window starts at the first request. Every admitted request counts, before its key is checked, so requests with a missing or wrong key use the budget too. A request refused at admission gets `403` and does not count. Admission refuses an `Origin` other than the listener's and a `Sec-Fetch-Site: cross-site` request, for every method. It also refuses any method other than GET, HEAD and OPTIONS (for MCP, POST and DELETE) that has neither the listener's `Origin` nor an API key. A GET, HEAD or OPTIONS without `Origin` is admitted, counts, and gets `401` when it has no valid key; that includes a keyless GET that opens the SSE stream. Past the limit every admitted request, including one with a valid key, gets `429` with Node's body (for example `Rate limit exceeded, retry in 12 seconds`) and a `retry-after` header until the window resets. Every counted response carries `x-ratelimit-limit`, `x-ratelimit-remaining` and `x-ratelimit-reset`.

A tool call that the client cancels with `notifications/cancelled`, or stops reading, still runs to the end; the client just gets no reply. A cancelled `confirm_apply` therefore either applies or, when Accept refuses or times out, leaves the proposal pending. If the connection dropped but the MCP session is still open, list pending actions in that same session; a new session cannot see them. Confirming the same action again never applies it twice: its action id is Accept's idempotency key.

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

## Stdio for an offline data copy

The server package also contains a stdio transport. Use it only when Print Partner is stopped or when the client points to a copy of the data directory. Two processes must not write to the same live SQLite database.

```bash
cd web
export PRINT_PARTNER_DATA_DIR=/path/to/data-copy
export PRINT_PARTNER_MCP_PLAN_ID=1
npm run mcp -w @print-partner/server
```

Example stdio configuration:

```json
{
  "mcpServers": {
    "print-partner-offline": {
      "command": "npm",
      "args": ["run", "mcp", "-w", "@print-partner/server"],
      "cwd": "/path/to/PrintPartner/web",
      "env": {
        "PRINT_PARTNER_DATA_DIR": "/path/to/data-copy",
        "DEPLOY_MODE": "self-host"
      }
    }
  }
}
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
| `list_pending_actions` | List proposals in the current MCP session. |
| `confirm_apply` | Apply one pending proposal. |
| `dismiss_proposed_action` | Discard one pending proposal. |

Pending proposals belong to the MCP session that created them. A reconnect may start a new session.

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
