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
   Checkoff links. The same transaction reads the farm settings once and adds
   each Build's beds to clear and suggested unmatched prints (GRE-1166, see
   [Clear beds and match](#clear-beds-and-match)).
2. `WorkflowClient::read` adds the inputs other owners compute
   (`WorkflowInputs`), then resolves each Build with `workflow::resolve`.
3. The task feed is `rank` over those resolved workflows. It never picks a next
   action of its own; each task's `next_action` is the Build workflow's.

## Next action rules

The resolver keeps Node's order (failed results, failed jobs, results awaiting
verification, active jobs, queued jobs, unreadable Plan, no Sources, drafts,
Sources changes, first Plan) with these replacement-product changes:

- **Clear beds before confirm.** Results awaiting verification that still sit
  on their bed are `clear_beds` ("Clear 2 beds"), ahead of `verify_prints`.
  The reason adds any other results still waiting for verification.
- **Match after confirm.** Suggested unmatched prints are `match_prints`
  ("Match finished prints") after `verify_prints` and before active jobs. The
  Checkoff stage shows them as needs attention when nothing above applies.

- **Saved draft, not accepted.** An open draft with changes is
  `review_working_plan`: "N Plan changes are saved in a draft and not used for
  Production yet." Autosave saves the draft; nothing in the copy says it was
  accepted.
- **Manual Checkoff.** A Build with remaining units and no printer Checkoff
  link (dismissed links do not count) gets `check_off_manually` in the
  Checkoff tab when its plates are not started or ready. Plates that are
  preparing, stale or failed keep `prepare_production`.
- **Finished Builds stay quiet.** When every required unit is checked off and
  no draft with changes is open, changed Sources show as complete history, not
  stale work. An autosaved draft with no changes that is still current with
  the Accepted Plan (`Working::Ready`) counts as no draft for the next action,
  the Plan stage and the Sources stage. A zero-change draft that is stale or
  has unresolved choices still asks for a refresh or a decision.
  A confirmed bed (link `verified` or `applied`) is never attention.
- **Stock shortfall** comes only from the adapter. `Stock::Ready{shortfalls>0}`
  makes `resolve_stock_shortfall` the next action ahead of Production;
  `not_ready` never claims stock is fine or short.

`next_action.target` names what to open: `plan_id`, the Build workspace `tab`
and, for draft actions, `draft_id`.

## Ranking

Builds whose next action is `view_completed_build` and archived Builds have
no task. The rest sort by:

1. Attention first: failed results or jobs, beds to clear, results awaiting
   verification, suggested unmatched prints, an unreadable Plan, unfinished
   Plan choices, a stale draft, a stock shortfall.
2. Earliest due date, then Builds with no due date.
3. Build id.

`rank_reason` says why: "Needs attention now.", "Needs attention now. Due
2026-11-10." (attention with a due date), "Due 2026-11-10." or "No due date." The feed reads at most 500 open Builds and answers 500 past that rather
than rank a partial list.

A Build whose stored data fails a check (for example a corrupt Plan history)
has no next action. The feed lists it under `unavailable` with `plan_id`,
`build_name` and `reason`, and still ranks every other Build. This is the
same per-Build rule Build reads use for `context_error`. Running out of the
read budget still fails the whole read. `GET /plans/{id}/workflow` for that
Build answers 500.

## Clear beds and match

GRE-1166 adds the two task kinds GRE-1089's TSK-01 names. Both are facts in
`workflow::Checkoff`, read by `read_model::farm::FarmTasks` from the
tenant's `printer.checkoff_links`, `printer.unattributed_prints`,
`printer.send_queue`, `printer.fleet` and `integrations` settings, parsed the
way the [farm reads](farm-reads.md) parse them. A setting over 4 MiB fails the
whole workflow read: the HTTP routes answer 500 and MCP answers "Build workflow
is unavailable".

- **`beds_to_clear`.** Each result's bed time is when its print came off
  (`completed_at`) or, while it runs, when it started (`created_at`). A claimed
  print's link is created at claim time but keeps the print's `completed_at`,
  so a late claim does not jump ahead. An awaiting-verify result is on its bed
  until another result on the same printer has a later bed time, or an
  unmatched print (open, claimed or dismissed) completed there later.
  Result times compare as RFC 3339 when both parse; otherwise the result
  stored later counts as later. The evidence is only what the settings still
  hold. Node and Rust prune a claimed or dismissed unmatched print 7 days after
  it was claimed or dismissed, and trim old terminal links. If nothing else ran
  on that printer since, an older result that is still awaiting verification
  shows as `clear_beds` again instead of `verify_prints`. Both kinds ask for
  the same confirmation in the same Checkoff tab. At most one result
  per printer is on its bed, so the count is beds, never more than
  `awaiting_verification`. A result without `created_at` is skipped, as the
  farm reads skip it, and stays a `verify_prints` result.
- **`match_suggestions`.** Open unmatched prints whose
  [match suggestion](farm-reads.md#match-suggestions) names the Build as
  `exact` or `likely`. The suggestion is computed by the same code as
  `GET /api/v2/printer-checkoff/unattributed/suggestions`, only when an open
  unmatched print exists. A `none` suggestion, a claimed or a dismissed print
  adds nothing. The task never claims anything; the operator claims or
  dismisses in the Match prints review.

Both actions target the Build's `checkoff` tab and carry the count in
`item_count`. The Node-compatible Build read (`GET /plans/{id}` context)
resolves its `workflow` from facts without these counts, as Node does; the
replacement UI reads `/workflow` and `/tasks`.

## Web contract

The web UI parses the workflow body with `parseBuildWorkflowWorkspace` in
`@print-partner/contracts`. It accepts both the Node body and the Rust body:
the Rust next action (with `target`, the replacement action kinds and null
counts), the Rust stage status (null `task_count` and `active_count`) and the
optional `stock` input. `web/scripts/rust-contracts/client.test.mjs` parses
every Rust `BuildWorkflow` fixture with it.

## Adapter inputs

`WorkflowInputs::inputs(tenant, build)` returns `BuildInputs { stock }` after
the storage transaction, outside the writer thread.

- **Due date.** Storage owns it: the read takes each Build's `due_date` from
  `build_profile_details`, which Accept writes from the draft header
  ([Draft headers](draft-headers.md)), in the same transaction as the Build's
  facts (GRE-1100). A Build without a row there has no due date.
- **Stock.** The Spoolman shortfall computation owns stock. This read does not
  call or depend on it.

The Rust-only startup (`rebuild_api::read_router`) passes `PendingInputs`:
`stock: not_ready`. Consumers wait for the integrated shortfall
result rather than a second local stock authority.

## Tests

`cargo nextest run --features desktop_rebuild -E 'test(build_workflow) | test(rebuild_workflow)'`

- `pp-storage/tests/build_workflow.rs` runs on a disposable on-disk copy of
  the Node accepted-Plan fixture: due-date ranking, a confirmed bed and a
  finished Build without attention, manual Checkoff without printer links,
  draft copy that says saved and never accepted, stock shortfall through the
  adapter, authentication before any answer, and a corrupt Build listed as
  unavailable while the feed still ranks the others.
- GRE-1089's task-feed checks are the `task_feed_*` tests in the same file,
  run with `cargo nextest run --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(task_feed)'`:
  - TSK-01 `task_feed_tasks_name_build_kind_reason_and_target`: every task
    has its Build's name, the workflow's kind, label and reason, and a target
    on its own Build and stage.
  - TSK-02 `task_feed_order_is_the_projection_order`: on a five-Build farm
    (one short of stock, two with due dates, one undated, one archived) the
    feed is `[1, 3, 2, 4]` and equals `rank` over each Build's own workflow
    read.
  - TSK-03 `task_feed_stale_only_with_open_work_and_no_task_when_checked_off`:
    changed Sources are a stale stage and a task while units remain, and
    neither once every unit is checked off.
  - TSK-04 `task_feed_offers_manual_checkoff_without_printer_links`.
  - TSK-05 `task_feed_writes_nothing`: three feed reads leave every table's
    rows and every file under the data directory unchanged (SQLite's `-shm`
    lock index is excluded).
  - TSK-01 kinds clear beds and match (GRE-1166):
    `task_feed_clear_beds_until_the_printer_moves_on` (two beds, then one
    after a later print starts on the Voron, then none after a later unmatched
    print on the Trident, which stays so after that print is dismissed),
    `task_feed_clear_beds_orders_a_late_claim_by_its_print_time` and
    `task_feed_match_prints_for_suggested_unmatched_prints` (a print carrying
    one of the Build's Required-unit tokens; no task without evidence or once
    claimed). `build_workflow_orders_clear_beds_confirm_and_match` pins the
    order against failed results, confirm and active jobs.
- `pp-core/tests/rebuild_read_http.rs`
  `rebuild_workflow_routes_and_mcp_agree` mounts `read_router` and compares the
  HTTP workflow, the task feed entry and the MCP answer to each other, then
  checks 401 and 404 answers.
