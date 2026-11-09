# Answer key: navigation comparison (GRE-1309)

Grade each answer correct (1) or incorrect (0). An answer is correct when it states every required fact for its question and does not give a listed wrong answer as its main answer. Paths are relative to the repository root.

## Answer key

Paths are relative to the repository root at the commit above.

### 1. Accept publishes a Plan

Required facts:
- The publish happens in `pp-storage`'s `plan_publication`: `execute` calls `publish_in_transaction` inside one `begin_immediate` writer transaction and commits only on an Applied, Existing or AlreadyApplied outcome (`rust/crates/pp-storage/src/plan_publication.rs`).
- The HTTP entry is `POST /plans/{id}/drafts/{draft_id}/apply`, through `lifecycle::apply` and `Accept::run` to `PublicationClient::apply` (`rust/crates/pp-api/src/drafts/http.rs`, `http/lifecycle.rs`).
- The atomicity test is `caller_rollback_and_token_exhaustion_leave_publication_unwritten` (`rust/crates/pp-storage/src/plan_publication/tests.rs`).

Wrong answers: Node `AppRepository.applyPlanChanges`; draft autosave; the MCP caller `pp-api/src/mcp/plan_accept.rs`; file publication in `pp-os` or `pp-core/src/state_file.rs`; the working-draft save rollback test.

### 2. Send-queue table and its read route

Required facts:
- The `printer_send_queue` table is owned by `pp-storage`'s `send_queue` module (`rust/crates/pp-storage/src/send_queue.rs`, schema step 44).
- The read route is `GET /api/v2/printer-send-queue`, mounted in `rust/crates/pp-api/src/farm.rs` and served through `read_model::farm`.

Wrong answers: `pp-api/src/send_queue.rs`, which mounts writes only; Node's `GET /printer-send-queue`; the `printer.send_queue` setting presented as the table.

### 3. Crate dependency edges

Required facts:
- `rust/scripts/check-boundaries.py` enforces the edges with its `ALLOWED` map, checked against `cargo metadata`.
- A new edge is added to `ALLOWED`. An edge that should exist only under `desktop_rebuild` also goes in `GATED`, and the crate map in `docs/architecture.md` is updated.

Wrong answers: `check-domain-deps.py`; `cargo deny`; `cargo machete`; "add it to `Cargo.toml`" alone.

Not required for a correct answer, but noted when given: widening `ALLOWED` is an owner-merged change (`docs/development/definition-of-done.md`).

### 4. Checkoff burst idempotency key

Required facts:
- The key is checked in `pp-storage`'s `checkoff_progress::burst`. `idempotency_key` validates it, and `replayed` matches the key and request hash, inside the writer transaction (`rust/crates/pp-storage/src/checkoff_progress/burst.rs`).
- `pp-api` doesn't check the key itself; it passes the body to storage.

Wrong answers: Accept's `plan_apply_requests.idempotency_key`; the `Idempotency-Key` header in the draft routes; send-queue or durable-job idempotency.

### 5. Local checks for a Rust-only PR

Required facts:
- The docs split PRs by normal and sensitive, not by language. A normal PR runs `just ci --fast`, and a sensitive PR (merge-gate item 6) runs the full `just ci` (`AGENTS.md`, `docs/development/definition-of-done.md`).
- The command and every SKIP line go under `## Evidence`, because a SKIP isn't a pass.

Wrong answers: `cargo test` or `cargo nextest run` alone; the hand-run list in `rust/README.md`; one command for every PR.


## Declared amendment (dated 2026-11-09, applies to both arms)

For question 4, the required fact "pp-api doesn't check the key itself; it passes the body to storage" is withdrawn. It was stricter than any answer. The amended requirement is: the check is in pp-storage's checkoff_progress::burst (idempotency_key and replayed), and the answer does not place a check in pp-api. An answer that says pp-api performs the check is incorrect. Other Q4 requirements and wrong answers still apply.
