# Navigation comparison, with and without signpost files (GRE-1309)

This record holds the W0.5 navigation comparison from the maintainability plan (GRE-1306, revision 20). It records the comparison only. The navigation gate is decided by GRE-1320 on main after A3, A5 and A8 merge, not here.

The questions, models and method below were fixed and committed before the first session ran. Results are added below them as sessions finish.

## Commit and arms

- **Commit:** `7908c70361dd22357704a9d9b38ead15e1b9c658` (main, 2026-11-09, after #1638).
- **Without arm:** that commit as it is, in a detached worktree.
- **With arm:** the same commit plus the signpost files from the PR heads of A3 (GRE-1315, family index), A5 (GRE-1316, nested `AGENTS.md`) and A8 (GRE-1317, receipts moved to `docs/records/`), copied into a second worktree. The PR head SHAs are listed here when that arm runs.

## Models

Each session is a fresh child thread with no earlier context, started through T3 Code `delegate_task`, so nothing carries over between sessions.

| Seat | Provider | Model | Setting |
| --- | --- | --- | --- |
| Claude | `claudeAgent` | `claude-opus-5-5` | effort `medium` |
| Codex | `codex` | `gpt-6.1-sol` | reasoningEffort `medium` |

## Questions

Each session gets one question. The prompt names the worktree, says to answer from the repository only, and asks for file paths and names.

1. Where does Accept publish a Plan, and which test proves the publish is atomic?
2. Which module owns the send-queue table, and which route reads it?
3. Which check enforces the dependency edges between the Rust crates, and how is a new edge added?
4. Where is the Checkoff burst idempotency key checked?
5. What does a Rust-only PR need to run locally before it is pushed?

## Scoring

- **Files opened:** distinct repository files whose contents the session read, counted from the thread's tool log. Read-tool calls and shell commands that print file contents (`cat`, `sed -n`, `head`, `tail`, `nl`, `less`) count. Search hits (`rg`, `grep`, `find`, `ls`) don't count as opened.
- **Minutes:** from the run's start to its completion timestamp in the thread log.
- **Correctness:** the coordinator marks the final answer against the key below. The answer is correct only when it contains every required fact, and incorrect if it names a listed wrong answer as the answer.
- **Sessions:** three per model, per question, per arm, so 60 in all.
- **Reruns:** a wrong answer earns that question one rerun for that model, and it fails only if wrong again. An inconclusive question (two passes and a tie) gets one rerun.
- **Pass rule:** a question passes when both models answered correctly and both opened fewer files with the signposts than without. Minutes are reported but don't gate.

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

## Results

Added as sessions finish.
