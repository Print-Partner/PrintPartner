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
- The CI job selector `scripts/ci-path-filter.py` and its test `scripts/test_ci_path_filter.py`.
- The duplication checker, its tests, and `scripts/duplication-baseline.json`.
- The PR evidence checker and its test, and the `scripts/linear/` directory.
  Linear PR sync runs a script from that directory with the Linear secret. Python imports files next to that script before the standard library.
- `scripts/release.mjs`, `web/scripts/atomic-package-build.mjs`, `Dockerfile`, and `docker-entrypoint.sh`, which the release workflow runs directly or builds into the published image.
- `rust/ast-grep-baseline.json`, `rust/clippy-limits-baseline.json`,
  `scripts/workaround-baseline.json`, and Rust snapshot directories under `rust/crates/`.

Who merges is decided by [Who merges](definition-of-done.md#who-merges). Chad merges the CI controls and the release path: the `.github/` paths, the CI selectors and the checks on the workflows themselves (the path filter, the Rust runner, the PR evidence checker, and the action-pin, CI tool-pin, desktop-workflow-state and CI event-tree checks), `scripts/linear/`, the release and image files, `rust/deny.toml` and `rust/scripts/check-macos-bundle.py`, plus things CODEOWNERS does not list: the Rust-only image `Dockerfile.rust` (GRE-1458), the release signing key `cosign.pub` and the Git hooks under `.githooks/` (owner decision, 2026-11-11), the safety guard, its classifier and its hook adapters (`scripts/agent-guard.py`, `scripts/merge_class.py` and `scripts/merge-classes.json` (owner decision, 2026-11-11), `.claude/`, and `.factory/` if a runner config is added there again), `docs/development/main-ci-controls.ruleset.json` and `scripts/test_merge_group_workflows.py`. Chad also merges the lint, boundary and size checkers and baselines (`check-ast-grep.py` with `rust/rules/` and the ast-grep baseline, `check-boundaries.py`, `check-clippy-limits.py` with its baseline, `check-domain-deps.py` and `check-react-routes.mjs` under `rust/scripts/`, the workaround and duplication baselines with their checkers, and the Rust snapshot directories) and the rules documents (`AGENTS.md`, `docs/development/definition-of-done.md` and this page; owner decision, 2026-11-11). A change that weakens a security boundary stays with Chad too: raising the `runtime-process-command` count or editing its rule, widening `ALLOWED`, or shrinking `DEV_ONLY` or `TEST_ONLY_FEATURES` in `check-boundaries.py`. CODEOWNERS requests Chad's review on those paths, and the merge does not wait for it (decided 2026-11-09). `rust/rules/` is not in CODEOWNERS today; adding it is a `.github/` change for Chad.

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

Web CI uses job selection rather than workflow path filters. Its `changes` job
publishes a `ui` output. The `ui` job runs when that output is not `false`.
There is no `web`, `web-build`, `sidecar` or Node `docker` job. The Rust image
is a separate workflow. Rust cross-OS runs `rust (linux)` on a pull request, a
push and a merge group, and builds Windows and macOS only on the nightly
schedule or a manual dispatch ([Merge queue](ci.md#merge-queue)).
[Web job selection](ci.md#web-job-selection) lists the `ui` paths and commands.
The required `ui` job fails if the selector fails. The independent, required
`release-audit` job runs without path selection on every non-draft
PR and on non-PR events. The workaround ratchet runs in `just ci`, not in a
pull-request job. A test loads the ratchet's declarations and requires the
filter's extension list to match. The audit job's pinned Node setup, version
assertion, read-only permissions and unconditional audit command are checked by
`test_merge_group_workflows.py`.

`checkNodeLanes` permits only the exact `needs: [changes]` and `ui` output
condition for the `ui` job. `job_runs()` understands that condition, including
the Linux event clause. Merge groups select `ui` without reading the diff. Chad approves these
CI-control changes and merges them.

## Cross-OS capacity

Pull requests do not run Windows or macOS. The nightly schedule and a manual
dispatch of Rust cross-OS build the workspace on `windows-latest` and `macos-26`.
`ci:cross-os` does not select a job. A push to `main` and a merge group run
`rust (linux)`, including nextest
([Merge queue](ci.md#merge-queue)). Hosted nextest is four hash partitions; the
required context is still the aggregate. The required contexts stay
`actionlint`, `zizmor` and `rust (linux)`. Chad does not edit ruleset 25285698
for GRE-1664. See
[Cross-OS PR label](ci.md#cross-os-pr-label).

The stale merge-group canceller runs every ten minutes or by manual dispatch
on main. Its minimum permissions are `actions: write` for cancellation,
`contents: read` for refs and `pull-requests: read` for PR merge state, using only
`GITHUB_TOKEN`. It checks out no code and downloads only main's public script.
Startup must read the expected main ref. It selects only the two cross-OS
workflows' queue runs whose ref is gone and PR is open or closed without merging,
and whose run is more than ten minutes old. A merged PR is always preserved;
ancestry cannot identify squash merges. Unparseable refs, PR read errors and
unexpected states abort preflight without cancellation. Each ref is rechecked
before cancellation; reappeared refs are skipped. A final probe failure stops
further writes, and cancel 409 skips a finished run and continues. Concurrency
keys include queue ref and head SHA to isolate reused names; reruns of the same
pair can cancel older runs of that pair. Build-job permissions and secrets stay
unchanged. See [Superseded runs](ci.md#superseded-runs).

The Mac runner stays unused. A follow-up must wait for Chad to configure a
restricted runner group: selected workflows pinned to main, with fork access
disabled. Repository YAML alone cannot enforce that trust boundary. Runner VM
registration and making `duplication` required also remain owner work.

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
The [committed ruleset JSON](main-ci-controls.ruleset.json) records the target configuration.
It omits node IDs, timestamps, API links, and the current user's bypass capability.

The ruleset has these protections:

- Block branch deletion and force pushes.
- Require a pull request and allow only squash merges.
- Require six checks from GitHub Actions (integration `15368`). They are
  `actionlint`, `zizmor`, `rust (linux)`, `ui`, `release-audit` and `rust image`.
- Require one independent GitHub approval, dismiss stale approvals, and require
  every review thread to be resolved. CODEOWNERS routes review requests to Chad
  for protected paths. GitHub cannot count an author's own approval, so another
  maintainer with repository write access must approve Chad's PRs.
- Validate the combined branch in the squash merge queue and keep the bypass list empty.

Devin Review posts bug and security findings on ready PRs. Its check is advisory.
The ruleset relies on GitHub approvals and CI, and Chad merges.

### Apply the GitHub review cutover

1. Open [the ruleset editor](https://github.com/Print-Partner/PrintPartner/settings/rules/25285698)
   as a repository administrator. Compare the live rules with the snapshot before saving.
2. In one save, set required approvals to one, enable dismissal of stale
   approvals, and replace `pp/score` with `ui`, `release-audit` and `rust image`.
   Select GitHub Actions as the expected source for all six checks. Keep thread
   resolution, the merge queue, branch protections and the empty bypass list.
3. Verify the new requirements on an open PR. Approve the current diff after
   review, confirm all required checks are green, and check the first merge queue
   entry reports all six checks before it lands. Apply the settings before
   merging the PR that removes the old status workflow.

An administrator can apply the reviewed JSON through the API after comparing it
with the live ruleset:

```sh
jq 'del(.id, .source, .source_type)' docs/development/main-ci-controls.ruleset.json \
  | gh api -X PUT repos/Print-Partner/PrintPartner/rulesets/25285698 --input -
```

To compare the snapshot with the live ruleset, run this read-only check from the repository root:

```sh
gh api repos/Print-Partner/PrintPartner/rulesets/25285698 \
  | jq -S 'del(.node_id, .created_at, .updated_at, ._links, .current_user_can_bypass) | .bypass_actors //= []' \
  | diff -u <(jq -S . docs/development/main-ci-controls.ruleset.json) -
```

No diff means the normalized live response matches the snapshot. A diff needs review before updating the snapshot.

<a id="turn-on-the-merge-queue"></a>

## Turn on the merge queue

The repository moved to the Print-Partner organization on 2026-11-07, which makes GitHub's merge queue available on this public repository at no cost. All six required checks report on `merge_group` events. [Merge queue](ci.md#merge-queue) describes what each one does there. The queue is on: the rule below was added to ruleset `25285698` on 2026-11-09 (ruleset version `56963310`), and `main-ci-controls.ruleset.json` records it. Agents never change the ruleset.

The rule, as added (it changed nothing else in the ruleset):

| Setting | Value | Why |
| --- | --- | --- |
| `merge_method` | `SQUASH` | Matches `allowed_merge_methods: ["squash"]`. |
| `grouping_strategy` | `ALLGREEN` | Only passing combinations land. A failing group ejects its PR; read the log before you queue it again. |
| `max_entries_to_build` | `2` | Each entry builds the full Linux Rust path on hosted runners. Hosted concurrency is the limit. |
| `min_entries_to_merge`, `max_entries_to_merge`, `min_entries_to_merge_wait_minutes` | `1`, `5`, `1` | A lone PR is not held. Merge limits only batch the final merge and save no CI. |
| `check_response_timeout_minutes` | `150` | Covers a full Linux and Windows run with hosted queueing. |

The queue checks the combined commit, so `strict_required_status_checks_policy` stays false.

To test the queue, queue two small PRs back to back and check that both land.

With the queue on, `gh pr merge <n> --squash --match-head-commit <sha>` adds the PR to the queue instead of merging it, so queueing a PR is merging it. Queue only a PR whose required checks already passed on its head. Removing a PR from the queue needs the web UI.


## Phase 2 for Chad

Phase 2 remains Chad's work and keeps GRE-635 open:

1. Create and install a GitHub App for agents with contents, pull requests, and workflows write access, plus checks read access.
   Store its key in the VMs' secret store and switch agent PR authorship to the App.
   Chad owns credentials and their handling.
2. Add a second write-access maintainer to the relevant CODEOWNERS paths before evaluating required code-owner review.
   Verify that a PR from either maintainer can receive the required approval.
3. Test enforcement with the separate agent identity.
   A workflow PR must not merge without Chad's approval.
   A PR touching only `rust/` must merge on green checks if the review policy permits it.
   A docs-only PR must receive both named lint checks.

## Edits to a pull request

GRE-1647 deleted `review-gate`, the evidence lint and the Claude review, which used to start on body, title and base edits. An edit can still start `linear-pr-sync.yml` for a same-repository pull request into the default branch. That workflow does not cancel a superseded run.

## Emergency path

Chad can temporarily disable **main CI controls** in **Settings > Rules**.
GitHub logs the change. Re-enable the ruleset after the emergency.
There is no standing admin bypass.
