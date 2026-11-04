# Share narrow filesystem portability mechanics

Status: accepted design on 2026-11-03 under Chad's delegated research choice.
Partly implemented: `pp-os` exists and `pp-storage` is its first consumer.
`pp-source` and `pp-core` are not migrated yet. This amends [ADR 0009](0009-keep-crates-and-deepen-modules.md)
without changing [ADR 0010](0010-whole-runtime-cutover.md).
Numbers 0011 through 0014 are reserved by the separate design proposal; this
record does not adopt those decisions or change that PR's administrative state.

Keep the existing operation crates and introduce a narrow `pp-os` during the
early Windows track. Storage, source and core repeat directory-relative access,
identity/no-follow checks, locking and durable publication. Sharing those
mechanics addresses repeated portability work. Wholesale engine consolidation
would relocate transactions and immutable files without removing their recovery
coordination. Keeping every implementation local would repeat the Windows port.
Neither option has a measured compilation or runtime advantage.

## Boundary and delivery

`pp-os` has no business policy, `pp-*`, SQL, HTTP, Tauri or Tokio dependencies.
Its private Unix/Windows adapters expose handle-based private filesystem access,
identity checks, no-follow/reparse refusal, lock primitives and durable file
publication. The first consumers are `pp-storage`, `pp-source` and `pp-core`.
Storage retains installation lease policy, lifetime, migration ordering, SQL and
transactions. Source retains immutable snapshot policy. Core retains composition,
worker lifetime and signals. Menus, slicer discovery and arbitrary OS-dependent
functions do not move merely because they differ by platform.

Start this bounded work within P-WINDOWS alongside the gated product entry point;
do not block the first read-only screen on a workspace reorganization. Assign an
execution owner and define the minimum interface from those consumers. Extract
Unix behavior without changing formats. Prove the same behavior using real
Windows handles, permissions, reparse points and rename/locking races. A
capability library's containment API does not itself guarantee refusal of links,
stable identity or durable publication.

Migrate one consumer per reviewable change, with its manifests, boundary checker,
interface documentation and existing tests. If an interface weakens an existing
guarantee, stop extraction and retain local adapters until corrected. Preserve
crash reconciliation between file publication and catalog activation; no SQL
transaction spans large file work. This is an incremental implementation brief
within existing platform/invariant checks, not an extra assurance program.

The crate count is not a goal. Retain typed family handles and private dispatch;
no public SQL closures or repository-per-table interfaces. Rust owns new wire
families through the working schema-to-Zod path. Keep `/ws/printers` and job
sockets alongside the separate durable domain feed.

Draft-only save and Accept separation remain functional work. Baseline
`WorkingDraftClient::save` publishes through `publish_in_transaction`. It cannot
stand in for draft-only autosave. Reuse its allocation/reconciliation logic while
proving that draft saves leave accepted state unchanged and Accept records the
snapshot, change and history atomically.

Both rebuild gates remain default-off. Schema ceilings are compile-time 35
without the feature and 41 with it; runtime opt-out is not a downgrade. Flag
retirement needs separate approval. This choice authorizes no merge, production
cutover, release, signing, real printer action or old-ticket disposition.

## Research evidence

Source inspection used implementation baseline
`a8d6323aa5acc6d065259ea4a613d0fd8c108dcf` and design PR #1437 at
`bcaf391bebcbf7661547bea37fc99027c8aca202`. These immutable links capture the
reasons for the boundary, not a claim that the target is delivered:

- [Storage lease admission and identity checks](https://github.com/poitee/PrintPartner/blob/a8d6323aa5acc6d065259ea4a613d0fd8c108dcf/rust/crates/pp-storage/src/lease.rs)
- [Source directory-relative operations and publication](https://github.com/poitee/PrintPartner/blob/a8d6323aa5acc6d065259ea4a613d0fd8c108dcf/rust/crates/pp-source/src/directory.rs)
- [Core state-file identity and durability](https://github.com/poitee/PrintPartner/blob/a8d6323aa5acc6d065259ea4a613d0fd8c108dcf/rust/crates/pp-core/src/state_file.rs)
- [Storage-owned publication transaction](https://github.com/poitee/PrintPartner/blob/a8d6323aa5acc6d065259ea4a613d0fd8c108dcf/rust/crates/pp-storage/src/plan_publication.rs)
- [Existing save also publishes](https://github.com/poitee/PrintPartner/blob/a8d6323aa5acc6d065259ea4a613d0fd8c108dcf/rust/crates/pp-storage/src/working_drafts/save.rs)
- [Core import phases and activation](https://github.com/poitee/PrintPartner/blob/a8d6323aa5acc6d065259ea4a613d0fd8c108dcf/rust/crates/pp-core/src/uploads.rs)
- [Working Rust-schema-to-Zod generator](https://github.com/poitee/PrintPartner/blob/a8d6323aa5acc6d065259ea4a613d0fd8c108dcf/web/scripts/rust-contracts/generate.mjs)
- [cap-std 4.0.3 directory API](https://docs.rs/cap-std/4.0.3/cap_std/fs/struct.Dir.html) documents relative operations and separate link operations, not proof of Print Partner's Windows guarantees.
- [Cargo timings](https://doc.rust-lang.org/cargo/reference/timings.html) explains how to measure the compilation critical path; fewer crates alone do not establish a speedup.
- [Schemars 0.8.22](https://docs.rs/schemars/0.8.22/schemars/) documents the existing generator's schema model, not operation validation or a required major upgrade.
- [Tauri architecture](https://v2.tauri.app/concept/architecture/) describes the native window/webview layers that still need platform evidence.
