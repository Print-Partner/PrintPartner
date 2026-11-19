# STEP part trees (GRE-1007)

Library shows the assembly tree of a STEP file (`.step` or `.stp`) in a Source's current revision. Rust reads each file once in the background and caches the tree. The UI only reads the cached answer.

## Why not occt-import-js in the webview

occt-import-js 0.0.23 took 417 s and about 2.7 GB to load the 185 MB Trident STEP file on 8 vCPUs, and returned a part tree only 2 levels deep (occt-import-js bug #60). Live parsing in the webview is not viable, and its tree loses nested part names.

## How the tree is read

`pp_domain::step_assembly::part_tree` reads the STEP product structure instead of the geometry:

- `PRODUCT` gives a part's id and name.
- `PRODUCT_DEFINITION_FORMATION` (or `..._WITH_SPECIFIED_SOURCE`) links a definition to its product.
- `PRODUCT_DEFINITION` (or `..._WITH_ASSOCIATED_DOCUMENTS`) is the node.
- `NEXT_ASSEMBLY_USAGE_OCCURRENCE` places one definition inside another, with an instance name.

A definition that no occurrence places is a root. A sub-assembly used twice appears twice. The file is streamed once. Only those four entity kinds are buffered (64 KiB per record at most), so memory follows the part count, not the file size. Strings may hold `;` and `''`, comments are skipped, and `\X2\`, `\X4\`, `\X\`, `\S\` and `\P..\` string directives are decoded.

Refusals: not an ISO 10303-21 file (`not_step`), no product structure (`no_products`), more than 100,000 nodes or 64 levels (`too_large`), and an assembly that contains itself (`cycle`).

## Background job and cache

`SourceFilesClient::step_conversion` (`rust/crates/pp-storage/src/source_files/step.rs`):

1. Checks the path the same way the thumbnail route does: a plain relative path ending in `.step` or `.stp`, reached through real folders only.
2. Looks up the file's job, keyed by revision id and path. A revision never changes.
3. On the first open it queues the file and answers `queued`. One worker thread per client takes the queue in order and marks the file `converting`.
4. The worker hashes the file (SHA-256). When `thumbs/sources/step/<sha256>-v1.json` already holds a whole tree for that hash, it reads no STEP. Otherwise it reads the tree and writes that file through a temporary name, synced before the rename.
5. Later opens answer `done` with the tree read from that cached file. A failure is kept as `failed` until the next start, and nothing is cached for it.

The same bytes at another path, in another Source, or after a restart reuse the cached file. After a restart the first open is `queued` again while the worker re-hashes the file, then answers from the cache without a conversion. A deleted or damaged cache file is converted again on the next open.

The cache sits inside `thumbs/sources`, which a full backup leaves out (`archive::SOURCE_THUMBNAIL_CACHE`). The thumbnail prune removes only folders named by a revision id, so it never removes `step`. The cache is regenerable and is not pruned.

## HTTP

`GET /api/v1/sources/{id}/step-conversion?path=` (and `/sources/{id}/...`), `readStepPartTree` in the generated client, behind the same read gate as the file list. It never waits for a conversion.

| Field | Meaning |
| --- | --- |
| `state` | `queued`, `converting`, `done` or `failed` |
| `sha256` | The file's hash, set when `done` |
| `failure` | `not_step`, `no_products`, `too_large`, `cycle`, `unreadable` or `cache_unwritable`, set when `failed` |
| `parts` | When `done`, every node depth first: `name`, `product_id`, `instance` (null at a root) and `depth` (0 at a root). The generated client cannot express a recursive schema, so the nesting is carried by `depth`; the cached file is the nested tree |

A missing `path` is 400 `source_file_path_required`. A path that is not a STEP file of the current revision is 404 `source_file_not_found`.

## UI

Library's file list (`web/apps/ui/src/library/SourceFiles.tsx`) gives each STEP file a **Part tree** button. It opens `StepPartTree`, which shows **Queued for conversion.**, **Converting…**, the nested part list with a count, or **Conversion failed.** with the reason. It asks again every second while the file is queued or converting, and stops once it is done or failed.

The files come from the Source's revision on disk, so the webview gets no file-system access. GRE-1007 adds no Tauri capability or plugin permission.

## Not in this change

The cache holds the part tree only. Tessellating the STEP geometry into a cached GLB mesh for a 3D view needs a native B-rep tessellator (OCCT) and is GRE-1784.
