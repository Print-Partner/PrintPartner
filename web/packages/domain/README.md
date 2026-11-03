# @print-partner/domain

Framework-free TypeScript domain logic for Plans: STL scanning and naming, layer merging, print progress, plate packing and 3MF export.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns the pure rules the Node server and the React UI share: how STL paths become parts, how layers merge, how printed and assembled units are counted, and how accepted units are packed onto plates.
- `web/apps/server` and `web/apps/web` import it. It depends on `@print-partner/contracts` for types.
- The Rust core is replacing the Node server. New UI code that talks to Rust routes uses the generated `@print-partner/rust-client` for wire types; see [Architecture](../../../docs/architecture.md#http-and-generated-typescript-contracts).

## Not responsible for

- API request and response types: `@print-partner/contracts`, or `@print-partner/rust-client` for Rust routes.
- SQLite, HTTP routes, jobs and printer sends: `web/apps/server` today, the Rust crates after cutover. Functions such as `saveMergeResultPlan` and `toggleCheckoffUnit` return new arrays; callers persist them.
- Slicer discovery and handoff: `pp-services` in Rust, and the legacy `slicer-sidecar`.

## Interface

`.` (`src/index.ts`) re-exports every module. Three subpaths load one module without the Node-only file readers: `./import-rules`, `./parts-grouping` and `./sliced-object-matching`.

| Module | Main exports |
| --- | --- |
| `scanner.ts`, `parsers.ts`, `stl-naming.ts` | `scanRepo`, `listStlRelativePaths`, `parseStlPath`, `previewParse`, `getDefaultNamingProfile`, `mergeNamingProfiles` |
| `import-rules.ts`, `stl-tree.ts`, `path-tree.ts`, `source-stl-search.ts` | `parseImportRulesJson`, `pathMatchesRules`, `buildStlTreePayload`, `buildPathTree`, `searchSourceStls` |
| `merge.ts` | `mergeLayers`, `saveMergeResultPlan`, `MergeWouldWipeProfileError` |
| `print-progress.ts`, `checkoff-missing.ts`, `print-checklist.ts` | `ensureProgressRows`, `toggleCheckoffUnit`, `getPrintedCounts`, `countUnprintedUnits`, `progressSummary` |
| `build-workflow.ts` | `resolveBuildWorkflow` |
| `accepted-plate-packer.ts`, `direct-export-layout.ts` | `packAcceptedUnits`, `arrangeAcceptedUnits`, `layOutDirectExportUnits` |
| `accepted-plate-3mf.ts` | `encodeAcceptedPlate3mf` (zips with `fflate`) |
| `stl-bounds.ts`, `stl-mesh.ts` | `readStlBounds`, `parseStlMesh`, `loadStlMesh`, `placeMeshOnBed` |
| `sliced-object-matching.ts`, `production-unit-identity.ts` | `createSlicedObjectMatcher`, `matchSlicedObjectName`, `currentPackageDispatchCoverage` |
| `source-categories.ts`, `source-category-edits.ts` | `loadSourceCategories`, `resolveSourceCategory`, `moveSourceCategoryPath` |
| Small helpers | `safePlanSlug`, `folderKeyFromRelativePath`, `repoNameFromSourceLayer`, `resolveMeshColor`, `PAPER` |

## Effects and flags

- Reads files synchronously with `node:fs`: `scanRepo` and `listStlRelativePaths` walk a Source directory; `readStlBounds` and `loadStlMesh` read one STL. Nothing writes files, opens SQLite, uses the network or reads env vars.
- Build: `build` runs `web/scripts/atomic-package-build.mjs`, which compiles into `dist/releases/release-<id>` and points the `dist/current` symlink at it. Exports resolve to `dist/current`; the `development` condition loads `src/` directly.

## Tests

Vitest runs `src/**/*.test.ts` (19 files, next to the code they test). There is no fixtures directory: the scanner, STL tree and STL geometry tests write throwaway files under the OS temp directory, and `paper-palette.test.ts` reads the web app's `index.css`.

```sh
npm --prefix web run test --workspace @print-partner/domain
npm --prefix web run typecheck --workspace @print-partner/domain
npm --prefix web run build --workspace @print-partner/domain
```
