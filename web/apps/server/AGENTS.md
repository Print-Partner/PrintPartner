# Node server

Retiring. Node owns live compatibility operations until whole-runtime cutover. Maintain correctness and data safety here.

Read [the authority page](../../../docs/architecture.md) before changing behaviour.
Follow [agent workflow](../../../docs/development/agent-workflow.md) for checks, evidence and handoffs.

Run from the repository root:

```sh
npm --prefix web run lint
npm --prefix web run typecheck
just ci --fast
```

- Do not add replacement product features here.
- Do not dual-write with Rust or share a live Rust-owned data directory.
- Do not hand-edit generated schema fixtures.
