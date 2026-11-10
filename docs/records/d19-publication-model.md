# D19 publication model and production links, GRE-612 PR 1

`pp-storage::plan_publication` is internal and requires `desktop_rebuild`.
The feature remains off by default. This slice ports publication assignments,
preparation, revision digests, accepted projection validation, production setting
validation, and Required-unit token remapping from the source-sync reference.
The optional `pp-contracts` dependency supplies `UnmappableLink`; the boundary
check rejects this dependency if it becomes required or reachable by default.

PR 2 adds the transaction entry described below. PR 3 adds `PublicationClient`,
authenticated admission, schema installation, and writer `Envelope` dispatch.

`node fixtures/parity/plan-publication/generate.mjs` regenerates literal Node
results in `fixtures/parity/plan-publication/node.json`; `--check` detects drift.
It requires Node 22.18 or newer and `npm --prefix web ci --ignore-scripts`.
The module hooks transform TypeScript and select development package exports,
so the real database probe needs no server or package build.
The fixture header records the generating Node version. The generator imports
`preparePlanPublication`, `digestPlanRevisionParts`, `assessCheckoffRemap`, and
`requiredUnitObjectName`. It extracts and executes the repository's inline
accepted revision check, `projectionPlanningFieldsMatch`, and private
`applyCheckoffRemap` and `strictProductionState` methods with row and setting
adapters. It does not copy their implementations. The transaction cases below also import
the real repository and SQLite database layer.

Rust tests compare literal digests, progress, assignment coordinates, object
names, remap mappings, and persisted settings. Shared projection cases compare
acceptance and rejection. Production validation accepts Checkoff coordinates
owned by another profile, matching Node. Remapping treats an ownerless queue item
as related when any part belongs to the profile or no referenced parts still exist,
even when its state is `done`. Node's exact error messages are retained in the
fixture; tests separately assert the Rust reference's more specific diagnostics.
Node's inline projection check has no progress-validation counterpart. Both
implementations remap Checkoff and send-queue coordinates by Required-unit token
(GRE-677): a kept unit follows its token to its new index, and a dropped or
excluded unit refuses the remap. The Node transaction cases
`mapped-remap-index-change` and `mapped-remap-index-0` record this. Direct Rust
tests cover progress corruption, excluded or missing tokens, and changed indexes.


