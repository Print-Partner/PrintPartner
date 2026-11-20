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

One reader, `read_comments` in `scripts/merge_class.py`, turns the raw PR comments into records for
both evaluators, the `pp/score` status and the coordinator's sensitive merge check. Both then apply
the same head rules (`head_fault`) on the exact head and, through `carry_source`, on a carried head.
Where the two evaluators once read a comment differently, the stricter reading now holds for both
(GRE-1859):

- A record line is a line that starts with `pp-score ` once surrounding whitespace and inline-code
  backticks are dropped. Prose that mentions the token mid-line is ignored.
- A record line at column 0 must be the record alone, with no trailing space or backtick. Every
  record line must match the format with a valid `obstacles=` token, or it refuses every head. So
  does a comment whose author or body cannot be read.
- A record wrapped in backticks, indented, or indented inside a fence is read and checked like any
  other record, so a low or invalid one fails its head and stops a carry walk. It never counts
  toward a pass, and a head with only wrapped records fails. Post records bare, at column 0; a bare
  line inside a code fence counts.
- A record fails its own head when its comment was edited (`updated_at` differs from `created_at`)
  or has no readable edit times, when it has no `obstacles=`, or when its seat is not an allowed seat
  for its `vendor=`. Such a head counts as scored, so a carry walk stops there.
- A record whose `vendor=` equals its `author=`, or a head whose records disagree on `author=`,
  fails that head.
- A record below 8, bare or wrapped, fails its head in both evaluators with one reason,
  `<seat> scored <n>`; the `pp/score` description reads `rejected: <seat> scored <n>`. A record
  of 8 fails only the sensitive merge check, which needs 9.

Records for heads that are neither the current head nor the head a carry reaches never count.

Never edit a score record; post a new one. An edited record fails its head in both evaluators; edits to records for other heads do not matter, because those never count. Through the `gh` routes (`gh pr comment`, `gh issue comment`, `gh api` and its GraphQL endpoint), the guard refuses edits and deletes of PR comments outside the coordinator worktree: any method but GET or HEAD on `issues/comments/<id>` or `pulls/comments/<id>` (including POST, fields without `-X`, and an `X-HTTP-Method-Override` header), `--edit-last`, `--delete-last`, and the GraphQL update and delete comment mutations. A program that calls the GitHub API with the account's token directly is not covered, and a deleted record cannot be detected, until score records move to an identity lanes cannot use (GRE-1408).

The allowed seats (`scorer_seats`, seat to vendor) and the vendors a record may name (`vendors`) are data in `scripts/merge-classes.json`; the record pattern is built from them. Removing a vendor makes old records that name it unparseable, so a PR that carries one refuses the merge.

When a trusted `scorer_authors` account posts a well-formed record, the `post-pp-score-status` workflow also posts a `pp/score` commit status on that exact head SHA (`N/10 from <seat>`, success at 8 or more). The status is copied from the comment; it is never invented or re-derived. Records for other SHAs are left untouched. For this status only:

- A record whose `vendor=` equals its `author=` is a same-family score and fails the head, as does a head whose records disagree on `author=`. The old `Score: N/10 from <seat> on head <sha>` form names no author family and no longer counts.
- If `author=` is `cursor-auto` or `unknown`, the head also needs a trusted Anthropic record and a trusted OpenAI record (`gpt-6.1-sol` or another OpenAI seat) on that exact SHA. The status uses the lower of those scores and succeeds only at 8 or more. A missing family, a score below 8, or a complete pair on a stale SHA fails closed, including on `merge_group`. Known families are unchanged: one trusted cross-family score of 8 or more is enough, and same-family scores still fail.
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

`LocalGit` in `scripts/merge_class.py` runs `rev-parse`, `cat-file -e`, `fetch`, `rev-list --parents`, `rev-list --first-parent`, `merge-base --all`, `merge-tree --write-tree`, `diff` (patch and `--name-only -z`) and `log -m --name-only -z --format=`. The guard runs them in the coordinator's clone, from any directory inside the worktree. Every lane worktree shares that clone's `.git/config`, `info/attributes`, `refs/replace` and `info/grafts`, and a lane may set `GIT_*` variables. The status runs in a fresh Actions checkout with default settings. So every call pins what could change the output to those defaults:

| Setting | How it is pinned |
| --- | --- |
| `diff.relative`, and the cwd inside the worktree | `-c diff.relative=false` and `--no-relative` on every diff and log. A test runs the guard from a subdirectory. |
| `diff.external`, `GIT_EXTERNAL_DIFF`, `diff.<driver>.command` and `.textconv` | `--no-ext-diff --no-textconv` on every diff and log. Tested. |
| `diff.renames` | `--no-renames` on every diff and log. |
| `diff.ignoreSubmodules`, `submodule.<name>.ignore` | `-c diff.ignoreSubmodules=none` and `--ignore-submodules=none`. |
| `diff.noprefix`, `diff.mnemonicPrefix` | Pinned `false`, and `--src-prefix=a/ --dst-prefix=b/`. |
| `diff.orderFile` | Pinned to an empty file. |
| `core.quotePath` | Pinned `true`. Path lists use `-z`. |
| `core.attributesFile` | Pinned to an empty file. |
| `color.ui`, `color.diff` | `-c color.ui=false` and `--no-color`. |
| `log.showSignature` | Pinned `false` and `--no-show-signature`. |
| `merge.renames`, `merge.directoryRenames`, `merge.renormalize` | Pinned to Git's defaults: `true`, `conflict`, `false`. |
| `core.pager`, `pager.*`, `GIT_PAGER` | `--no-pager`. Output is captured, never a terminal. |
| `refs/replace`, `info/grafts` | `--no-replace-objects` and `GIT_GRAFT_FILE=/dev/null`. Tested. |
| `GIT_*` environment (`GIT_DIFF_OPTS`, `GIT_CONFIG_*`, `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_NAMESPACE`, `GIT_REPLACE_REF_BASE` and the rest) | Dropped, except the transport variables a fetch needs (`GIT_ASKPASS`, `GIT_SSH*`, `GIT_TERMINAL_PROMPT`, `GIT_SSL_*`, `GIT_HTTP_*`, `GIT_PROXY_*`, `GIT_CURL_*`). |

The rest cannot change a carry decision:

- `diff.algorithm`, `diff.indentHeuristic`, `diff.context`, `diff.interHunkContext`, `diff.suppressBlankEmpty`: these change only the shape of hunks. Both diffs are made the same way, and `--full-index` prints every file's full blob IDs, so equal output still means equal paths, modes and blobs.
- `core.abbrev`: every command here prints full object names.
- `i18n.logOutputEncoding`, `log.decorate`, `log.mailmap`, `format.*`: `--format=` prints no commit text.
- `info/attributes` and in-tree `.gitattributes`: diff drivers are off. `binary` changes only the patch body, and the index lines remain. Merge drivers and `merge.conflictStyle` act only on paths both sides changed, which condition 6 refuses.
- `core.autocrlf`, `core.eol`, clean and smudge filters, `core.worktree`, `core.ignoreCase`: these act on a worktree, and every command here compares commits and trees.
- `fetch.*`, the remote URL, `objects/info/alternates`: objects are addressed by their hash, so a fetched object is the one asked for, or the call fails and nothing carries. A shallow clone is unshallowed first, and a shallow boundary can only hide parents, which refuses.

When the current head has no records and no carry, the workflow posts `pp/score=failure` (`no score and no carry: <reason>`), which replaces an earlier carried success. The `merge_group` run decides every carry again, so a newer failing record on H0 or a failing check on H fails the queue entry. If the run cannot read the PR's current head, it posts the statuses for heads with records, posts nothing on the unknown head, and exits 1. That head keeps its last `pp/score`. Every merge goes through the merge queue, whose run decides the carry again.

Runs for one PR share a `concurrency` group, but GitHub does not keep dispatch order in a group, and a newer pending run replaces an older one. What a comment-mode run guarantees: each round reads the PR's comments, its head SHA and the head's required-check result once, decides from those values (the carry gets the check result as a value and does not read the checks again), and keeps the head SHA, the check result, and the ids and edit times of the comments. After posting, it reads them again and compares. If any changed, it decides and posts again, up to 3 times. If they are still changing after the third round, it posts `pp/score=failure` on the head (`re-evaluation needed: ...`) and exits 1, so it never leaves a success it could not confirm. A run that settles has re-read, after its last post, the same head, comment ids and edit times, and check result that post was decided from. A new comment starts its own run. A check that changes after the last re-read starts no run, and GitHub's required checks then block the merge on their own. `--pr` accepts only a canonical number (no leading zeros or spaces), so a dispatch lands in the same group as the PR's comments.

In the depth-1 workflow checkout, the script first fetches the head's full commit history without blobs (`git fetch --unshallow --filter=blob:none`). Git fetches the blobs a merge or diff reads on demand. It then fetches `main`'s tip for condition 2. If a fetch fails, nothing carries.
