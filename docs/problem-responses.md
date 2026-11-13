# Problem responses

Every error from the replacement Rust API (`pp-api`, mounted behind the `desktop_rebuild` feature and `PP_DESKTOP_REBUILD=1|true`) is an RFC 9457 problem document served as `application/problem+json` with `Cache-Control: private, no-store`. The Node compatibility server is unchanged, and with the feature off `pp_api::problem` is not compiled and pp-api errors keep their legacy JSON bodies byte for byte. MCP is out of scope and keeps its own errors: JSON-RPC errors inside the protocol, and plain `{"detail": ...}` `application/json` bodies for its guard refusals on `/mcp` and `/api/v1/mcp` (401, 403, 503, 500; `src/mcp.rs`).

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
- `code` keeps Node's machine code where the route has one, cut to 64 bytes. [Codes](#codes) lists the ones in the closed list.
- `errors` lists field errors as `{ "pointer": "/decision/value", "detail": "is invalid" }`: at most 16 entries, pointers up to 256 bytes, details up to 128 bytes. A field error names the field but never echoes its value. An overlong pointer is shortened to its longest whole-token ancestor, so an RFC 6901 escape or a UTF-8 character is never split; a field error whose pointer would be the whole document is dropped. Draft edits name the failing member even though their wire shape is untagged.
- Routes may add extension members, such as a draft conflict's `workspace` or a manifest observation failure's `reason`. An extension never replaces a standard member. `reason` is sent only when it is a closed failure name such as `Io(NotFound)` (letters, digits, `_` and parentheses, at most 64 bytes).

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
| `overloaded` | 503 | An admission semaphore or the writer queue is full, or the auth writer is stopping. Retry later. |
| `timeout` | 408, 504 | The request body or an upstream call ran out of time. |
| `unavailable` | 503 | The service is stopping or cancelled the request. |
| `outcome-unknown` | 503 | A write may or may not have committed. Reload and reconcile; do not blindly retry. |
| `upstream` | 502 | An OAuth provider failed or answered badly. |
| `not-implemented` | 501 | |
| `internal` | 500 | |

Each URN is `urn:print-partner:problem:<suffix>`. `ProblemType::classify(status, code)` picks the type for the older families; every `ToProblem` impl and a few other call sites state it directly (unreadable JSON, full admission semaphores).

## Codes

`code` is a string on the wire. `ProblemCode` (`rust/crates/pp-api/src/problem/wire.rs`) is the closed list of the codes that have a typed producer in `pp-api` or that the replacement UI branches on. `web/packages/rust-client` exports it as `problemCodeSchema`. It is not every code a route can send: about 45 more come from string literals, storage text and serialized outcome kinds in the older families (drafts, Builds, catalog, accepted, checkoff, plates, filaments, auth, send queue). A client must accept a code outside the list, and no build panics on one. A family's codes join the list when a ticket rewrites that family.

| Codes | Sent by |
| --- | --- |
| `draft_changed`, `base_changed`, `header_changed`, `duplicate_build_name`, `duplicate_order`, `build_archived`, `draft_not_found`, `idempotency_conflict`, `outcome_unknown`, `already_applied`, `reconciliation_required`, `snapshot_not_restorable`, `snapshot_not_found`, `inputs_changed`, `printed_units_affected`, `accepted_baseline_required`, `required_unit_set_unavailable`, `invalid_header` | The draft and History routes. These are the 18 codes the replacement UI branches on. |
| `transaction_unavailable`, `outcome_unknown` | Every write through the write seam (`WriterFailure`): storage was busy past the deadline, or the write may or may not have committed. |
| `invalid_integration`, `integration_not_found`, `integration_limit`, `integration_not_editable`, `integrations_unreadable`, `invalid_printer`, `printer_not_found`, `printer_limit`, `printers_unreadable`, `filament_slot_in_use` | The integration and printer routes under `/api/v2/integrations` and `/api/v2/printers`. |
| `source_not_found`, `source_snapshot_unavailable`, `source_too_many_files` | `GET /sources/{id}/files`. |

MCP keeps its own code list, in [MCP](mcp.md). A family served both ways has two mappings from the same typed error, one per surface, both in `pp-api`.

## Map a new family's error

A new family implements `pp_api::problem::ToProblem` for its error enum and serves it with `problem::refused`:

```rust
impl ToProblem for SourceFilesError {
    fn to_problem(&self) -> Problem {
        match self {
            Self::NotFound => Problem::new(ProblemType::NotFound, 404, "Source not found")
                .with_problem_code(ProblemCode::SourceNotFound),
            // one arm per variant
            Self::Writer(failure) => failure.to_problem(),
        }
    }
}
```

- Each arm states its `ProblemType`, status and fixed detail, and sets its code with `Problem::with_problem_code`. A new code is a new `ProblemCode` variant, followed by a regenerated `web/packages/rust-client`; `with_code(&str)` is for the older families only.
- The family's `Writer(WriterFailure)` variant hands the failure to `WriterFailure::to_problem`. `WriterFailure` is sorted once, into the crate-private `WriterFailureKind`; HTTP and MCP (`mcp::write_result::writer_failure`) each answer its seven rows. A new `WriterFailure` variant fails to compile in that one match.
- An error whose text differs by caller is paired with that text: integrations and printers share `PrinterSetupError`, and `ToProblem` is implemented for `(&PrinterSetupError, &Wording)`.
- Add a test that pins the status and whole body of every variant.

## What never appears

Responses never carry SQL, filesystem paths, driver or provider URLs, or error chains. Callers pass fixed text or bounded user input to `pp_api::problem`, never `error.to_string()` of a storage, filesystem or network error. `problem_response_canary_never_leaks_sql_paths_or_urls` in `rust/crates/pp-api/src/problem/tests/canary.rs` sends every typed failure each router maps, plus real SQLite, IO and reqwest errors, all carrying a canary string in their SQL, path or URL, and checks that no body or header contains it.

## Code

`ProblemType`, `ProblemCode`, `Problem` and the bounds live in `rust/crates/pp-api/src/problem/wire.rs`. `rust/crates/pp-api/src/problem.rs` is the one builder, plus the `layer` backstop every pp-api router mounts with `route_layer` and `parse_body` for field errors. `ToProblem` and `refused` turn a family's typed error into a response ([Map a new family's error](#map-a-new-familys-error)). `writer_failure` maps `pp_storage::WriterFailure`, the failures every write through the write seam shares, so each storage family maps only its own error variants. `web/packages/rust-client` exports the generated `problemSchema`, `Problem`, `ProblemType` and `ProblemCode`. Route misses stay with the gateway.

Parity tests compare Rust problem bodies with Node bodies through `pp_api::problem::without_envelope`, which drops `type`, `title` and `status` only when they are consistent.

Run the checks with the `test(problem_response)` nextest filter and `desktop_rebuild` on.
