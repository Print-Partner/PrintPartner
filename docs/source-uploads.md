# Source uploads

`POST /sources/{id}/upload-files` uploads loose files into an existing Source on the Rust-only runtime (GRE-428 PR 1). The route and its wiring compile only with the default-off `desktop_rebuild` feature and mount only when `PP_DESKTOP_REBUILD` is on. ZIP upload is GRE-1444. Lifting the fixed size limits is GRE-1445.

## Route

The route is mounted at `/` and `/api/v1` from `pp_api::catalog::admit_upload_routes`, behind the catalog guard: loopback, `Host`, `Origin` or an API key, and a session. Each multipart file part's file name is its path inside the Source. Form fields without a file name are ignored.

The route checks in this order:

1. The Source, before any byte is read. Unknown: 404.
2. `Content-Length` against the free-space reserve of 5 GiB (`pp_core::source_uploads::FREE_SPACE_RESERVE`). Missing: 411. Too little space would be left: 507.
3. The import slot. There is one slot, matching the worker admission's one Source import at a time and one job per Source. A second upload while one runs is 409, before any byte is read.
4. The parts stream into the retained capture vault. The route answers `202 {"job_id"}` once the import is recorded.

Other refusals:

| Status | Cause |
| --- | --- |
| 400 | No file part, an unsafe or repeated path, or a broken body. |
| 413 | Input over the journal's 256 MiB limit. |
| 503 | Imports closed. |

Each refusal is an RFC 9457 problem. It creates no job and leaves the Source unchanged. A body cut off mid-file is discarded, never stored short: each part ends only on an explicit end marker. Each upload is a new import, so a retry after a lost 202 imports again; identical files keep the revision.

The import job, kind `supplied-source-import`, is served by `GET /jobs/{id}`, the job list and `/ws/jobs/{id}`. A failed import's `error` gives the reason in words.

## Runtime

`start_rust_resources` builds one `SourceAcquisitions` before the gateway serves. It runs `recover_before_accepting` to discard half-written candidates and settle retained captures, then `resume_imports` to settle imports that a stopped process left as owned input. A capture that still needs repair keeps uploads closed (503); the server still starts.

The send worker and the import worker share the owner's one worker admission. Each claims only its own kind.

`stop_rust` drains the imports within the gateway's drain bound. An upload still streaming is discarded, and an admitted import resumes at the next start. If an import worker is still running when the bound runs out, the runtime keeps storage ownership until the process exits, as it does for tracked work. It logs this and records `source_imports_not_drained` and `storage_ownership_retained` in the shutdown receipt.

## Checks

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core -p pp-storage --features pp-core/desktop_rebuild,pp-storage/desktop_rebuild -E 'binary(source_upload_http) | binary(source_acquisition_recovery) | binary(import_worker_policy)'
```
