# STEP part trees (GRE-1007)

`pp_domain::step_assembly` reads the full assembly tree of a STEP file (ISO 10303-21) from its product structure. This is the first of three changes:

1. GRE-1007 (this page): the reader.
2. GRE-1801: a background worker that converts each file once per SHA-256 and caches the tree.
3. GRE-1785: the gateway route and Library's queued, converting, done and failed states.

## Why not occt-import-js in the webview

occt-import-js 0.0.23 took 417 s and about 2.7 GB to load the 185 MB Trident STEP file on 8 vCPUs. It returned a part tree only 2 levels deep (occt-import-js bug #60). Live parsing in the webview is not viable, and its tree loses nested part names.

## How the tree is read

`part_tree` reads the STEP product structure instead of the geometry:

- `PRODUCT` gives a part's id and name.
- `PRODUCT_DEFINITION_FORMATION` (or `..._WITH_SPECIFIED_SOURCE`) links a definition to its product.
- `PRODUCT_DEFINITION` (or `..._WITH_ASSOCIATED_DOCUMENTS`) is the node.
- `NEXT_ASSEMBLY_USAGE_OCCURRENCE` places one definition inside another, with an instance name.

A definition that no occurrence places is a root. A sub-assembly used twice appears twice. The file is streamed once. Only those four entity kinds are buffered; geometry records are skipped as they stream past. Strings may hold `;` and `''`, comments are skipped, and the `\X2\`, `\X4\`, `\X\`, `\S\` and `\P..\` string directives are decoded. An occurrence that names a definition the reader did not keep, such as one written as a complex instance, is skipped.

On the pinned Trident file (185,183,500 bytes), an optimized build returns one root, 1,625 nodes and 9 levels in about 3 s, with about 3 MiB peak RSS (gpt-6.2-sol score on `4c2445ee`).

## A well-formed exchange only

A tree is returned only for this shape, followed by nothing but whitespace and closed comments:

```text
ISO-10303-21; HEADER; <header entities> ENDSEC; DATA; <#id=...; records> ENDSEC; END-ISO-10303-21;
```

- Every statement is a name (or `#id=` and a name), at most one balanced parameter list, and `;`.
- There is exactly one `HEADER` section and one `DATA` section, in that order. The header holds named entities only; the data section holds `#id=` records only.

Departures fail in one of two ways:

- `Incomplete`: the file ends early, mid-record, mid-string, mid-comment, before `ENDSEC;`, or without the final line. A trailing unclosed comment counts too.
- `Malformed`:
  - a missing or extra section, or sections out of order;
  - a second `DATA` section (edition 3 `ANCHOR`, `REFERENCE` and multiple `DATA` sections are not supported);
  - text between sections, or a record in the header;
  - a statement that runs on past its parameter list, which is how a missing `;` between two records shows up;
  - anything after `END-ISO-10303-21;`;
  - a product-structure id used twice. Ids of skipped geometry records are not tracked.

A cut-off or altered file never yields a partial or different tree.

## Bounds

`Limits::default()` caps one read. Each cap is checked when a record is kept or a node is built, and fails the read as `TooLarge` at once:

| Bound | Default |
| --- | --- |
| Nodes in the tree, counting every use of a shared part | 100,000 |
| Levels in the tree (a lone root is 1) | 60 |
| Product-structure records kept while reading | 400,000 |
| Bytes in one product-structure record, from its `#` through its `;`, comments included | 64 KiB (65,536) |
| Bytes of ids and names kept while reading | 16 MiB |
| Bytes of ids and names in the finished tree | 32 MiB |

Geometry records are never buffered, whatever their size. The 60-level cap keeps every tree readable as nested JSON: a cached tree nests two values per level plus two, which stays under serde_json's limit of 128. Real printer assemblies nest a handful of levels; the Trident has 9.

## Cancellation

The cancel flag is checked before every 64 KiB read, so one long record or comment cannot delay it. It is also checked for every node while the tree is built.

Other refusals: a file that does not start with `ISO-10303-21;` (`NotStep`), no product structure (`NoProducts`), and an assembly that contains itself (`Cycle`).

## Not in this change

- GRE-1801: the per-hash background worker and cache. That code has been through two Sol rounds and is on branch `gre-1007-202611182157-e222-storage`.
- GRE-1785: the gateway route `GET /api/v1/sources/{id}/step-conversion?path=`, `readStepPartTree` in the generated client, and Library's **Part tree** view.
- GRE-1784: tessellating the STEP geometry into a cached GLB mesh, which needs a native B-rep tessellator (OCCT).

The reader takes any `Read`, so it needs no webview file access. GRE-1007 adds no Tauri capability or plugin permission.
