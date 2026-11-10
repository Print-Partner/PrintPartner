# Replacement UI

Active. Rust owns domain behaviour; this UI owns presentation and calls `web/packages/rust-client`.

Read [the authority page](../../../docs/adr/0008-replace-the-old-screens.md) before changing behaviour.
Follow [agent workflow](../../../docs/development/agent-workflow.md) for checks, evidence and handoffs.

Run from the repository root:

```sh
npm --prefix web run lint --workspace @print-partner/ui
npm --prefix web run typecheck --workspace @print-partner/ui
npm --prefix web run test --workspace @print-partner/ui
```

- Do not call Node routes or import old UI and domain packages.
- Do not hand-edit the generated Rust client; run its generator.
- Do not use Tauri invoke for domain operations.
