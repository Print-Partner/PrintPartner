# pp-services

Operation policy ported from the Node server: GitHub Source sync decisions, slicer discovery and profile parsing, MCP dispatch, and the kit bundle format. It has no `pp-*` dependencies; callers inject GitHub reads and unported domain operations through traits.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Decides which refs, commits and files a GitHub sync uses. Every GitHub read goes through the `GithubPort` trait.
- Finds Orca Slicer and Bambu Studio profile directories and native macOS app bundles, and parses their profiles into the records Node's profile sync produced.
- Dispatches MCP tools, prompts and proposals for one session. Mutating tools only propose; `confirm_apply` settles a proposal.
- Reads and writes the `print-partner-kit` bundle format (`desktop_rebuild` only).

## Not responsible for

- SQL, Source rows, sync and job state: `pp-storage`. Nothing here writes SQL.
- GitHub transport, auth, pagination and checked outbound network: a caller-supplied `GithubPort` implementation. None exists in the workspace yet.
- Staging Source bytes and snapshots: `pp-source`.
- Watching profile directories, launching slicers, and running workers: `pp-core` and the shell.
- HTTP and MCP routing, authentication: `pp-api`.
- Printer protocols: `pp-drivers`. There is no `pp-services -> pp-storage/pp-net/pp-drivers` edge.

## Interface

| Module | Main items |
| --- | --- |
| `sync` | `GithubPort`, `plan_github_sync`, `list_branches`, `list_tags`, `resolve_ref_name`, `check_source_update`, `remote_update_status`, `update_event_due`. Submodules `github_url` (`parse_github_url`), `plan` (`plan_snapshot`, `SyncLimits`, `SnapshotPlan`) and `repos_txt` (`parse_repos_txt_text`), re-exported from `sync`. |
| `discovery` | `SlicerKind`, `DiscoveryRoots`, `discover`, `discover_kind`, `discover_all`. The caller supplies the home dir and config root. |
| `native_slicers` | `NativeRoots`, `discover_native_kind`, `discover_macos`. Off macOS, `discover_native_kind` returns `Unsupported`; `NativeRoots::for_home` exists only on macOS. |
| `profile_sync` | `parse_slicer_json`, `parse_slicer_ini`, `parse_profile_file`, `resolve_inherited`, `read_profile`, `scan_root`, `locate`, `WatchRoot`, `FlatConfig`. Matches Node's JS number and property-order output. |
| `mcp_domain` | `McpSession<P: DomainPort>` (`list_tools`, `call_tool`, `list_prompts`, `get_prompt`, `pending_ids`), `DomainPort`, `UnportedDomain`, `server_info`, `list_custom_filaments`. The catalog is `src/mcp_domain/catalog.json`. |
| `kit` (`desktop_rebuild`) | `parse_kit_bundle`, `build_editable_kit_bundle`, `kit_json_text`, `kit_bundle_zip`, `source_refs::collect_kit_bundle_source_refs`. |

`pp-api` uses `mcp_domain` behind its own `desktop_rebuild` feature. No binaries.

## Effects and flags

- Reads files: `discovery`, `native_slicers` (bundle `Info.plist`, executables) and `profile_sync` read paths the caller passes. Nothing reads env vars or spawns processes.
- Writes files: `McpSession` reads and writes `<data_dir>/custom_filaments.json` when a `propose_add_custom_filament` proposal is applied and a data dir was given.
- `sync` functions are `async` but use no runtime; network goes through the injected `GithubPort`.
- No SQLite, sockets or threads.

Cargo features:

- `desktop_rebuild` (default off): compiles `kit` and adds `zip`, `ryu-js` and `crc32fast`.

## Tests

- `tests/github_sync_golden.rs`: replays `fixtures/parity/github-sync/node.json` (`generate.mjs` beside it).
- `tests/profile_sync_golden.rs`: replays `fixtures/parity/profile-sync`.
- `tests/native_discovery.rs`: profile and bundle discovery on `tests/fixtures/discovery/{present,absent,unsupported}`; most cases are Unix-only.
- `tests/mcp_dispatch_parity.rs`: replays `fixtures/parity/mcp-proposals/*.json` and diffs `tools/list` against `web/apps/server/fixtures/mcp/tools-inventory.json`.
- `tests/kit_bundle_golden.rs` (`desktop_rebuild`): replays `fixtures/parity/kit-bundle`. Regenerate with `node fixtures/parity/kit-bundle/generate.mjs`.
- Unit tests in `src/discovery.rs`, `src/profile_sync.rs`, `src/sync/github_url.rs` and `src/kit/js.rs`.

Fixture paths under `fixtures/` are at the repo root. No `pp-testkit` dev-dependency.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-services
cargo nextest run --manifest-path rust/Cargo.toml -p pp-services --features desktop_rebuild
```
