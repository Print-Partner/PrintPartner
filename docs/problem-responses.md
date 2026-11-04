# Problem responses

Every error from the replacement Rust API (`pp-api`, mounted behind the `desktop_rebuild` feature and `PP_DESKTOP_REBUILD=1|true`) is an RFC 9457 problem document served as `application/problem+json` with `Cache-Control: private, no-store`. The Node compatibility server and the feature-off build are unchanged. MCP keeps JSON-RPC errors.

```json
{
  "type": "urn:print-partner:problem:precondition-failed",
  "title": "Precondition Failed",
  "status": 409,
  "code": "draft_changed",
  "workspace": { "...": "route extension member" }
}
```

- `type` is one of the stable URNs below. Branch on it.
- `title` is fixed per type.
- `detail` keeps the text the Node API sent, cut to 512 bytes. It is absent where Node sent none: draft conflicts and the draft 422.
- `code` keeps Node's machine code where the route has one, cut to 64 bytes.
- `errors` lists field errors as `{ "pointer": "/decision/value", "detail": "is invalid" }`: at most 16 entries, pointers up to 256 bytes, details up to 128 bytes. A field error names the field but never echoes its value.
- Routes may add extension members, such as a draft conflict's `workspace` or a manifest observation failure's `reason`. An extension never replaces a standard member.

## Types

| `type` suffix | Typical status | Meaning |
| --- | --- | --- |
| `malformed-request` | 400, 413, 414, 415 | The request could not be read: unparsable JSON, wrong media type, oversized body or target, bad path or query. axum extractor rejections land here. |
| `validation` | 400, 422 | The request was readable but a value is invalid. |
| `unauthenticated` | 401 | No valid session or API key. |
| `forbidden` | 403 | Known caller without permission, or a refused Host, Origin or transport. |
| `not-found` | 404 | |
| `method-not-allowed` | 405 | The `Allow` header is kept. |
| `conflict` | 409 | The target's state does not allow the operation. |
| `precondition-failed` | 409, 412 | The version or digest the request expected is no longer current (`draft_changed`, `base_changed`). |
| `overloaded` | 503 | An admission semaphore or the writer queue is full. Retry later. |
| `timeout` | 408, 504 | The request body or an upstream call ran out of time. |
| `unavailable` | 503 | The service is stopping or cancelled the request. |
| `outcome-unknown` | 503 | A write may or may not have committed. Reload and reconcile; do not blindly retry. |
| `upstream` | 502 | An OAuth provider failed or answered badly. |
| `not-implemented` | 501 | |
| `internal` | 500 | |

Each URN is `urn:print-partner:problem:<suffix>`. `ProblemType::classify(status, code)` picks the type; a few call sites state it directly (unreadable JSON, full admission semaphores).

## What never appears

Responses never carry SQL, filesystem paths, driver or provider URLs, or error chains. Callers pass fixed text or bounded user input to `pp_api::problem`, never `error.to_string()` of a storage, filesystem or network error. `problem_response_canary_never_leaks_sql_paths_or_urls` in `rust/crates/pp-api/src/problem/tests/canary.rs` sends every typed failure each router maps, plus real SQLite, IO and reqwest errors, all carrying a canary string in their SQL, path or URL, and checks that no body or header contains it.

## Code

`ProblemType`, `Problem` and the bounds live in `rust/crates/pp-api/src/problem/wire.rs`. `rust/crates/pp-api/src/problem.rs` is the one builder, plus the `layer` backstop every pp-api router mounts with `route_layer` and `parse_body` for field errors. `web/packages/rust-client` exports the generated `problemSchema`, `Problem` and `ProblemType`. Route misses stay with the gateway.

Parity tests compare Rust problem bodies with Node bodies through `pp_api::problem::without_envelope`, which drops `type`, `title` and `status` only when they are consistent.

Run the checks with the `test(problem_response)` nextest filter and `desktop_rebuild` on.
