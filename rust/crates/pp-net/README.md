# pp-net

Checked outbound HTTP to printers, the checked `reqwest` setup for other outbound HTTPS, and canonical parsing of inbound request paths and literal-IP HTTP authorities. It has no `pp-*` dependencies.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns outbound target policy: resolve a host once, check every resolved address, and connect only to checked IPs. LAN and loopback printers are allowed; metadata, link-local, unspecified, multicast and reserved addresses are refused.
- Bounds every request with a deadline and a body limit, follows no redirects, uses no proxy, and sends each write once.
- Reports whether a failed write may have reached the printer, so callers reconcile instead of retrying.
- Owns the checked `reqwest` client setup (`network`): DNS checked on every connection, including each redirect hop; rustls with the aws-lc-rs provider named in the code, never the process default; certificates checked against the URL's host; no proxy and no retries.
- Canonicalizes origin-form request targets and path segments for the gateway (`target`).
- Serves one inbound HTTP/1 connection and closes it without losing the last response (`http1`). After hyper is done with the connection, it shuts down the write side and reads and discards what the client still sends, for up to two seconds, before it closes (RFC 9112, section 9.6). Without that, a 413 sent before the body was read is lost to a reset on Windows (os error 10053).

## Not responsible for

- Printer protocols, auth schemes and upload bodies: `pp-drivers`.
- Deciding to send anything to a printer: an explicit user action, admitted outside `pp-net`.
- Listeners, sessions and inbound admission decisions: `pp-gateway`.
- What other clients send. `pp-core` builds its GitHub and filament provider clients with `network::checked_client` and adds its own redirect rule, deadlines and body caps. `pp-api`'s OAuth, update-check and SMTP clients do not use `pp-net`; GRE-1783 covers them.

## Interface

| Module | Main items |
| --- | --- |
| `outbound` | `HttpGuard::new(resolver, deadline, max_body)`; `get(url, api_key)`, `request(method, url, headers, body, deadline)` for one GET or PUT, `post(url, api_key, body, deadline)` for one POST. `HttpReply`, `PostBody`, `PostError` (`Unsent`, `Unknown`, `SourceFailed`). |
| `outbound` | `stream(url, headers, deadline, body)`: one GET whose body is read as it arrives under a `BodyDeadline` (`Total` or `Idle`) (`StreamReply`, `ReplyBody::chunk`, `ReplyBody::prefix`, `IdleTimeout`). `with_body_limit(max_body)` up to `MAX_LISTING_BODY` (8 MiB); `MAX_BODY` (2 MiB). |
| `outbound` | `permitted(ip)`: the address policy. `Resolver` trait and `SystemResolver`. `MAX_POST_DEADLINE` (120 s). |
| `network` | `checked_client(dns)` and `checked_client_trusting(dns, extra_roots)` return a `reqwest::ClientBuilder`. `CheckedDns::new(resolver, policy, refusal)`; `Policy::Lan` (what `permitted` allows) and `Policy::Public` (also refuses loopback, private and unique-local addresses). `tls_config(extra_roots)`, `crypto_provider()`, `MAX_ADDRESSES` (32). reqwest skips DNS for an IP-literal host, so callers check one with `Policy::allows`. |
| `inbound` | `origin(request)` validates a literal-IP Host, matching browser Origin, and the absence of forwarded authority or cross-site requests. `authority(request)` applies the same checks except that an unsafe request may omit Origin; the gateway uses it only for MCP, whose key guard decides. Both return the validated origin and Host without changing the peer identity or authenticating the caller. |
| `http1` | `serve(builder, io, service)` serves with upgrades, then lingers for up to `LINGER` (2 s) unless an upgrade took the connection. Dropping its future closes at once. Used by the gateway's listener. `pp-api`'s `auth_admission_parity` harness compiles `src/http1.rs` through `#[path]`, because `pp-api` may not depend on `pp-net`. |
| `target` | `CanonicalTarget::parse(uri)` and `decode_segment(segment)`. They refuse encoded `/`, `\` or `%`, control characters, `.` and `..` segments, and `//`. |

`HttpGuard::new` accepts a deadline up to 30 s and `max_body` up to 2 MiB; `with_body_limit` allows 8 MiB. URLs must be `http://` with no credentials. Users: `pp-drivers`, `pp-gateway`, and `pp-core` with `desktop_rebuild`. No binaries.

## Effects and flags

- Network: DNS through `SystemResolver` (`tokio::net::lookup_host`) and TCP connections to checked addresses. `network` clients also read the platform's root certificates.
- Tokio: each connection runs on a spawned task; connection attempts race over the checked addresses.
- No files, SQLite, env vars or child processes.

Cargo features: none.

## Tests

- `tests/canonical_target.rs`: `proptest` properties for `CanonicalTarget`.
- `tests/http1_linger.rs`: GRE-1264. An early 413 reaches a client that sends a 32 MiB body, a client that keeps sending is closed after the linger, and an upgraded connection skips it.
- Unit tests in `src/target.rs` and `src/outbound.rs`. The `outbound` tests run local HTTP servers on loopback to check deadlines, body limits, redirect refusal and DNS policy.
- `tests/ticket_t_20_network.rs`: GRE-455. Fake DNS with metadata, mixed, empty and oversized answers; `Lan` and `Public` classification; a redirect hop to a refused name; a local TLS server checked against the URL's host; aws-lc-rs used even when another process default is installed; raw sockets through `HttpGuard::connect`; `HTTP_PROXY` and friends ignored; a stalled handshake released at the deadline and on drop.
- `tests/streamed_replies.rs`: streamed replies past the buffered limit, the idle and total body deadlines, `prefix`, and the 8 MiB listing limit, on loopback.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-net
```
