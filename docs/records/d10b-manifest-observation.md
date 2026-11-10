# D10b manifest observation receipt

GRE-604 D10b adds `pp_storage::manifest_options::observation` behind `desktop_rebuild`.
GRE-644 slice 1 wires its build graph manifest commands, described below. HTTP routes remain deferred.

`capture` reads the build, its stored kit, and its ordered source layers in one transaction.
`basis_matches` repeats the capture, so a later transaction can reject a stale observation.
`observe` builds the response of Node's `buildPlanManifestBuilder` in
`web/apps/server/src/services/plan-manifest-builder.ts` through storage-issued `DraftReads` requests:

- It reads each checkout manifest and STL inventory. It filters paths with the source's import rules and sorts them as `listStlRelativePaths` does.
- A source without declared option groups infers groups from shipped and custom path hints, then from sibling folders.
- Groups merge in layer order and keep JavaScript property order. Each variant names the sources whose scanned parts it matches.
- Stored kit selections take precedence. Repository and community manifest defaults fill the other groups in layer order.

As in Node, an unreadable or invalid manifest reports `exists: false` with the fallback YAML.
Invalid UTF-8 decodes with replacement characters. An invalid or unreadable custom hints file fails the observation.
A source without a checkout path is skipped. A source whose checkout root the adapter refuses is omitted.

Adapter reads, manifest and hint parsing, and STL scans charge the caller's `PreparationBudget`.
The stored kit and each source's import rules charge their bytes as documents.
Both also charge every JSON node and its depth to the same budget. Import rules charge one rule per accepted path.
Import rules over the JSON byte, depth, or node limit refuse the observation.
Malformed or non-array import rules give an empty rule set, as in Node.
A malformed stored kit falls back to an empty kit, as in Node.
A stored kit over the JSON byte, depth, or node limit refuses the observation.
Path filtering and pattern matching check cancellation.
Each source returns a SHA-256 stamp of its reads, so a caller can compare two observations.
`validate_known_selections` returns the `manifestSelectionsInputError` messages for a kit save.

The tests compare the response bytes with `rust/crates/pp-storage/tests/fixtures/manifest-builder-node.json`.
`manifest-builder-node.mts` in the same directory regenerates that file from the Node services.
Builder globs normalize backslashes to slashes, lowercase both inputs, and trim
ECMAScript whitespace. They check normalized equality, then match the entire path.
A pattern without a slash also matches against the basename.
Only `*` and `?` are wildcards. They consume zero or more UTF-16 code units and
exactly one unit, respectively, excluding LF, CR, U+2028 and U+2029.
Slashes have no special wildcard restrictions. All regex punctuation is literal.
Literal comparison follows ECMAScript non-Unicode `/i` canonicalization after
lowercasing. Uppercase expansions and non-ASCII-to-ASCII uppercase mappings do
not fold. Lowercasing still maps Kelvin sign to `k` before comparison.
Lone surrogate pattern units are preserved, and `?` consumes half a supplementary character.

The matcher uses two indices and one saved `*` retry point instead of a regex.
It has O(path units × pattern units) worst-case work and O(path units + pattern
units) normalization storage. `BuilderGlob::new` normalizes a pattern once for
all paths. The matcher has no `PreparationBudget` parameter. The observation
checks cancellation once on entry and before each path, so cancellation waits
for at most one path's match, even when no paths were scanned or a variant has no parts.
This resolves the backtracking and UTF-16 parity defects in GRE-642 and GRE-641.

Regenerate or verify the Node differential corpus with
`node rust/crates/pp-storage/tests/fixtures/manifest-globs-node.mjs` or append `--check`.
The generator reads the actual `matchKeyMatches` and `globToRegExp` functions.
Rust case mapping comes from std, while Node uses ICU.
`manifest-case-node.mjs` writes, or with `--check` verifies, `manifest-case-node.jsonl`.
It pins each UTF-16 unit's non-unicode `/i` class and each scalar's `toLowerCase` result.
`builder_glob_case_mappings_match_node_tables` compares Rust against those tables.
The Desktop contracts workflow runs both `--check` commands on Node 22 and 24, so
an ICU or Unicode upgrade on either side fails CI instead of drifting.
Run the ignored `builder_glob_backtracking_probe` test in an exclusive landing slot
with `--release -- --ignored --nocapture` to check the 10 ms release bound.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib manifest_options
```

