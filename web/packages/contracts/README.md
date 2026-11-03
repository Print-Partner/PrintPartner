# @print-partner/contracts

Hand-maintained TypeScript types and Zod schemas for the HTTP API shared by the React UI and the Node server.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns the request, response and settings shapes that `web/apps/web` and `web/apps/server` both import, plus small pure helpers over them.
- `@print-partner/domain` imports it for shared types.
- It is the hand-maintained contracts mirror that [Architecture](../../../docs/architecture.md#http-and-generated-typescript-contracts) plans to retire. Types for Rust routes are generated into `@print-partner/rust-client`, which the replacement UI imports instead. Nothing in `web/apps` imports `rust-client` yet.

## Not responsible for

- Generated Rust route types: `@print-partner/rust-client` ([README](../rust-client/README.md)).
- Domain logic such as merging, packing or STL parsing: `@print-partner/domain`.
- Rust DTOs: `pp-api-types`. `src/rust-wire.generated.ts` is written by `npm --prefix web run contracts:reverse:generate`; do not edit it.

## Interface

One entry point, `.` (`src/index.ts`). It re-exports every module below and defines the core API types itself.

| Module | Main exports |
| --- | --- |
| `index.ts` | `ApiError`, `HealthResponse`, `ProfileSummary`, `SourceSummary`, `PartRow`, `JobSnapshot`, printer, integration, webhook and assistant types; `ExternalAccessMode` with `mcpAccessEnabled`; `formatTimestamp`, `validateDiscordWebhookUrl` |
| `accepted-plates.ts` | `AcceptedPlateWorkspace`, `parseAcceptedPlanBasis`, plate move/pin requests |
| `plan-drafts.ts` | `PlanDraftWorkspace`, `rustOwnedWireParsers`, apply/reconcile requests; parses with schemas from `rust-wire.generated.ts` |
| `build-workflow.ts` | `BuildWorkflowWorkspace`, `parseBuildWorkflowWorkspace` |
| `production-setup.ts` | `productionSetupSchema` and related route/grouping schemas |
| `source-naming.ts`, `source-category-tree.ts` | STL naming profile schemas; category path helpers |
| `filename-grouping.ts`, `plan-features.ts`, `tenant-features.ts` | Feature and grouping schemas |
| `hosted-planning.ts`, `reference-sharing.ts`, `accepted-media.ts` | Capability constants, `referenceShareSchema`, `THUMBNAIL_RENDERER_VERSION` |

`wire-schema.ts` (`boundaryRefinement`) is internal to `plan-drafts.ts` and the test fixtures.

## Effects and flags

- Runtime: none. The code is pure; it reads no env vars and does no I/O. Its only dependency is `zod`.
- Build: `build` runs `web/scripts/atomic-package-build.mjs`, which compiles into `dist/releases/release-<id>` and points the `dist/current` symlink at it. `main` and `types` resolve to `dist/current`. The `development` export condition loads `src/index.ts` directly.

## Tests

Vitest runs `src/**/*.test.ts` (10 files, next to the code they test). `test-fixtures/desktop/autosave-v1.json` and `test-fixtures/t9/` are fixtures for the parity and contract scripts in `web/scripts`; see `test-fixtures/desktop/README.md`.

```sh
npm --prefix web run test --workspace @print-partner/contracts
npm --prefix web run typecheck --workspace @print-partner/contracts
npm --prefix web run build --workspace @print-partner/contracts
```
