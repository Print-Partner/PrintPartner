# Architecture decision records

Each record states one accepted decision and why. A later record can refine,
amend or supersede an earlier one; the earlier file stays in place and this
table names the replacement. [Architecture](../architecture.md) applies the
current decisions to the target design.

| ADR | Decision | Status | Replaced or changed by |
| --- | --- | --- | --- |
| [0001](0001-model-build-workflow-as-preparation-and-making.md) | Model the Build workflow as preparation and making | No status line; in force | None |
| [0002](0002-keep-sources-and-plan-state-contextual.md) | Keep Sources and Plan state contextual | No status line; in force | None |
| 0003 | Number not used | No record in the repository history | Not applicable |
| [0004](0004-share-references-without-redistributing-models.md) | Share references without redistributing models | No status line; in force | None |
| [0005](0005-desktop-product-identity.md) | Keep one desktop product identity | Accepted for M0, 2026-10-12 | None |
| [0006](0006-autosave-text-contract.md) | Autosave text contains Unicode scalar values | Accepted modernization for the desktop contract boundary | None |
| [0007](0007-rust-crate-layout.md) | Record the Rust crate layout and dependency edges | Accepted record of the layout on 2026-10-13 | Ownership direction refined by [0009](0009-keep-crates-and-deepen-modules.md); its dependency evidence stays historical |
| [0008](0008-replace-the-old-screens.md) | Replace the old screens instead of porting them | Accepted 2026-11-02 | None. Supersedes parity-first screen delivery and the UI v11 sequence. Cutover is recorded separately in [0010](0010-whole-runtime-cutover.md) |
| [0009](0009-keep-crates-and-deepen-modules.md) | Keep current crates and deepen modules | Accepted 2026-11-02, amended 2026-11-03 | Amended by [0015](0015-shared-filesystem-portability.md), which permits a narrow `pp-os` |
| [0010](0010-whole-runtime-cutover.md) | Cut over the whole runtime with explicit rollback limits | Accepted 2026-11-02; production execution needs separate owner approval | None |
| 0011 to 0014 | Reserved by a separate design proposal | Not adopted | Not applicable |
| [0015](0015-shared-filesystem-portability.md) | Share narrow filesystem portability mechanics | Accepted design 2026-11-03; not implemented | None. Amends [0009](0009-keep-crates-and-deepen-modules.md) without changing [0010](0010-whole-runtime-cutover.md) |

"No status line; in force" means the record predates status lines and no later
record supersedes it.

## Decisions recorded outside this folder

- [UI and UX target](../product/ui-ux-target.md): accepted 2026-11-02. It is
  the replacement UI contract that ADR 0008 builds against.
- [Desktop rebuild](../desktop-rebuild.md): its parity-first sequencing and
  default-on proposal are superseded by ADRs 0008 to 0010. It remains the
  reference for the `desktop_rebuild` flag and its slices.

## Adding a record

1. Take the next unused number. 0011 to 0014 are reserved, so the next is 0016.
2. Start the file with the title, then a `Status:` line with the date and who
   accepted it.
3. Link any record it refines, amends or supersedes, and add a row to the table
   above. Update that record's row in the same PR.
