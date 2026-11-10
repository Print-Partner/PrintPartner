
## B01

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

**Owner:** `pp-storage` owns the `printer_send_queue` table, in `rust/crates/pp-storage/src/send_queue.rs`. The table's DDL is in `rust/crates/pp-storage/src/send_queue/schema.sql` and is installed by schema step 44 (`SCHEMA_STEP = 44`). The same module has: - `rows(connection, tenant)`, which every reader calls. It returns `None` while the old Node setting `printer.send_queue` (`LEGACY_KEY`) still exists for that tenant. - `recover`, which moves that old setting into the table. - `settle` and `trim`. The user actions (Send now, Send ready, Remove) live in `pp-storage/src/send_queue/writes.rs` (`SendQueueClient`). **Read route:** `GET /api/v2/printer-send-queue`. - It is mounted in `farm_router` at `rust/crates/pp-api/src/farm.rs:56`. - Its handler is `async fn send_queue` at `farm.rs:101`, which returns `farm.send_queue()`. - `Farm::send_queue` at `rust/crates/pp-storage/src/read_model/farm.rs:765` returns the queue that `read_settings` loaded through `crate::send_queue::rows(connection, "default")` at `farm.rs:536`. If `rows` returns `None`, it reads the old setting instead. - The route is also listed in `pp-api/src/client_contract.rs:332` and covered by `rust/crates/pp-core/tests/runtime_m0/farm_reads.rs:20`. `rust/crates/pp-api/src/send_queue.rs` (`send_queue_router`) only writes: `POST .../drain`, `POST .../{id}/dispatch`, `POST .../{id}/reconcile` and `DELETE .../{id}`. Its tests are in `pp-api/tests/ticket_t_26_send_queue_http.rs`.

## B02

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

`pp-storage` owns the table, and `GET /api/v2/printer-send-queue` reads it. Only that route reads it; the other send-queue routes change items. **Owner:** `rust/crates/pp-storage/src/send_queue.rs` - `rust/crates/pp-storage/src/send_queue/schema.sql` creates the `printer_send_queue` table (schema step 44). - In `send_queue.rs`, `recover` moves Node's old `printer.send_queue` setting into the table, `settle` updates an item when its upload job changes, and `rows(connection, tenant)` is the shared reader. `rows` returns `None` while the old setting still exists. - `rust/crates/pp-storage/src/send_queue/writes.rs` holds the user actions (`SendQueueClient`, `apply`). Tests are in `send_queue/tests.rs`. **Read route:** `GET /api/v2/printer-send-queue` - It is registered in `farm_router` at `rust/crates/pp-api/src/farm.rs:56` and handled by `async fn send_queue` at `farm.rs:101`, which returns `farm.send_queue()`. - That method is `Farm::send_queue()` at `rust/crates/pp-storage/src/read_model/farm.rs:765`. The farm read fills the queue at `farm.rs:536` by calling `crate::send_queue::rows(connection, "default")`, or reads `LEGACY_KEY` while the old setting exists. - `farm_router` mounts the route only when `PP_DESKTOP_REBUILD` is `1` or `true`. **Write routes:** `send_queue_router` in `rust/crates/pp-api/src/send_queue.rs:74` handles `POST …/drain`, `POST …/{id}/dispatch`, `POST …/{id}/reconcile` and `DELETE …/{id}`. They are tested in `rust/crates/pp-api/tests/ticket_t_26_send_queue_http.rs`. `rows` is also called from `read_model/context.rs:855` and `read_model/history.rs:634`.

## B03

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

**Owner:** the Rust storage crate. The `printer_send_queue` table lives in `rust/crates/pp-storage/src/send_queue.rs`. That module says the table is "installed by schema step 44" (GRE-1208). - The DDL is in `rust/crates/pp-storage/src/send_queue/schema.sql`. - `recover` moves the old Node JSON setting (`printer.send_queue`, held in `LEGACY_KEY`) into the table when the writer opens. - `rows(connection, tenant)` is the only reader. It returns `None` while the old setting still exists, and readers then fall back to that setting. - User writes go through `writes` (`SendQueueClient`, built by `send_queue(policy)` in `send_queue/writes.rs:142`). `settle` moves a sent item to done or error. **Read route:** `GET /api/v2/printer-send-queue`. It is mounted in `rust/crates/pp-api/src/farm.rs:56` as `.route("/printer-send-queue", get(send_queue))`. The handler `send_queue` (`farm.rs:101`) calls `farm.send_queue()`. - That method is in `rust/crates/pp-storage/src/read_model/farm.rs:765`. The farm data behind it is loaded at `farm.rs:536`, which calls `crate::send_queue::rows(connection, "default")` and falls back to `LEGACY_KEY`. - The route is tested in `rust/crates/pp-core/tests/runtime_m0/farm_reads.rs:20`. **Not reads:** the other queue routes in `rust/crates/pp-api/src/send_queue.rs` (`send_queue_router`) all change data: `POST /printer-send-queue/drain`, `POST /{id}/dispatch`, `POST /{id}/reconcile` and `DELETE /{id}`. They are tested in `pp-api/tests/ticket_t_26_send_queue_http.rs`. Other places in `pp-storage` also call `rows`: `read_model/farm.rs:562` (`queue_list`), `read_model/history.rs:634`, `read_model/context.rs:855` and `plan_publication.rs:361`.

## B04

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

- **Owner:** `pp_storage::send_queue` in `rust/crates/pp-storage/src/send_queue.rs` owns the `printer_send_queue` table. - **Schema:** `rust/crates/pp-storage/src/send_queue/schema.sql` defines it; `SCHEMA_STEP = 44`. - **Storage read:** `send_queue::rows()` selects tenant rows ordered by `position`. - **Read route:** `GET /api/v2/printer-send-queue`. - **Route implementation:** `farm_router()` registers `get(send_queue)` in `rust/crates/pp-api/src/farm.rs`; handler `send_queue()` returns `farm.send_queue()`. - **Read chain:** `FarmClient::read()` → `execute()` → `queue_list()` → `send_queue::rows()` in `rust/crates/pp-storage/src/read_model/farm.rs`. - **Compatibility:** `queue_list()` reads the legacy `printer.send_queue` setting while it exists. - **Evidence:** `ticket_t_26_a_legacy_setting_wins_over_table_rows` in `rust/crates/pp-storage/src/send_queue/tests.rs` verifies that precedence.

## B05

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

- `rust/scripts/check-boundaries.py` enforces Rust crate edges through its explicit `ALLOWED` map. - `violations(packages)` rejects `pp-*` dependencies absent from that map. - It reads manifests through `cargo metadata --manifest-path rust/Cargo.toml --locked --format-version 1 --no-deps`. - `GATED` and `rebuild_edge_is_gated()` require designated edges to remain optional and enabled only through `desktop_rebuild`. - `planted_edges_rejected()` checks that deliberately invalid graphs fail. - CI runs `python3 rust/scripts/check-boundaries.py --self-test` in `.github/workflows/chain-tip-gate.yml`, step `check-boundaries`. - `scripts/local-ci.py` also runs it in the `boundaries` step. - Add an edge by updating `ALLOWED`, the consuming crate’s `rust/crates/<crate>/Cargo.toml`, and dependency documentation together. - That requirement is explicit in `docs/architecture.md`, section “Crates and allowed edges”; update that graph and relevant crate READMEs. - Planned edges in `docs/adr/0007-rust-crate-layout.md` grant no permission until `ALLOWED` changes.

## B06

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

- `rust/scripts/check-boundaries.py` enforces crate edges through its explicit `ALLOWED` map.
- It reads `cargo metadata --manifest-path rust/Cargo.toml --locked --format-version 1 --no-deps`.
- `violations(packages)` rejects `pp-*` dependencies absent from the consuming crate’s allowed set.
- `GATED` and `rebuild_edge_is_gated()` additionally enforce optional, default-off `desktop_rebuild` edges.
- `planted_edges_rejected()` checks synthetic forbidden edges; it runs on every invocation.
- Hosted CI runs `python3 rust/scripts/check-boundaries.py --self-test` in `.github/workflows/chain-tip-gate.yml`, step `check-boundaries`.
- Local CI includes that command in `scripts/local-ci.py`, step `boundaries`.
- To add an edge, declare it in `rust/crates/<consumer>/Cargo.toml` and add the dependency to `ALLOWED["<consumer>"]`.
- For a rebuild-only edge, also update `GATED` and wire the optional dependency through `desktop_rebuild`.
- Update `docs/architecture.md` → “Crates and allowed edges”; `docs/adr/0007-rust-crate-layout.md` explicitly says planned documentation alone does not authorize an edge.

## B07

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The key is checked in two steps, both in `rust/crates/pp-storage/src/checkoff_progress/burst.rs`:

1. **Format check:** `idempotency_key(raw)` (line 64) trims the key the way JavaScript does and requires 1 to 200 characters. Otherwise it returns 400 `idempotency_key must be 1 to 200 characters`. Three callers use this one function:
   - `Burst::parse` (line 81), for `POST /printer-checkoff/verify-batch`
   - the manual Build burst parser in `checkoff_progress/manual.rs:75`, for `POST /plans/{id}/checkoff/bursts`
   - the status lookup in `checkoff_progress.rs:618`, for `GET /printer-checkoff/verify-batch?idempotency_key=...` (`Op::BurstStatus`)
2. **Replay check:** `verify_batch` (line 351) calls `replayed(...)` (line 515) before any write. If the key's stored receipt has a different `request_hash`, it returns 409 `already used for a different request`. If the burst was undone, it returns 409 `burst_undone`. If the key is on the retired list (`key_hash` under `RETIRED_KEY`), it returns 409 `too old to replay`. Otherwise it returns the stored receipt. `manual.rs` builds its burst key with the same check and stores receipts the same way; I didn't trace whether it also goes through `replayed()`.

**Tests:**
- `rust/crates/pp-storage/tests/checkoff_bursts.rs`: `verify_batch_replays_the_receipt_and_refuses_a_changed_body`, `verify_batch_refuses_a_key_whose_receipt_was_trimmed`, `verify_batch_refuses_malformed_bursts_before_any_write`
- `rust/crates/pp-api/tests/checkoff_bursts_http.rs`: `verify_batch_http_replays_across_restart_and_undoes_once`

MCP `confirm_apply` uses the key `mcp:<action_id>` (`rust/crates/pp-api/src/mcp/checkoff.rs:21`).

## B08

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

**Owner:** the `pp_storage::send_queue` module owns the `printer_send_queue` table.
- `rust/crates/pp-storage/src/send_queue.rs` holds the module. Its header calls it "Printer send queue storage (GRE-1208)". It installs the table at schema step 44 (`SCHEMA_STEP`) from `rust/crates/pp-storage/src/send_queue/schema.sql`.
- `recover` moves Node's legacy `printer.send_queue` setting (`LEGACY_KEY`) into the table. `rows(connection, tenant)` is the only reader, and it returns `None` while the legacy setting still exists.
- `rust/crates/pp-storage/src/send_queue/writes.rs` holds the user actions on queue items. `send_queue()` returns a `SendQueueClient`.

**Read route:** `GET /api/v2/printer-send-queue`.
- It is registered in `rust/crates/pp-api/src/farm.rs:56` as `.route("/printer-send-queue", get(send_queue))`. The handler `async fn send_queue` at line 101 returns `farm.send_queue()`.
- The data comes through `read_settings` in `rust/crates/pp-storage/src/read_model/farm.rs:521`. At line 536 it calls `crate::send_queue::rows(connection, "default")` and falls back to the legacy setting. `Farm::send_queue()` at line 765 builds the response.
- The write routes (drain, `{id}/dispatch`, `{id}/reconcile`, DELETE `{id}`) are in `send_queue_router` in `rust/crates/pp-api/src/send_queue.rs:74`. `rust/crates/pp-core/src/printer_status.rs:323` mounts that router.

**Tests:** `rust/crates/pp-core/tests/runtime_m0/farm_reads.rs` covers the v2 GET route. `ticket_t_3_drain_repeated_upload.rs` uses `/api/v1/printer-send-queue` and its `drain` route.

## B09

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

A Rust-only PR runs `just ci --fast` before it is pushed, unless it is a sensitive PR, which runs the full `just ci`. - **Command:** `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast`. `just ci` is a wrapper for `python3 scripts/local-ci.py`, which holds the only list of steps (`AGENTS.md`, `docs/development/ci.md` "Local CI"). - **What `--fast` runs:** every step except `nextest-off`, `nextest-on`, `doc-off` and `doc-on`. In place of `nextest-on` it runs `nextest-affected`: nextest with `desktop_rebuild` on, limited to the changed crates and their reverse dependents (`rdeps(=<pkg>)`). The filter rules are in `scripts/ci-affected-nextest-filter.py` and are applied to the diff from `git merge-base origin/main HEAD`, so fetch `origin` first. The code is `FAST_LEAVES_OUT`, `FAST_NEXTEST` and `fast_steps()` in `scripts/local-ci.py`. - **Rust steps that still run:** `fmt` (`cargo fmt --check`), `clippy-off` and `clippy-on` (`cargo clippy --workspace --all-targets --keep-going -- -D warnings`, without and with `--features desktop_rebuild`), `clippy-limits` (`rust/scripts/check-clippy-limits.py`), `historical-schema`, `web-assets` (builds `rust/bundle-manifest.json`), `rust-deps` (`cargo machete`), and the boundary, ast-grep and workaround-ratchet checks. - **Sensitive PRs:** item 6 of the merge gate in `docs/development/definition-of-done.md` defines them: schema or migrations, auth, backup, secrets, CODEOWNERS paths and similar. These run the full `just ci`, which adds nextest and doctests with `desktop_rebuild` off and on (`docs/contributing/gate-evidence.md`, "Run CI locally before you push"). - **Outside the script:** `docs/development/ci.md` lists `cargo deny --manifest-path rust/Cargo.toml --all-features check` as something to run by hand. CI runs it on any change under `rust/**`. Windows and macOS always print SKIP locally. - **Evidence:** fix every FAIL before pushing. Put the run and every SKIP line under `## Evidence` in the PR body (`definition-of-done.md`, around line 129). A SKIP does not count as a pass.

## B10

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

- Rust-only changes have no blanket exemption: `docs/contributing/gate-evidence.md` requires local CI before **each push**.
- Normal PR: `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast`.
- Sensitive PR: use the full `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci`.
- `docs/development/definition-of-done.md`, “Merge gate” item 6, defines sensitive changes: schema/migrations, auth/sessions, backups, Accept/publication, secrets, and protected paths.
- `justfile`’s `ci` recipe invokes `python3 scripts/local-ci.py`; that command also works directly.
- `scripts/local-ci.py::fast_steps()` retains formatting, both Clippy modes, ratchets, boundaries, assets and fixture checks.
- Fast mode replaces full nextest passes with `nextest-affected`; `affected_nextest_filter()` selects changed crates and reverse dependents.
- Full mode also runs `nextest-off`, `nextest-on`, `doc-off`, and `doc-on`.
- `docs/development/ci.md` separately lists `cargo deny --manifest-path rust/Cargo.toml --all-features check` and the fuzz equivalent with `--locked`; `just ci` excludes these.
- Fix every `FAIL`; record the command and every `SKIP` under `## Evidence`—a skip is not a pass (`docs/contributing/gate-evidence.md`).

## B11

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

**Check:** `rust/scripts/check-boundaries.py`, run as `python3 rust/scripts/check-boundaries.py --self-test`. It runs in the `boundaries` step of `scripts/local-ci.py` (`just ci`), in `.github/workflows/chain-tip-gate.yml` (step `check-boundaries`) and in `desktop-runtime.yml`. See `docs/development/ci.md:316`.

**How it works:** the script reads the workspace with `cargo metadata --no-deps --locked` and compares every `pp-*` dependency against the `ALLOWED` map, which lists each crate's permitted `pp-*` dependencies. The comparison is in `violations()`.
- `GATED` lists edges that must be optional and turned on only through the `desktop_rebuild` feature. `rebuild_edge_is_gated()` checks this.
- `DEV_ONLY` (`pp-testkit`) may only appear under `[dev-dependencies]`.
- `TEST_ONLY_FEATURES` is checked by `test_only_feature_violations()`.
- The script also asserts that the set of workspace packages equals the keys of `ALLOWED`, so a new crate must be listed there too.
- `planted_edges_rejected()` feeds in fake graphs with bad edges and confirms they are rejected.

**Adding an edge:**
1. Add the target crate to the source crate's set in `ALLOWED`. If the edge belongs to the desktop rebuild, also add it to `GATED` and make the dependency optional, enabled only by `desktop_rebuild = ["dep:<crate>", ...]`.
2. Add the dependency in that crate's `Cargo.toml`.
3. Update the edge tables in `docs/architecture.md` ("Crates and allowed edges") and `docs/adr/0007-rust-crate-layout.md` ("Current allowed edges"). Both are copied from `ALLOWED`, and edges listed as "planned" there are documentation only until `ALLOWED` changes.

**Who merges it:** widening `ALLOWED`, or shrinking `DEV_ONLY` or `TEST_ONLY_FEATURES`, is merged by the owner (Chad), not the coordinator. See `docs/development/definition-of-done.md` (merge gate) and `docs/development/ci-controls.md:37`.

## B12

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

- **Owner:** `pp_storage::send_queue`, in `rust/crates/pp-storage/src/send_queue.rs`.
- **Table:** `printer_send_queue`, defined in `rust/crates/pp-storage/src/send_queue/schema.sql`; installed at schema step **44**.
- **Storage reader:** `send_queue::rows(connection, tenant)` selects the tenant’s rows ordered by `position`.
- **Read model:** `rust/crates/pp-storage/src/read_model/farm.rs` calls `crate::send_queue::rows`; `Farm::send_queue()` supplies the response.
- **Route:** **`GET /api/v2/printer-send-queue`**.
- **Route implementation:** `rust/crates/pp-api/src/farm.rs`; `farm_router()` registers `get(send_queue)`, and the `send_queue()` handler calls `farm.send_queue()`.
- **Compatibility:** while `printer.send_queue` exists in legacy settings, readers use that setting.
- **Supporting test:** `ticket_t_26_a_legacy_setting_wins_over_table_rows` in `rust/crates/pp-storage/src/send_queue/tests.rs`.
- **Documentation:** `docs/send-queue.md`, “Schema 44” and “Reading a queue Node left.”

## B13

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

**Owner:** the `pp-storage` crate owns the send-queue table, through `rust/crates/pp-storage/src/send_queue.rs`.
- The table is `printer_send_queue`. Schema step 44 creates it (`SCHEMA_STEP = 44`), and the SQL is in `rust/crates/pp-storage/src/send_queue/schema.sql`.
- `send_queue::recover` moves the old Node setting `printer.send_queue` (`LEGACY_KEY`) into the table. `send_queue::rows(connection, tenant)` returns `None` while that setting still exists, and readers then fall back to the setting.
- User writes are in `send_queue/writes.rs`: send now, send ready, and remove. `settle` sets a dispatched item to done or error.

**Read route:** `GET /api/v2/printer-send-queue`.
- It is registered in `farm_router` in `rust/crates/pp-api/src/farm.rs:56`. The handler is `async fn send_queue` (line 101), which returns `farm.send_queue()`.
- `Farm::send_queue()` is in `rust/crates/pp-storage/src/read_model/farm.rs:765`. The queue is loaded by `crate::send_queue::rows(connection, "default")` (line 536), and falls back to `LEGACY_KEY` if that returns `None`.
- The write routes are in `rust/crates/pp-api/src/send_queue.rs`, in `send_queue_router`: `POST /printer-send-queue/drain`, `POST /printer-send-queue/{id}/dispatch`, `POST /printer-send-queue/{id}/reconcile`, and `DELETE /printer-send-queue/{id}`. They are tested in `rust/crates/pp-api/tests/ticket_t_26_send_queue_http.rs`, for example `ticket_t_26_send_queue_routes_mount_only_with_the_flag_and_need_a_session`.
- `rows` has other readers: `plan_publication.rs:361`, `read_model/history.rs:634`, and `read_model/farm.rs:562`.

## B14

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

Accept publishes through `POST /api/v2/plans/{id}/drafts/{draft_id}/apply`.

- `rust/crates/pp-api/src/drafts/http.rs:313` registers that route with `lifecycle::apply`.
- `rust/crates/pp-storage/src/plan_publication.rs:1378` contains `publish_in_transaction`, which saves the pre-Accept snapshot, writes the Plan revision, replaces projected Parts, and allocates Required units.
- In that same file, `execute` at line 665 starts an immediate transaction and commits successful publication outcomes.
- The atomicity test is `transaction::caller_rollback_and_token_exhaustion_leave_publication_unwritten` in `rust/crates/pp-storage/src/plan_publication/tests.rs:1430`.
- It publishes successfully, drops the uncommitted transaction, then verifies token-allocation failure also leaves the original rows unchanged and publication tables empty.

`docs/architecture.md:252` documents the atomic Accept contract. This was a read-only inspection; I did not run the test.

## B15

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

- **Owner:** `pp_storage::send_queue`, in `rust/crates/pp-storage/src/send_queue.rs`, owns storage for the `printer_send_queue` table. - **Schema:** `rust/crates/pp-storage/src/send_queue/schema.sql` defines the table and `printer_send_queue_one_sending` index; schema step **44** installs them. - **Reader:** `send_queue::rows()` reads table items in stored order. While the legacy `printer.send_queue` setting exists, readers use that instead. - **Read model:** `rust/crates/pp-storage/src/read_model/farm.rs` calls `send_queue::rows()`; `Farm::send_queue()` exposes the response. - **Route:** **`GET /api/v2/printer-send-queue`**, registered by `farm_router()` and handled by `send_queue()` in `rust/crates/pp-api/src/farm.rs`. - **Supporting test:** `ticket_t_26_open_moves_a_clean_legacy_queue_into_the_table` in `rust/crates/pp-storage/src/send_queue/tests.rs`. - **Documentation:** `docs/send-queue.md`, particularly “Schema 44” and “Reading a queue Node left.”

## B16

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

**The check is `rust/scripts/check-boundaries.py`.**

- **What it checks:** the `ALLOWED` dict (line 6) lists, for each `pp-*` crate, the other `pp-*` crates it may depend on. The script runs `cargo metadata --no-deps --locked` and fails if any crate in the workspace is missing from `ALLOWED`, or if `violations()` finds a `pp-*` dependency that isn't listed.
- **Other rules in the same file:**
  - `GATED`: these edges must be optional dependencies that only the `desktop_rebuild` feature turns on. `rebuild_edge_is_gated()` checks this.
  - `DEV_ONLY`: `pp-testkit` may only appear under `[dev-dependencies]`.
  - `TEST_ONLY_FEATURES`: `pp-storage/test-unmarked-writer` may only be turned on from `[dev-dependencies]`. `test_only_feature_violations()` checks this.
  - `planted_edges_rejected()` feeds in fake bad dependency graphs and confirms the script rejects them.
- **Where it runs:** `python3 rust/scripts/check-boundaries.py --self-test`. CI runs it in the `check-boundaries` step of `.github/workflows/chain-tip-gate.yml`. Locally it is the `boundaries` step in `scripts/local-ci.py` (`just ci`), along with `check-domain-deps.py --self-test`.

**Adding a new edge:**
1. Add the dependency to the crate's `Cargo.toml`.
2. Add the target crate to that crate's set in `ALLOWED`. A new crate also needs its own key in `ALLOWED`. If the edge should only exist in desktop-rebuild builds, also add it to `GATED` and make it an optional dependency that `desktop_rebuild` turns on with `dep:<name>`.
3. In the same PR, update the edge tables in `docs/adr/0007-rust-crate-layout.md` ("Current allowed edges") and `docs/architecture.md` ("Crates and allowed edges"). Both say they are copied from `ALLOWED`.

**Merge rule:** `docs/development/definition-of-done.md` says a PR that widens `ALLOWED`, or shrinks `DEV_ONLY` or `TEST_ONLY_FEATURES`, is owner-merged. Adding a new edge widens `ALLOWED`, so the owner merges it. Tightening an edge is a coordinator merge.

## B17

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

A Rust-only PR runs the same local CI as any other PR. The docs have no separate Rust-only list. The command depends on whether the PR is normal or sensitive: - **Normal PR:** `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast` (`docs/development/ci.md` §"Run the required checks with one command"; `docs/development/definition-of-done.md:129`; `docs/development/agent-workflow.md:87`; `docs/contributing/gate-evidence.md` §"Run CI locally before you push"). - **Sensitive PR** (item 6 of the merge gate in `definition-of-done.md`): the full `just ci`, which is the same as `python3 scripts/local-ci.py`. - **What `--fast` leaves out:** `scripts/local-ci.py:220-236` (`FAST_LEAVES_OUT`, `fast_steps()`) drops `nextest-off`, `nextest-on`, `doc-off` and `doc-on`. It runs `nextest-affected` in place of `nextest-on`. That step is nextest with `--features desktop_rebuild`, limited to the changed crates and their reverse dependents (`rdeps`) by `scripts/ci-affected-nextest-filter.py`. The filter compares against `git merge-base origin/main HEAD`, so fetch `origin` first. Changes to `rust/Cargo.lock`, `rust/tests/**` or `rust/scripts/**` make it test every crate. - **Rust steps still in the fast set:** `fmt`, `historical-schema`, `web-assets` (builds `rust/bundle-manifest.json`), `clippy-off`, `clippy-on` (`cargo clippy ... --workspace --all-targets --keep-going [--features desktop_rebuild] -- -D warnings`), `clippy-limits` (`rust/scripts/check-clippy-limits.py`), `ast-grep`, `boundaries`, `rust-deps` and `node-fixtures`. `AGENTS.md` also lists `cargo fmt --manifest-path rust/Cargo.toml --all -- --check`. - **Evidence:** put the command and its PASS/FAIL/SKIP summary under `## Evidence` in the PR body. List every SKIP line, because a SKIP is not a pass. `just ci --fast --list` prints the step ids. One difference from the user's saved memory: it says to skip JEV steps, and this checkout's docs don't mention JEV at all.

## B18

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

The `pp-storage` crate's `send_queue` module owns the table, and `GET /api/v2/printer-send-queue` reads it.

- **Owner:** `rust/crates/pp-storage/src/send_queue.rs`. Its table is `printer_send_queue`, created in `send_queue/schema.sql` as schema step 44 (`SCHEMA_STEP = 44`). `recover` moves Node's old `printer.send_queue` setting into the table. Readers call `rows(connection, tenant)`. It returns `None` while the old setting still exists, and then the caller reads the setting instead.
- **Writes:** `send_queue/writes.rs` holds `SendQueueClient`, the Send now, Send ready and Remove actions. `send_queue::settle` marks an item `done` or `error`.
- **Read route:** `GET /api/v2/printer-send-queue` in `rust/crates/pp-api/src/farm.rs` (line 56). Its handler `send_queue` (line 101) calls `farm.send_queue()` in `pp-storage/src/read_model/farm.rs:765`. That returns the queue loaded by `read_settings` (line 521), which calls `crate::send_queue::rows(connection, "default")` at line 536.
- **Write routes:** `rust/crates/pp-api/src/send_queue.rs` defines `send_queue_router`. Its routes are `POST …/drain`, `POST …/{id}/dispatch`, `POST …/{id}/reconcile` and `DELETE …/{id}`. They only mount when the `PP_DESKTOP_REBUILD` flag is on. The tests are in `rust/crates/pp-api/tests/ticket_t_26_send_queue_http.rs`, for example `ticket_t_26_send_queue_routes_mount_only_with_the_flag_and_need_a_session`.
- **Other readers of `rows`:** `read_model/farm.rs::queue_list` (line 562), `read_model/context.rs:855`, `read_model/history.rs:634` and `plan_publication.rs:361`.

The Node compatibility server still has its own version in `web/apps/server/src/services/printer-send-queue-store.ts`.

## B19

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

The check is **`rust/scripts/check-boundaries.py`**, run as `python3 rust/scripts/check-boundaries.py --self-test`. - **How it checks:** it runs `cargo metadata --no-deps --locked` on `rust/Cargo.toml`. It asserts that the workspace packages match the keys of the `ALLOWED` dict exactly. Then `violations()` rejects any `pp-*` dependency that isn't in `ALLOWED[package]`. - **Extra rules:** edges listed in `GATED` must be optional and turned on only through the `desktop_rebuild` feature (`rebuild_edge_is_gated()`). `DEV_ONLY = {"pp-testkit"}` may only be a dev-dependency. `TEST_ONLY_FEATURES` (`pp-storage/test-unmarked-writer`) is checked by `test_only_feature_violations()`. - **Self-test:** `planted_edges_rejected()` feeds in fake graphs and confirms they are refused. It always runs, and the script prints `{"packages": N, "allowed_edges": true, "intentional_violation_rejected": true}`. - **Where it runs:** step `check-boundaries` in `.github/workflows/chain-tip-gate.yml` and in `.github/workflows/desktop-runtime.yml`. Locally it is the `boundaries` step listed in `docs/development/ci.md`, together with `check-domain-deps.py --self-test`. **To add a new edge:** 1. Add the crate to the target's set in `ALLOWED` in `rust/scripts/check-boundaries.py`, then add the dependency to that crate's `Cargo.toml`. 2. If it's desktop-rebuild work, also add it to `GATED` and make it `optional` behind `desktop_rebuild = ["dep:..."]`. 3. Update the "Current allowed edges" table in `docs/adr/0007-rust-crate-layout.md`, which is copied from `ALLOWED`. Edges marked "planned" there are documentation only until `ALLOWED` changes. 4. Widening `ALLOWED` counts as weakening a security boundary, so only the owner (Chad) can merge that PR, not the coordinator. This is set in `docs/development/definition-of-done.md` (merge gate) and `docs/development/ci-controls.md`.

## B20

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

A Rust-only PR has no separate rule. It runs the same local CI as every other PR, and how much it runs depends on whether the PR is normal or sensitive.

- **Normal PR:** run `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast` before you push. The rule is in `docs/development/agent-workflow.md:87`, `docs/development/definition-of-done.md:129` and `docs/development/contributing.md:37`.
- **Sensitive PR:** run the full `just ci`. A PR is sensitive if it touches schema or migrations, auth or sessions, backup or restore, Accept or publication, secrets, or protected or owner paths (merge gate item 6 in `definition-of-done.md:42`).
- **What runs it:** the `ci` recipe in `justfile` calls `python3 scripts/local-ci.py`, which holds the only list of steps. `just ci --list` prints the step ids, and `--only` and `--skip` select steps.
- **What `--fast` drops:** `FAST_LEAVES_OUT` (`scripts/local-ci.py:221`) removes `nextest-off`, `nextest-on`, `doc-off` and `doc-on`. `fast_steps()` swaps in `nextest-affected`, which runs nextest with `desktop_rebuild` on, limited to the crates the branch changed and the crates that depend on them. `scripts/ci-affected-nextest-filter.py` builds that filter from the diff against `git merge-base origin/main HEAD`, so fetch `origin` first.
- **Rust steps that still run in the fast set:** `fmt`, `clippy-off`, `clippy-on`, `clippy-limits`, `historical-schema` (`rust/scripts/fetch-historical-schema-commits.sh`), `rust-deps` (`cargo machete`), `web-assets` (rebuilds `rust/bundle-manifest.json`), `ast-grep` and `boundaries`. The table is in `docs/development/ci.md`, under "Run the required checks with one command".
- **After the run:** put the command and its PASS/FAIL/SKIP summary under `## Evidence`, and list every SKIP line. A SKIP is not a pass. `rust (windows)`, `rust (macos)` and `Lint Evidence block` always show as SKIP because they can't run locally.

I only read the docs and scripts; I didn't run anything.

## B21

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

Accept publishes through this Rust path:

- `rust/crates/pp-api/src/drafts/http.rs:313` mounts `POST /plans/{id}/drafts/{draft_id}/apply`, also under `/api/v2`.
- `rust/crates/pp-api/src/drafts/http/lifecycle.rs:135`: `apply` → `Accept::run` → `WorkingDraftClient::publication()` → `PublicationClient::apply`.
- `rust/crates/pp-storage/src/plan_publication.rs:665`: `execute` opens an immediate transaction and calls `publish_in_transaction` (line 1378).
- That transaction publishes revisions, Parts, input sets, Required-unit mappings, progress, accepted pointers, and the replay receipt.

The atomicity proof is `transaction::caller_rollback_and_token_exhaustion_leave_publication_unwritten` in `rust/crates/pp-storage/src/plan_publication/tests.rs:1430`. It drops a successful publish without committing, then commits a token-allocation refusal; all original rows remain unchanged and publication tables remain empty.

## B22

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

Accept goes through `POST /api/v2/plans/{id}/drafts/{draft_id}/apply`. The test that proves it is atomic is `gre_1120_accept_failing_inside_its_transaction_leaves_no_partial_acceptance`. - **Route:** `rust/crates/pp-api/src/drafts/http.rs:313` sends `/plans/{id}/drafts/{draft_id}/apply` to `lifecycle::apply` in `rust/crates/pp-api/src/drafts/http/lifecycle.rs:135`. The request needs an `Idempotency-Key` header (`keyed_write`, same file). - **Publish:** `rust/crates/pp-storage/src/working_drafts/save.rs` opens one SQLite transaction (`transact`, line 484). Inside it, line 663 calls `publication::publish_in_transaction` (`rust/crates/pp-storage/src/plan_publication.rs:1378`). That function writes the Plan revision into a transaction it does not own, mirroring Node's `AppRepository.applyPlanChanges`. - **Atomicity test, end to end:** `rust/crates/pp-api/src/drafts/http/tests/accept_header.rs:231`. It adds a trigger that aborts inserts into `plan_snapshot_headers`, then repeats with `build_profile_details`. In both cases the apply returns 500, the error text doesn't leak, and `logical_database` matches the state before the call. Once the trigger is dropped, the same Accept succeeds and writes exactly one snapshot header. - **Atomicity test, storage only:** `caller_rollback_and_token_exhaustion_leave_publication_unwritten` in `rust/crates/pp-storage/src/plan_publication/tests.rs:1430`. It covers two cases: the caller drops the transaction after a successful publish, and token allocation fails. In both, `plan_revisions`, `parts`, `plan_apply_*`, `plan_revision_input_sets` and `required_units` stay empty. I found these by reading the code; I didn't run either test.

## B23

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

- `rust/scripts/check-boundaries.py` enforces Rust workspace dependency edges through its `ALLOWED` map.
- `violations(packages)` checks `pp-*` dependencies obtained with `cargo metadata --locked --format-version 1 --no-deps`; an unapproved edge fails an assertion.
- `GATED` and `rebuild_edge_is_gated()` additionally enforce optional, default-off `desktop_rebuild` edges.
- `planted_edges_rejected()` checks that deliberately invalid dependency graphs are rejected.
- Run the check with `python3 rust/scripts/check-boundaries.py --self-test`.
- CI runs it in `.github/workflows/chain-tip-gate.yml`; `scripts/local-ci.py` includes it in the `boundaries` step.
- To add `A → B`, add `B` to `ALLOWED["A"]` and declare the dependency in `rust/crates/A/Cargo.toml`.
- For a rebuild-gated edge, also update `GATED` and the manifest’s optional dependency/feature wiring.
- Update dependency documentation, including `docs/architecture.md`, in the same change: its “Crates and allowed edges” section explicitly requires checker, manifests, and documentation to change together.
- Planned edges in `docs/adr/0007-rust-crate-layout.md` grant no permission until `ALLOWED` changes.

## B24

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

Accept is the `POST /plans/{id}/drafts/{draft_id}/apply` route (also served under `/api/v2/...`). It publishes the Plan in Rust, and all paths below are under `rust/crates/`.

- **Route:** `pp-api/src/drafts/http.rs:313` registers it. The handler is `apply` in `pp-api/src/drafts/http/lifecycle.rs:135`, which builds an `Accept` and runs `Accept::run` through `run_write`.
- **Client:** `WorkingDraftClient::publication()` in `pp-storage/src/working_drafts.rs:729` returns a `plan_publication::PublicationClient`.
- **Publish:** `plan_publication::execute` in `pp-storage/src/plan_publication.rs:665` opens one `BEGIN IMMEDIATE` transaction (`writer_admission::begin_immediate`). It calls `publish_in_transaction` (line 1378, a port of Node's `AppRepository.applyPlanChanges`) and commits only on `Applied`, `Existing` or `AlreadyApplied`. Any other result or error rolls the transaction back.

**Atomicity test:** `gre_1120_accept_failing_inside_its_transaction_leaves_no_partial_acceptance` in `pp-api/src/drafts/http/tests/accept_header.rs:231`. It adds a SQLite trigger that aborts inserts into `plan_snapshot_headers`, then into `build_profile_details`. In each case Accept returns 500 and the database matches its state before the call (checked with `logical_database`). After the trigger is dropped, the same Accept succeeds with `revision_id` 2.

There is also a storage-level test, `caller_rollback_and_token_exhaustion_leave_publication_unwritten` in `pp-storage/src/plan_publication/tests.rs:1430`. It shows that a caller rollback, or running out of Required-unit tokens, leaves nothing written.

I only read the code and ran neither test.

## B25

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The key is checked in the Rust storage layer:

- `rust/crates/pp-storage/src/checkoff_progress/burst.rs:515` — `replayed()` finds the stored `idempotency_key`, compares `request_hash`, and returns the saved response. Changed requests, undone bursts, and retired keys receive HTTP 409.
- Same file, `verify_batch():351` — calls `replayed()` before applying printer Checkoff changes.
- Same file, `idempotency_key():64` — trims and validates the key (1–200 characters).
- `rust/crates/pp-storage/src/checkoff_progress/manual.rs:291` — `apply()` calls the same `replayed()` check before manual Checkoff changes.
- `rust/crates/pp-api/src/checkoff.rs` — routes: `POST /printer-checkoff/verify-batch` and `POST /plans/{id}/checkoff/bursts`.
- `rust/crates/pp-storage/tests/checkoff_bursts.rs:130` — `verify_batch_replays_the_receipt_and_refuses_a_changed_body`.
- Same test file: `verify_batch_refuses_a_key_whose_receipt_was_trimmed` and `verify_batch_resend_after_undo_is_refused_and_writes_nothing`.

## B26

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

**Check:** the dependency edges are enforced by `rust/scripts/check-boundaries.py`. You run it with `python3 rust/scripts/check-boundaries.py --self-test`. Locally it is the `boundaries` step in `scripts/local-ci.py`, so `just ci --only boundaries` runs it. In hosted CI it is the `check-boundaries` step in `.github/workflows/chain-tip-gate.yml`, and `.github/workflows/desktop-runtime.yml` also runs it. **How it works:** - It reads `cargo metadata --no-deps --locked`. It asserts that the workspace crates are exactly the keys of the `ALLOWED` map. The `violations()` function then rejects any `pp-*` dependency that isn't listed under its crate in `ALLOWED`. - `GATED` lists edges that must be optional and turned on only through the `desktop_rebuild` feature. `rebuild_edge_is_gated()` checks this. - `DEV_ONLY` (`pp-testkit`) is allowed only as a dev-dependency. `test_only_feature_violations()` guards `TEST_ONLY_FEATURES`. - `planted_edges_rejected()` feeds in fake bad graphs. If any of them passes, the script exits with "intentional boundary violation was not rejected". **Adding a new edge:** 1. Add the target crate to the source crate's set in `ALLOWED`. If the edge belongs to the desktop rebuild, also add it to `GATED` and make it optional behind `desktop_rebuild` in the crate's `Cargo.toml`. 2. Add the dependency to the crate's `Cargo.toml`. 3. Update the edge docs in the same PR: "Current allowed edges" in `docs/adr/0007-rust-crate-layout.md` and "Crates and allowed edges" in `docs/architecture.md`. The architecture doc says a new edge "must update the checker, manifests, and dependency documentation together". Planned edges in ADR 0007 are documentation only until `ALLOWED` changes. The architecture doc also says not to assume a new `pp-services -> pp-storage/net/drivers` edge. Crate dependencies on `pp-domain` have a separate guard, `rust/scripts/check-domain-deps.py`, which runs in the same CI step.

## B27

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

A Rust-only PR runs `just ci --fast` before each push, or the full `just ci` if it counts as sensitive. Both commands run `scripts/local-ci.py`.

- **Normal PR:** `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast`. It runs every step except `nextest-off`, `nextest-on`, `doc-off` and `doc-on` (`docs/development/ci.md`, "Run the required checks with one command").
  - In place of `nextest-on` it runs `nextest-affected`: nextest with `desktop_rebuild` on, limited to the changed crates and the crates that depend on them.
  - `scripts/ci-affected-nextest-filter.py` picks those crates from the diff since `git merge-base origin/main HEAD`. Fetch `origin` first. A change to `rust/Cargo.lock`, `web/` or `fixtures/` makes it test everything (`all()`).
- **Sensitive PR:** the full `just ci`. That adds nextest and doctests with `desktop_rebuild` both off and on. Sensitive means it touches schema or migrations, auth and sessions, backup and restore, Accept and publication, secrets or the protected paths (`docs/development/definition-of-done.md`, Merge gate item 6).
- **Steps that matter most for Rust:** `fmt`, `clippy-off`, `clippy-on` (`-D warnings`), `clippy-limits`, `historical-schema` (`rust/scripts/fetch-historical-schema-commits.sh`), `ast-grep`, `boundaries`, `rust-deps` (`cargo machete`) and `web-assets` (`rust/bundle-manifest.json`). `just ci --list` prints the step ids, and `--only` and `--skip` take them.
- **Not covered by `just ci`:** run `cargo deny --manifest-path rust/Cargo.toml --all-features check` yourself. CI runs it on any change under `rust/**`.
- **Results:** fix every `FAIL` before pushing. A `SKIP` (missing tool, wrong pinned version, or Node 22.23.3 not set) is not a pass, so list it under `## Evidence` (`docs/contributing/gate-evidence.md`, "Run CI locally before you push"). `rust (windows)` and `rust (macos)` always show as `SKIP` locally.

## B28

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

- `docs/development/agent-workflow.md`: before each push, a normal Rust-only PR runs `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast`.
- Without `just`, use `python3 scripts/local-ci.py --fast`.
- `docs/development/definition-of-done.md`, merge-gate item 6: sensitive changes require full `just ci`; examples include schema, migrations, auth, backup, publication and secret handling.
- `scripts/local-ci.py`: `STEPS` defines the checks; `fast_steps()` retains everything except full nextest and doctest passes.
- Fast mode runs `nextest-affected` with `desktop_rebuild` enabled, covering changed crates and reverse dependents.
- `scripts/ci-affected-nextest-filter.py` defines that affected-crate selection.
- Checks include fmt, Clippy in both feature modes, boundaries, ratchets, dependency checks, web assets and Node fixture freshness—even for Rust-only changes.
- `docs/development/ci.md` also lists uncovered dependency checks: `cargo deny --manifest-path rust/Cargo.toml --all-features check` and `cargo deny --manifest-path rust/fuzz/Cargo.toml --all-features --locked check`.
- `docs/contributing/gate-evidence.md`: fix every `FAIL`; record the command, summary and every `SKIP` under `## Evidence`. A skip is not a pass.
- `docs/development/agent-workflow.md`: runners without the guard hook must invoke `scripts/agent-guard.py` before pushing.

## B29

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

In this checkout, Accept publishes a Plan through the HTTP route `POST /api/v2/plans/:id/drafts/:draftId/apply` (the Rust core). The test that shows it's atomic is `caller_rollback_and_token_exhaustion_leave_publication_unwritten`.

**Where the publish happens**
- **Route:** registered at `rust/crates/pp-api/src/drafts/http.rs:313` as `"/plans/{id}/drafts/{draft_id}/apply"`. The handler is `apply` at `rust/crates/pp-api/src/drafts/http/lifecycle.rs:135`.
- **Client entry point:** `PublicationClient::apply` at `rust/crates/pp-storage/src/plan_publication.rs:166` puts the command on the single writer queue.
- **Transaction:** `execute` (around line 670 of the same file) opens one transaction with `writer_admission::begin_immediate`. It then calls `publish_in_transaction` at `plan_publication.rs:1378`, which is the Rust port of Node's `AppRepository.applyPlanChanges`.
- **Commit:** `tx.commit()` runs only when the outcome is `Applied`, `Existing` or `AlreadyApplied`. Any other outcome, or an error, drops the transaction and rolls it back.
- The pre-Accept snapshot is saved inside the same transaction, before the publication writes, by `save_before_accept` in `plan_publication/snapshot.rs:74`.

**The atomicity test**
- `caller_rollback_and_token_exhaustion_leave_publication_unwritten` at `rust/crates/pp-storage/src/plan_publication/tests.rs:1430`, inside `mod transaction`.
- First it runs a successful publish and drops the transaction without committing.
- Then it makes the token allocator fail on every call, so the publish returns `token_allocation_failed` after 32 tries, and commits that transaction anyway.
- It then checks that every table still matches its `before` rows, and that `plan_revisions`, `parts`, `plan_apply_requests`, `plan_apply_admissions`, `plan_revision_input_sets` and `required_units` are empty.

I only read the code and didn't run the test.

## B30

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The authoritative check is in Rust storage: - `rust/crates/pp-storage/src/checkoff_progress/burst.rs:351` — `verify_batch()` loads tenant receipts and calls `replayed()` before applying changes. - Same file, line 515 — `replayed()` matches `idempotency_key`, then compares the canonical `request_hash`. - Matching requests return the stored receipt; changed bodies, undone bursts, and retired keys return HTTP 409. - Same file, line 64 — `idempotency_key()` validates the trimmed key’s length: 1–200 characters. - `rust/crates/pp-api/src/checkoff.rs:128` — `verify_batch()` routes `POST /printer-checkoff/verify-batch` into `Request::VerifyBatch`; also mounted under `/api/v2`. - `rust/crates/pp-storage/src/checkoff_progress/manual.rs:300` — manual bursts reuse `replayed()`. - `rust/crates/pp-storage/tests/checkoff_bursts.rs` — tests `verify_batch_replays_the_receipt_and_refuses_a_changed_body`, `verify_batch_refuses_a_key_whose_receipt_was_trimmed`, and `verify_batch_resend_after_undo_is_refused_and_writes_nothing`. - `rust/crates/pp-api/tests/checkoff_bursts_http.rs` — `verify_batch_http_replays_across_restart_and_undoes_once` verifies the HTTP path.

## B31

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

**Where Accept publishes:** Accept is the route `POST /plans/{id}/drafts/{draft_id}/apply`, registered in `rust/crates/pp-api/src/drafts/http.rs:313` and handled by `lifecycle::apply` (`rust/crates/pp-api/src/drafts/http/lifecycle.rs:135`). The handler hands off to `rust/crates/pp-storage/src/plan_publication.rs`: - `execute` (around line 665) opens one transaction with `writer_admission::begin_immediate`, calls `publish_in_transaction` (line 1378) and commits only when the outcome is `Applied`, `Existing` or `AlreadyApplied`. - `publish_in_transaction` writes the new Accepted Plan revision and emits `plan.revision_accepted` (see `docs/domain-events.md:19`). Draft save-and-accept also calls it, from `rust/crates/pp-storage/src/working_drafts/save.rs:663`. **Tests that prove it's atomic:** - **End-to-end over HTTP:** `gre_1120_accept_failing_inside_its_transaction_leaves_no_partial_acceptance` in `rust/crates/pp-api/src/drafts/http/tests/accept_header.rs:231`. It installs a `RAISE(ABORT)` trigger on `plan_snapshot_headers`, then on `build_profile_details`, and posts to `/api/v2/plans/1/drafts/2/apply`. It expects a 500 that doesn't leak the error text, and checks that `logical_database` is unchanged. A retry after dropping the trigger then succeeds (`revision_id` 2). - **Storage level:** `caller_rollback_and_token_exhaustion_leave_publication_unwritten` in `rust/crates/pp-storage/src/plan_publication/tests.rs:1430`. A rolled-back caller transaction, and a run where Required-unit token allocation fails, both leave `plan_revisions`, `parts`, `required_units` and the apply tables empty.

## B32

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The authoritative check is in Rust storage: - `rust/crates/pp-storage/src/checkoff_progress/burst.rs:515` — `replayed()` finds the stored `idempotency_key`, compares `request_hash`, and checks retired key hashes. - Matching requests return the stored receipt; changed bodies, undone bursts, and retired keys return 409. - Same file, `verify_batch()` at line 351 calls this check inside the transaction before applying beds. - Same file, `idempotency_key()` at line 64 validates the key: JS-trimmed, 1–200 characters; `Burst::parse()` calls it. - `rust/crates/pp-api/src/checkoff.rs:128` — `verify_batch()` handles `POST /printer-checkoff/verify-batch` and submits `Request::VerifyBatch`. - `rust/crates/pp-storage/src/checkoff_progress/manual.rs:291` — manual burst `apply()` also uses `burst::replayed()`. - `rust/crates/pp-storage/tests/checkoff_bursts.rs:130` — `verify_batch_replays_the_receipt_and_refuses_a_changed_body()` tests replay/conflict. - `rust/crates/pp-api/tests/checkoff_bursts_http.rs:46` — `verify_batch_http_replays_across_restart_and_undoes_once()` tests persistence across restart.

## B33

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

- **Enforcement:** `rust/scripts/check-boundaries.py`, specifically `violations(packages)`, checks `pp-*` dependencies against the `ALLOWED` map.
- It reads `cargo metadata --locked --format-version 1 --no-deps` and fails on forbidden edges or unlisted workspace packages.
- `GATED` and `rebuild_edge_is_gated()` additionally enforce optional dependencies activated only through default-off `desktop_rebuild`.
- `planted_edges_rejected()` verifies that deliberately invalid dependency graphs fail.
- **CI:** `.github/workflows/chain-tip-gate.yml`, step `check-boundaries`, runs `python3 rust/scripts/check-boundaries.py --self-test`.
- **Local CI:** `scripts/local-ci.py` includes that command in its `boundaries` step.
- **Adding an edge:** update the consumer’s `rust/crates/<crate>/Cargo.toml` and its entry in `ALLOWED`; update `GATED` when applicable.
- Update dependency documentation alongside those changes, including `docs/architecture.md` under **Crates and allowed edges**.
- That section explicitly requires checker, manifests, and dependency documentation to change together; new edges must respect the documented ownership constraints.

## B34

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

- Accept uses `POST /plans/{id}/drafts/{draft_id}/apply`, registered in `rust/crates/pp-api/src/drafts/http.rs`.
- `lifecycle::apply` → `Accept::run` in `rust/crates/pp-api/src/drafts/http/lifecycle.rs` calls `PublicationClient::apply`.
- Rust publishes in `rust/crates/pp-storage/src/plan_publication.rs`: `execute` opens an immediate transaction and calls `publish_in_transaction`, then commits successful outcomes.
- Rust atomicity proof: `transaction::caller_rollback_and_token_exhaustion_leave_publication_unwritten` in `rust/crates/pp-storage/src/plan_publication/tests.rs` verifies rollback leaves stored rows unchanged.
- Node compatibility publication is `AppRepository.applyPlanChanges` in `web/apps/server/src/db/repository.ts`.
- The explicit failure-injection proof is **`"rolls back every publication write and retries cleanly"`**, in `web/apps/server/src/db/plan-drafts.test.ts:3478`.
- It aborts insertion into `plan_apply_requests`, asserts every publication table equals its prior snapshot, then removes the trigger and verifies a successful retry with exactly one receipt.

## B35

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

- `docs/development/agent-workflow.md:87`: before each push, a normal Rust-only PR runs `just ci --fast`; Rust-only changes have no exemption. - `justfile:5`: `ci` invokes `python3 scripts/local-ci.py` with the same arguments. - `docs/development/ci.md:300`: use `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast`. - `scripts/local-ci.py`: includes `fmt`, `clippy-off`, `clippy-on` (`-D warnings`), boundary checks, ratchets, assets and parity checks. - `docs/development/ci.md:303`: fast mode replaces full nextest passes with `nextest-affected`, using `desktop_rebuild` for changed crates and reverse dependents; it omits doctests. - `docs/development/definition-of-done.md`, “Merge gate,” item 6: sensitive changes—including schema, auth, backup, secrets and protected paths—require full `just ci`. - `docs/development/ci.md`: Rust changes also trigger checks outside that wrapper: `cargo deny --manifest-path rust/Cargo.toml --all-features check` and the corresponding `rust/fuzz/Cargo.toml --all-features --locked check`. - `docs/contributing/gate-evidence.md:74`: fix every `FAIL`; record the command, results and every `SKIP` under `## Evidence`. Exit 0 with skips is not a complete pass. - `docs/development/agent-workflow.md:102`: runners without the guard hook must run `scripts/agent-guard.py command` against the intended push command first.

## B36

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

No separate rule exists for Rust-only PRs. They follow the same rule as any PR: a normal PR runs the fast local CI set before each push, and a sensitive PR runs the full set. - **Normal PR:** run `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast` (`docs/development/ci.md` "Run the required checks with one command", `docs/contributing/gate-evidence.md` "Run CI locally before you push", `docs/development/definition-of-done.md` line 129). `just ci` passes its arguments to `python3 scripts/local-ci.py`, which holds the only list of steps (`STEPS`). - **What `--fast` does:** `fast_steps()` in `scripts/local-ci.py` keeps every step except `nextest-off`, `nextest-on`, `doc-off` and `doc-on`. In place of `nextest-on` it runs `nextest-affected`: nextest with `desktop_rebuild` on, limited to the crates the branch changes and the crates that depend on them. `affected_nextest_filter()` builds that list using the rules in `scripts/ci-affected-nextest-filter.py`, from the diff between `git merge-base origin/main HEAD` and the working tree. A change to `rust/Cargo.toml`, `Cargo.lock`, the toolchain or `rust/tests/**` runs all tests. Fetch `origin` first so the merge base is current. - **Steps that matter for Rust:** `fmt`, `clippy-off` and `clippy-on` (`-D warnings`), `clippy-limits`, `ast-grep`, `boundaries`, `rust-deps`, `historical-schema`, `workaround-ratchet` and `duplication`. - **Sensitive PR:** run the full `just ci`, which also runs `nextest-off`, `nextest-on`, `doc-off` and `doc-on`. Item 6 of the merge gate in `definition-of-done.md` defines a sensitive PR: schema or migrations, auth and sessions, backup and restore, Accept and publication, secrets, or the CODEOWNERS paths such as `rust/rules/`, `rust/deny.toml` and the lint baselines. - **Evidence:** list the run under `## Evidence` in the PR body, including every `SKIP` line. A SKIP is not a pass. `AGENTS.md` also lists `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` among its checks.

## B37

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

- **Owner:** `pp_storage::send_queue` in `rust/crates/pp-storage/src/send_queue.rs` owns the `printer_send_queue` table. - **Schema:** `rust/crates/pp-storage/src/send_queue/schema.sql` creates it; `SCHEMA_STEP = 44`. - **Storage reader:** `send_queue::rows()` selects tenant rows ordered by `position`. - **Read route:** `GET /api/v2/printer-send-queue`. - **Route registration:** `farm_router()` in `rust/crates/pp-api/src/farm.rs` registers `get(send_queue)` under `/api/v2`. - **Handler:** `send_queue()` returns `Farm::send_queue()` through the shared farm read. - **Read model:** `rust/crates/pp-storage/src/read_model/farm.rs` connects `execute()` → `queue_list()` → `send_queue::rows()`. - **Compatibility:** `queue_list()` reads the legacy `printer.send_queue` setting while it exists. - **Test:** `ticket_t_26_a_legacy_setting_wins_over_table_rows` in `rust/crates/pp-storage/src/send_queue/tests.rs` verifies that precedence.

## B38

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The key is checked in two steps, both in `rust/crates/pp-storage/src/checkoff_progress/burst.rs`:

1. **Format check:** `idempotency_key()` (burst.rs:64). It trims the key the way JavaScript does and requires 1 to 200 characters; otherwise it returns 400 "idempotency_key must be 1 to 200 characters". Three places call it:
   - `Burst::parse` (burst.rs:79), for `POST /printer-checkoff/verify-batch`.
   - The manual-burst parser `parse` in `checkoff_progress/manual.rs:64`, at line 75.
   - The `Request::BurstStatus` arm in `checkoff_progress.rs:618`, for `GET /printer-checkoff/verify-batch?idempotency_key=...`. The HTTP handler is `burst_status` in `rust/crates/pp-api/src/checkoff.rs:143`, which pulls the query parameter.
2. **Replay check:** `replayed()` (burst.rs:515). It runs inside the transaction and looks for the key among the stored receipts:
   - Same request hash: it returns the stored receipt.
   - Different hash: 409 "idempotency_key was already used for a different request".
   - Burst was undone: 409 `burst_undone`.
   - Key is in the retired list (`printer.checkoff_bursts_retired`): 409 "…too old to replay".

   `verify_batch` (burst.rs:358) and the manual `apply` (manual.rs:300) call it.

The TypeScript schema in `web/packages/contracts/src/checkoff-burst.ts:30` (`z.string().trim().min(1).max(200)`) applies the same limit on the client side.

Tests:
- `verify_batch_replays_the_receipt_and_refuses_a_changed_body` and `verify_batch_refuses_a_key_whose_receipt_was_trimmed` in `rust/crates/pp-storage/tests/checkoff_bursts.rs`
- `manual_burst_replays_after_restart_and_refuses_a_changed_body` in `rust/crates/pp-storage/tests/checkoff_manual_bursts.rs`
- `verify_batch_http_replays_across_restart_and_undoes_once` in `rust/crates/pp-api/tests/checkoff_bursts_http.rs`

## B39

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

The table is owned by **`pp_storage::send_queue`**; its read route is **`GET /api/v2/printer-send-queue`**.

- `rust/crates/pp-storage/src/send_queue.rs`: `rows()` selects from `printer_send_queue`, ordered by `position`.
- `rust/crates/pp-storage/src/send_queue/schema.sql`: defines the table, installed at schema step 44.
- `rust/crates/pp-storage/src/read_model/farm.rs`: `queue_list()` calls `crate::send_queue::rows()`; `Farm::send_queue()` exposes the response.
- `rust/crates/pp-api/src/farm.rs`: `farm_router()` registers the route; async handler `send_queue()` returns `farm.send_queue()`.
- `docs/send-queue.md`: documents ownership and the legacy behavior: while `printer.send_queue` exists in settings, readers use that setting instead of table rows.

The Rust implementation is behind the default-off `desktop_rebuild` feature and runtime gate.

## B40

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The check is in **`rust/crates/pp-storage/src/checkoff_progress/burst.rs`**, function **`replayed()`** (line 515).

- It finds a stored receipt by `idempotency_key` and compares `request_hash`.
- Same key and request returns the stored response; a different request returns **409**.
- Undone bursts and keys whose receipts were retired also return **409**.
- `verify_batch()` in that file calls it before applying any bed changes.
- Route: `POST /printer-checkoff/verify-batch`, represented by `Request::VerifyBatch` in `rust/crates/pp-storage/src/checkoff_progress.rs`.
- Manual bursts share the check: `apply()` in `rust/crates/pp-storage/src/checkoff_progress/manual.rs`, for `POST /plans/{id}/checkoff/bursts`.
- Key format validation is `idempotency_key()` in `burst.rs`: trimmed, 1–200 characters.
- Printer test: `verify_batch_replays_the_receipt_and_refuses_a_changed_body` in `rust/crates/pp-storage/tests/checkoff_bursts.rs`.
- Manual test: `manual_burst_replays_after_restart_and_refuses_a_changed_body` in `rust/crates/pp-storage/tests/checkoff_manual_bursts.rs`.

## B41

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The key is checked in Rust storage, in `rust/crates/pp-storage/src/checkoff_progress/burst.rs`. The check happens in two steps: 1. **Format check:** `idempotency_key()` (burst.rs:64) trims the key the way JavaScript does and requires 1 to 200 characters. Otherwise it returns 400 `idempotency_key must be 1 to 200 characters`. Three places call it: - `Burst::parse` (burst.rs:81), for the multi-bed `POST /printer-checkoff/verify-batch`. - The manual-burst parser (`manual.rs:75`), for `POST /plans/{id}/checkoff/bursts`. - `validate()` in `rust/crates/pp-storage/src/checkoff_progress.rs:618`, for the status lookup `GET /printer-checkoff/verify-batch?idempotency_key=...`. The lookup itself is `lookup::burst_status` in `lookup.rs:27`. 2. **Replay check:** `replayed()` (burst.rs:515) runs inside the transaction before any write. `verify_batch()` calls it at burst.rs:358 and `manual::apply` at manual.rs:300. - If the key's stored receipt has a different `request_hash`, it returns 409 `already used for a different request`. - If the burst was undone, it returns 409 `burst_undone`. - If the request matches, it returns the stored receipt and applies nothing again. - If the key's hash is in the retired list, it returns 409 `too old to replay`. The lists are `printer.checkoff_bursts_retired` and `checkoff.manual_bursts_retired`. MCP's `confirm_apply` sends its burst under the key `mcp:<action_id>`; the prefix is defined in `rust/crates/pp-api/src/mcp/checkoff.rs:21`. The tests are in `rust/crates/pp-storage/tests/checkoff_bursts.rs`: - `verify_batch_replays_the_receipt_and_refuses_a_changed_body` - `verify_batch_refuses_a_key_whose_receipt_was_trimmed` - `verify_batch_resend_after_undo_is_refused_and_writes_nothing` The HTTP-level tests are `pp-api/tests/checkoff_bursts_http.rs` and `checkoff_manual_bursts_http.rs`.

## B42

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

- A normal Rust-only PR runs `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast` before every push (`docs/contributing/gate-evidence.md:74`).
- A sensitive PR runs the full `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci`.
- Sensitive includes schema/migrations, auth/sessions, backup/restore, Accept/publication, secrets, and protected paths (`docs/development/definition-of-done.md:42`).
- `justfile` delegates to `scripts/local-ci.py`; without `just`, use `python3 scripts/local-ci.py --fast` or omit `--fast`.
- Both modes include `fmt`, `clippy-off`, `clippy-on`, boundary checks, ratchets, assets, and fixture checks (`docs/development/ci.md:307`).
- Fast mode runs `nextest-affected` with `desktop_rebuild` enabled, covering changed crates and reverse dependents (`scripts/ci-affected-nextest-filter.py`).
- Full mode additionally runs `nextest-off`, `nextest-on`, `doc-off`, and `doc-on` (`scripts/local-ci.py:221`).
- Rust-only does not automatically exempt the web-assets or Node parity steps; keep Node 24.21.0 and parity Node 22.23.3 available.
- `cargo deny` is outside that runner: `docs/development/ci.md` lists `cargo deny --manifest-path rust/Cargo.toml --all-features check` and the fuzz-manifest check.
- Fix every `FAIL`; record the command and every `SKIP` under `## Evidence`. A skipped check is not a pass (`docs/contributing/gate-evidence.md:74`).

## B43

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

A Rust-only PR follows the same local pre-push rule as any other PR. The docs don't define a separate rule for Rust-only changes. What you run depends on whether the PR is normal or sensitive.

- **Normal PR:** run `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast` (or `python3 scripts/local-ci.py --fast`). Sources: `docs/development/definition-of-done.md` (Evidence section), `docs/development/agent-workflow.md:87` and `docs/contributing/gate-evidence.md` ("Run CI locally before you push").
- **What `--fast` runs:** every step except `nextest-off`, `nextest-on`, `doc-off` and `doc-on`. In place of `nextest-on` it runs `nextest-affected`, which is `cargo nextest run --no-fail-fast --no-tests=warn -E "$NEXTEST_FILTER" --features desktop_rebuild`. The rest still runs: fmt, clippy `-D warnings` with `desktop_rebuild` off and on, the boundary, ast-grep, clippy-limits and workaround-ratchet checks, and the parity-fixture checks. This is defined in `scripts/local-ci.py` (`FAST_LEAVES_OUT`, `fast_steps()`, `affected_nextest_filter()`) and described in `docs/development/ci.md:303`.
- **The test filter:** it comes from `scripts/ci-affected-nextest-filter.py`. It covers each changed crate plus the crates that depend on it (`rdeps(=<pkg>)`), measured from `git merge-base origin/main HEAD`. Uncommitted edits count. Changes to `rust/Cargo.lock`, `rust/scripts/**`, `rust/tests/**` or `fixtures/` switch it to all tests (`all()`). Fetch `origin` first so the merge base is current.
- **Sensitive PR:** run the full `just ci`, which runs nextest and doctests with `desktop_rebuild` both off and on. A PR is sensitive if it touches schema or migrations, auth, backup, Accept, secrets, CI, or the paths owned by CODEOWNERS (item 6 of the merge gate in `definition-of-done.md`).
- **Evidence:** put the command and its PASS/FAIL/SKIP summary under `## Evidence`, including every SKIP line. Fix every FAIL before you push. A change under `rust/crates/pp-desktop/` also needs a screenshot or video (`gate-evidence.md`).
- **Before running CI:** `AGENTS.md` also lists `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` and `just guidance`.

## B44

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The key is checked in Rust storage, in `rust/crates/pp-storage/src/checkoff_progress/burst.rs`, at two levels: - **Format check:** `idempotency_key()` (`burst.rs:64`) trims the key and accepts 1 to 200 characters. Anything else gets a 400, "idempotency_key must be 1 to 200 characters". Three places call it: - the burst body parser, `burst.rs:81`; - the manual burst parser, `manual.rs:75`; - the status lookup, in `checkoff_progress.rs:618` (`Request::BurstStatus` → `Op::BurstStatus`). - **Replay check:** `replayed()` (`burst.rs:515`) compares the key against the stored receipts. The same key with the same `request_hash` returns the stored receipt. A different hash gets a 409, "already used for a different request". A key that was undone is refused (manual bursts only). A key in the retired list gets a 409, "too old to replay". Two functions call it: `verify_batch()` at `burst.rs:358` and the manual `apply()` at `manual.rs:300`. - **Status lookup:** `lookup::burst_status()` (`lookup.rs:27`) reports applied, undone, expired, unknown or not found for a key. - **HTTP routes** (`rust/crates/pp-api/src/checkoff.rs:74`, `checkoff_router`): `POST /printer-checkoff/verify-batch` (`verify_batch`), `POST /plans/{id}/checkoff/bursts` (`manual_burst::<false>`), and `GET /printer-checkoff/verify-batch?idempotency_key=` (`burst_status`). `burst_status` itself returns a 400 unless the key appears exactly once in the query (`checkoff.rs:143`). - **MCP:** confirmed proposals use a key made from `KEY_PREFIX` plus the action id (`pp-api/src/mcp/checkoff.rs:21,139`). - **Tests** in `rust/crates/pp-storage/tests/checkoff_bursts.rs`: - `verify_batch_replays_the_receipt_and_refuses_a_changed_body` - `verify_batch_refuses_a_key_whose_receipt_was_trimmed` - `verify_batch_refuses_malformed_bursts_before_any_write` - `verify_batch_resend_after_undo_is_refused_and_writes_nothing` HTTP tests are in `pp-api/tests/checkoff_bursts_http.rs` and `checkoff_manual_bursts_http.rs`.

## B45

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

The check is **`rust/scripts/check-boundaries.py`**. You run it as `python3 rust/scripts/check-boundaries.py --self-test`, and in CI it runs as the `check-boundaries` step of `.github/workflows/chain-tip-gate.yml`. `docs/development/ci.md` lists it under the `boundaries` row, next to `check-domain-deps.py --self-test`. - **How it works:** it reads the crate graph with `cargo metadata --no-deps --locked` and compares each `pp-*` dependency against the `ALLOWED` map (line 6). Every workspace crate must appear in `ALLOWED`. `GATED` lists edges that must be optional and reachable only through the `desktop_rebuild` feature; `rebuild_edge_is_gated()` checks that. `DEV_ONLY = {"pp-testkit"}` may only appear under `[dev-dependencies]`, and `TEST_ONLY_FEATURES` covers `pp-storage/test-unmarked-writer`. `violations()` and `test_only_feature_violations()` report what's wrong. `planted_edges_rejected()` feeds in fake graphs to prove a bad edge is refused. - **How to add an edge:** 1. Add the target crate to the source crate's set in `ALLOWED` in `check-boundaries.py`. 2. If the edge belongs to the desktop rebuild, also add it to `GATED`. It must then be an optional dependency enabled only by `desktop_rebuild = ["dep:<crate>", ...]`. 3. Add the dependency to the crate's `Cargo.toml`. 4. Update the "current" edge list in `docs/adr/0007-rust-crate-layout.md` (line 39 says it is copied from `ALLOWED`). Edges marked "planned" there are documentation only until `ALLOWED` changes. 5. Rerun `check-boundaries.py --self-test`. - **Who merges:** `check-boundaries.py` is a CODEOWNERS path. `docs/development/definition-of-done.md` (merge gate) and `docs/development/ci-controls.md` say that a PR that **widens `ALLOWED`** is merged by the owner, not the coordinator. Tightening an edge is the coordinator's to merge as a sensitive PR.

## B46

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The key is checked in Rust storage, before any burst mutations:

- `rust/crates/pp-storage/src/checkoff_progress/burst.rs:351` — `verify_batch()` loads tenant receipts and calls `replayed()`.
- Same file, line 515 — `replayed()` matches `idempotency_key` and compares `request_hash`: identical requests return the saved receipt; changed requests return 409.
- It also refuses undone bursts and keys whose receipts expired, checking retired key hashes.
- Same file, line 64 — `idempotency_key()` validates the trimmed key; `Burst::parse()` calls it.
- `rust/crates/pp-api/src/checkoff.rs` — `checkoff_router()` exposes `POST /printer-checkoff/verify-batch`; handler `verify_batch()` submits `Request::VerifyBatch`.
- `rust/crates/pp-storage/tests/checkoff_bursts.rs` — `verify_batch_replays_the_receipt_and_refuses_a_changed_body()` tests replay/conflict behavior.
- That test file also covers expired keys and resend after undo.
- `rust/crates/pp-api/tests/checkoff_bursts_http.rs` — `verify_batch_http_replays_across_restart_and_undoes_once()` verifies persistence across restart.

## B47

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

- The enforcing check is `rust/scripts/check-boundaries.py`, specifically `violations(packages)` and its `ALLOWED` map. - It reads the workspace through `cargo metadata --locked --format-version 1 --no-deps` and rejects unlisted `pp-*` dependency edges. - `planted_edges_rejected()` verifies that intentional violations are rejected. - CI runs `python3 rust/scripts/check-boundaries.py --self-test` in `.github/workflows/chain-tip-gate.yml`, step `check-boundaries`. - `scripts/local-ci.py` includes that command in its `boundaries` step. - Add a new edge to the source crate’s set in `ALLOWED` and declare the dependency in `rust/crates/<source>/Cargo.toml`. - Update the dependency documentation in `docs/architecture.md` together with the checker and manifests, as that document requires. - For an edge requiring rebuild gating, update `GATED`; `rebuild_edge_is_gated()` requires an optional dependency activated only by default-off `desktop_rebuild`. - Planned edges in `docs/adr/0007-rust-crate-layout.md` grant no permission until `ALLOWED` changes.

## B48

**Question:** Which module owns the send-queue table, and which route reads it?

**Answer:**

The table belongs to **`pp_storage::send_queue`**, and **`GET /api/v2/printer-send-queue`** reads it.

- `rust/crates/pp-storage/src/send_queue.rs`: `rows()` selects from `printer_send_queue`, ordered by `position`.
- `rust/crates/pp-storage/src/send_queue/schema.sql`: defines the table and `printer_send_queue_one_sending` index.
- `rust/crates/pp-storage/src/read_model/farm.rs`: `queue_list()` calls `send_queue::rows()`; `send_queue()` returns `{items}`.
- `rust/crates/pp-api/src/farm.rs`: `farm_router()` registers the route under `/api/v2`; its handler is `send_queue()`.
- Legacy behavior: `queue_list()` reads the `printer.send_queue` setting while that setting exists.
- `rust/crates/pp-storage/src/send_queue/tests.rs`: `ticket_t_26_a_legacy_setting_wins_over_table_rows` verifies that precedence.
- `docs/send-queue.md`: documents table ownership, schema step 44, and the read route.

All paths are relative to the specified checkout.

## B49

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

- Normal Rust-only PR: run `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast` before each push (`docs/contributing/gate-evidence.md`). - Sensitive PR: run full `just ci`; sensitivity includes schema/migrations, auth/sessions, backup/restore, Accept/publication, secrets and protected paths (`docs/development/definition-of-done.md`, merge-gate item 6). - `justfile` delegates to `python3 scripts/local-ci.py`; that command accepts the same flags without `just`. - `scripts/local-ci.py::STEPS` is the authoritative check list; `just ci --list` lists its steps. - `scripts/local-ci.py::fast_steps()` retains both `clippy-off` and `clippy-on`, including `-D warnings`. - Fast mode runs `nextest-affected` with `desktop_rebuild`; full mode runs nextest and doctests with the feature off and on. - Rust-only changes still run asset/UI checks, ratchets and Node parity freshness checks; fast mode only removes the full nextest/doctest passes (`docs/development/ci.md`). - Fix every `FAIL`; record the command and every `SKIP` under PR `## Evidence` (`docs/contributing/gate-evidence.md`). - `cargo deny` is outside the wrapper: local commands are `cargo deny --manifest-path rust/Cargo.toml --all-features check` and the fuzz equivalent with `rust/fuzz/Cargo.toml --locked` (`docs/development/ci.md`). - Before pushing, runners without an automatic guard must invoke `python3 -B scripts/agent-guard.py command --cwd "$PWD" '<push command>'` (`docs/development/agent-workflow.md`).

## B50

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

- Accept enters through `POST /api/v2/plans/{id}/drafts/{draft_id}/apply`, registered in `rust/crates/pp-api/src/drafts/http.rs`. - The handler is `apply` in `rust/crates/pp-api/src/drafts/http/lifecycle.rs`; it calls `PublicationClient::apply`. - Publication lives in `rust/crates/pp-storage/src/plan_publication.rs`: `execute` opens an immediate SQLite transaction and calls `publish_in_transaction`. - That function writes the immutable revision, Parts, Required units, progress, accepted pointer/version, draft consumption, receipt and domain event together. - The atomicity proof is `transaction::caller_rollback_and_token_exhaustion_leave_publication_unwritten` in `rust/crates/pp-storage/src/plan_publication/tests.rs:1430`. - It drops an applied transaction without committing, then forces token allocation failure and commits that refusal. - It asserts the original rows remain unchanged and publication tables remain empty. - Read-only inspection only; no tests were run.

## B51

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The key is checked in `rust/crates/pp-storage/src/checkoff_progress/burst.rs`, in two steps: 1. **Format check:** `idempotency_key()` (line 64) trims the key and requires 1 to 200 characters. If it fails, the request gets a 400. It runs from `Burst::parse` (line 81) and from the `burst_status` query parse in `checkoff_progress.rs:618`. 2. **Replay check:** `replayed()` (line 515) looks for a stored receipt with the same key. - If the request hash matches, it returns the stored receipt and writes nothing. - If the hash differs, it refuses with 409 "already used for a different request". - It also refuses with 409 if the burst was undone (`undone_at`) or if the key's hash is in the retired-key list ("too old to replay"). `replayed()` is called from: - `verify_batch()` in `burst.rs:350`, which serves `POST /printer-checkoff/verify-batch`. - `apply()` in `checkoff_progress/manual.rs:291`, which serves `POST /plans/{id}/checkoff/bursts`. The HTTP routes are set up in `checkoff_router` (`rust/crates/pp-api/src/checkoff.rs:74`). `GET /printer-checkoff/verify-batch?idempotency_key=...` returns what a key settled to. It goes through `burst_status` in `pp-api/src/checkoff.rs`, which requires the key exactly once, then `lookup::burst_status` in `pp-storage/src/checkoff_progress/lookup.rs:27`. The MCP path in `pp-api/src/mcp/checkoff.rs` applies a confirmed proposal under a key built from `KEY_PREFIX` plus the action id. Tests: - `pp-storage/tests/checkoff_bursts.rs`: `verify_batch_replays_the_receipt_and_refuses_a_changed_body`, `verify_batch_refuses_a_key_whose_receipt_was_trimmed` - `pp-storage/tests/checkoff_manual_bursts.rs`: `manual_burst_replays_after_restart_and_refuses_a_changed_body` - `pp-api/tests/checkoff_bursts_http.rs`: `verify_batch_http_replays_across_restart_and_undoes_once`

## B52

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

Accept publishes through Rust’s SQLite publication transaction: - `rust/crates/pp-api/src/drafts/http.rs`: `POST /plans/{id}/drafts/{draft_id}/apply` → `lifecycle::apply`. - `rust/crates/pp-api/src/drafts/http/lifecycle.rs`: `Accept::run` calls `publication.apply(PublicationCommand)`. - `rust/crates/pp-storage/src/plan_publication.rs`: `execute` opens an immediate transaction and calls `publish_in_transaction`. - `publish_in_transaction` saves the previous snapshot, inserts the revision, publishes `build_profiles.accepted_plan_revision_id` and increments `accepted_plan_version`, consumes the draft, and records the receipt. - Atomicity test: `rust/crates/pp-storage/src/plan_publication/tests.rs` → `transaction::caller_rollback_and_token_exhaustion_leave_publication_unwritten`. - It drops a successful publication without committing, then commits a token-allocation refusal; all original rows remain unchanged and publication tables remain empty.

## B53

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

Accept publishes a Plan through `POST /api/v2/plans/{id}/drafts/{draft_id}/apply`. The atomicity test is `gre_1120_accept_failing_inside_its_transaction_leaves_no_partial_acceptance`.

**Where the publish happens**
- **Route:** `rust/crates/pp-api/src/drafts/http.rs:313` mounts `/plans/{id}/drafts/{draft_id}/apply` to `lifecycle::apply`. `rust/crates/pp-api/src/client_contract.rs:171` lists the `/api/v2/...` form. `rust/crates/pp-storage/src/working_drafts/autosave.rs:5` says Accept is the only publisher.
- **Storage:** `rust/crates/pp-storage/src/plan_publication.rs`. `execute` (line 665) opens one transaction with `writer_admission::begin_immediate` and calls `publish_in_transaction` (line 1378), which mirrors Node's `AppRepository.applyPlanChanges`. It commits only when the result is `Applied`, `Existing` or `AlreadyApplied`. Any other result drops the transaction, which rolls it back.
- **Snapshot:** `save_before_accept` in `rust/crates/pp-storage/src/plan_publication/snapshot.rs:74` saves the pre-Accept snapshot.

**Tests that prove it is atomic**
- **HTTP level:** `rust/crates/pp-api/src/drafts/http/tests/accept_header.rs:231`, `gre_1120_accept_failing_inside_its_transaction_leaves_no_partial_acceptance`. It adds an `ABORT` trigger on `plan_snapshot_headers`, then on `build_profile_details`, and calls Accept each time. It checks for a 500 that doesn't leak the error text, and that `logical_database` is unchanged. After the trigger is dropped, the same Accept succeeds with `revision_id` 2.
- **Storage level:** `rust/crates/pp-storage/src/plan_publication/tests.rs:1430`, `caller_rollback_and_token_exhaustion_leave_publication_unwritten`. It checks that a rolled-back caller transaction, and a run where Required-unit tokens can't be allocated, both leave `plan_revisions`, `parts`, `required_units` and the related tables empty.

I found these by reading the code only. I didn't run either test.

## B54

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

- Accept enters through `POST /plans/{id}/drafts/{draft_id}/apply`, mounted in `rust/crates/pp-api/src/drafts/http.rs`. - Its handler is `apply` in `rust/crates/pp-api/src/drafts/http/lifecycle.rs`. - Publication lives in `rust/crates/pp-storage/src/plan_publication.rs`, in `publish_in_transaction`. - It saves the pre-Accept snapshot, writes the immutable revision and Required-unit mappings, updates the accepted pointer, consumes the draft, and records the receipt and event. - `execute` wraps publication in one immediate SQLite transaction and commits successful outcomes. - The atomicity proof is `caller_rollback_and_token_exhaustion_leave_publication_unwritten` in `rust/crates/pp-storage/src/plan_publication/tests.rs:1430`. - It drops a transaction after successful publication and verifies no publication survives; token-allocation failure also leaves the original rows unchanged and publication tables empty.

## B55

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

The check is **`rust/scripts/check-boundaries.py`**, a Python script that reads `cargo metadata --manifest-path rust/Cargo.toml --locked --no-deps`.

- **What it enforces:** a dict called `ALLOWED` at the top of the script lists the `pp-*` crates each crate may depend on. The `violations()` function rejects any other `pp-*` dependency. Startup asserts also fail if a workspace crate is missing from `ALLOWED`, or if `ALLOWED` names a crate that doesn't exist.
- **Extra rules:** edges listed in `GATED` must be optional and turned on only by the `desktop_rebuild` feature; `rebuild_edge_is_gated()` checks this. `DEV_ONLY = {"pp-testkit"}` may only appear under `[dev-dependencies]`. The features in `TEST_ONLY_FEATURES` are checked by `test_only_feature_violations()`. `planted_edges_rejected()` runs fake graphs to prove that bad edges are refused.
- **How it runs:** the command is `python3 rust/scripts/check-boundaries.py --self-test`. In local CI it is the `boundaries` step in `scripts/local-ci.py` (`just ci --only boundaries`). In hosted CI it is the `check-boundaries` step of `.github/workflows/chain-tip-gate.yml`, and `desktop-runtime.yml` also runs it.

**To add a new edge:**
1. Add the dependency to the crate's `Cargo.toml`.
2. Add the target crate to that crate's set in `ALLOWED`. If it is a rebuild-only edge, also add it to `GATED` and make it `optional` with `dep:<name>` under `desktop_rebuild`.
3. Copy the change into the "Current allowed edges" table in `docs/adr/0007-rust-crate-layout.md`; that table is copied from `ALLOWED`.
4. Run `--self-test` again.

Widening `ALLOWED` counts as weakening a security boundary, so only the owner (Chad) may merge that PR, not the coordinator. See `docs/development/definition-of-done.md` (merge gate) and `docs/development/ci-controls.md`.

## B56

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The duplicate-key check is in **`rust/crates/pp-storage/src/checkoff_progress/burst.rs:515`**, function **`replayed()`**. - It finds the tenant’s stored receipt by `idempotency_key` and compares `request_hash`. - Same key and request returns the saved response; changed request, undone burst, or retired key returns HTTP 409. - `burst.rs:351`, **`verify_batch()`**, calls `replayed()` before applying any bed changes. - `burst.rs:64`, **`idempotency_key()`**, validates the JS-trimmed key length: 1–200 characters; **`Burst::parse()`** calls it. - `rust/crates/pp-api/src/checkoff.rs`: **`checkoff_router()`** exposes `POST /printer-checkoff/verify-batch`, also under `/api/v2`; handler **`verify_batch()`** forwards to storage. - Manual bursts share the check: `rust/crates/pp-storage/src/checkoff_progress/manual.rs:291`, **`apply()`**, calls `replayed()`. - Coverage: `rust/crates/pp-storage/tests/checkoff_bursts.rs:130`, **`verify_batch_replays_the_receipt_and_refuses_a_changed_body()`**. - Manual coverage: `rust/crates/pp-storage/tests/checkoff_manual_bursts.rs:161`, **`manual_burst_replays_after_restart_and_refuses_a_changed_body()`**.

## B57

**Question:** Where is the Checkoff burst idempotency key checked?

**Answer:**

The key is checked in the Rust storage layer, `rust/crates/pp-storage/src/checkoff_progress/burst.rs`. Paths below are relative to the `<checkout>` checkout.

- **Format check:** `idempotency_key()` in `burst.rs` (line 64) trims the key and refuses it unless it is 1 to 200 characters. `Burst::parse` (line 79) calls it on the request body. `parse_op` in `checkoff_progress.rs` (line 618) also calls it for the status lookup.
- **Replay check:** `replayed()` in `burst.rs` (line 515) looks for an earlier receipt with the same key:
  - If the stored `request_hash` matches, it returns that receipt and writes nothing.
  - If the hash differs, it refuses with 409 "idempotency_key was already used for a different request".
  - If the burst was undone, it refuses.
  - If the key's hash is on the retired list, it refuses with "too old to replay".
- **Where `replayed()` runs:** `verify_batch()` (`burst.rs` line 351) calls it for `POST /printer-checkoff/verify-batch`. `manual::apply()` (`manual.rs` line 290) calls it for manual bursts.
- **Status lookup:** `lookup::burst_status()` (`lookup.rs` line 27) answers `GET /printer-checkoff/verify-batch?idempotency_key=...`. The HTTP handler in `rust/crates/pp-api/src/checkoff.rs` (around line 141) first requires the key to appear exactly once.
- **MCP:** `rust/crates/pp-api/src/mcp/checkoff.rs` builds the key as `KEY_PREFIX` plus the action id (line 139) and sends it through the same verify-batch path.
- **Tests:**
  - `verify_batch_replays_the_receipt_and_refuses_a_changed_body` in `pp-storage/tests/checkoff_bursts.rs`
  - `manual_burst_replays_after_restart_and_refuses_a_changed_body` in `pp-storage/tests/checkoff_manual_bursts.rs`
  - `verify_batch_http_replays_across_restart_and_undoes_once` in `pp-api/tests/checkoff_bursts_http.rs`
  - `manual_burst_http_reads_units_records_replays_and_undoes_once` in `pp-api/tests/checkoff_manual_bursts_http.rs`

## B58

**Question:** Which check enforces the dependency edges between the Rust crates, and how is a new edge added?

**Answer:**

- The enforcing check is `rust/scripts/check-boundaries.py`, using its explicit `ALLOWED` map. - It reads `cargo metadata --manifest-path rust/Cargo.toml --locked --format-version 1 --no-deps`. - `violations(packages)` rejects unlisted `pp-*` edges; it also checks that all workspace crates appear in `ALLOWED`. - `GATED` and `rebuild_edge_is_gated()` enforce optional, default-off `desktop_rebuild` edges. - `planted_edges_rejected()` verifies that intentionally invalid dependency graphs fail. - Run it with `python3 rust/scripts/check-boundaries.py --self-test`. - CI runs that command in `.github/workflows/chain-tip-gate.yml`, step `check-boundaries`; `scripts/local-ci.py` includes step `boundaries`. - Add an edge by updating `ALLOWED` and the consuming crate’s `rust/crates/<crate>/Cargo.toml`; update `GATED` if applicable. - `docs/architecture.md`, “Crates and allowed edges,” requires updating the checker, manifests, and dependency documentation together. - `docs/adr/0007-rust-crate-layout.md` clarifies that documented “planned” edges grant no permission until `ALLOWED` changes.

## B59

**Question:** What does a Rust-only PR need to run locally before it is pushed?

**Answer:**

- Normal Rust-only PR: run `just ci --fast` before pushing — `docs/development/agent-workflow.md`. - Sensitive PR: run full `PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci` — `docs/contributing/gate-evidence.md`. - Sensitive includes schema/migrations, auth/sessions, backup/restore, Accept/publication, secrets and protected paths — `docs/development/definition-of-done.md`, merge-gate item 6. - `justfile` forwards both commands to `python3 scripts/local-ci.py`; the same arguments work without `just`. - `scripts/local-ci.py::fast_steps()` replaces full nextest/doctests with `nextest-affected`, enabling `desktop_rebuild`. - Fast mode retains `fmt`, `clippy-off`, `clippy-on`, `clippy-limits`, boundary checks and the other `STEPS`. - Rust checks include `cargo fmt --manifest-path rust/Cargo.toml --all --check` and workspace Clippy with `-D warnings`. - Rust-only changes still run asset/manifest preparation, workaround/duplication checks and Node fixture freshness through that runner. - Full mode additionally runs `nextest-off`, `nextest-on`, `doc-off`, `doc-on` — `scripts/local-ci.py::STEPS`. - Fix every `FAIL`; record the command, summary and every `SKIP` under `## Evidence`; a skip is not a pass — `docs/contributing/gate-evidence.md`.

## B60

**Question:** Where does Accept publish a Plan, and which test proves the publish is atomic?

**Answer:**

Accept publishes a Plan inside one SQLite transaction in `pp-storage`. The test that proves it is atomic is `caller_rollback_and_token_exhaustion_leave_publication_unwritten`. Paths below are relative to the checkout root you gave. - **Route:** `POST /plans/{id}/drafts/{draft_id}/apply` is registered in `rust/crates/pp-api/src/drafts/http.rs:313`. It goes to `lifecycle::apply` (`rust/crates/pp-api/src/drafts/http/lifecycle.rs:135`). `Accept::run` then calls `client.publication()` and `PublicationClient::apply`. MCP Accept takes the same path through `rust/crates/pp-api/src/mcp/plan_accept.rs:132`. - **Publish:** in `rust/crates/pp-storage/src/plan_publication.rs`, `execute` (line 665) opens a transaction with `writer_admission::begin_immediate`. It calls `publish_in_transaction` (line 1378), which mirrors Node `applyPlanChanges`. It commits only on `Applied`, `Existing` or `AlreadyApplied`; any other outcome drops the transaction, so nothing is written. Save-and-Accept calls the same function in `rust/crates/pp-storage/src/working_drafts/save.rs:663`. - **Atomicity test:** `caller_rollback_and_token_exhaustion_leave_publication_unwritten`, in `mod transaction` of `rust/crates/pp-storage/src/plan_publication/tests.rs:1430`. It does two things: - It runs a publish whose outcome is `applied`, then drops the transaction without committing. - It runs a second publish whose token allocator always fails, so the outcome is `token_allocation_failed`, and commits that transaction. Afterwards every table still equals its `before` fixture. `plan_revisions`, `parts`, `plan_apply_requests`, `plan_apply_admissions`, `plan_revision_input_sets` and `required_units` are all empty. - **Related test:** `transaction_snapshot_holds_the_replaced_plan_inputs` (line 1226) checks that the snapshot taken before Accept is written in the same transaction. - **Doc:** `docs/architecture.md:223` says that publication "owns one atomic acceptance transaction".
