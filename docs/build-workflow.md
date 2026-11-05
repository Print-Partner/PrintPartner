# Build workflow and task feed

GRE-1123. Today, Build Overview and MCP read one projection, so they cannot
disagree about a Build's next action. The Rust API serves it only with the
`desktop_rebuild` Cargo feature and `PP_DESKTOP_REBUILD=1` or `true`.

| Read | Route | Answers |
| --- | --- | --- |
| One Build | `GET /plans/{id}/workflow` at `/`, `/api/v1`, `/api/v2` | Stages, next action, active work |
| Task feed | `GET /api/v1/tasks`, `/api/v2/tasks` | Every open Build's next action, most urgent first |
| MCP | `get_build_workflow` with `{"plan_id": n}` | The same body as `GET /plans/{id}/workflow` |

All three need a session cookie or an API key (MCP: API key only) and change
no Build data. The HTTP routes check the credential inside the read
transaction and write nothing. MCP is not fully read-only: the shared MCP
guard resolves the API key before any tool runs, and that records the key's
`last_used_at`. An unknown or
malformed Build id answers 404 after the credential check; a missing or bad
credential answers 401. MCP accepts `plan_id` as any positive safe integer,
including `1.0`.

## One projection

1. `read_model::build_workflow::execute` reads every requested Build's
   `workflow::Facts` in one deferred SQLite transaction, through the same
   `context::capture` that Build reads use: Sources freshness, Accepted Plan
   progress, the newest open draft, plate state, the send queue and printer
   Checkoff links.
2. `WorkflowClient::read` adds the inputs other owners compute
   (`WorkflowInputs`), then resolves each Build with `workflow::resolve`.
3. The task feed is `rank` over those resolved workflows. It never picks a next
   action of its own; each task's `next_action` is the Build workflow's.

## Next action rules

The resolver keeps Node's order (failed results, failed jobs, results awaiting
verification, active jobs, queued jobs, unreadable Plan, no Sources, drafts,
Sources changes, first Plan) with these replacement-product changes:

- **Saved draft, not accepted.** An open draft with changes is
  `review_working_plan`: "N Plan changes are saved in a draft and not used for
  Production yet." Autosave saves the draft; nothing in the copy says it was
  accepted.
- **Manual Checkoff.** A Build with remaining units and no printer Checkoff
  link (dismissed links do not count) gets `check_off_manually` in the
  Checkoff tab when its plates are not started or ready. Plates that are
  preparing, stale or failed keep `prepare_production`.
- **Finished Builds stay quiet.** When every required unit is checked off and
  no draft is open, changed Sources show as complete history, not stale work.
  A confirmed bed (link `verified` or `applied`) is never attention.
- **Stock shortfall** comes only from the adapter. `Stock::Ready{shortfalls>0}`
  makes `resolve_stock_shortfall` the next action ahead of Production;
  `not_ready` never claims stock is fine or short.

`next_action.target` names what to open: `plan_id`, the Build workspace `tab`
and, for draft actions, `draft_id`.

## Ranking

Builds whose next action is `view_completed_build` and archived Builds have
no task. The rest sort by:

1. Attention first: failed results or jobs, results awaiting verification, an
   unreadable Plan, unfinished Plan choices, a stale draft, a stock shortfall.
2. Earliest due date, then Builds with no due date.
3. Build id.

`rank_reason` says why: "Needs attention now.", "Due 2026-11-10." or "No due
date." The feed reads at most 500 open Builds and answers 500 past that rather
than rank a partial list.

A Build whose stored data fails a check (for example a corrupt Plan history)
has no next action. The feed lists it under `unavailable` with `plan_id`,
`build_name` and `reason`, and still ranks every other Build. This is the
same per-Build rule Build reads use for `context_error`. Running out of the
read budget still fails the whole read. `GET /plans/{id}/workflow` for that
Build answers 500.

## Web contract

The web UI parses the workflow body with `parseBuildWorkflowWorkspace` in
`@print-partner/contracts`. It accepts both the Node body and the Rust body:
the Rust next action (with `target`, the replacement action kinds and null
counts), the Rust stage status (null `task_count` and `active_count`) and the
optional `stock` input. `web/scripts/rust-contracts/client.test.mjs` parses
every Rust `BuildWorkflow` fixture with it.

## Adapter inputs

`WorkflowInputs::inputs(tenant, build)` returns `BuildInputs { due_date, stock }`
after the storage transaction, outside the writer thread.

- **Due date.** Storage has no due-date field yet. Order fields and the
  storage-owned due date belong to slice 7 (GRE-1100, GRE-1095); until then
  the adapter supplies it.
- **Stock.** The Spoolman shortfall computation owns stock. This read does not
  call or depend on it.

The Rust-only startup (`rebuild_api::read_router`) passes `PendingInputs`: no
due dates and `stock: not_ready`. Consumers wait for the integrated shortfall
result rather than a second local stock authority.

## Tests

`cargo nextest run --features desktop_rebuild -E 'test(build_workflow) | test(rebuild_workflow)'`

- `pp-storage/tests/build_workflow.rs` runs on a disposable on-disk copy of
  the Node accepted-Plan fixture: due-date ranking, a confirmed bed and a
  finished Build without attention, manual Checkoff without printer links,
  draft copy that says saved and never accepted, stock shortfall through the
  adapter, authentication before any answer, and a corrupt Build listed as
  unavailable while the feed still ranks the others.
- `pp-core/tests/rebuild_read_http.rs`
  `rebuild_workflow_routes_and_mcp_agree` mounts `read_router` and compares the
  HTTP workflow, the task feed entry and the MCP answer to each other, then
  checks 401 and 404 answers.
