# Reruns to grade

## C1

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

`rust/scripts/check-boundaries.py` enforces the crate dependency edges.

- **How it checks:** it runs `cargo metadata --locked --no-deps` on `rust/Cargo.toml`. It then compares every `pp-*` dependency against the `ALLOWED` dict at line 6, which maps each crate to the set of `pp-*` crates it may depend on. `violations()` does the comparison. The script also asserts that the workspace crates match the `ALLOWED` keys exactly, so a crate missing from `ALLOWED` fails too.
- **Other rules in the same file:** `GATED` edges compile only through the `desktop_rebuild` feature (`rebuild_edge_is_gated()`); `DEV_ONLY` (`pp-testkit`) is allowed only under `[dev-dependencies]`; features in `TEST_ONLY_FEATURES` can only be turned on from `[dev-dependencies]`; `planted_edges_rejected()` feeds fake graphs with forbidden edges and fails the run if any gets through.
- **Command:** `python3 rust/scripts/check-boundaries.py --self-test`. CI runs it as the `check-boundaries` step in `.github/workflows/chain-tip-gate.yml` and again in `.github/workflows/desktop-runtime.yml`.
- **Adding an edge:** 1. Add the target crate to the source crate's set in `ALLOWED`. If the edge is desktop-rebuild-only, also add it to `GATED` and make the dependency `optional`, enabled only as `dep:<name>` from the `desktop_rebuild` feature. 2. Add the dependency in the crate's `Cargo.toml`. 3. Update the "current" edge table in `docs/adr/0007-rust-crate-layout.md`, which is copied from `ALLOWED`. The "planned" edges there are documentation only until `ALLOWED` changes.
- **Who merges:** adding an edge widens `ALLOWED`, so the PR stays with the owner rather than the coordinator.

## C2

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

Accept publishes a Plan through one route, and the publish runs as a single SQLite transaction in `pp-storage`.

- **Route:** `POST /plans/{id}/drafts/{draft_id}/apply` (also served as `/api/v2/plans/{id}/drafts/{draft_id}/apply`). It is mounted in `rust/crates/pp-api/src/drafts/http.rs` (around line 313) and handled by `lifecycle::apply` in `rust/crates/pp-api/src/drafts/http/lifecycle.rs`. `docs/architecture.md` says this is the only route that publishes (GRE-417).
- **Handler:** `Accept::run` builds a `PublicationCommand` and calls `PublicationClient::apply`. If the result is `ReconciliationRequired`, it prepares the draft and calls `apply` once more under the same idempotency key.
- **Storage:** `PublicationClient::apply` queues the command. The worker's `execute` (line ~665) opens a transaction, calls `publish_in_transaction` (line ~1378) and then runs `tx.commit()`. Both are in `rust/crates/pp-storage/src/plan_publication.rs`.
- **Atomicity test:** `gre_1120_accept_failing_inside_its_transaction_leaves_no_partial_acceptance` in `rust/crates/pp-api/src/drafts/http/tests/accept_header.rs:231`. It adds an `ABORT` trigger on `plan_snapshot_headers`, then on `build_profile_details`, and sends Accept. Each time it checks for a 500 that doesn't leak the error text, and that `logical_database` matches the snapshot taken before. It then accepts successfully and checks for revision 2, the new name and one snapshot header.
- **Storage-level test:** `caller_rollback_and_token_exhaustion_leave_publication_unwritten` in `rust/crates/pp-storage/src/plan_publication/tests.rs:1430` checks the same thing below the HTTP layer.

`WorkingDraftClient::save` (`rust/crates/pp-storage/src/working_drafts/save.rs:663`) also calls `publish_in_transaction`, but the draft router doesn't mount `/save`. Its rollback test is `publication_failure_rolls_back_snapshot_reconciliation_and_source_closure` in `rust/crates/pp-storage/tests/working_draft_save.rs:395`.

## C3

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

The dependency edges are enforced by **`rust/scripts/check-boundaries.py`**, run as `python3 rust/scripts/check-boundaries.py --self-test`.

- **What it checks:** it runs `cargo metadata --no-deps` on `rust/Cargo.toml`. It fails if any `pp-*` dependency is missing from the crate's set in the `ALLOWED` map (line 6). The check itself is `violations()`. It also fails if the workspace crate list differs from the keys of `ALLOWED` (line 85).
- **Extra rules:** `GATED` edges must be optional and turned on only by the `desktop_rebuild` feature (`rebuild_edge_is_gated()`); `DEV_ONLY` (`pp-testkit`) may appear only under `[dev-dependencies]`; `TEST_ONLY_FEATURES` is checked by `test_only_feature_violations()`; `planted_edges_rejected()` feeds in fake graphs that must be refused.
- **Where it runs:** step `boundaries` in `scripts/local-ci.py`, the `check-boundaries` step in `.github/workflows/chain-tip-gate.yml`, and `.github/workflows/desktop-runtime.yml`.
- **Adding a new edge:** 1. Add the dependency to the crate's `Cargo.toml`. 2. Add the target crate to that crate's set in `ALLOWED`. 3. If the edge belongs to the desktop rebuild, also add it to `GATED` and make it `optional` with `dep:<name>` under the `desktop_rebuild` feature. 4. Update the edge tables in `docs/architecture.md` ("Crates and allowed edges") and `docs/adr/0007-rust-crate-layout.md` ("Current allowed edges") in the same PR. ADR 0007 says planned edges are documentation only until a change updates `ALLOWED`. 5. Don't edit `ALLOWED` to make the planted test cases pass.
