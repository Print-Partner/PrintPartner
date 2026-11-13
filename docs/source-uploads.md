# Source uploads

`POST /sources/{id}/upload-files` uploads loose files into an existing Source on the Rust-only runtime (GRE-428 PR 1). `POST /sources/{id}/upload-zip` uploads one ZIP that the import unpacks (GRE-1444). The routes and their wiring compile only with the default-off `desktop_rebuild` feature and mount only when `PP_DESKTOP_REBUILD` is on. Lifting the fixed size limits is GRE-1445.

## Routes

Both routes are mounted at `/` and `/api/v1` from `pp_api::catalog::admit_upload_routes`, behind the catalog guard: loopback, `Host`, `Origin` or an API key, and a session. For `upload-files`, each multipart file part's file name is its path inside the Source. `upload-zip` takes exactly one file part, the ZIP. Form fields without a file name are ignored.

Each route checks in this order:

1. The Source, before any byte is read. Unknown: 404.
2. `Content-Length` against the free-space reserve of 5 GiB (`pp_core::source_uploads::FREE_SPACE_RESERVE`). Missing: 411. Too little space would be left: 507.
3. The import slot. There is one slot, matching the worker admission's one Source import at a time and one job per Source. A second upload while one runs is 409, before anything is stored. Part of its body may be read first.
4. The parts stream into the retained capture vault. The route answers `202 {"job_id"}` once the import is recorded.

Other refusals:

| Status | Cause |
| --- | --- |
| 400 | No file part, an unsafe or repeated path, a broken body, or a second part sent to `upload-zip`. |
| 401 or 403 | Storage refused the credential when admitting the import, answered as on every storage write (`pp_api::problem::writer_failure`). |
| 409 | Another import holds the one slot. Retry when it finishes. |
| 413 | Input over the journal's 256 MiB limit. |
| 500 | A storage fault or another unexpected failure. The cause is logged, never sent. |
| 503 | Imports closed (startup recovery left a capture to repair, or shutdown has begun), or the storage writer is busy or stopped. Retry later. |

Each refusal is an RFC 9457 problem. It creates no job and leaves the Source unchanged. A body cut off mid-file is discarded, never stored short: each part ends only on an explicit end marker. Each upload is a new import, so a retry after a lost 202 imports again; identical files keep the revision.

The import job, kind `supplied-source-import`, is served by `GET /jobs/{id}`, the job list and `/ws/jobs/{id}`. A failed import's `error` gives the reason in words. Files of a type the import doesn't parse stay in the revision as kind `artifact`.

## ZIP uploads

The import worker unpacks the stored ZIP with `extract_zip_with_label_policy` before `prepare_media`, in the order `pp-source-media` uses. The revision holds the ZIP's files at their folder paths, the STLs made from any 3MF, and every other file as an artifact. The ZIP itself is not kept.

The ZIP limits are `pp_source::archive::ArchiveLimits::default()`. GRE-1445 owns lifting them. A ZIP that breaks a rule is stored, and its job ends `error` with the rule in words. The Source keeps its previous revision.

| Rule | Job `error` |
| --- | --- |
| An entry path that leaves its folder (`../`), is absolute, or has control characters | A ZIP entry has a path that is not allowed, such as one that leaves its folder (../) |
| More than 10,000 entries | The ZIP holds more than 10,000 entries |
| More than 1 GiB unpacked | The ZIP unpacks to more than 1 GiB |
| More than 256 MiB compressed | The ZIP is larger than 256 MiB, or its file list is too long |
| An entry compressed beyond the ratio limit | A ZIP entry is compressed more than the import allows |
| Encryption, multiple disks or another unsupported feature | The ZIP uses a feature the import does not support, such as encryption |
| Not a ZIP, or a damaged one | The file is not a readable ZIP |

An upload can't be cancelled yet. The job routes are read-only: no route cancels a job, so a POST or DELETE on a job answers 404 or 405 and changes nothing. Cancelling an import needs a cleanup claim that runs while the server is up, and that claim doesn't exist yet.

## Runtime

`start_rust_resources` builds one `SourceAcquisitions` before the gateway serves. It runs `recover_before_accepting` to discard half-written candidates and settle retained captures, then `resume_imports` to settle imports that a stopped process left as owned input. A capture that still needs repair keeps uploads closed (503); the server still starts.

The send worker and the import worker share the owner's one worker admission. Each claims only its own kind.

`stop_rust` drains the imports within the gateway's drain bound. An upload still streaming is discarded: its reader wakes every 250 ms to check for shutdown, so a slow client can't hold the drain. Its key and slot are released, and the client gets 503. An admitted import resumes at the next start. If an import worker is still running when the bound runs out, the runtime keeps storage ownership until the process exits, as it does for tracked work. It logs this and records `source_imports_not_drained` and `storage_ownership_retained` in the shutdown receipt.

## Checks

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core -p pp-storage --features pp-core/desktop_rebuild,pp-storage/desktop_rebuild -E 'binary(source_upload_http) | binary(source_import_zip) | binary(source_acquisition_recovery) | binary(import_worker_policy)'
```
