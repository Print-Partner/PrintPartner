# D9 build graph receipt

GRE-602 adds the read-only first half of `pp_storage::build_graph` behind
`desktop_rebuild`. Authenticated `List`, `Read`, and `ListLayers` commands run
through the existing writer queue and return typed summaries and ordered layers.
`List` rejects more than 64 builds, shares one graph budget across summaries,
and checks cancellation between builds.
`ListLayers` caps results at 1,024 layers and checks cancellation during iteration.
Summary facts reuse the accepted graph reader and freshness calculation.
Corrupt freshness inputs return explicit unavailable freshness and integrity
progress for that build while retaining healthy builds in lists.
Summaries validate PPJS metadata losslessly. Existing scalar snapshot projections
report `manifest_text` integrity failures for non-scalar metadata; lossless public
snapshot serialization remains deferred.

GRE-602 also adds `Create`, `Touch`, `SetBase`, `AttachAddon`,
`ReplaceAttachment`, `Detach`, and `Delete` through the same authenticated queue.
Mutations use deadline-aware writer admission and immediate transactions. Typed outcomes distinguish missing builds,
sources, and layers from duplicate names and attachments. Names reuse the gated
`pp-domain::build_identity::BuildName`, also reexported by `pp-contracts`
through `pp-api-types`; IDs use `pp_domain::PositiveId`.
Create and Touch return boxed summaries. Attachment changes mark
`config_modified_at`, and returned layers retain the 1,024-layer cap. Delete
removes tenant-scoped production, filament, and plan feature settings and defers foreign keys
so accepted graph references can cascade together. Errors, cancellation, and
budget exhaustion roll back the transaction.

With `desktop_rebuild`, graph reads report `Failure::BudgetExhausted` separately
from stored integrity failures. `BudgetResource` identifies the shared
200,000-row or 64 MiB batch budget, or the 1,000,000 Plate overlap comparison cap.
The 64 KiB stored-row text cap remains an integrity check. Accepted reads also
propagate resource exhaustion instead of embedding it in context errors. The
feature-off reader retains its existing error classification.

D9 is complete with the [final coverage and integration audit](desktop-rebuild-d9-audit.md).
Required-unit planning integration belongs to GRE-606 D11. Manifest option observation is
GRE-604 D10b, described below.
No runtime caller is added. Future user-visible callers must also check
`PP_DESKTOP_REBUILD` for exactly `1` or `true`.

