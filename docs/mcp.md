# MCP

Print Partner exposes product operations to AI assistants over the Model Context Protocol. This page will describe the MCP server in the Rust core. It is a stub until the API milestone ports MCP from Node.

[MCP setup](assistant-mcp.md) describes how to connect a client to the current Node server.

## Rules the Rust port keeps

- MCP runs over streamable HTTP at `/api/v1/mcp`.
- Every request needs an API key, including requests from the same machine.
- Read tools return current data. Write tools return a proposal and change nothing until the client calls `confirm_apply`.
- Tool names and schemas stay the same, so existing clients keep working.

## Ported tools

- `get_build_workflow` reads the Build workflow with the request's API key, the same projection as `GET /plans/{id}/workflow` and the task feed ([Build workflow](build-workflow.md)). Errors answer `{"error": "plan_id required"}`, `{"error": "Build not found"}` or `{"error": "Build workflow is unavailable"}`. The last covers a storage or authentication failure, a Build that fails its integrity checks, and a body that can't be serialized. Every error sets `isError`.

- `propose_verify_printer_checkoff` keeps Node's schema and propose step. `confirm_apply` applies it as a one-bed Checkoff burst under the idempotency key `mcp:<action_id>` and answers with the burst receipt, which includes `undo_token`. Confirming an action id this session no longer holds, for example after a restart, answers from the stored burst with `reconciled: true` and does not apply it again. See [Checkoff bursts](records/d19-publication-client.md#checkoff-bursts-gre-1090).
- `propose_apply_plan_draft` `{ plan_id, draft_id }` and its `confirm_apply` accept a Working Plan through the same Accept preview and Accept as the Plan tab ([Draft headers](draft-headers.md#accept)). Propose checks the hydrated Build planning brief, reads the preview, writes nothing, and proposes the previewed revision. Confirm checks the brief again and accepts exactly that revision with the action id as the idempotency key; a header saved in between is refused with `draft_changed`. A draft with no Required-unit selection is proposed with `revision: null`, and confirm selects the automatic reconciliation before it previews and accepts. Propose errors: `plan_id and draft_id required`, `Build planning brief not found`, `Selected Working Plan not found`, `Working Plan cannot apply source choices: …`, `Working Plan acceptance requires required-unit decisions`, `Working Plan cannot be accepted` (Accept would refuse, or the preview lists running work or production), `Build not found` and `Working Plan preview is unavailable`. Confirm answers `{ "ok": false, "detail": … }`, including `Working Plan acceptance preparation failed: <kind>`, and keeps the action pending.
- `list_plans`, `get_plan_decisions` `{ plan_id, limit? }` and `list_plan_snapshots` `{ plan_id }` read storage with the request's API key and answer Node's text (GRE-441). `list_plans` returns `{"plans": [{id, name, part_count, build_stale}]}` for the key's tenant, by name. `get_plan_decisions` returns the latest `limit` decisions, oldest first (default 40, at least 1), and `list_plan_snapshots` every snapshot summary, oldest first. `plan_id` and `limit` are read like Node's `asInt`: a number or numeric string, truncated. A `plan_id` that names no Build of the key's tenant answers `{"error": "plan_id required"}`, as Node does without `PRINT_PARTNER_MCP_PLAN_ID`. A storage failure answers `{"error": "Plans are unavailable"}`, `Plan decisions are unavailable` or `Plan snapshots are unavailable`. Like `GET /plans`, `list_plans` refuses a tenant with more than 64 Builds, which Node lists.
- `get_remaining` `{ plan_id }` and `get_printer_checkoff` `{ plan_id?, state?, integration_id? }` read storage with the request's API key and answer Node's text (GRE-1297). `get_remaining` summarizes the Build's Accepted Plan progress: printed, total and remaining units, the percent printed, whether it can be archived and the included Part count. It answers `Plan not found` for a number that names no Build of the key's tenant, `plan_id required` when `plan_id` is not a number, and Node's errors for an Accepted Plan that needs compatibility repair, is not initialized or is inconsistent. `get_printer_checkoff` lists the tenant's printer Checkoff links and undismissed unattributed prints, filtered by Build, link state and link integration. Each row's `host_name` is the fleet printer's name when its `printer_id`, or else its only printer on that integration, is in the fleet. `pending_verification` counts the listed links awaiting verification. An unknown `state` answers `invalid checkoff state`, and a storage failure answers `Printer Checkoff is unavailable`.

- `add_addon` `{ plan_id, source_name }` proposes a synced Source attachment, with compatibility warnings and suggested exclusions. Confirmation attaches the Source and merges the confirmed exclusions. `propose_update_build_brief` proposes requirements, Source contributions and compatibility findings; confirmation validates them and clears the selected draft when contributions were supplied. Both use the request's API key for every storage operation. A failed confirmation keeps the proposal pending. If the writer may have committed but its reply was lost, confirmation answers `{ "ok": false, "outcome_unknown": true, … }` and drops the proposal, so a retry cannot apply it twice; check the Build before proposing again. Neither publishes or accepts a Plan.

Rust ignores optional addon documents over 1 MiB. An unparseable data-dir `merge_conflicts.yaml` falls back to the shipped copy. Confirmation writes its decision log in the same transaction as the change. Storage errors answer `Build planning is unavailable`. Rust also refuses confirmation when merging exclusions into a malformed or over-budget stored kit, and when the Source is already attached. Confirmation also refuses when the proposed canonical Source name no longer exists, instead of selecting a similar name. These are Rust-only refusals.

These tools are implemented in the Rust API behind both rebuild gates. Gateway entry-point proof waits on GRE-1333 and is tracked in [GRE-1386](https://linear.app/greetz/issue/GRE-1386/prove-addon-and-build-brief-mcp-tools-through-the-rust-only-gateway). The default Node-backed runtime keeps its existing tools until the whole-runtime cutover in [ADR 0010](adr/0010-whole-runtime-cutover.md).

### Draft Part edits

`propose_edit_plan_draft_parts` `{ plan_id, draft_id, parts: [{ part_id, included?, quantity_override? }], expected_snapshot_digest? }` changes which Parts an open Working Plan includes, and their quantities (GRE-1298).

- Propose reads the draft with the request's API key and writes nothing. The pending action's `params` hold `plan_id`, `draft_id`, the edits, and `expected_snapshot_digest`: the given digest, or else the draft's current one. Propose needs no Build planning brief; Node refuses without one, but the edit never reads it.
- `confirm_apply` runs the Plan tab's Part edit (`PATCH /plans/{id}/drafts/{draft_id}/parts`, `WorkingDraftClient::edit`) against that digest. It answers `{ "ok": true, "result": { "draft_id", "snapshot_digest", "parts": [{ "part_id", "included", "quantity_override", "quantity_effective" }] } }` with the new digest and the edited Parts. It then adds an `applied_action` entry to the Build's decision log. The edit never accepts or publishes a Plan.
- `quantity_override: null` clears an override.
- A refusal is `isError` with a `code` and a message for people. Clients should read the `code`; the message may change. Propose answers `{ "error", "code" }` and stores nothing: `arguments_required`, `not_found` (no such Build or draft for the key), `not_open`, `parts_required`, `part_not_found`, `edit_empty` (an edit sets neither field), `invalid_edit` (wrong field types, a negative or fractional quantity, or a Part listed twice) and `invalid_digest`. Confirm answers `{ "ok": false, "code", "detail" }` and keeps the action pending: `draft_changed`, `base_changed`, `not_open`, `not_found`, `accepted_baseline_required`, `edit_refused` and `unavailable`. `outcome_unknown` means the edit may have committed; that action is dropped, so check the draft before proposing again.

Every other product tool still answers that it has no Rust port. `get_farm_status` and `get_print_stats` have no Rust port yet.

## To be written

- How to create and rotate the API key in the desktop app and the Docker image.
- How MCP access works when network sharing is on.
- The tool list and its schemas.
