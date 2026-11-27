# Rust parity evidence

[GRE-1958](https://linear.app/greetz/issue/GRE-1958) compared the checked-in
surfaces at [`c9a3df0ab0a37f6edd8698421987c250fe653c8a`](https://github.com/Print-Partner/PrintPartner/tree/c9a3df0ab0a37f6edd8698421987c250fe653c8a).
This is a source snapshot, not a work queue or a cutover sign-off. Node stays
in production. For order, see [Pick a ticket](../development/contributing.md#pick-a-ticket).

- **Present:** the Rust route, handler, screen or reader exists in the stated mode.
- **Missing:** the compared Rust surface lacks the named behavior, even if it advertises a name.
- **Superseded:** an owner decision replaced the old parity requirement.
- **Unverified:** the cited sources do not prove the broader parity or acceptance claim.

## HTTP

Compare the [Node compatibility catalog](../../rust/crates/pp-gateway/operations.json)
with the [Rust client route catalog](../../rust/crates/pp-api/src/client_contract.rs)
and the [Rust-only router composition](../../rust/crates/pp-core/src/rebuild_api.rs).
The Rust client catalog covers calls made by the replacement UI, not all Rust
routes. Comparing method and path templates (normalizing parameter names,
leaving versions intact) gives 19 exact matches for its 66 routes among the
781 compatibility routes. This is path overlap, not behavior parity.

| Status | Evidence | Decision or ticket |
| --- | --- | --- |
| **Present** | Rust-only mode registers Build, Plan, Checkoff, and Library operations. For example, both catalogs list `GET /api/v2/plans/{id}`; Rust also has its own backup/restore routes under `/api/v2/installation`. The [registration test](../../rust/crates/pp-core/src/rebuild_api.rs) checks the client catalog against the composed routers. | [Daily-loop contract](../product/daily-loop.md#steps), [GRE-1371](https://linear.app/greetz/issue/GRE-1371) and [GRE-1161](https://linear.app/greetz/issue/GRE-1161) for backup/restore |
| **Missing** | Node has a webhook management surface, including `GET /api/v1/webhooks`; the Rust-only router has no webhook management route. | [Owner re-scope](../architecture.md#current-state-and-transition) defers webhooks |
| **Superseded** | Moving all 781 compatibility routes one by one is not the delivery plan. The [operation inventory](operation-inventory.md) still reports zero migrated routes from its ownership inputs; that does not count the separate Rust-only router. | [Whole-runtime cutover](../adr/0010-whole-runtime-cutover.md) |
| **Unverified** | The catalog comparison does not resolve the remaining Node routes one by one, establish equivalent request/response behavior, or prove a cutover on owner data. A zero-proxy count covers only the recorded journey in the inventory. | [Whole-runtime cutover](../adr/0010-whole-runtime-cutover.md) |

## MCP

The Node [tool specs](../../web/apps/server/src/assistant/tool-specs.ts) and
[`tools/list` handler](../../web/apps/server/src/mcp/product-mcp.ts) expose
83 product tools (excluding six `ui_*` tools) plus three session tools.
The [Node golden](../../web/apps/server/fixtures/mcp/tools-inventory.json)
and [Rust catalog](../../rust/crates/pp-services/src/mcp_domain/catalog.json)
have the same 86 names and input schemas. Both catalogs include the two Farm
tools; both runtimes gate their listing on Farm access. `plan_build` is a
prompt, not a tool.

| Status | Evidence | Decision or ticket |
| --- | --- | --- |
| **Present** | Rust mounts authenticated MCP at `/mcp` and `/api/v1/mcp` with the rebuild gates on. The [HTTP dispatch](../../rust/crates/pp-api/src/mcp.rs), [session dispatch](../../rust/crates/pp-services/src/mcp_domain.rs) and [domain dispatch](../../rust/crates/pp-api/src/mcp/checkoff.rs) account for 23 tools with handlers: the three session tools, eight Build/Plan/Checkoff reads, ten proposals and two Farm reads. This is a handler count, not a guarantee of Node-equivalent results. | [Owner re-scope](../architecture.md#current-state-and-transition) limits Rust MCP to used tools; [GRE-1300](https://linear.app/greetz/issue/GRE-1300) covers Build input proposals |
| **Missing** | Of the 63 other listed tools, 62 reach the `has no Rust port yet` fallback (for example, `search_source_files` and `get_plan_checkoff`). The remaining tool, `propose_add_custom_filament`, can propose, but the product MCP session gives its confirm handler no data directory. | [Owner re-scope](../architecture.md#current-state-and-transition); listing alone is not a decision to port every tool |
| **Unverified** | No new tool calls or output comparisons were run. The name/schema comparison is against the checked-in catalogs, not a live discovery or compatibility test. | [GRE-1428](https://linear.app/greetz/issue/GRE-1428) tracks the owner’s daily loop, including Build creation with MCP or the UI |

Since that snapshot, [manual Accepted Plan Checkoff](../mcp.md#accepted-plan-checkoff)
has a Rust read and two proposal handlers. Sliced 3MF Checkoff import is tracked
[separately](https://linear.app/greetz/issue/GRE-1985).

## UI

Compare the Node [sign-in routes](../../web/apps/web/src/App.tsx) and
[authenticated routes](../../web/apps/web/src/AuthenticatedApp.tsx) with the
[replacement router](../../web/apps/ui/src/routes.tsx).

| Status | Evidence | Decision or ticket |
| --- | --- | --- |
| **Present** | Rust-backed UI registers sign-in and setup; Today, Builds, Library, Farm and Settings; and the Build workspace with Sources, Plan, Production, Checkoff and History. Node has separate Sources, Builds, Plan, Progress, Production and Printers pages. The new destinations group behavior differently. | [Accepted UI target](../product/ui-ux-target.md) |
| **Superseded** | The old screen layout, paths and redirects are not requirements. Node's global Progress and Production pages give way to Build Checkoff, Today and Farm; the Community Board is dropped. Required exports and data are still in scope. | [Replace the old screens](../adr/0008-replace-the-old-screens.md) and [dropped screens](../product/ui-ux-target.md#dropped-from-the-old-ui) |
| **Missing** | Node exposes `/forgot-password` and `/reset-password` pages. The replacement router has neither screen, even though Rust has the [HTTP endpoints](../../rust/crates/pp-api/src/auth/http.rs) and a [reset request parity fixture](auth-reset.md). This is a screen gap, not a finding that old screens must be ported. | [Replace the old screens](../adr/0008-replace-the-old-screens.md) |
| **Unverified** | A registered screen is not proof that the owner can finish the workflow. The [daily-loop suite](../../web/test/daily-loop/daily-loop.spec.ts) has 12 cases; this comparison didn't run it, measure speed, or verify the owner's walk. | [GRE-1428](https://linear.app/greetz/issue/GRE-1428) and the [owner acceptance rule](../product/daily-loop.md#score) |

## Stored formats

Compare the [Node SQLite version](../../web/apps/server/src/db/schema.ts),
the [Rust schema versions](../../rust/crates/pp-storage/src/schema.rs), and
the [Node import inventory](../../rust/crates/pp-storage/src/import/inventory.rs).
For the historical `live_formats` fields, see the [report and its limits](../development/operation-inventory.md).

| Status | Evidence | Decision or ticket |
| --- | --- | --- |
| **Present** | Node writes SQLite schema 34. Rust-only writes schema 51 (the legacy feature-off constant is 35). The [import](../node-import.md) reads supported Node folders or full backups, converts to a separate Rust-owned copy and reports completeness. | [GRE-412](https://linear.app/greetz/issue/GRE-412) and [Rust owns its data formats](../adr/0016-rust-owns-its-data-formats.md) |
| **Superseded** | Matching Node's on-disk versions or sharing a live directory is not the goal. Preserve Plan, Plate and Required-unit IDs, history, drafts, Checkoff, printers and Source revisions through import; restore the untouched Node copy for rollback. | [Rust owns its data formats](../adr/0016-rust-owns-its-data-formats.md) |
| **Unverified** | The report's 10 live-format rows reflect compatibility manifest values, including SQLite 34/35. Neither an “in step” label nor a missing Rust writer proves whether the one-time import preserves the data. This snapshot does not verify completeness on a real installation. | [GRE-412](https://linear.app/greetz/issue/GRE-412) and the [import acceptance step](../product/daily-loop.md#steps) |
