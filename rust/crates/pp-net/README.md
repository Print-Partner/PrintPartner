# pp-net

Checked outbound HTTP to printers, and canonical parsing of inbound request paths. It has no `pp-*` dependencies.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns outbound target policy: resolve a host once, check every resolved address, and connect only to checked IPs. LAN and loopback printers are allowed; metadata, link-local, unspecified, multicast and reserved addresses are refused.
- Bounds every request with a deadline and a body limit, follows no redirects, uses no proxy, and sends each write once.
- Reports whether a failed write may have reached the printer, so callers reconcile instead of retrying.
- Canonicalizes origin-form request targets and path segments for the gateway (`target`).

## Not responsible for

- Printer protocols, auth schemes and upload bodies: `pp-drivers`.
- Deciding to send anything to a printer: an explicit user action, admitted outside `pp-net`.
- Listeners and inbound admission: `pp-gateway`.
- Other HTTP clients. `pp-core` runs its own `reqwest` fetches and reuses `permitted` and `Resolver` for their address checks. GitHub reads go through `pp-services::sync::GithubPort`, which has no workspace implementation yet.

## Interface

| Module | Main items |
| --- | --- |
| `outbound` | `HttpGuard::new(resolver, deadline, max_body)`; `get(url, api_key)`, `request(method, url, headers, body, deadline)` for one GET or PUT, `post(url, api_key, body, deadline)` for one POST. `HttpReply`, `PostBody`, `PostError` (`Unsent`, `Unknown`, `SourceFailed`). |
| `outbound` | `stream(url, headers, deadline, body)`: one GET whose body is read as it arrives under a `BodyDeadline` (`Total` or `Idle`) (`StreamReply`, `ReplyBody::chunk`, `ReplyBody::prefix`, `IdleTimeout`). `with_body_limit(max_body)` up to `MAX_LISTING_BODY` (8 MiB); `MAX_BODY` (2 MiB). |
| `outbound` | `permitted(ip)`: the address policy. `Resolver` trait and `SystemResolver`. `MAX_POST_DEADLINE` (120 s). |
| `target` | `CanonicalTarget::parse(uri)` and `decode_segment(segment)`. They refuse encoded `/`, `\` or `%`, control characters, `.` and `..` segments, and `//`. |

`HttpGuard::new` accepts a deadline up to 30 s and `max_body` up to 2 MiB; `with_body_limit` allows 8 MiB. URLs must be `http://` with no credentials. Users: `pp-drivers`, `pp-gateway`, and `pp-core` with `desktop_rebuild`. No binaries.

## Effects and flags

- Network: DNS through `SystemResolver` (`tokio::net::lookup_host`) and TCP connections to checked addresses.
- Tokio: each connection runs on a spawned task; connection attempts race over the checked addresses.
- No files, SQLite, env vars or child processes.

Cargo features: none.

## Tests

- `tests/canonical_target.rs`: `proptest` properties for `CanonicalTarget`.
- Unit tests in `src/target.rs` and `src/outbound.rs`. The `outbound` tests run local HTTP servers on loopback to check deadlines, body limits, redirect refusal and DNS policy.
- `tests/streamed_replies.rs`: streamed replies past the buffered limit, the idle and total body deadlines, `prefix`, and the 8 MiB listing limit, on loopback.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-net
```
