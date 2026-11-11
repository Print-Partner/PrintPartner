# Today UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Ranked task feed | `GET /api/v1/tasks` | Exists (GRE-1123), a view over the workflow projection |

The task feed must be a read view over the ported Build Workflow projection (GRE-415), not a second ranking engine. Today, Build Overview and MCP must not be able to disagree about the next action.

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 4 | Today task feed, single-bed confirmation and undo | The evening loop works |
