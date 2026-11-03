# Keep current crates and deepen modules

Status: accepted on 2026-11-02 by Chad. Refines the ownership direction of
[ADR 0007](0007-rust-crate-layout.md), not its historical dependency evidence.

Amended on 2026-11-03 following the owner's delegated research choice.
Keep the operation crates and `pp-services` name, not a permanent crate-count
target. Improve typed operation-family interfaces and private implementation
modules. Permit a narrow `pp-os` for shared filesystem mechanics during early
Windows work under [ADR 0015](0015-shared-filesystem-portability.md), replacing
the earlier decision to delay it.

A broad family split or engine/model/connect/app consolidation would relocate
tested code before product delivery without making SQLite and filesystem
publication one atomic resource. Existing storage publication already owns its
transaction; immutable Source bytes still need staged publication and recovery.
The trade-off is keeping a large `pp-storage` and its navigation and merge costs
while fixing ownership problems incrementally. This is a design decision, not
an implemented reorganization or a measured performance improvement.

Storage retains SQL and transaction ownership. Rework `Envelope` family by family
under GRE-339; remove `pp-api`'s `rusqlite` dependency as a separate verified slice.
Do not replace those jobs with crate renames or unrestricted public SQL closures.
See the [architecture](../architecture.md) for allowed edges and family owners.
Deleting `pp-compat` at approved Node retirement is lifecycle cleanup, not a
prerequisite reorganization.
