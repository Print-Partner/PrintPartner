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

- **Files opened** (counted by [`navcount.py`](gre-1309/navcount.py); prompt in [`prompt.txt`](gre-1309/prompt.txt)): distinct repository files whose contents the session read, counted from the thread's tool log. Read-tool calls and shell commands that print file contents (`cat`, `sed -n`, `head`, `tail`, `nl`, `less`) count. Search hits (`rg`, `grep`, `find`, `ls`) don't count as opened.
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

### Without arm (complete, 2026-11-09)

All 30 sessions ran on `7908c703` between 21:23 and 21:40 UTC. Files are counted by `navcount.py`, which reads each session's tool log and applies the rule in Scoring. "Correct" is the coordinator's mark against the key.

| Question | Model | Session | Files opened | Minutes | Correct |
| --- | --- | --- | --- | --- | --- |
| 1 | Claude | 1 | 2 | 0.6 | yes |
| 1 | Claude | 2 | 5 | 0.6 | yes |
| 1 | Claude | 3 | 1 | 0.5 | yes |
| 1 | Codex | 1 | 5 | 0.9 | yes |
| 1 | Codex | 2 | 4 | 0.4 | yes |
| 1 | Codex | 3 | 2 | 0.7 | yes |
| 2 | Claude | 1 | 3 | 0.4 | yes |
| 2 | Claude | 2 | 3 | 0.4 | yes |
| 2 | Claude | 3 | 3 | 0.5 | yes |
| 2 | Codex | 1 | 1 | 0.4 | yes |
| 2 | Codex | 2 | 1 | 0.4 | yes |
| 2 | Codex | 3 | 2 | 0.5 | yes |
| 3 | Claude | 1 | 2 | 0.3 | yes |
| 3 | Claude | 2 | 0 | 0.4 | yes |
| 3 | Claude | 3 | 3 | 0.5 | yes |
| 3 | Codex | 1 | 1 | 0.4 | yes |
| 3 | Codex | 2 | 1 | 0.4 | yes |
| 3 | Codex | 3 | 1 | 0.5 | yes |
| 4 | Claude | 1 | 3 | 0.4 | yes |
| 4 | Claude | 2 | 1 | 0.3 | yes |
| 4 | Claude | 3 | 2 | 0.5 | yes |
| 4 | Codex | 1 | 1 | 0.4 | yes |
| 4 | Codex | 2 | 1 | 0.6 | yes |
| 4 | Codex | 3 | 1 | 0.5 | yes |
| 5 | Claude | 1 | 2 | 0.3 | yes |
| 5 | Claude | 2 | 1 | 0.4 | yes |
| 5 | Claude | 3 | 2 | 0.5 | yes |
| 5 | Codex | 1 | 2 | 0.4 | yes |
| 5 | Codex | 2 | 4 | 0.6 | yes |
| 5 | Codex | 3 | 2 | 0.5 | yes |

Per question and model, as the mean of the three sessions:

| Question | Claude files | Claude minutes | Codex files | Codex minutes |
| --- | --- | --- | --- | --- |
| 1 | 2.67 | 0.57 | 3.67 | 0.67 |
| 2 | 3.0 | 0.43 | 1.33 | 0.43 |
| 3 | 1.67 | 0.4 | 1.0 | 0.43 |
| 4 | 2.0 | 0.4 | 1.0 | 0.5 |
| 5 | 1.67 | 0.4 | 2.67 | 0.5 |

Files opened in each session, as `navcount.py` counted them:

- Q1 claude r1: `rust/crates/pp-storage/src/plan_publication.rs`, `rust/crates/pp-storage/src/plan_publication/tests.rs`
- Q1 claude r2: `rust/crates/pp-api/src/drafts/http.rs`, `rust/crates/pp-api/src/drafts/http/tests/accept_header.rs`, `rust/crates/pp-storage/src/plan_publication.rs`, `rust/crates/pp-storage/src/plan_publication/tests.rs`, `rust/crates/pp-storage/src/working_drafts.rs`
- Q1 claude r3: `rust/crates/pp-api/src/drafts/http/tests/accept_header.rs`
- Q1 codex r1: `rust/crates/pp-api/src/drafts/http.rs`, `rust/crates/pp-api/src/drafts/http/lifecycle.rs`, `rust/crates/pp-storage/src/plan_publication.rs`, `rust/crates/pp-storage/src/plan_publication/tests.rs`, `web/apps/server/src/db/plan-drafts.test.ts`
- Q1 codex r2: `docs/desktop-rebuild.md`, `rust/crates/pp-api/src/drafts/http/lifecycle.rs`, `rust/crates/pp-storage/src/plan_publication.rs`, `rust/crates/pp-storage/src/plan_publication/tests.rs`
- Q1 codex r3: `docs/architecture.md`, `rust/crates/pp-storage/src/plan_publication/tests.rs`
- Q2 claude r1: `rust/crates/pp-api/src/farm.rs`, `rust/crates/pp-storage/src/read_model/farm.rs`, `rust/crates/pp-storage/src/send_queue.rs`
- Q2 claude r2: `rust/crates/pp-api/src/farm.rs`, `rust/crates/pp-storage/src/read_model/farm.rs`, `rust/crates/pp-storage/src/send_queue.rs`
- Q2 claude r3: `rust/crates/pp-api/src/farm.rs`, `rust/crates/pp-storage/src/read_model/farm.rs`, `rust/crates/pp-storage/src/send_queue.rs`
- Q2 codex r1: `rust/crates/pp-storage/src/send_queue.rs`
- Q2 codex r2: `rust/crates/pp-storage/src/send_queue.rs`
- Q2 codex r3: `docs/send-queue.md`, `rust/crates/pp-storage/src/send_queue.rs`
- Q3 claude r1: `docs/architecture.md`, `rust/scripts/check-boundaries.py`
- Q3 claude r2: none
- Q3 claude r3: `docs/adr/0007-rust-crate-layout.md`, `docs/architecture.md`, `rust/scripts/check-boundaries.py`
- Q3 codex r1: `rust/scripts/check-boundaries.py`
- Q3 codex r2: `rust/scripts/check-boundaries.py`
- Q3 codex r3: `rust/scripts/check-boundaries.py`
- Q4 claude r1: `rust/crates/pp-api/src/checkoff.rs`, `rust/crates/pp-storage/src/checkoff_progress.rs`, `rust/crates/pp-storage/src/checkoff_progress/burst.rs`
- Q4 claude r2: `rust/crates/pp-storage/src/checkoff_progress/burst.rs`
- Q4 claude r3: `rust/crates/pp-storage/src/checkoff_progress/burst.rs`, `rust/crates/pp-storage/src/checkoff_progress/manual.rs`
- Q4 codex r1: `rust/crates/pp-storage/src/checkoff_progress/burst.rs`
- Q4 codex r2: `rust/crates/pp-storage/src/checkoff_progress/burst.rs`
- Q4 codex r3: `rust/crates/pp-storage/src/checkoff_progress/burst.rs`
- Q5 claude r1: `docs/development/ci.md`, `justfile`
- Q5 claude r2: `docs/development/ci.md`
- Q5 claude r3: `docs/contributing/gate-evidence.md`, `docs/development/definition-of-done.md`
- Q5 codex r1: `docs/development/ci.md`, `docs/development/definition-of-done.md`
- Q5 codex r2: `docs/contributing/gate-evidence.md`, `docs/development/ci.md`, `docs/development/definition-of-done.md`, `scripts/local-ci.py`
- Q5 codex r3: `docs/contributing/gate-evidence.md`, `docs/development/definition-of-done.md`

### Grading notes

Two grading choices on the without arm were made after seeing the answers. They are recorded here rather than folded into the key above.

- **Q1, extra tests named:** three answers named a second test next to `caller_rollback_and_token_exhaustion_leave_publication_unwritten`. Codex session 1 added the Node test "rolls back every publication write and retries cleanly". Claude sessions 2 and 3 led with the HTTP test `gre_1120_accept_failing_inside_its_transaction_leaves_no_partial_acceptance`. Every answer also named the Rust storage test, so all three are marked correct.
- **Q4, the `pp-api` fact:** no answer said in so many words that `pp-api` doesn't check the key. Every answer placed every check in `pp-storage`, and none said `pp-api` checks it. That is counted as meeting the fact.

### Measurement limits

- **Search output isn't counted:** both models mostly navigate with `rg` and `grep`, often with line numbers or context. Search output is not counted as opening a file, so a session can read code through search hits and score 0 or 1 files. Q3 Claude session 2 did this.
- **Floor effect:** the without arm averages 1.0 to 3.7 files per question and model, and every answer was correct. For the pass rule (fewer files with the signposts, for both models), a 1.0 baseline leaves almost no room to improve. That holds for Codex on Q3 and Q4.
- **Root `AGENTS.md`:** each session started in the coordinator's checkout of a newer `main`, then worked in the arm's worktree by absolute path. The runner may load that checkout's root `AGENTS.md` into context. It is the same file for both arms.

### With arm (not run)

The with arm takes its signpost files from the PR heads of A3 (GRE-1315), A5 (GRE-1316) and A8 (GRE-1317). None of those PRs or branches existed on 2026-11-09, so this arm has not run, and the per-question comparison isn't recorded yet. It will use the same commit, questions, models, prompt and counting script.
