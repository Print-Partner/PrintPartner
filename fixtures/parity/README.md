# Parity fixtures

Golden data captured from the TypeScript app, used as the reference for the Rust port.

## Parity runner (GRE-451, T-10-fixtures)

`pp_api::fixtures`, behind the default-off `desktop_rebuild` feature, replays copied fixtures against Rust routers on disposable data directories with no OAuth provider and no mailer. `Ledger` requires each Rust response to equal Node's status, complete JSON body and literal `Set-Cookie` headers, unless the case has a declared `Correction`. A correction names its kind (security, WebSocket, or durable-store) and reason, and holds the literal Rust response. It fails if Rust now matches Node, if it is never used, or if it is declared twice. `StateGraph` records every row of every SQLite table and the SHA-256 of every other file, so a test can compare state changes as well as responses.

`rust/crates/pp-api/tests/ticket_t_10_fixtures.rs` runs `auth-login` (11 exact, 3 security corrections, no state change on any refusal) and `auth-session` (4 exact, 3 clearing-cookie corrections, Node's session row count, and only the `sessions` table changing when that count moves). It also runs `jobs-http`, `jobs-ws`, `plans-http`, `drafts-http`, `layers-http`, `manifest-http`, `manifest-checkouts-http`, `variants-http`, `catalog-http` and `accepted-http` (see below). Run it with `cargo nextest run --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --no-tests=fail -E 'test(ticket_t_10_fixtures)'`.

## jobs-http/node.json (GRE-451)

`generate.mjs` registers Node's real job read routes, flat and under `/api/v1`, over an `InProcessJobRunner` that has finished an update check and, a second later, failed a checklist export for profile 7. It records 13 complete responses: the `/api/v1/jobs` list under each filter, both jobs on both read-one aliases, and an unknown id on both aliases. Job ids and timestamps become labels numbered in first-seen order (`job-1`, `time-1`); `pp_api::fixtures::Labels` labels the Rust responses the same way. `updated_at` gets its own `updated-N` labels, because Node stamps it with a second clock read after `finished_at` and their equality depends on a millisecond tick.

`ticket_t_10_fixtures` replays the cases against `jobs_router`, with 5 exact matches and 8 declared durable-store corrections: Rust snapshots add `created_at`, and a failed durable job reports "Job failed" and its last progress instead of the handler's in-memory error (both documented for GRE-609 in `docs/desktop-rebuild.md`). The reads change no table or file. Node's read-one returns the stored snapshot without `updated_at`, which only its list adds; Rust's read-one now does the same.

```sh
node fixtures/parity/jobs-http/generate.mjs --check
```

## jobs-ws/node.json (GRE-451)

`generate.mjs` registers Node's real job socket (`registerJobWebSocket`, with `PP_DESKTOP_REBUILD=1`) over the same two retained jobs as `jobs-http`, listens on loopback and opens `/ws/jobs/:jobId` with a `ws` client. Each case records every text frame plus the close code and reason the client saw. The five cases are the finished job, the failed job, an unknown id, the finished job read by another tenant, and a live job (`ws_live`). Node mounts the socket only at the root, so there is no `/api/v1` case.

Two parts of the probe are stubs, not Node's app:

- The probe mounts no auth. Node's app sets `request.tenantId` from the session; here an `x-parity-tenant` header sets it. The runner's own owner check still decides who reads a job, so the foreign case exercises Node's tenant refusal but not its session lookup.
- `ws_live` follows a third update check. The probe replaces `runner.runJob` so the job stays queued, then sends the two patches `runJob` emits (running at 10%, then done) through the runner's own `emit`, one after each frame. Node's handlers have nothing a probe can hold open without network access.

A socket that has not closed within 10 seconds fails the capture. Node stamps `updated_at` with its own clock read, so it gets `updated-N` labels, and the probe spaces the live steps 5 ms apart so each frame's stamp is distinct.

`ticket_t_10_fixtures` opens the same paths on `jobs_router` with the owning session, or a second account's session for the other tenant. It reads frames off the raw socket, joins fragmented text, skips ping and pong frames, and fails if a socket stays open for 10 seconds. For `ws_live`, the seeded worker claims the job after the first frame and finishes it after the second. The results are 2 exact matches and 3 declared WebSocket corrections:

- An unknown or foreign job matches exactly. Each gets no frame, then 1008 "Job not found".
- For the finished and failed jobs, Node ends the stream with `socket.close()`, an empty close frame the client reads as 1005, and Rust sends 1000. Rust's frame is the durable snapshot that `jobs-http` corrects (`created_at`, and "Job failed" with no progress).
- In `ws_live`, Rust streams the durable model's wording, as `docs/desktop-rebuild.md` and `jobs_engine_parity` record. The queued frame says "Waiting for a compatible worker" instead of "Queued", and the running frame says "Running" with no progress instead of "Running…" at 10. Every frame carries `created_at` and `finished_at`, and none carries `updated_at`, which Node adds only to live frames. The stream closes 1000.

No socket changes a table or file. The test checks this just before the live case. After that, only the worker writes.

```sh
node fixtures/parity/jobs-ws/generate.mjs --check
```

CI runs every GRE-451 generator with `--check` in the Desktop contracts workflow ("Verify GRE-451 parity Node fixtures"), and `scripts/local-ci.py` runs the same commands. PR CI skips the `desktop_rebuild` nextest on self-hosted Linux, so the same workflow also replays the goldens against Rust on the Node 22 leg ("Replay GRE-451 parity fixtures against Rust"):

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test ticket_t_10_fixtures
```

## planning-http/node.json (GRE-1119)

`generate.mjs` builds Node's real app on a scratch data directory, creates ten Builds, then seeds them with SQL rows and Source files: briefs in every planning phase, a synced structural base and overlay whose trees differ (including two same-content renames that pair like Node only when each directory is read in sorted order), a stale brief, contradictory evidence, every readiness blocker, a malformed brief, a brief for another Build, a draft whose digest does not verify, and 205 decisions. It records 28 responses from `GET /plans/:id/build-planning` and `GET /plans/:id/decisions` on the flat, `/api/v1` and `/api/v2` mounts, with the tables each changed (none). The seed goes into `node.json` beside the responses, with `{data}` for the data directory, so Rust reads the same data. Every stored value is literal, so nothing is labelled.

`pp-core`'s `build_planning_read` test replays the seed through `read_router` and requires Node's status and complete 200 body for every case (Rust serves absent optional brief fields as `null`, which the test removes from the brief before comparing), Node's `detail` for 400 and 404, a 401 without a session, and no changed table or file. See [Build planning and decisions](../../docs/build-planning.md).

```sh
node fixtures/parity/planning-http/generate.mjs --check
```

## plans-http/node.json (GRE-451)

`generate.mjs` builds Node's real app (`buildApp` over self-host ports, no API key) on a scratch data directory and sends 28 requests in order to the Plan routes Rust's `build_router` serves: create, list, read-one, layers, touch and delete, each on the flat, `/api/v1` and `/api/v2` mounts, plus duplicate, blank, unnamed and unknown-id refusals. Each case records the complete response and the database tables it changed. Plan ids are sequential on a fresh database and stay literal; timestamps become `time-N` labels.

`ticket_t_10_fixtures` replays the cases in order against `build_router` with a session cookie and compares each response and each case's changed tables, with 27 exact matches and 1 declared durable-store correction. Node's `POST /plans` inserts the Plan before it looks up `base_project_id`, so a refused create leaves a Plan behind. Rust refuses before it writes: that case changes no table in Rust, and the list after it does not show the leftover Plan. The fixture made two Rust fixes go red then green. `/api/v1` now answers with Node's legacy summary (`remaining_units` and `total_units` instead of `accepted_progress`, with Node's 409 and 500 refusals when accepted progress is unavailable). A create body with no `name`, or a null one, now gets "Profile name is required".

```sh
node fixtures/parity/plans-http/generate.mjs --check
```

## drafts-http/node.json (GRE-451)

`generate.mjs` builds Node's real app on a copy of `rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db`, where Plan 1 has consumed draft 1 and open draft 2 and Plan 2 belongs to another tenant. It sends 30 requests in order to the draft routes Rust's `draft_router` serves, on the flat and `/api/v2` mounts: list, workspace read, part edit (including a replayed edit), abandon, and their invalid-id, invalid-body, missing, foreign-tenant, stale and not-open refusals. Each case records the complete response and the tables it changed. No response carries a clock value, so nothing is labelled.

`ticket_t_10_fixtures` replays the cases against `draft_router` on its own copy of the same database, with that database's session cookie: 30 exact matches, no corrections, and the same changed tables for every case.

```sh
node fixtures/parity/drafts-http/generate.mjs --check
```

## layers-http/node.json (GRE-451)

`generate.mjs` uses the same copied database after its `seed` SQL adds three Sources with no files (two for tenant `default`, one for `foreign`). It sends 33 requests in order to the Plan layer mutations Rust's `build_router` serves, on the flat, `/api/v1` and `/api/v2` mounts: attach an add-on, replace it, set the base, detach, and their duplicate, missing, foreign and no-Source refusals. Two cases send `project_id` as `2.0` and `1e0`; their bodies are recorded verbatim as `rawBody`, because `JSON.stringify` would write them as integers. Each case records the complete response and the tables it changed.

`ticket_t_10_fixtures` runs the same seed on its copy and replays the cases against `build_router`: 33 exact matches, no corrections, and the same changed tables for every case. The fixture made one Rust fix go red then green. Node reads `project_id` with `Number()`, so a body with no usable id (missing, `null`, `0`) passes Node's Plan check and then finds no Source: 404 "Project not found" (or "Layer not found" for a replace of a layer the Plan does not have). Rust refused such a body with 400 "Request is invalid"; it now gives Node's answers and writes nothing. A body that is not a JSON object is still a 400. A second fix also went red then green: Rust read `project_id` only as an integer, so `2.0` found no Source, while Node's `Number()` reads it as 2. Rust now accepts any whole-number JSON value.

```sh
node fixtures/parity/layers-http/generate.mjs --check
```

## manifest-http/node.json (GRE-451)

`generate.mjs` uses the same copied database after its `seed` SQL adds two Sources with no local checkout, Plan 3 (base and add-on layers on those Sources, with a stored kit) and Plan 4 (no layers, with a stored kit that is not JSON). It sends 46 requests in order to the kit-manifest and plan-manifest-builder routes Rust's `build_router` serves, on the flat, `/api/v1` and `/api/v2` mounts: kit reads (stored, unreadable, never saved, `HEAD`), builder reads, saves (full, partial, unknown keys), and refusals for missing, foreign, zero and non-numeric Plan ids, a missing body, malformed JSON, and every field the kit parser checks. Each case records the complete response and the tables it changed.

Plan 1's base Source names a local checkout outside the data directory. Node would read that path; `SourceReads` applies Node's isolated rule and refuses any path outside `repos/<id>` (GRE-1056 tracks trusted-mode paths). So the corpus reads only Plan 1's kit, which needs no checkout. Option groups from real checkouts are in `manifest-checkouts-http` below.

`ticket_t_10_fixtures` runs the same seed on its copy and replays the cases against `build_router`: 43 exact matches and 3 declared corrections. A corrected case must change no table. Malformed JSON gets Rust's "Request is invalid" (security: no Fastify parser codes). Two saves select an option group that no attached Source defines: Node skips the check and saves, Rust refuses with "is an unknown option group", as `docs/desktop-rebuild.md` documents (validation). They run last, so no later read sees Node's write.

The fixture made three Rust fixes go red then green. A Plan id that cannot name a Plan (`0`, `abc`, `-1`, `1.5`) is now Node's 404 "Profile not found" after authentication, instead of 400 "Request is invalid". An empty body is now "kit is required". Selection and replacement refusals now use Node's text: `kit.selections group ids must not be empty`, `kit.selections.<group> must be a non-empty string`, `kit.selections.<group>[<index>] must be a non-empty string` (checked for every entry before duplicates), `kit.selections.<group> must not contain duplicate variant ids` and `kit.replacements.<key> must be a string`.

```sh
node fixtures/parity/manifest-http/generate.mjs --check
```

## manifest-checkouts-http/node.json (GRE-451)

`generate.mjs` covers the builder reads and kit saves over real option groups that `manifest-http` could not, because Rust had no production `DraftReads` adapter until GRE-1052. It uses the same copied database after its `seed` SQL adds Sources 2-6 and two Plans: Plan 3 (base Source 2, add-on Source 3, both with a `print-partner.manifest.yaml` that declares option groups and default selections, and Source 3 with import rules) and Plan 4 (base Source 4 with no manifest, so its groups are inferred from sibling folders and a custom `path-hints.yaml`; add-on Source 5, whose checkout does not exist; add-on Source 6, whose manifest is invalid YAML). `files` holds every checkout file and the custom hints, relative to the data directory, and `checkouts` points each Source's `local_path` at `repos/<id>` there. Node writes them under its scratch directory before it opens the app.

It sends 20 requests in order on the flat, `/api/v1` and `/api/v2` mounts: builder reads before and after each save (merged groups across layers, the Sources each variant matches, manifest defaults under saved selections), saves with scalar and list selections, a save with no selections, and the cardinality refusals (an empty `pick_one` list, two `pick_one` variants, more than `max`, and the first failing group of several). Each case records the complete response and the tables it changed.

`ticket_t_10_fixtures` writes the same files, sets the same paths and runs the seed on its copy, then replays the cases against `build_router` built with `build_graph_with_observations` over the production `SourceReads`, as `pp_core::rebuild_api::clients` wires it: 17 exact matches and 3 declared validation corrections, none of which writes anything. Node checks only how many variants a known group gets and saves the rest; Rust refuses a selection for a variant (`contains an unknown variant id`) or group (`is an unknown option group`) that no attached Source defines, as `docs/desktop-rebuild.md` documents. Those cases run last, so no later read sees Node's write. With no reads adapter the first builder read fails with 503 "Manifest source observation failed".

```sh
node fixtures/parity/manifest-checkouts-http/generate.mjs --check
```

## variants-http/node.json (GRE-1221)

`generate.mjs` uses the same copied database after its `seed` SQL adds Sources 2-10, Plans 3-13 and four stored selections (not JSON, a number, empty, and an object whose keys and numbers Node reorders and reformats). `files` holds the checkouts: Source 2's manifest declares `variant_dimensions` with strings, numbers (`1.5`, `2.0`, `.nan`, `.inf`, `1e21`, `-0`), entries Node drops (booleans, null, objects, lists, a non-list value) and anchors; Source 4's manifest is invalid YAML, Source 5's lists its dimensions, Source 6's is a string, Source 8 has no manifest, and Source 9's manifest does not load because a selection is empty, and Source 10's does not load because `String(project)` throws on a mapping with its own `toString` key. Source 2 also declares a `__proto__` dimension, which Node drops. Source 3 has no `local_path` and Source 7's checkout does not exist.

It sends 78 requests in order on the flat, `/api/v1` and `/api/v2` mounts: dimension reads for every shape, missing, foreign and non-numeric Plans, ids Node's `Number()` reads (`0x3`, `" 3e0 "`, `3.0`), saves that match strings and JavaScript `String()` of numbers, raw bodies whose key order Node reorders, duplicate keys, a `null` and an empty body, bodies Fastify's parser refuses (`__proto__`, not JSON, empty under `application/json`, also on a missing or non-numeric Plan), and each refusal (body shape, `source_id`, no base Source, another Source, unknown dimension, unsupported option). Each case records the complete response and the tables it changed; each save also records the stored selection text and the Source's `imported_paths`.

`ticket_t_10_fixtures` writes the same files and replays every case against `build_router` over the production `SourceReads`: 74 exact matches and four declared corrections (the parser refusals), and the stored text after all 18 saves equals Node's.

```sh
node fixtures/parity/variants-http/generate.mjs --check
```

## catalog-http/node.json (GRE-451)

`generate.mjs` uses the same copied database after its `seed` SQL adds Source 2 (tenant `default`, with an import rule and a category), Source 3 (tenant `foreign`) and five activity events, one for `foreign`, one of another kind and one whose payload is not JSON. It sends 56 requests in order to the catalog routes Rust's `admit_catalog_routes` serves, on the flat and `/api/v1` mounts: Source list, detail, create, patch and delete, import rules, Source activity, Source naming, Source categories, global STL naming and its preview, with their missing, foreign, non-numeric, duplicate, retained-history and invalid-body refusals. Each case records the complete response and the tables it changed. A created Source's `local_path` sits under Node's scratch data directory, so the golden writes that directory as `{data}`, and the Rust test writes its sandbox the same way.

Creating a GitHub Source makes Node start a `sync` job, which would clone over the network. The generator replaces the job runner's `start` with a stub that records the call, and the case lists it under `effects`. Rust's catalog create starts no job; the test checks that the only recorded effect is that one `sync` start, and that Rust and Node changed the same tables (`projects` only).

`ticket_t_10_fixtures` runs the same seed on its copy and replays the cases against `admit_catalog_routes`: 53 exact matches and 3 declared corrections, none of which writes anything. Malformed JSON gets Rust's "Invalid JSON body" (security: no Fastify parser codes). Renaming a Source to a taken name gets "Source already exists: <name>", Node's create refusal, instead of Node's SQLite constraint text (security). An invalid global naming profile gets Rust's "Invalid export role order" instead of zod's issue path (validation).

The fixture made three Rust fixes go red then green. Source naming refusals now carry Node's `code` (`source_not_found`, `invalid_source_naming`, `source_naming_conflict`, `invalid_source_naming_state`) next to `detail`. A naming id that is not a positive integer (`abc`) is now Node's 400 "Source id must be a positive integer" instead of 404, and a body without a boolean `use_defaults` gets Node's discriminator message. The naming preview now writes a whole `quantity` as `2`, as `JSON.stringify` does, instead of `2.0`.

```sh
node fixtures/parity/catalog-http/generate.mjs --check
```

## accepted-http/node.json (GRE-451)

`generate.mjs` builds Node's real app on a copy of `rust/crates/pp-storage/tests/fixtures/accepted-plan-node.db`, where Plan 1 has an accepted revision, Part 1 is excluded and Part 2 is included. It sends 32 requests to the Accepted Plan read routes Rust's `accepted_router` serves: Plan Checkoff on the flat, `/api/v1` and `/api/v2` mounts, and Part assembled units on the flat and `/api/v1` mounts (Node registers Part routes only there). The requests run in five phases, each on a fresh copy:

- `ready`: both reads on every mount, ids that `Number()` reads as 1 or 2 (`1.0`, `1e0`, `+1`, `0x1`, `%201%20`), and ids that name nothing (`0`, `-1`, `1.5`, `abc`, `Infinity`, `9007199254740993`, an unknown id).
- `foreign_plan`: Plan 1 moved to tenant `foreign`.
- `foreign_parts`: every Part moved to tenant `foreign`.
- `dirty`: Plan 1 with no accepted revision, which needs compatibility repair.
- `corrupt`: every revision snapshot digest altered.

A phase with a seed drops every trigger and turns foreign keys off before it runs its SQL, as `ticket_t_61_read_model_http` does. Each case records the complete response and the tables it changed; no read changes any.

`ticket_t_10_fixtures` runs the same phases on its own copies with the database's session cookie and replays them against `accepted_router`: 31 exact matches and 1 declared security correction. With Plan 1 on another tenant, Node finds Part 2, its accepted read then fails and the route answers 500 "Internal Server Error". Rust answers 404 "Part not found", as it does for the Plan itself.

```sh
node fixtures/parity/accepted-http/generate.mjs --check
```

## auth-reset/node.json (GRE-864)

`generate.mjs` drives Node's `POST /auth/forgot-password` for a known account, an unknown account, and a rejected loopback delivery. The golden records one shared public response, plus delivery counts, reset-token counts, and the mail-failure error class. Addresses and tokens stay out of the file. `rust/crates/pp-api/tests/auth_reset_parity.rs` replays the cases. See [auth-reset/README.md](auth-reset/README.md).

## custom-filaments.json (GRE-579)

Golden for `web/apps/server/src/services/custom-filaments.ts`.

- **Shipped entries: 0.** The TS source contains no built-in custom filament list. Custom filaments are user data in `custom_filaments.json` under the data dir, created through `addCustomFilament`. The golden records the empty shipped catalog and the entry shape.
- **Flags: none.** The entry type has no flag field.
- The vitest check `web/apps/server/src/services/custom-filaments.parity.test.ts` confirms the golden against TS: an empty store lists zero entries, and an added entry has exactly the fields listed in `entry_fields`.

### Filaments flagged by West3D

The research brief "Print Partner Rust conversion gap review (2026-10-09)" lists these four as wrong hex values in `services/custom-filaments.ts`:

| Name | Found in `custom-filaments.ts`? | Values |
| --- | --- | --- |
| Burnt Orange | No | Not shipped by TS. Unchanged. |
| Midnight Purple | No | Not shipped by TS. Unchanged. |
| Hot Pink | No | Not shipped by TS. Unchanged. |
| Voron Red | No | Not shipped by TS. Unchanged. |

These names exist only in the West3D Shopify catalog, `web/apps/server/src/data/ambrosia_fallback.json`. That file is not a custom filament list. Those values are not in the golden and were not changed. Fixing the data, and deciding whether the brief's reference is correct, is Chad's call.

## community-manifest-slug.json (GRE-681)

Slugs that `isCommunityManifestSlug` in `web/apps/server/src/lib/community-manifest-slug.ts` accepts (`valid`) and refuses (`invalid`). The Vitest check in `community-manifest-slug.test.ts` and the Rust check for `CommunitySlug::parse` in `rust/crates/pp-storage/src/manifest_options/observation_tests.rs` both read this file. It only catches cases it lists: when either grammar changes, add the new edge cases here, as GRE-691 did by reserving `kit-catalog`.

## required-unit-decisions/node.json (GRE-878)

`generate.mjs` copies `accepted-plate-node.db` and `required-units/unsafe.db` to a scratch directory and calls Node's `PlanDraftWorkspaceService` with the Required-unit decisions in its `cases` list: excluding a Part and restoring it, splitting a quantity, accepting a prior completion, and replacing it. Each case closes and reopens the database part way through. The file records each request, its result and the draft rows after it, with the Required-unit, progress and immutable Plate history before the calls. The generator fails if that history changes.

Tokens and reconciliation IDs are replaced with labels numbered in sorted order across the whole case, so every equality link stays visible. `rust/crates/pp-storage/tests/required_unit_decision_parity.rs` sends the same requests through the public Rust clients, applies the same labels and compares the result. It also checks that swapping two reused tokens or clearing the completed progress row makes the comparison fail.

From `web/`, after `npm ci`, with Node 22.23.3 or 24.21.0:

```sh
TMPDIR=<scratch> node --conditions=development --import tsx ../fixtures/parity/required-unit-decisions/generate.mjs [--check]
```

## kit-bundle/kit-bundle.json (GRE-425)

`generate.mjs` runs Node's `export-kit.ts` and `kit-bundle-share.ts` and records 57 kit uploads (raw `kit.json` and zips) with what `parseKitBundleBuffer` returned or threw, 13 bundles through `collectKitBundleSourceRefs` and the unmatched, patch and export records built from each reference, 13 project rows through `kitSourceRefFromProject`, and one editable bundle as `JSON.stringify(data, null, 2)` and as an fflate zip. Inputs over 1 MiB are recipes that both sides build; results over 1 MiB are a SHA-256 of their JSON text.

`rust/crates/pp-services/tests/ticket_t_58_kit.rs` replays the file against `pp_services::kit`. Successes and refusal messages must match exactly. Rust reports its own text for JSON syntax errors (V8 wording), archive errors (adm-zip wording) and non-iterable `sources` or `layers` (a V8 `TypeError` naming the source expression); the test checks only that the refusal is the same kind. Two project cases are declared differences. Node looks the lowercased role up on a plain object, so `constructor` and `__proto__` resolve to members of `Object.prototype`: the export drops `category` (or writes `{}`) and the patch writes that metadata. Rust reads both as unknown roles, category `unassigned`. Other prototype names (`toString`, `valueOf`, `__defineGetter__`) are lowercased past the prototype and match exactly. A number literal past f64 range parses to `null` in Rust; Node holds `Infinity` and writes it as `null`.

From the repo root, after `npm --prefix web ci`:

```sh
node fixtures/parity/kit-bundle/generate.mjs [--check]
cargo nextest run --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --no-tests=fail -E 'test(ticket_t_58_kit)'
```

## profile-sync/node.json (GRE-437)

`generate.mjs` runs Node's real `startProfileSyncWatcher` over the three slicer trees in `roots/` (Orca, Bambu Studio and PrusaSlicer, laid out as `defaultWatchDirs` expects), with a repository that records each upsert. It calls `syncAll()` once and stops the watcher. Each outcome is either the table and complete upsert input plus the broadcast event, or `failed` when Node logged a parse failure. Paths are relative to this directory.

The trees hold user saves that inherit a parent (looked up case-insensitively), a missing parent, INI, array, string and `null` parents, a `type` field that overrides the directory, BOM and CRLF files, Unicode separators (U+2028, U+2029, U+00A0, U+3000, U+FEFF, U+0085) at trim and `\s` boundaries, number literals that overflow to Infinity (top level, in arrays and objects, as parents a child inherits, and one in key position), their finite neighbours (`1e308`, the `Number.MAX_VALUE` rounding edge, `-0`, `5e-324` and subnormals that round), invalid number forms that `JSON.parse` refuses even when they would overflow (`-.5e999`, `.5e999`, `1.e999`, `-01e999`, `+1e999`), malformed JSON, a directory named `*.json`, number formatting cases, duplicate and array-index keys, and material names for `inferMaterialType`. `.gitattributes` keeps line endings byte-exact on Windows checkouts.

`rust/crates/pp-services/tests/ticket_t_60_profile_sync.rs` scans the same roots with `pp_services::profile_sync::scan_root` and requires every outcome to match, in order, including the `resolvedFlatConfig` string byte for byte.

```sh
cd web && node --conditions=development --import tsx ../fixtures/parity/profile-sync/generate.mjs --check
```

## github-sync/node.json (GRE-429)

`generate.mjs` runs Node's GitHub sync code under web's tsx loader: `parseGithubUrl`, `githubRefName`, `normalizeGithubSourceLocation` and `resolveGithubUrlRef` on 33 URLs, `parseReposTxtLine` on 16 lines, `parseReposTxtText` and `importReposTxt`'s skip count on 3 files, `listGithubBranches` and `listGithubTags` on 7 URLs each, `syncGithubSource` on 36 cases, and one `checkAllSourceUpdates` run over 13 Source rows. A `globalThis.fetch` stub answers Octokit from the `world` object in the golden and logs every GitHub read. A stubbed `LocalSourceSnapshotStore.materialize` records the snapshot request (revision key, files and selection) and publishes nothing, so no file is downloaded. Each case records its result or error and the reads it made. An error that carries an HTTP status records only the status, because Octokit's message text is not part of the contract.

`rust/crates/pp-services/tests/ticket_t_59_sync.rs` serves the same `world` through a `GithubPort` stub and compares every case, including the read log, with `pp_services::sync`. All 103 cases match exactly (the update check counts as one case). The plan applies Node's `sourceRelativePath` rules, so `colon-path` and `manifest-dir` match Node too. `pp_source`'s stricter path rules (see `rust/crates/pp-source/README.md`) apply later, when the snapshot store publishes the files.

```sh
node fixtures/parity/github-sync/generate.mjs --check
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail -E 'test(ticket_t_59_sync)'
```
