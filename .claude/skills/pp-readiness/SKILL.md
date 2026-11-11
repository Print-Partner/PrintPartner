---
name: pp-readiness
description: Work on Print Partner agent readiness. Use when picking up a readiness sub-issue under GRE-1067, re-running the readiness audit, or deciding whether a readiness criterion applies.
---

# Print Partner readiness

Readiness work is tracked in Linear under [GRE-1067](https://linear.app/greetz/issue/GRE-1067), one sub-issue per group of criteria. The baseline is readiness report `f29b55f1-7038-4694-98b0-153675cdcb15`: 59.3%, Level 3, on main `6448fb2f`.

## Pick up a sub-issue

1. Read the sub-issue, or its exported [task brief](../../../docs/development/agent-workflow.md#start-from-a-task-brief), and the root [agent guide](../../../AGENTS.md). Start from current `main` in a new worktree.
2. Split the work into PRs of about 600 lines or fewer. Open the next PR only after the previous one has its required review (`review-gate`) and green CI.
3. Use existing tools first: ESLint, Knip, Clippy, `scripts/local-ci.py`, and the verification helpers. Add a new tool only when nothing in the repo covers the criterion.
4. Prove each check with a negative control: show it fails on a bad input before you rely on it passing.

## Honest scoring

- Every audit counts all five current apps: `web/apps/server`, `web/apps/web`, `rust/crates/pp-server`, `rust/crates/pp-desktop`, and `slicer-sidecar`. Don't drop one to raise the score.
- Don't add features only to satisfy a criterion: no analytics, dummy schemas, empty health endpoints, or default-on telemetry.
- When a criterion conflicts with a product rule, record it as not applicable with the reason in the sub-issue, and get Chad's sign-off.
- Compiling, test collection, and skipped steps are not passing tests. Report executed counts.

## Re-run the audit

Run a full readiness audit only on a clean checkout of a named commit. Store the report, then update GRE-1067 with the new score and the criteria that changed.
