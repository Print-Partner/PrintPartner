# Sensitive merge score records

The coordinator guard and the `pp/score` status read score comments only from `scorer_authors` in
`scripts/merge-classes.json`, currently `poitee`, and both read them with one reader,
`read_comments` in `scripts/merge_class.py` (see [How records are read](#how-records-are-read)). That account posts every
seat's score, so the login does not identify the model. A dedicated bot
identity is the follow-up.

Each score occupies a line in this format, with a full lowercase head SHA. `vendor=`
takes a name from `vendors` (the `vendors` list in `scripts/merge-classes.json`). `author=`
takes one of those names, or `cursor-auto` or `unknown`. `obstacles=` comes last:

```text
pp-score head=<40 hex> seat=<seat-id> vendor=<anthropic|cursor|deepseek|openai|xai|zhipu|google|qwen|meituan|tencent> score=<integer> author=<anthropic|cursor|deepseek|openai|xai|zhipu|google|qwen|meituan|tencent|cursor-auto|unknown> obstacles=<none|refusal,guard,approval-stall>
```

`obstacles=` is the scorer's disclosure of what it hit while scoring (Chad, 2026-11-17): `refusal`
(the model refused part of the task), `guard` (`scripts/agent-guard.py` refused a command), and
`approval-stall` (a tool approval stalled). Write `none`, or the ones hit, comma-separated, each
once, in any order. A score that leaves out an obstacle the scorer hit is invalid. The parsers
cannot check that `none` is true, so the scorer brief carries that rule; they make the field
required. Listing an obstacle does not fail the score by itself. A record without the field fails
its head. A record with any other token refuses every head. Records for older heads written before
the field existed still parse, and they matter only when a carry walk stops at their head.

`score=` is a whole number of at most two digits; `8.5` does not parse. A `pp-score-trial` line is not a record.

A scorer thread posts its own record from its scorer worktree (branch
`t3/score-<pr>-<seat>-<yyyymmdd>`); see [Safety guard](agent-workflow.md#safety-guard).

The coordinator records the head branch's author family in `author=`.
This is trusted coordinator metadata, not a vendor inferred from the GitHub
account. All score records for the current head must agree on it.
`cursor-auto` is for Cursor Auto-authored PRs. `unknown` is for an author family
that cannot be named. Those two are not seat vendors.

The allowed seats are data: `scorer_seats` in `scripts/merge-classes.json` maps each seat to its
vendor. Since 2026-11-12 (owner decision) they are `claude-opus-5-5` (Anthropic), `grok-4.7`
(xAI), `glm-5.3-flash-free` (Zhipu) and `deepseek-v4.1-flash-free` (DeepSeek). OpenCode runs the
last two as `opencode/glm-5.3-flash-free` and `opencode/deepseek-v4.1-flash-free`, variant max.
Since 2026-11-18 (owner decision) `gpt-6.2-sol` (OpenAI) and `composer-2.5` (Cursor) are also
sensitive-merge seats, and on 2026-11-18 `gpt-6.2-astra` (OpenAI) joined them. `gpt-6.1-sol` is not.
Since 2026-11-18 (owner decision, Chad's card answer at 7:38 PM ET) these seats count as well:
`gemini-3.8-flash` (Google / Cursor; T3 lists it as `cursor/gemini-3.8-flash`, the record uses the
unprefixed id and `vendor=google`), `qwen38-gufo` and `qwen38-flash` (Qwen), `longcat-2.5-free`
(Meituan) and `hy4-flash-free` (Tencent). A `gemini-3.8-flash` record with `vendor=cursor` does not
count.
Since 2026-11-19 (Chad) `gemini-4-neon-high` (Google; Gemini 4 Neon through the Antigravity CLI on
Google AI Pro) is a sensitive-merge seat too.
A sensitive PR needs two allowed seats from two different vendors, neither of them the PR author's
vendor (Anthropic for Claude lanes, OpenAI for Sol lanes), each scoring at least 9 on the exact
head, or, when the head has no records of its own, on the head a carry reaches (see
[Score carry-forward](#score-carry-forward); Grok does not count toward the two there). Repeated
seats count once. Other seats do not qualify.

Missing fields, malformed score lines, unknown seats or vendors, mismatched
seat vendors, same-vendor scoring, conflicting author metadata, and scores
below 9 on the head whose records count refuse the merge. An `author=` that is not a seat vendor
(`cursor-auto` or `unknown`) anywhere on the PR also refuses it. GitHub errors or unreadable data
also refuse the merge. Owner and lane classifications are unchanged.

### How records are read

`read_comments` in `scripts/merge_class.py` turns the raw PR comments into records for both evaluators, and `head_fault` applies the same head rules to the exact head and, through `carry_source`, to a carried head. Where the two once read a comment differently, the stricter reading now holds for both (GRE-1859):

- A record line starts with `pp-score ` once surrounding whitespace and inline-code backticks are dropped; prose that mentions the token mid-line is ignored. At column 0 the line must be the record alone. A record line that does not match the format with a valid `obstacles=` token refuses every head, as does a comment whose author or body cannot be read.
- A record wrapped in backticks or indented is checked like any other, so a low or invalid one fails its head, but it never counts toward a pass. Post records bare, at column 0; a bare line inside a code fence counts.
- A record fails its own head, which then counts as scored for a carry walk, when its comment was edited or has no readable edit times, when it has no `obstacles=`, or when its seat is not an allowed seat for its `vendor=`. Records for other heads never count.
- A seat from the author's own family, records that disagree on `author=`, or a record below 8 fail the head with one reason in both evaluators (`<seat> scored <n>` for a low record). A record of 8 fails only the sensitive merge check, which needs 9.

Never edit a score record; post a new one. An edited record fails its head in both evaluators; edits to records for other heads do not matter, because those never count. Through the `gh` routes (`gh pr comment`, `gh issue comment`, `gh api` and its GraphQL endpoint), the guard refuses edits and deletes of PR comments outside the coordinator worktree: any method but GET or HEAD on `issues/comments/<id>` or `pulls/comments/<id>` (including POST, fields without `-X`, and an `X-HTTP-Method-Override` header), `--edit-last`, `--delete-last`, and the GraphQL update and delete comment mutations. A program that calls the GitHub API with the account's token directly is not covered, and a deleted record cannot be detected, until score records move to an identity lanes cannot use (GRE-1408).

The allowed seats (`scorer_seats`, seat to vendor) and the vendors a record may name (`vendors`) are data in `scripts/merge-classes.json`; the record pattern is built from them. Removing a vendor makes old records that name it unparseable, so a PR that carries one refuses the merge.

When a trusted `scorer_authors` account posts a well-formed record, the `post-pp-score-status` workflow also posts a `pp/score` commit status on that exact head SHA (`N/10 from <seat>`, success at 8 or more). The status is copied from the comment; it is never invented or re-derived. Records for other SHAs are left untouched. For this status only:

- A record whose `vendor=` equals its `author=` is a same-family score and fails the head, as does a head whose records disagree on `author=`. The old `Score: N/10 from <seat> on head <sha>` form names no author family and no longer counts.
- If `author=` is `cursor-auto` or `unknown`, the head also needs a bare trusted Anthropic record and a bare trusted OpenAI record (`gpt-6.1-sol` or another OpenAI seat) on that exact SHA; a wrapped record counts toward no pass. The status uses the lower of those scores and succeeds only at 8 or more. A missing family, a score below 8, or a complete pair on a stale SHA fails closed, including on `merge_group`. Known families are unchanged: one trusted cross-family score of 8 or more is enough, and same-family scores still fail.
- `pp_score_seats` in `scripts/merge-classes.json` adds seats the status accepts on top of `scorer_seats`. It holds the Codex seats `gpt-6.1-sol`, `gpt-6.2-sol` and `gpt-6.2-astra` (all OpenAI; 6.2-sol added 2026-11-17; 6.2-sol and 6.2-astra are also in `scorer_seats` since 2026-11-18). Since the 2026-11-18 7:38 PM ET owner decision it also holds `gemini-3.8-flash`, `qwen38-gufo`, `qwen38-flash`, `longcat-2.5-free` and `hy4-flash-free`, the same five seats as `scorer_seats`. The sensitive merge guard still reads `scorer_seats` only.
- The `post-pp-score-status` workflow checks out the default branch for a comment or a `workflow_dispatch`, whatever ref a dispatch selects, and the queue entry's base SHA for `merge_group`. It never runs the script or `merge-classes.json` from a PR head. A change to the record format takes effect for every PR once it merges, including the PR that makes it. A dispatch still runs the workflow file of the ref it selects. Only accounts with write access can dispatch, and they can already post a commit status directly (see below).
- A head with no records of its own can carry the `pp/score` of an earlier head H0 across merges of main that leave the PR's code exactly as scored (Chad, 2026-11-18 11:02 PM ET). See [Score carry-forward](#score-carry-forward).
- The workflow runs on a new comment, on `workflow_dispatch` and on `merge_group`; a push alone does not re-run it. After an update-branch, once CI is green on the new head, dispatch the workflow with the PR number (or post any comment) to post the carried status. `--carry-only` previews the carry for the current head as if it had no records.
- On `merge_group`, the workflow checks out the queue entry's base SHA, reads the PRs in the group from its squash commits (`... (#N)`), and posts `pp/score` on the merge group SHA. It posts success only when every PR has a trusted score of 8 or more on its current head, or, for a head with no records of its own, a carry that it decides again from live facts; otherwise, or when anything cannot be read, it posts failure naming the PR or the reason, so the entry leaves the queue instead of waiting for the timeout.

`pp/score` is a plain commit status. While scores post as `poitee`, any account or workflow with write access to statuses can also post `pp/score=success`; a dedicated scorer identity was deferred by the owner on 2026-11-16.

## Score carry-forward

Merging main into a scored PR makes a new head with no records. That head can use the records of the nearest earlier head H0 that has records, through one or more hops. There is one carry decision, `carry_source` in `scripts/merge_class.py` (which calls `decide_carry`), and both evaluators call it: the `pp/score` status (`plan_carry` in `scripts/post_pp_score_status.py`) and the coordinator's sensitive merge check (`sensitive_scores`, called from `scripts/agent-guard.py`). If a condition fails or a fact cannot be read, nothing carries and the head needs fresh scores. In the guard, an unreadable fact refuses the merge.

Each hop from P to a new head H must hold all of these. Each is fixed by the commit SHAs, so a hop that qualified stops qualifying only when M falls past the bound in condition 2:

1. H has exactly two parents, `[P, M]`. A rebase, squash, force-push or octopus merge does not qualify.
2. M was `main`'s tip at some time: it is on the first-parent history of `main`'s current tip (read from GitHub), within 5,000 commits. Being reachable from `main` is not enough: the inner commit of a merged side branch, or a commit merged with `-s ours`, is reachable but its content need not be in `main`. The ruleset forbids force-pushes to `main`, so a yes turns to no only past the bound.
3. P and M have exactly one merge base B, and B is neither P nor M. A criss-cross history has no single PR diff, so it does not carry.
4. `git merge-tree --write-tree P M` is clean and its tree equals H's tree, so the merge commit has no hand edits.
5. The PR's diff `B..P` is byte-identical to its diff `M..H`: the same paths, modes and full blob IDs on both sides, with no rename pairing, text conversion or external diff.
6. No commit in `P..M` changed a path that the PR changes, even if a later commit undid it. Merge commits count against each parent.
7. No commit in `P..M` changed a gate file: an owner or sensitive path in `scripts/merge-classes.json`, or `scripts/post_pp_score_status.py`, its test file or this page.

`decide_carry` also requires:

- H has no records of its own, and every trusted record on the PR, on any head, names the same `author=` family.
- The walk stops at the first head with records, so newer records win, even failing ones. It goes back at most 5 hops.
- Every required check of `main`'s rulesets except `pp/score` (today `actionlint`, `zizmor` and `rust (linux)`) concluded `success` on H itself. Nothing else qualifies: not `skipped`, `neutral`, `cancelled`, `timed_out`, `action_required` or `stale`, and not a check that is pending or missing. GitHub lets a skipped check satisfy branch protection, but the gate cannot prove that a skip came from the path filter rather than a draft, a failed `changes` job or a cancelled run, so it fails closed. **As a result, a PR whose path filters skip `rust (linux)`, `actionlint` or `zizmor` never carries.** That covers most docs-only, gate-only and non-Rust PRs; they need fresh scores after every update. The checks of earlier hops are not read. Those heads never merge, and GitHub requires the checks again on H and on the merge group SHA.

`carry_source` then applies the shared head rules to H0 (a fault, only wrapped records, the author's own family, disagreeing `author=`). Each evaluator then applies its own bar to H0's records:

- **`pp/score` status:** H0's records, Grok included, must pass as they would on H0 itself (`plan_head`). Grok (`xai`) and wrapped records are then dropped, and at least one must remain. If the PR touches an owner or sensitive path (both sides of a rename, from a complete file list), every record on H0 must be above 8. The status reads `carried from <sha7>: N/10 from <seat>`.
- **Coordinator sensitive merge:** H0's records must pass as on an exact head: every record on H0 is a `scorer_seats` seat at 9 or more, and bare records from two different vendors, neither the author's, remain. Grok records are checked but do not count toward the two. The guard reads the head's required checks once, as the status does, and reads commits from the coordinator worktree's clone, bounded by its 45-second budget. See [Git settings the carry ignores](#git-settings-the-carry-ignores).

So the guard never accepts a carry that the status refuses, and when it accepts one, it uses the same H0. The difference is deliberate: the status passes one seat, while a sensitive merge always needs two vendors. `carry_both` in `scripts/test_post_pp_score_status.py` runs both evaluators on every carry fixture, from raw comment bodies, and fails when they reach different `carry_source` results or a refusal reason differs.

### Git settings the carry ignores

The read commands of `LocalGit` in `scripts/merge_class.py` see no Git configuration it did not write. These commands, which a carry decides from (`rev-list`, `rev-parse`, `merge-base`, `merge-tree`, `diff` and `log`), run in a scratch Git directory that it creates under `TMPDIR` and leaves in place. Before each command it rewrites that directory's config, `HEAD` and `objects/info/alternates`. The directory has no refs, grafts, attributes or remote, and its config turns off the commit-graph and the multi-pack-index. Each command runs with system and global config off (`GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`), with `HOME` and `XDG_CONFIG_HOME` set to the scratch directory, with every other `GIT_*` variable dropped, and with `--no-replace-objects`. So no command fetches, and none runs a program named by the environment or a config. The commands keep explicit flags: `--no-relative`, `--no-ext-diff`, `--no-textconv`, `--no-renames`, `--ignore-submodules=none`, `--full-index` and `-z`. The status and the guard therefore read the same Git defaults. A test writes 28 settings into local, included, `HOME` and XDG config and sets `GIT_CONFIG_*` variables, and checks that every `LocalGit` output is byte-identical.

Tracked `.gitattributes` in the merged trees cannot apply. The scratch directory has no worktree, index or `HEAD` tree to read them from, merge drivers act only on paths both sides changed (which condition 6 refuses), and diff drivers are off.

Only `ensure` runs commands in the clone with its configuration and transport variables, to fetch whole commits by full SHA; it then drops the scratch directory, so every later read uses a new one. One `rev-parse --git-common-dir` also runs in the clone, with no `GIT_*` variable, to find the object store. A clone that lacks a blob a read needs (a `blob:none` clone) refuses rather than fetching it.

### What the guard defends against

In scope is state that ordinary Git commands or environment variables can set, by accident or on purpose:

- Settings in the shared `.git/config`, its includes, global, XDG and system config. Tests: `test_no_git_configuration_reaches_the_carry` (local, included, `HOME`/XDG and `GIT_CONFIG_SYSTEM`) and `test_no_setting_anywhere_changes_what_local_git_reads` (28 settings).
- The merge command's environment and working directory. Tests: `test_git_config_env_and_cwd_cannot_change_what_the_carry_sees`, `test_both_decide_the_same_from_a_subdirectory_with_diff_relative`, `test_a_read_never_fetches_or_runs_a_transport_program` and `test_no_caller_git_variable_reaches_a_read`.
- Replace refs and grafts. Test: `test_replace_refs_and_grafts_do_not_change_the_parents`.
- `info/attributes`. Covered by the 28-setting test.
- Other refs, the commit-graph and the multi-pack-index: by design (the scratch directory has no refs, and both indexes are off), not tested.

Out of scope:

- Forged objects of any kind (commits, trees, blobs or pack indexes) in the shared object store, its alternates or the scratch directory.
- A transport program named by the environment or the clone's config. The coordinator's own fetch runs it, so it is code execution as the coordinator.
- A replaced `git` binary, and edits to the coordinator checkout's own files, which include the guard.
- Score records posted with the scorer account's token (GRE-1408).

When the current head has no records and no carry, the workflow posts `pp/score=failure` (`no score and no carry: <reason>`), which replaces an earlier carried success. The `merge_group` run decides every carry again, so a newer failing record on H0 or a failing check on H fails the queue entry. If the run cannot read the PR's current head, it posts the statuses for heads with records, posts nothing on the unknown head, and exits 1. That head keeps its last `pp/score`. Every merge goes through the merge queue, whose run decides the carry again.

Runs for one PR share a `concurrency` group, but GitHub does not keep dispatch order in a group, and a newer pending run replaces an older one. What a comment-mode run guarantees: each round reads the PR's comments, its head SHA and the head's required-check result once, decides from those values (the carry gets the check result as a value and does not read the checks again), and keeps the head SHA, the check result, and the ids and edit times of the comments. After posting, it reads them again and compares. If any changed, it decides and posts again, up to 3 times. If they are still changing after the third round, it posts `pp/score=failure` on the head (`re-evaluation needed: ...`) and exits 1, so it never leaves a success it could not confirm. A run that settles has re-read, after its last post, the same head, comment ids and edit times, and check result that post was decided from. A new comment starts its own run. A check that changes after the last re-read starts no run, and GitHub's required checks then block the merge on their own. `--pr` accepts only a canonical number (no leading zeros or spaces), so a dispatch lands in the same group as the PR's comments.

In the depth-1 workflow checkout, the script first fetches the head's full history, blobs included (`git fetch --unshallow`), because the scratch directory never fetches. It then fetches `main`'s tip for condition 2. If a fetch fails, nothing carries.
