# Golden tests for behaviour kept from the old screens

[ADR 0008](../adr/0008-replace-the-old-screens.md) deletes the old screens. Behaviour worth keeping moves to Rust with golden tests, not screen parity. GRE-1116 added the suite. Each test runs Rust code and compares its output with a file that Rust wrote and that is committed next to the test. A change in the behaviour fails the test.

## What is pinned

| Behaviour | Test | Golden |
| --- | --- | --- |
| STL pack layout: `<role>/<folder>/<stem>_<NN>.stl` names, case-insensitive `_2` suffixes, unsafe segments, the `remaining` scope, refusals and the download name | `golden_stl_pack_layout_and_archive` | `rust/crates/pp-api/tests/golden/stl-pack.json` |
| STL pack archive bytes from `ZipStream`, with a fixed DOS date | same test, `archive_hex` | same file |
| STL pack through `GET /api/v2/plans/{id}/exports/stl-pack` on the accepted-Plate fixture | `golden_stl_pack_route_is_byte_identical` | `stl-pack-route.json` |
| Managed Plate 3MF download: member names and uncompressed bytes, welded mesh, and build-item placement | `golden_managed_plate_3mf_members_and_placement` | `managed-plate-3mf.json` |
| HTML checklist: grouping, ordering, escaping, swatches, order number | `golden_checklist_html` | `checklist.html` |
| HTML checklist through `GET /api/v2/plans/{id}/exports/checklist` | `golden_checklist_route_normalizes_only_the_generated_time` | `checklist-route.html` |
| Checkoff attribution of printer object names to Required units: Required-unit names, `stem_NN` file names, duplicates, completed units, ambiguity, excluded Parts, the fallback file name | `golden_checkoff_attribution` | `checkoff-attribution.json` |
| Plate packing: packing order and positions, too-large units, packing around a pinned unit, grouping rules, stored layout digests v1 and v2 | `golden_plate_packing` | `plate-packing.json` |
| 3MF members that make a package a toolpath package, slicer project, model package or unsupported, and an unsafe member | `golden_three_mf_members` | `rust/crates/pp-domain/tests/golden/three-mf-members.json` |
| Object names from EXCLUDE_OBJECT (OrcaSlicer, BambuStudio, quoted PrusaSlicer), M486 and objects_info, grouped per Part and matched to library STLs, `stem_NN` included | `golden_sliced_object_names` | `rust/crates/pp-domain/tests/golden/sliced-object-names.json` |

The `pp-api` tests are in `rust/crates/pp-api/tests/golden_old_screens.rs`. The `pp-domain` tests are in `rust/crates/pp-domain/tests/golden_print_files.rs`, because `rust/scripts/check-boundaries.py` does not let `pp-api` depend on `pp-domain`. Both use `pp_testkit::golden`.

GRE-424's `ticket_t_58_three_mf_managed_archive_golden_survives_restart` opens the
managed Plate ZIP and its 3MF on the Node accepted-Plate fixture. It checks
package members, manifest provenance and identities, and the mesh with the
saved translation applied, against `pp-api/tests/support/managed_plate_3mf.model`.
GRE-1775 adds a regenerable golden of all three uncompressed 3MF members.
The model pins the welded vertices and triangles and the build item's identity
transform: the writer applies the saved Plate translation to the vertices
rather than emitting a `transform` attribute. Neither test pins the compressor's bytes.

GRE-1981's `ticket_t_58_three_mf_direct_golden_survives_restart` checks the
standalone 3MF package members, selected Required-unit identity, material and
mesh in `pp-api/tests/support/direct_3mf.model`. It downloads the same bytes
after storage restarts, without publishing Plates or writing a job.

## Timestamps

Every golden is byte for byte except one value. The checklist route stamps the wall clock into its `Generated` line. The route test checks that the value is RFC 3339 and within five minutes of now, then replaces only that value with `GENERATED`. The pure checklist test passes a fixed time, so it needs no normalization. The STL pack writes the 1980-01-01 DOS date, so its archive bytes are a contract and nothing is normalized.

## Run

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
  --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(golden_)'
```

## Change a golden on purpose

1. Make the behaviour change.
2. Rewrite the files:

   ```sh
   PP_GOLDEN_UPDATE=1 cargo nextest run --manifest-path rust/Cargo.toml \
     -p pp-api -p pp-domain --no-fail-fast -E 'test(golden_)'
   ```

   An update run writes each file and still fails, so CI can never refresh a golden and pass.
3. Run the command in [Run](#run) without the variable. It must pass.
4. Review `git diff` on the golden files and explain each changed line in the PR.

Each test binary lists its golden files. `golden_every_file_is_checked` fails if the directory holds a file that no test compares.
