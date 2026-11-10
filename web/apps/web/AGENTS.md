# Old web UI

Frozen to correctness and data-safety fixes. Node owns its domain behaviour; the old UI owns presentation until cutover.

Read [the authority page](../../../docs/adr/0008-replace-the-old-screens.md) before changing behaviour.
Follow [agent workflow](../../../docs/development/agent-workflow.md) for checks, evidence and handoffs.

Run from the repository root:

```sh
npm --prefix web run lint
npm --prefix web run typecheck
just ci --fast
```

- Do not build new screens here; use `web/apps/ui`.
- Do not port these screens for route or visual parity.
- Do not bypass autosave or restart recovery.
