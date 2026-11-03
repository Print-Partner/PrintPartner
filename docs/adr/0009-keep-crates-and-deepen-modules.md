# Keep current crates and deepen modules

Status: accepted on 2026-11-02 by Chad. Refines the ownership direction of
[ADR 0007](0007-rust-crate-layout.md), not its historical dependency evidence.

Keep the current crate set and `pp-services` name. Improve operation-family
interfaces and private implementation modules inside them; delay any `pp-os`
crate. A broad family split or six-crate consolidation would move tested code
before product delivery, disrupt current work, and spread transaction coordination
across callers. The trade-off is keeping a large `pp-storage` and its navigation
and merge costs while fixing the actual ownership problems incrementally.

Storage retains SQL and transaction ownership. Rework `Envelope` family by family
under GRE-339; remove `pp-api`'s `rusqlite` dependency as a separate verified slice.
Do not replace those jobs with crate renames or unrestricted public SQL closures.
See the [architecture](../architecture.md) for allowed edges and family owners.
Deleting `pp-compat` at approved Node retirement is lifecycle cleanup, not a
prerequisite reorganization.
