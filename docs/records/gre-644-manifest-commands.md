# GRE-644 manifest commands, slice 1

`build_graph::ManifestOptionsCommand` adds `ReadKit`, `SaveKit`, and
`ReadBuilder` through `BuildCommand::Manifest`, behind `desktop_rebuild`.
`BuildOutcome::Manifest` returns typed outcomes. Use `into_success_body()` for
lossless JSON bytes, including UTF-16 selection IDs.

`WriterOwner::build_graph_with_observations` accepts the existing `DraftReads`
adapter and `PreparationLimits`. Capture authenticates and reads the graph in
a reader transaction, then releases that reader before adapter calls. Two
observations share one preparation budget and must have equal source stamps.
The queued command authenticates again and checks `basis_matches` inside its
immediate write transaction. Save validates selections in that transaction,
then writes the kit setting and configuration timestamp together. Cancellation
before commit rolls both back. Save refuses stale inputs without retrying;
builder reads retry once. Kit reads need no filesystem adapter.

This adapts the reference's command location to main's separate
`manifest_options` model. The builder outcome holds serialized bytes because
main keeps its builder model internal. Stored malformed kits read as empty,
missing Plans win over invalid save input, and omitted save fields reset to
empty defaults, as Node does. SQL read errors propagate instead of becoming
empty kits as in the reference.

Unlike the reference, selection validation runs inside the write transaction.
Unlike both Node and the reference, save rejects unknown group and variant IDs,
as required by this slice. D10b's validator continues to check cardinality;
the command adds unknown-ID checks without changing `observation.rs`.
Kit refusal texts and Plan id handling follow Node, as the GRE-451
`manifest-http` parity corpus checks: an id that names no Plan is 404.
The GRE-451 `manifest-checkouts-http` corpus replays builder reads and kit
saves over real Source checkouts through the production `SourceReads`, with
the unknown group and variant refusals declared as corrections.
Save observes attached checkouts even when selections are omitted, as the
reference does, so source failures and stale stamps still refuse that save.
Node only builds option groups when the update supplies selections.
Two observations share a budget instead of receiving separate budgets as in
the reference.

The public command tests cover Node response parity, save/read persistence,
stamp changes, database changes before writer admission, bounded builder retry,
unknown selections, cancellation, missing and foreign Plans, adapter failures,
and rollback when the freshness update fails. The Node fixture follows D10b's
JSON and generator pattern. Regenerate it from `web`:

```sh
npx tsx --conditions=development ../rust/crates/pp-storage/tests/fixtures/manifest-commands-node.mts apps/server > ../rust/crates/pp-storage/tests/fixtures/manifest-commands-node.json
```

Storage commands landed without HTTP. GRE-801 registers gated ReadKit on the
Build router (`docs/gre-801-manifest-http.md`). GRE-802 registers gated SaveKit
(`docs/gre-802-manifest-http.md`). GRE-803 registers gated ReadBuilder
(`docs/gre-803-manifest-http.md`).
