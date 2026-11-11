# PR Evidence workflow trust

GRE-886 assessment, 2026-10-18. Repository baseline
`852337c440ac38e86606aa776ac204dccde18df0`.

Superseded note (2026-11-10): this is a dated snapshot. The current ruleset
`25285698` requires `actionlint`, `zizmor`, `rust (linux)`, `Lint Evidence block`
and `review-gate`. `rust (windows)` was removed from required checks on 2026-11-08.
The Droid review path was removed in the same change.

The Evidence check is a PR-author-controlled reporting aid. It does not provide
independent validation of the PR or its claimed evidence. Closing that trust gap
requires a maintainer decision about enforcement outside the PR-owned workflow.

## Current behavior

[pr-evidence.yml](../../.github/workflows/pr-evidence.yml) runs on `pull_request`
opened, edited, synchronize, and reopened events. GitHub takes a `pull_request`
workflow from the PR merge ref. An author can replace its lint steps with a
successful command while preserving the job name `Lint Evidence block`.
Checking out the default branch protects the checker files that an unchanged
workflow executes. It does not protect the workflow definition.
PRs opened by `dependabot[bot]` skip the Evidence lint step; the job still reports success.

The current workflow checks out the repository default branch with persisted
credentials disabled. It uses pinned actions, Node 22.23.3, and read-only
`contents` and `pull-requests` permissions. In the unchanged workflow,
every PR selects `ubuntu-latest`. The PR can also edit this routing expression.
Runner access therefore depends on controls outside this YAML, including fork
approval policy.
This assessment does not attest to runner isolation or provision runners.

The body enters the checker through `PR_BODY`, not shell interpolation. The
checker accepts nonempty Evidence or a nonempty Exemption reason. It neither
executes the body nor verifies the truth of its claims. Its eight existing tests
pass locally. Those parser tests do not prove workflow provenance.

## Live enforcement snapshot

Read-only GitHub API queries on 2026-10-18 returned these facts:

- The repository is public, user-owned, and has default branch `main`.
- `GET repos/poitee/PrintPartner/branches/main/protection` returned 404 with
  `Branch not protected`. Rulesets still apply independently.
- `GET repos/poitee/PrintPartner/rules/branches/main` returned repository ruleset
  `25285698`. Its required checks are `actionlint`, `zizmor`, `rust (linux)`, and
  `rust (windows)`, each restricted to GitHub Actions integration `15368`.
- `Lint Evidence block` is absent from that required-check list. The PR rule
  requires zero approvals and sets `require_code_owner_review` to false.
- [CODEOWNERS](../../.github/CODEOWNERS) assigns workflows and both Evidence
  checker files to `@poitee`. That ownership does not currently require approval.

The ticket's description of an Evidence required check is therefore not the live
configuration observed here. Requiring its existing check name would make a
failure block merging, but would not authenticate the workflow that reports it.
Restricting a check to the GitHub Actions app also does not identify a particular
workflow file.

The live intake recheck found 58 open PRs and no overlap with the Evidence
workflow or checker files. PR #667 owns `docs/development/ci.md`, so this
assessment uses a separate document.

[The Evidence paragraph in ci.md](ci.md#ephemeral-pr-runners) still calls `Lint Evidence
block` a required check, contrary to this live snapshot. The maintainer's next
slice must reconcile that section after the overlapping documentation work lands.
The [gate evidence guide](../contributing/gate-evidence.md#ci-and-local-checks)
also has stale `pull_request_target`, protected-default-branch, and "never
executes the PR head" claims. Reconcile that paragraph in the same next slice.
The [CI controls record](ci-controls.md) also lists an obsolete PR Evidence
`dangerous-triggers` exemption. The current zizmor configuration has no such
exemption for this workflow.

## Standard trusted validation options

### Enforce review of workflow changes

Keep `pull_request` and require approval from the existing code owner through a
repository ruleset. This makes workflow edits subject to an enforced human gate.
It does not provide an independent machine result. A maintainer must choose
approval, stale-review, and bypass settings. Merely adding another CODEOWNERS
entry would not activate enforcement.

First establish an eligible approver distinct from the PR author. The
[CI controls record](ci-controls.md#phase-2-for-chad) notes that agent PRs use
`poitee`, the sole owner named for these paths, and GitHub forbids self-approval.
Separate agent authorship or another eligible code owner is a prerequisite to
enforcement without a bypass. Test that identity arrangement before enabling
required review. Identity and credential provisioning remain outside this slice.

### Use a base-controlled metadata workflow

`pull_request_target` can run a trusted workflow for PR metadata. A bounded
follow-up could run the default-branch checker on a GitHub-hosted runner with
read-only permissions, no secrets, no PR checkout, and no PR artifacts or
dependencies. The body must remain data passed through an environment variable
or file. This avoids privileged local execution.

Changing the event alone is insufficient. A maintainer must approve the elevated
trust context and confirm Actions event policy, fork behavior, body-edit reruns,
and how the resulting check attaches to the PR. Confirm enforcement on a
disposable PR before requiring the result. Never run PR code in this context.

### Require a centrally controlled workflow or external check

Organization or enterprise rulesets can require a workflow selected from a
source repository. That separates workflow provenance from the submitted PR.
This user-owned repository cannot adopt that organization-level option through
a YAML-only change. Ownership, plan availability, and policy need confirmation.

An independent GitHub App can also validate the body and report a required check
whose expected source is that app. It needs a separately controlled deployment,
credentials, and event handling. That is infrastructure work beyond this slice.
Calling a reusable workflow from PR-controlled YAML alone does not secure the
caller, which the PR author can still change or omit.

## Decision and next slice

Keep runtime behavior unchanged in this slice. Correct the checkout comment so
maintainers do not mistake trusted checker files for a trusted workflow.

Next slice: the maintainer chooses whether Evidence remains advisory, requires
enforced code-owner review, or needs independent metadata validation. If
independent validation is chosen, assess a hosted base-controlled metadata
workflow first. Verify it with disposable PRs that change the workflow, leave
Evidence empty, supply an exemption, and edit the body without a new commit.
Record the reported SHA and required-check behavior before changing enforcement.

No workflow trigger, permissions, runner routing, repository policy, deployment,
or production data changes are part of this assessment.

## Sources

- [GitHub event semantics](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows).
- [Secure use of pull_request_target](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target).
- [Required checks and code-owner review rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets).
- [Organization ruleset workflow behavior](https://docs.github.com/en/enterprise-cloud%40latest/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/troubleshooting-rules).
- [GRE-886](https://linear.app/greetz/issue/GRE-886/review-trust-of-pr-owned-evidence-workflow).
