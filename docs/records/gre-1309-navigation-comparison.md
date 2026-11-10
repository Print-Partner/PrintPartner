# Navigation comparison, with and without signpost files (GRE-1309)

This record holds the W0.5 navigation comparison from the maintainability plan (GRE-1306, revision 20). It records the comparison only. The navigation gate is decided by GRE-1320 on main after A3, A5 and A8 merge, not here.

Companion PRs: the counts (#1643), the counting tools (#1644), the grading inputs and grades (#1645) and the logs archive (#1646) are separate. The paths below refer to files in those PRs, and they resolve once those PRs merge.

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

- **Files opened** (as recorded, counted by [`navcount.py`](gre-1309/navcount.py); corrected, counted by `recount.py` in the tools PR #1644; prompt in [`prompt.txt`](gre-1309/prompt.txt)): distinct repository files whose contents the session read, counted from the thread's tool log. Shell commands that print file contents (`cat`, `sed -n`, `head`, `tail`, `nl`, `less`) count. Search hits (`rg`, `grep`, `find`, `ls`) don't count as opened. The counter reads only shell commands, and it stops with an error on any other tool item type, so a Read-tool call cannot be missed silently. None of the session logs contains a Read-tool item; every file access in them is a shell command.
- **Minutes:** from the run's start to its completion timestamp in the thread log.
- **Correctness:** the coordinator marks the final answer against the key below. The answer is correct only when it contains every required fact, and incorrect if it names a listed wrong answer as the answer.
- **Sessions:** three per model, per question, per arm, so 60 in all.
- **Reruns:** a wrong answer earns that question one rerun for that model, and it fails only if wrong again. An inconclusive question (two passes and a tie) gets one rerun.
- **Pass rule:** a question passes when both models answered correctly and both opened fewer files with the signposts than without. Minutes are reported but don't gate.

### Amendments (2026-11-09)

- **Pass rule, plan revision 22.** The original rule above stays on the record, and is also reported in the results. For the navigation gate, the revision 22 rule compares medians:
  - Count: content-bearing tool calls per session, measured as distinct repository files with content, including search output (`distinct_files_with_content`).
  - Per question and model, take the median of the three sessions per arm.
  - A question passes when both models have a lower with-arm median.
  - Headroom: a question has headroom when its without-arm median is 2 or more for each model. Revision 22 does not define it; this is the definition used here. A question without headroom is "cannot decide", not "fail".
  - The gate is met when at least three of the five questions with headroom pass.
- **Q4 key.** The required fact "`pp-api` doesn't check the key itself; it passes the body to storage" is withdrawn, because it was stricter than any answer. The amended requirement is that the check is in `pp-storage`'s `checkoff_progress::burst` (`idempotency_key` and `replayed`), and that the answer does not place a check in `pp-api`. This applies to both arms.

### Adjudication of borderline grades (2026-11-09)

The blind grades in `grading/grades.tsv` stand as the original record. Two rulings change the correctness count. Both apply to both arms, and both were made before the reruns.

- **Q1, commit on outcome.** The key's fact that `execute` commits only on `Applied`, `Existing` or `AlreadyApplied` is ruled not required for this question. The question asks where publish happens and which test proves it atomic. An answer that names `execute`, the immediate transaction, `publish_in_transaction` and the atomicity test is correct. Changed to correct: without-arm Q1 Codex run 2; with-arm Q1 Codex runs 2 and 3.
- **Q3, GATED.** The `GATED` step applies only to an edge that must be `desktop_rebuild`-only. The question doesn't say the edge is desktop-only, so the general add-edge steps are the requirement. Changed to correct: with-arm Q3 Codex run 3.
- **Not changed.** With-arm Q1 Claude run 3 places publish in the working-draft save path. With-arm Q3 Claude runs 1 and 2, and without-arm Q3 Claude run 3, do not update the crate map in `docs/architecture.md`, which the key requires.
- **Result before reruns:** without arm 29 of 30 correct, with arm 27 of 30.

**Signpost finding.** All three with-arm Claude Q1 answers (runs 1 to 3) cite the working-draft save path, `rust/crates/pp-storage/src/working_drafts/save.rs`. No without-arm answer does. The family index row for Accept (`docs/families.md`) links `plan-autosave.md` as its document, and that page describes the draft save-and-accept flow. Run 3 used that path as the publish location. The treatment is frozen, so the row is recorded as a defect to fix after the run, not changed here.

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

All 30 sessions ran on `7908c703` between 21:23 and 21:40 UTC. The as-recorded counts come from `navcount.py`, which reads each session's tool log and applies the rule in Scoring. The corrected counts and the minutes in the table come from `recount.py` (tools PR #1644), which also expands shell variables, parses multi-line Codex scripts, and reports minutes to two decimals. `navcount.py` reports minutes to one decimal, as recorded. "Correct" is the coordinator's mark against the key.

| Question | Model | Session | Files opened, as recorded | Files opened, corrected | Minutes | Correct |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Claude | 1 | 2 | 2 | 0.56 | yes |
| 1 | Claude | 2 | 5 | 5 | 0.62 | yes |
| 1 | Claude | 3 | 1 | 5 | 0.5 | yes |
| 1 | Codex | 1 | 5 | 5 | 0.91 | yes |
| 1 | Codex | 2 | 4 | 4 | 0.42 | yes (adjudicated; blind grade no) |
| 1 | Codex | 3 | 2 | 4 | 0.72 | yes |
| 2 | Claude | 1 | 3 | 3 | 0.4 | yes |
| 2 | Claude | 2 | 3 | 3 | 0.41 | yes |
| 2 | Claude | 3 | 3 | 3 | 0.5 | yes |
| 2 | Codex | 1 | 1 | 3 | 0.36 | yes |
| 2 | Codex | 2 | 1 | 4 | 0.37 | yes |
| 2 | Codex | 3 | 2 | 5 | 0.49 | yes |
| 3 | Claude | 1 | 2 | 3 | 0.37 | yes |
| 3 | Claude | 2 | 0 | 2 | 0.36 | yes |
| 3 | Claude | 3 | 3 | 2 | 0.36 | no (its re-run is also no, so the cell fails) |
| 3 | Codex | 1 | 1 | 2 | 0.44 | yes |
| 3 | Codex | 2 | 1 | 3 | 0.44 | yes |
| 3 | Codex | 3 | 1 | 3 | 0.46 | yes |
| 4 | Claude | 1 | 3 | 3 | 0.41 | yes |
| 4 | Claude | 2 | 1 | 1 | 0.32 | yes |
| 4 | Claude | 3 | 2 | 2 | 0.47 | yes |
| 4 | Codex | 1 | 1 | 1 | 0.4 | yes |
| 4 | Codex | 2 | 1 | 2 | 0.62 | yes |
| 4 | Codex | 3 | 1 | 2 | 0.51 | yes |
| 5 | Claude | 1 | 2 | 2 | 0.28 | yes |
| 5 | Claude | 2 | 1 | 1 | 0.37 | yes |
| 5 | Claude | 3 | 2 | 2 | 0.5 | yes |
| 5 | Codex | 1 | 2 | 3 | 0.4 | yes |
| 5 | Codex | 2 | 4 | 4 | 0.6 | yes |
| 5 | Codex | 3 | 2 | 6 | 0.51 | yes |

Per question and model, the median of the three corrected file counts:

| Question | Claude files | Codex files |
| --- | --- | --- |
| 1 | 5 | 4 |
| 2 | 3 | 4 |
| 3 | 2 | 3 |
| 4 | 2 | 2 |
| 5 | 2 | 4 |

The as-recorded table above and these medians differ because the counts were corrected: shell variables are now expanded, and multi-line Codex scripts are parsed in full. Ten sessions changed. The as-recorded column keeps the original values. Q3 Claude runs 1 and 3 and all Q3 Codex runs were blocked in the original sessions, so their corrected values come from the re-runs.

Files opened in each session, as recorded before the counting correction (`navcount.py` output). These are not the corrected counts; the corrected values are in the table above.

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
- **Floor effect:** the corrected without-arm medians run from 2 to 5 files per question and model. 29 of 30 without-arm answers were correct after adjudication. The one that was not (Q3 Claude run 3) failed its re-run, so under the rule the cell fails. An extra second re-run, outside the rule, passed; it is disclosed and not counted. For the pass rule (fewer files with the signposts, for both models), a 2-file baseline leaves little room to improve.
- **Root `AGENTS.md`:** each session started in the coordinator's checkout of a newer `main`, then worked in the arm's worktree by absolute path. The runner may load that checkout's root `AGENTS.md` into context. It is the same file for both arms.

### Reruns (2026-11-09)

Under the Scoring rule, each cell with a wrong answer earned one rerun, and the cell fails if that rerun is also wrong. The reruns ran on the same frozen checkouts with the same prompt and settings. They are correctness checks, so their answers are not in the three-session medians.

| Cell | Wrong runs in the 30 | Rerun correct | Cell passes |
| --- | --- | --- | --- |
| With arm, Q1 Claude | run 3 | no: names a generic transaction and omits the immediate writer mode | no |
| With arm, Q3 Claude | runs 1 and 2 | no: names only the ADR 0007 table, not the crate map in `docs/architecture.md` | no |
| Without arm, Q3 Claude | run 3 | no (no architecture crate map). An extra second re-run, outside the rule, passed; not counted | no |

The 30 session grades above are unchanged by the reruns. Grades are in `grading/rerun-grades.tsv`, graded blind by a fresh agent under the key and the rulings in `grading/key-rulings.md`.

**Limitation.** The rerun answers were taken from the completion notices, not from the raw thread logs. They were condensed for layout, and the required facts were kept. Their verbatim text is not yet in the archive.

### With arm (complete, 2026-11-09)

**Treatment.** Commit `6deb5b542` on `lynchdc39987/gre-1309-with-arm-astra-3`: `7908c7036` plus the signposts. A3 is the family index (`docs/families.md`). A5 is the nested `AGENTS.md` files. A8 is the receipt move: nine records moved verbatim into `docs/records/`, with link-target repairs, 2,258 lines. The sessions ran in a detached checkout of that commit. The prompt names that checkout's path; the without arm's path differs, and nothing else in the prompt changed.

**Sessions.** 30 sessions, same models, settings and prompt as the without arm. Logs are in `docs/records/gre-1309/logs.tar.gz` (sha256 `834b83ff3d7e6f64e4724893da62a48edb18db7fa5e7cd5572c5bc85b00cf4d9`), with `logs/with/` for these sessions, `logs/without/` and `logs/without/reruns/` for the without arm, and the file lists in `counts/`. Counts are in `docs/records/gre-1309/counts/sessions-with.csv`.

**Counts.** Medians of three sessions per cell. "Content" is the revision 22 count. "Files read" is the corrected files-opened count from the original rule.

| Question | Model | Content, without | Content, with | Fewer | Files read, without | Files read, with | Fewer |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Claude | 37 | 34 | yes | 5 | 5 | no |
| 1 | Codex | 765 | 405 | yes | 4 | 4 | no |
| 2 | Claude | 6 | 7 | no | 3 | 3 | no |
| 2 | Codex | 140 | 138 | yes | 4 | 3 | yes |
| 3 | Claude | 17 | 16 | yes | 2 | 2 | no |
| 3 | Codex | 35 | 39 | no | 3 | 4 | no |
| 4 | Claude | 9 | 10 | no | 2 | 2 | no |
| 4 | Codex | 274 | 448 | no | 2 | 1 | yes |
| 5 | Claude | 9 | 5 | yes | 2 | 1 | yes |
| 5 | Codex | 50 | 49 | yes | 4 | 5 | no |

**Gate result.**
- Revision 22 rule: 2 of 5 questions pass (Q1 and Q5). The gate needs 3. **Not met.**
- Sensitivity, counting without files found only in outputs over 50 KB: 1 of 5 pass (Q1). **Not met.**
- Original rule (both models correct and both open fewer files): 0 of 5 pass.
- No question is "cannot decide": every without-arm median is 6 or more.
- The codex Q1 and Q4 medians depend on very large search outputs, so the sensitivity result matters. Each cell is three sessions, so differences of a few files are within noise.

**Correctness.** Blind grading by a fresh agent against `grading/key.md`, including the Q4 amendment. The grader saw 60 answers with arm, seat and session names removed (`grading/answers-blind.md`). The map is in `grading/blind-map.json`, committed after grading.
- Correct: without arm 28 of 30, with arm 24 of 30.
- Wrong answers, without arm (2 of 30): Q1 Codex run 2 (no commit-on-outcome condition, a borderline call); Q3 Claude run 3 (ADR 0007 table only; `docs/architecture.md` crate map not updated).
- Wrong answers, with arm (6 of 30): Q1 Claude run 3 (publish placed in the working-draft save; `execute` and `begin_immediate` not named); Q1 Codex runs 2 and 3 (no commit-on-outcome condition, a borderline call); Q3 Claude runs 1 and 2 (ADR table only); Q3 Codex run 3 (no `GATED` update, which the key requires only for a desktop-rebuild-only edge, a borderline call).
- Two categories are borderline: the commit-on-outcome condition for Q1, and the `GATED` step for Q3. The grader counted both as required. They were adjudicated on 2026-11-09 (see the Adjudication section), and the original grades are kept.

**Open items.**
- **Re-runs.** Done (see the Re-runs section). Under the rule, three cells fail: with-arm Q1 Claude, with-arm Q3 Claude, and without-arm Q3 Claude. An extra second re-run for the without-arm Q3 Claude cell passed; it was run outside the rule and is not counted.
- **Borderline adjudication.** Done (see the Adjudication section, 2026-11-09). The original grades are kept.
- **Missing paths.** Eleven paths named in with-arm search commands are absent from the with checkout. None was opened.
- **Logs.** The raw logs are in one archive, `logs.tar.gz`, in PR #1646. That PR is blocked by the repository large-file limit, so the archive is not yet on main. The logs contain local paths, including the username.

