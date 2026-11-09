# D19 publication transaction and job conflicts, GRE-612 PR 2

`publish_in_transaction` mirrors Node `AppRepository.applyPlanChanges` and takes
an existing `&Transaction`. D13 save can call it inside its own transaction with
an authenticated tenant and actor, profile and draft IDs, admitted and execution
requests, an idempotency key, and a Required-unit token allocator. It returns a
publication `Outcome`. The caller commits success or rolls back errors.

The transaction verifies the accepted projection, selected reconciliation and
live selection basis before publishing immutable revisions, Parts, input sets,
Required-unit mappings, progress, accepted pointers, and a replayable receipt.
`plan_publication/schema.sql` supplies the admission table and immutability
triggers for PR 3's schema installation. PR 2 adds no startup migration or client.
The default build remains unchanged.

`jobs::publication_conflicts` and `printer_upload_conflicts` move here from D16
because publication must check execution jobs inside the same transaction.
Exports use their profile ID. Uploads and deferred starts also resolve Checkoff
coordinate ownership. All terminal jobs, including uploaded-only uploads, are
skipped. An active deferred start bound to an uploaded-only parent still blocks
publication and validates the parent upload binding. The supplied-source import
arm waits for GRE-610's payload variant.

The generator records 26 real SQLite transactions covering receipt replay,
already-applied drafts, idempotency conflicts, changed drafts and bases, pinned
Source inputs, archived Builds, a corrupt accepted pointer, reconciliation
reasons, active production without remapping, and Checkoff links, resolved units,
and ownerless queues, with and without their optional unit arrays. Rust restores
each database on Node's schema and compares literal outcomes and publication
rows, normalizing only publication time and the field casing of receipts and
production counts.

Preparation sorts assignments by Part and unit index before validating them, as
Node does. Preparation and the transaction's Build check measure the 64 KiB
operational text limit as Node does: the UTF-8 length of Node's named Part and
Build fields. A lone UTF-16 surrogate counts as the 3 bytes of U+FFFD. Rows the
transaction inserts are still checked by the sum of all their text columns. The
Build check runs after the archived check, so an archived Build returns
`build_archived`. A set accepted revision with a nonpositive Plan version returns
`accepted_baseline_required`.

Node `applyPlanChanges` does not check durable jobs, so its running-export case
publishes. Rust returns the exact job conflict list and leaves publication rows
unchanged. Both runtimes publish with a terminal uploaded-only upload.

Rust keeps Node's Checkoff remap refusal reasons for deleted Parts, missing match
keys, and indexes at or past the new quantity. After those checks, both runtimes
write coordinates by Required-unit token and refuse if the referenced token was
dropped or excluded (GRE-677).

This rule applies whenever a kept token's index changes. `Selection::PriorCompletion`
renumbers units with no quantity change. In the index-1 fixture both runtimes move
the coordinate to the kept token's new index. At index 0 both refuse the dropped
token and write nothing. Index 2 and a cross-Part filename rename refuse with
Node's quantity and match-key reasons. Fixtures record Node's literal outcomes,
and Rust compares them exactly.
The prior-completion fixtures change the STL, keep the quantity at 3 and accept
prior completion with units 1 and 2 completed. Both runtimes refuse a link at
index 0, whose token is dropped, and move links at indexes 1 and 2 to 0 and 1.
