# Change a CI workflow

This page covers review, linters, action pins and required checks for every workflow.
[Continuous integration](ci.md) describes the Rust cross-OS jobs, their exclusions and how to run them locally.

1. Open a pull request against `main` with the workflow change and its verification evidence.
2. Request review from Chad, `@poitee`, for any path listed in `.github/CODEOWNERS`.
3. Run actionlint 1.7.12 with ShellCheck and zizmor 1.31.1 before pushing.

   ```sh
   actionlint -color
   zizmor --config .github/zizmor.yml --min-severity informational .
   ```

   Use `zizmor --offline` for a local run without GitHub authentication.
   CI also runs the online audits with its read-only GitHub token.
4. Fix findings or document a narrow exception and its reason in `.github/zizmor.yml`.
5. Wait for the PR checks and the independent review on the final commit.
   The applied ruleset and the pending review requirements are described below.

## Ownership and checks

CODEOWNERS assigns these paths to `@poitee`:

- `.github/CODEOWNERS`, `.github/workflows/`, and `.github/actions/`.
- `.github/dependabot.yml` and `.github/zizmor.yml`.
- `rust/scripts/check-*` and `rust/deny.toml`.
- The action pin, CI tool pin, desktop workflow, and workaround checks and their tests.
- The Windows tier selector `scripts/ci-path-filter.py` and its test `scripts/test_ci_path_filter.py`.
- The duplication checker, its tests, and `scripts/duplication-baseline.json`.
- The PR evidence checker and its test, and the `scripts/linear/` directory.
  Linear PR sync runs a script from that directory with the Linear secret. Python imports files next to that script before the standard library.
- `scripts/release.mjs`, `web/scripts/atomic-package-build.mjs`, `Dockerfile`, and `docker-entrypoint.sh`, which the release workflow runs directly or builds into the published image.
- `rust/ast-grep-baseline.json`, `rust/clippy-limits-baseline.json`,
  `scripts/workaround-baseline.json`, and Rust snapshot directories under `rust/crates/`.

The workflow `.github/workflows/actionlint.yml` starts on every `pull_request`, on pushes to `main` and `master`, on merge groups and on manual dispatch.
Its `changes` job selects the `actionlint` and `zizmor` jobs.
On PRs, both jobs run when `.github/workflows/**`, `.github/actions/**`, either linter's configuration, or the CI filter script and its tests change.
For other paths, including documentation-only PRs, both jobs are skipped and still report their named checks.
Non-PR runs always execute both linters.
Both jobs fail if the `changes` job fails, so a broken filter cannot skip a required check.
Both jobs have only `contents: read` permission and disable persisted checkout credentials.
A new PR push cancels that PR's older run. Pushes to `main` share one group per workflow: one run executes and one waits, and a newer push replaces the waiting run, which GitHub reports as cancelled. Other non-PR runs have their own group and never wait for or cancel another run.

Actionlint 1.7.12 checks workflow syntax and embedded shell commands with ShellCheck.
The job verifies the actionlint download against its SHA-256 checksum before execution.
It reports through the job exit status, so fork PRs do not need permission to create check runs.

Zizmor 1.31.1 uses the regular persona and fails on all reported severities, including informational findings.
There is no confidence filter.
This keeps low-severity notices visible without adding the pedantic persona's style checks.
The pinned `zizmor-action` runs online audits and emits job annotations without a SARIF upload or write permission.
Its default pin policy requires every external action, including `actions/*`, to use a full commit SHA.

## Existing findings

The first offline zizmor run reported twelve findings.
CI also found a version comment that did not match its pinned action.
The workflow and configuration changes handle them as follows:

| Finding | Resolution |
| --- | --- |
| `artipacked` in Desktop contracts | Set `persist-credentials: false` on checkout. |
| `template-injection` in Desktop WebKit | Pass the artifact URL through an environment variable before the shell command. |
| `dangerous-triggers` in PR evidence | Allow only the trigger line. Checkout runs trusted default-branch tooling. The PR body enters the checker through an environment variable. |
| `dangerous-triggers` in Linear PR sync | Allow only the trigger line. PR events run the immutable base commit. Scheduled runs use the default branch. The workflow never runs PR-head code. |
| `dangerous-triggers` in Review gate | Allow only the `pull_request_target` line. The gate must run the default branch's logic so a PR cannot edit its own gate. It checks out nothing, runs no PR code, receives no secrets, and only reads the API with `actions`, `checks`, `contents`, and `pull-requests` read permission. |
| `dangerous-triggers` in Review gate rerun | Allow only the `issue_comment` and `workflow_run` lines. Both run the default branch's file, check out nothing, read no comment text into a shell, and only rerun the gate, which re-checks every result's origin. |
| Two `self-repository` notices in Release | Allow only the two local reusable-workflow lines. They resolve at the caller's commit. Actionlint 1.7.12 rejects zizmor's suggested `$/` replacement syntax. |
| Mismatched action version comment in Chain tip gate | Replace the moving `# v2` comment with the verified `# v2.87.27` tag. Keep the existing commit SHA. |
| `dependabot-cooldown` for every ecosystem | Set a seven-day cooldown on the GitHub Actions, npm, pip, Docker and Cargo entries. |

Every exception has its reason in `.github/zizmor.yml` and requires the owner's review.
No audit rule is disabled.

## Update action pins

Dependabot already checks the `github-actions` ecosystem weekly at the repository root.
It opens pin updates with a seven-day cooldown for new action releases.
Review those PRs with the same checks and owner approval as a manual workflow change.

To update a pin manually:

1. Choose an upstream release and resolve its tag to the full 40-character commit SHA.
   If the tag is annotated, resolve the tag object to its commit.
2. Set `uses: owner/action@<commit>` and keep a version comment such as `# v1.2.3` on that line.
3. Review the upstream diff and rerun both linters.
4. To update actionlint itself, update the exact download version and checksum together.
   To update zizmor, change its exact `version` input and review the changed audits.

Run [`scripts/check-github-actions-pinned.sh`](../../scripts/check-github-actions-pinned.sh) from the repository root.
Web CI runs the same script. It exits with status zero when `unpinned=0`.

```sh
bash scripts/check-github-actions-pinned.sh
```

## Dependency updates

Every Dependabot ecosystem waits 7 days after a release for routine version updates.
The cooldown does not delay security updates.

Dependabot ignores semver-major updates for npm, Docker and Cargo.
Cargo pins direct crates with `=` and uses `versioning-strategy: lockfile-only`, so Dependabot cannot change a pinned major version.
Bump a major version in a ticketed PR that updates the manifest pin, `Cargo.lock` and any code the new API needs.

<a id="settings-for-chad"></a>

## Applied main-branch ruleset

The GitHub ruleset
[main CI controls](https://github.com/Print-Partner/PrintPartner/rules/25285698), ID `25285698`,
is active on the default branch, currently `main`.
The [committed ruleset JSON](main-ci-controls.ruleset.json) records its configuration.
It omits node IDs, timestamps, API links, and the current user's bypass capability.

The ruleset has these protections:

- Block branch deletion and force pushes.
- Require a pull request and allow only squash merges.
- Require GitHub Actions checks `actionlint`, `zizmor`, `rust (linux)`, `Lint Evidence block`,
  and `review-gate`. `rust (windows)` is advisory and not required.
- Require every review thread to be resolved, so an open finding from any reviewer blocks the merge.
- `review-gate` runs on `pull_request_target` from the default branch and only reads the API; see [Code review](definition-of-done.md#code-review) for what it checks and its remaining risk.
  Dependabot's weekly `github-actions` PRs change `.github/workflows/`, and their author is not the owner, so they need both a passing Droid or Claude review and the owner's marker for the head. The Claude jobs run for Dependabot PRs once the owner stores `CLAUDE_CODE_OAUTH_TOKEN` in the Dependabot secrets too, because Dependabot runs receive only those.
- `claude-review.yml` and `droid-review.yml` stay on `pull_request`, not `pull_request_target`. On `pull_request`, fork PRs receive no secrets, so a fork's Droid job fails on the missing key, and the Claude jobs also require a same-repository head and the owner or Dependabot as author. Anyone who can push a same-repository branch already has write access, which lets them run any workflow with secrets through a push or a manual dispatch, so these workflows grant nothing beyond write access. `pull_request_target` would hand secrets to a run that checks out PR code, the classic pwn-request pattern. After reviewing such a PR's head, the owner or the coordinator posts the `pp-review-gate` marker for that head, which passes it.
- Allow branches that are not up to date with `main`.
- Keep the bypass list empty, with no standing admin or bot bypass.

`rust (macos)` and `rust (windows)` are absent, consistent with their advisory status in [Continuous integration](ci.md).
`droid-review` replaced the `pp/score-verified` model-score check on 2026-10-31.

No approving reviews are required. Chad authors PRs as `poitee`, and GitHub forbids
self-approval. CODEOWNERS requests Chad's review but does not make it a merge requirement.

To apply the committed JSON to the live ruleset (admin only):

```sh
jq 'del(.id, .source, .source_type)' docs/development/main-ci-controls.ruleset.json \
  | gh api -X PUT repos/Print-Partner/PrintPartner/rulesets/25285698 --input -
```

To compare the snapshot with the live ruleset, run this read-only check from the repository root:

```sh
gh api repos/Print-Partner/PrintPartner/rulesets/25285698 \
  | jq -S 'del(.node_id, .created_at, .updated_at, ._links, .current_user_can_bypass)' \
  | diff -u docs/development/main-ci-controls.ruleset.json -
```

No diff means the normalized live response matches the snapshot. A diff needs review before updating the snapshot.

<a id="turn-on-the-merge-queue"></a>

## Turn on the merge queue

The repository moved to the Print-Partner organization on 2026-11-07, which makes GitHub's merge queue available on this public repository at no cost. All five required checks report on `merge_group` events; [Merge queue](ci.md#merge-queue) describes what each one does there. The queue stays off until Chad adds the rule below. Agents never change the ruleset.

Chad adds one `merge_queue` rule to ruleset `25285698` and changes nothing else in it:

| Setting | Value | Why |
| --- | --- | --- |
| `merge_method` | `SQUASH` | Matches `allowed_merge_methods: ["squash"]`. |
| `grouping_strategy` | `ALLGREEN` | Only passing combinations land. A failing group ejects its PR; read the log before you queue it again. |
| `max_entries_to_build` | `2` | Each entry builds the full Linux Rust path on hosted runners. Hosted concurrency is the limit. |
| `min_entries_to_merge`, `max_entries_to_merge`, `min_entries_to_merge_wait_minutes` | `1`, `5`, `5` | A lone PR is not held. Merge limits only batch the final merge and save no CI. |
| `check_response_timeout_minutes` | `150` | Covers a full Linux and Windows run with hosted queueing. |

The required status checks and `strict_required_status_checks_policy: false` stay as they are; the queue replaces "branch must be up to date".

In the web UI: **Settings > Rules > main CI controls > Require merge queue**, with the values above. Or, as an admin, from the repository root:

```sh
jq 'del(.id, .source, .source_type)
    | .rules += [{"type": "merge_queue", "parameters": {
        "check_response_timeout_minutes": 150, "grouping_strategy": "ALLGREEN",
        "max_entries_to_build": 2, "max_entries_to_merge": 5, "merge_method": "SQUASH",
        "min_entries_to_merge": 1, "min_entries_to_merge_wait_minutes": 5}}]' \
  docs/development/main-ci-controls.ruleset.json \
  | gh api -X PUT repos/Print-Partner/PrintPartner/rulesets/25285698 --input -
```

Afterwards, save the live ruleset into `docs/development/main-ci-controls.ruleset.json` with the read-only command above in a docs PR, then queue two small PRs back to back to test it.

With the queue on, `gh pr merge <n> --squash --match-head-commit <sha>` adds the PR to the queue instead of merging it, so queueing a PR is merging it. Queue only a PR whose required checks already passed on its head. Removing a PR from the queue needs the web UI.


## Phase 2 for Chad

Phase 2 remains Chad's work and keeps GRE-635 open:

1. Create and install a GitHub App for agents with contents, pull requests, and workflows write access, plus checks read access.
   Store its key in the VMs' secret store and switch agent PR authorship to the App.
   Chad owns credentials and their handling.
2. Require code-owner review, dismiss stale approvals, require approval of the latest push, and require review threads to be resolved.
   First test whether code-owner review is enforced with `required_approving_review_count: 0`.
   If it is, only CODEOWNERS paths need Chad's approval. Otherwise, a count of one requires Chad to approve every PR.
3. Test enforcement with the separate agent identity.
   A workflow PR must not merge without Chad's approval.
   A PR touching only `rust/` must merge on green checks if the review policy permits it.
   A docs-only PR must receive both named lint checks.

## Edits to a pull request

An edit to a pull request body, title or base can start workflows. This matrix comes from the `on:` blocks and job conditions, and from the run history of PR #1547 (heads 91478f2 and 8ac5680), where `pr-evidence.yml` ran on every body edit.

| Event | `review-gate` | `Lint Evidence block` | `droid.yml`, `linear-pr-sync.yml` |
| --- | --- | --- | --- |
| Push (`synchronize`) | runs and waits for the Claude review; cancels the older run | runs, except on a draft PR, where the job skips | do not start |
| Body or title edit (`edited`) | edit run: a full run in the same cancelling group as pushes (the newest run for the PR wins); it waits for a running Claude job (up to 30 minutes) and up to two minutes for one that has not appeared | runs, except on a draft PR; a title edit lints the same body again, a body edit lints the new one | `droid.yml` starts and its job skips on a draft PR or unless the body or title contains `@droid`; `linear-pr-sync.yml` starts and syncs only same-repository PRs into the default branch |
| Base change (`edited`) | edit run: judges the head against the new base and cancels an older run that read the old one | runs, except on a draft PR | same as above |
| Owner comment with a `pp-review-gate` marker (created, edited or deleted) | rerun by `review-gate-rerun.yml`; a marker from anyone else does not rerun | does not run | `droid.yml` starts only when the comment is created, and skips unless it contains `@droid` |
| Review workflow finishes | rerun by `review-gate-rerun.yml` | does not run | do not start |

Neither required check can safely skip an edit on a ready PR. A draft PR is the one exception for `Lint Evidence block`, by existing design: its job skips while the PR is a draft, and that skip does replace an earlier failure, including on `converted_to_draft`. GitHub does not merge a draft, and marking the PR ready runs the lint again on the current body, so the failure cannot reach `main`. `review-gate` has no draft skip.

- `Lint Evidence block` lints the whole body: Evidence lines and `Exemption:` lines both decide the verdict, and fixing a failing Evidence section by editing the body is the normal path. A job-level `if` that skips an edit would leave a skipped required check, which GitHub counts as a pass, and the newer run replaces an earlier failure, so an unrelated edit would clear a failing lint. Not listening to `edited` would keep an old success after a body edit that breaks Evidence. A short job that repeats the previous verdict avoids neither the runner slot nor the queue.
- `review-gate` has to run on a base change, because a review of a head against one base does not cover the same head against `main`. GitHub does not filter `edited` events by field, and a job-level `if` has the same skipped-required-check problem, so every edit starts an edit run. It reads the API and takes seconds unless a Claude job is running or has not appeared yet, and it cancels the older run for the same PR, so the newest run decides.

`scripts/test_merge_group_workflows.py` keeps `Lint Evidence block` in the shape the table shows, and `scripts/test_review_gate.py` keeps `review-gate` in it. No test pins the `droid.yml` and `linear-pr-sync.yml` cells. To reduce edit-triggered runs, batch body changes into a single edit. Editing before a push still starts the edit-triggered workflows.

## Review gate outage

When GitHub posts an Actions outage, open the affected workflow run page and check
for the outage notice. After the outage ends, rerun `review-gate` on every open
PR's current head, including heads whose gate never started during the outage.

The gate gives `claude-code-review` and `claude-security-review` a **2-minute
grace period** to appear. If either is still missing, it stops waiting and fails
with the job name and this runbook link unless another valid review path passes.
Jobs that exist and are queued or running retain the **30-minute total ceiling**
when neither job is missing.

For a missing review job, rerun its `claude-review.yml` workflow **once only**
when an external cause, such as the GitHub outage, explains the missing job.
A rerun is the usual fix; if the job is still missing, investigate the run and
workflow configuration instead of repeatedly rerunning it. Rerun `review-gate`
after the review finishes if it remains red. Do not push empty commits to force
a run. Do not add a job-level `if` to skip a required check: GitHub counts a
skipped required job as a pass.

## Emergency path

Chad can temporarily disable **main CI controls** in **Settings > Rules**.
GitHub logs the change. Re-enable the ruleset after the emergency.
There is no standing admin bypass.
