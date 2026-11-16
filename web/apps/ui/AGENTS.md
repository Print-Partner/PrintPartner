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
- Keep the layers in `src` apart (`print-partner/ui-boundary`, see [README](README.md#layers-inside-src-gre-1537)): a screen folder never imports another screen, a shared layer (`core`, `frame`, `screen`, `status`, `theme`, `type`, `fonts`) never imports a screen, and only composition (the files directly in `src`) and test files import composition. Put a helper two screens need in a shared layer.
- Never add to the recorded cross-layer imports in `web/eslint-suppressions.json`; when you remove one, prune its count (`npx eslint . --prune-suppressions` from `web/`).
