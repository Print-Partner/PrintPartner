# Operation family index

Use this table to find existing entry points for the families in
[Architecture](architecture.md#operation-families-and-write-ownership).
Architecture owns target scope and runtime ownership. These paths do not claim
that a whole family ships. Package filters cover existing tests, including
partial implementations; they do not prove deferred work.

Run checks from the repository root. For a row marked **Rust**, use its filter:

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --workspace --features desktop_rebuild --no-tests=fail -E 'FILTER'
```

For **Index**, run `python3 scripts/check-family-index.py`; this checks paths,
not behaviour. No Rust family coverage is claimed for those rows.

| Family | Module or handle | Route module | Nextest filter | Doc page | Check |
| --- | --- | --- | --- | --- | --- |
| Authentication, sessions, API keys, pairing | [rust/crates/pp-storage/src/auth.rs](../rust/crates/pp-storage/src/auth.rs) | [rust/crates/pp-api/src/auth](../rust/crates/pp-api/src/auth) | `package(pp-api)` | [network-sharing.md](network-sharing.md) | Rust |
| Product Module choices and settings | [rust/crates/pp-storage/src/features.rs](../rust/crates/pp-storage/src/features.rs) | [rust/crates/pp-api/src/builds/features.rs](../rust/crates/pp-api/src/builds/features.rs) | `package(pp-storage)` | [gre-438-admin-http.md](gre-438-admin-http.md) | Rust |
| Sources, manifests, imports, scan, docs, sync | [rust/crates/pp-storage/src/catalog.rs](../rust/crates/pp-storage/src/catalog.rs) | [rust/crates/pp-api/src/catalog](../rust/crates/pp-api/src/catalog) | `package(pp-storage)` | [gre-805-catalog-http.md](gre-805-catalog-http.md) | Rust |
| Builds, drafts, parts, order/customer/due-date fields | [rust/crates/pp-storage/src/build_graph.rs](../rust/crates/pp-storage/src/build_graph.rs) | [rust/crates/pp-api/src/builds.rs](../rust/crates/pp-api/src/builds.rs) | `package(pp-storage)` | [draft-headers.md](draft-headers.md) | Rust |
| Accept, proposals, accepted Plans, Required units, Plates | [rust/crates/pp-storage/src/plan_publication.rs](../rust/crates/pp-storage/src/plan_publication.rs) | [rust/crates/pp-api/src/drafts](../rust/crates/pp-api/src/drafts) | `package(pp-storage)` | [plan-autosave.md](plan-autosave.md) | Rust |
| Checkoff, assembly, bed confirmation, unmatched prints | [rust/crates/pp-storage/src/checkoff_progress.rs](../rust/crates/pp-storage/src/checkoff_progress.rs) | [rust/crates/pp-api/src/checkoff.rs](../rust/crates/pp-api/src/checkoff.rs) | `package(pp-storage)` | [farm-log.md](farm-log.md) | Rust |
| Workflow, readiness, Today tasks, History | [rust/crates/pp-storage/src/read_model.rs](../rust/crates/pp-storage/src/read_model.rs) | [rust/crates/pp-api/src/workflow.rs](../rust/crates/pp-api/src/workflow.rs) | `package(pp-storage)` | [build-workflow.md](build-workflow.md) | Rust |
| Exports and production setup | [rust/crates/pp-core/src/production](../rust/crates/pp-core/src/production) | [rust/crates/pp-api/src/plates.rs](../rust/crates/pp-api/src/plates.rs) | `package(pp-core)` | [desktop-rebuild.md#production-setup-in-rust-gre-1209](records/d19-publication-client.md#production-setup-in-rust-gre-1209) | Rust |
| Printers, queue, discovery, Bambu | [rust/crates/pp-storage/src/send_queue.rs](../rust/crates/pp-storage/src/send_queue.rs) | [rust/crates/pp-api/src/send_queue.rs](../rust/crates/pp-api/src/send_queue.rs) | `package(pp-storage)` | [send-queue.md](send-queue.md) | Rust |
| Stock and Spoolman shortfalls | [rust/crates/pp-storage/src/read_model/stock.rs](../rust/crates/pp-storage/src/read_model/stock.rs) | [rust/crates/pp-api/src/accepted.rs](../rust/crates/pp-api/src/accepted.rs) | `package(pp-storage)` | [build-workflow.md](build-workflow.md) | Rust |
| Native slicers and profiles | [rust/crates/pp-services/src/native_slicers.rs](../rust/crates/pp-services/src/native_slicers.rs) | None; no Rust route for this slice | `package(pp-services)` | [architecture.md](architecture.md) | Rust |
| Optional external slicer jobs | None; Rust implementation deferred | None; no Rust route for this slice | None; no Rust coverage | [docker.md](docker.md) | Index |
| Home Assistant, webhooks, Discord | None; Rust implementation deferred | None; no Rust route for this slice | None; no Rust coverage | [architecture.md](architecture.md) | Index |
| Jobs, cancel, restart recovery | [rust/crates/pp-storage/src/jobs.rs](../rust/crates/pp-storage/src/jobs.rs) | [rust/crates/pp-api/src/jobs.rs](../rust/crates/pp-api/src/jobs.rs) | `package(pp-storage)` | [gre-800-jobs-http.md](gre-800-jobs-http.md) | Rust |
| Backup, restore, migration/downgrade refusal | [rust/crates/pp-storage/src/archive.rs](../rust/crates/pp-storage/src/archive.rs) | None; no Rust route for this slice | `package(pp-storage)` | [storage.md](storage.md) | Rust |
| Admin, legal, logs, diagnostics, health | [rust/crates/pp-storage/src/admin.rs](../rust/crates/pp-storage/src/admin.rs) | [rust/crates/pp-api/src/admin.rs](../rust/crates/pp-api/src/admin.rs) | `package(pp-api)` | [gre-438-admin-http.md](gre-438-admin-http.md) | Rust |
| Reference sharing | None; Rust implementation deferred | None; no Rust route for this slice | None; no Rust coverage | [reference-sharing.md](reference-sharing.md) | Index |
| Assistant and old API aggregators | [rust/crates/pp-services/src/mcp_domain.rs](../rust/crates/pp-services/src/mcp_domain.rs) | [rust/crates/pp-api/src/mcp.rs](../rust/crates/pp-api/src/mcp.rs) | `package(pp-api)` | [assistant-mcp.md](assistant-mcp.md) | Rust |
| Community board, hosted/tenant/Postgres modes | None; excluded | None; no Rust route for this slice | None; no Rust coverage | [architecture.md](architecture.md) | Index |
