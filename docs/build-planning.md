# Build planning and decisions reads

`GET /plans/{id}/build-planning` and `GET /plans/{id}/decisions` are served by
the replacement Rust API with the same data as Node. They mount only with the
default-off `desktop_rebuild` feature compiled and `PP_DESKTOP_REBUILD` set to
`1` or `true`. With the flag off they answer 404 from Rust and Node keeps
serving them.

`pp_core::rebuild_api::read_router`, which Rust-only startup composes, mounts
both at `/`, `/api/v1` and `/api/v2`, as Node does, from the same
`WriterOwner` as the Build and Checkoff reads. Admission is the Accepted Plan
reads' (`pp_api::accepted::Gate`): loopback Host and Origin checks, then a
session cookie or an API key. A request without one gets 401. The id is read
with Node's `Number()`, so `0x2` names Build 2; an id that names no Build of the
caller's tenant gets 404 `Profile not found`.

Both reads write nothing. `pp_storage::read_model::planning` reads every row
in one deferred SQLite transaction. Build planning then hashes Source trees on
the request's blocking thread, not on the storage writer.

## Decisions

`{"decisions": [...]}` holds the Build's latest 200 `plan_decisions` rows,
oldest first, ordered by `created_at` then row id. Each record is Node's
`mapPlanDecision`: `id`, `plan_id`, `created_at`, `actor`, `kind`,
`action_type`, `params`, `label`, `summary`, `rationale` and `result`.
`params` is `params_json` when it holds a JSON object, else `{}`. `result` is
`null` when `result_json` is null or empty, else parsed like `params`.

## Build planning

`{"planning": null}` when the Build has no `build_planning.v1.<id>` setting.
Otherwise `planning` holds:

| Field | Value |
| --- | --- |
| `brief` | The stored brief after Node's `hydrateBuildPlanningBrief` |
| `planning_phase` | `preparing`, `draft`, `applied` (with `revision_id`), `abandoned` or `missing_draft`, each with `draft_id` except `preparing` |
| `readiness` | `ready` and the ordered `blockers` (`code`, `detail`) of Node's `deriveBuildPlanningReadiness` |
| `grouped_difference_count`, `difference_count` | Distinct difference groups and differences in the hydrated brief |

Hydration follows Node step by step:

1. Each evidence item that names a tenant Source gets `sync_status` `synced`
   and `pinned_revision` (the Source's `last_commit_sha`) when the Source has a
   resolved root, a sync time and a commit; otherwise `pending` and no pin.
   The root is Node's `resolveSourceFilesystemRoot` for the stored
   `local_path` (`SourceReads::node_root`): any absolute path for the trusted
   single-user desktop, a folder inside `repos/<id>` for isolated accounts.
2. When the structural base is synced, its tree is compared with each synced
   overlay's tree: every regular file except `.git` and
   `.printpartner-source-snapshot.json`, by SHA-256, with symlinks skipped.
   Changed, renamed, removed and added files become differences grouped by
   `<base evidence>:<overlay evidence>:<directory>`. Stored contradictions are
   dropped and recomputed from evidence with the same title and different
   extracts.
3. A resolution survives only if its group still holds the same differences.
   `draft_id` survives only if the differences, every resolution and the
   Source revisions recorded for the draft are unchanged.
4. The phase reads the surviving draft as Node's `getPlanDraft` does,
   verifying its snapshot digest.

A malformed brief, a brief for another Build, an unreadable Source tree or a
draft whose digest does not verify answers 500, as Node does. Node checks
`draft_id` in the query only once a brief exists and then ignores it: a value
that is not one positive integer gets 400.

## Differences from Node

- An optional brief field Node omits is served as `null`, as every
  `@print-partner/rust-client` field is required. Brief keys Node's validator
  does not read are dropped.
- A stored brief with an explicit `null` in an optional field reads as absent;
  Node refuses it. Node never writes one. Stored timestamps must be RFC 3339,
  RFC 2822 or `YYYY-MM-DD`, a subset of what Node's `Date.parse` accepts.
- Errors are RFC 9457 problems. Node's 500 body is Fastify's default.

## Evidence

`fixtures/parity/planning-http/generate.mjs` seeds Node's real app with ten
Builds (briefs in every phase, synced and stale Source trees, contradictions,
every readiness blocker, a corrupt draft, 205 decisions) and records 27
responses and the seed itself in `node.json`. `pp-core`'s
`build_planning_read` test replays the same seed into a Rust data directory,
sends each request through `read_router` and requires Node's status and
complete body (brief `null` fields aside), a 401 without a session, and an
unchanged database and file tree.

```sh
node fixtures/parity/planning-http/generate.mjs --check
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(build_planning_read)'
```
