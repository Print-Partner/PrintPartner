# Old web UI

Frozen to correctness and data-safety fixes. Node owns its domain behaviour; the old UI owns presentation until cutover.

When deciding whether a screen change belongs here, read [Replace the old screens](../../../docs/adr/0008-replace-the-old-screens.md).

Run from the repository root:

```sh
npm --prefix web run lint
npm --prefix web run typecheck
just ci --fast
```

- Do not build new screens here; use `web/apps/ui`.
- Do not port these screens for route or visual parity.
- Do not bypass autosave or restart recovery.
