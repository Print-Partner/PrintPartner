# MCP

Print Partner exposes product operations to AI assistants over the Model Context Protocol. This page describes the ported Rust tools.

[MCP setup](assistant-mcp.md) describes how to connect a client.

## Rules the Rust port keeps

- MCP runs over streamable HTTP at `/api/v1/mcp`.
- Every request needs an API key, including requests from the same machine.
- Read tools return current data. Write tools stage a proposal; they do not change the Build, Plan, or Checkoff until the client calls `confirm_apply`.
- Existing tool names and required inputs stay the same. Rust may add optional inputs.

## Decide proposals

In Rust, `list_pending_actions` returns the installation's open proposals in any authenticated MCP session, including after reconnect or restart. Each row includes `staged_by` (the MCP client name), `created_at`, `expires_at`, `depends_on`, and `state`. The list returns the newest 200 open proposals, oldest first, and reports `omitted` if more exist.

`confirm_apply` takes one proposal by `action_id` and runs its existing Rust operation. It refuses an expired proposal or one with an unapplied dependency; `blocked_by` identifies the dependency to resolve first. A refusal that wrote nothing leaves the proposal pending. An uncertain outcome stays visible as `unknown` and cannot be applied again. Check the affected work before proposing again.

`dismiss_proposed_action` removes an open proposal from the list without running its operation. It cannot dismiss a proposal while it is applying. A second decision normally returns `already_applied` or `already_dismissed`; Checkoff can return its stored receipt instead, without writing again.

## Ported tools

### Plan history

These five reads use the request's API key. An ID owned by another tenant is treated as missing. They do not create a Draft or accept a Plan.

| Tool | Reads |
| --- | --- |
| `get_plan_option_groups` `{ plan_id }` | Current kit selections and observed variants with their Source provenance. |
| `get_plan_snapshot` `{ plan_id, revision_id? }` | Current Build configuration, Accepted revision history, and the open Draft's ID, base and stale flag. With a revision ID, returns only that immutable revision's basis and Part count. |
| `get_plan_review` `{ plan_id }` | Accepted revision basis, issue counts, blockers, role and filament totals, and Source layers. It checks local artifacts but does not query live spool inventory. |
| `search_plan_parts` `{ plan_id, query, limit?, revision_id? }` | Case-insensitive filename and relative path matches in the current Accepted revision or a named revision. Each hit includes its Part key, Part ID, revision Part ID and Required-unit tokens. |
| `compare_plans` `{ plan_a_id, plan_b_id }` | Both Builds' revision bases, base Sources, addons, refs, kit selection changes, recent decisions and Parts unique to either Plan. |

An empty Build has no Accepted revision (`basis.kind: "empty"`). A stale Build can still report its Accepted revision; `build_stale` means its current configuration no longer matches that revision. A missing Plan answers `plan_not_found`, a missing or unrelated revision answers `not_found`, and an uninitialized or compatibility-dirty head or changing option-group observation answers `stale_plan`. These refusals set `isError` and carry an `error` message and a machine-readable `code`.

### Other reads and proposals

- `get_build_workflow` reads the Build workflow with the request's API key, the same projection as `GET /plans/{id}/workflow` and the task feed ([Build workflow](build-workflow.md)). Errors answer `{"error": "plan_id required"}`, `{"error": "Build not found"}` or `{"error": "Build workflow is unavailable"}`. The last covers a storage or authentication failure, a Build that fails its integrity checks, and a body that can't be serialized. Every error sets `isError`.

- `propose_verify_printer_checkoff` keeps Node's schema and propose step. `confirm_apply` applies it as a one-bed Checkoff burst under the idempotency key `mcp:<action_id>` and answers with the burst receipt, which includes `undo_token`. Confirming an already applied action answers from the stored burst with `reconciled: true` and does not apply it again. See [Checkoff bursts](records/d19-publication-client.md#checkoff-bursts-gre-1090).
- `propose_apply_plan_draft` `{ plan_id, draft_id }` and its `confirm_apply` accept a Draft through the same Accept preview and Accept as the Plan tab ([Draft headers](draft-headers.md#accept)). Propose checks the hydrated Build planning brief and stages the previewed revision. Confirm checks the brief again and accepts exactly that revision with the action id as the idempotency key; a header saved in between is refused with `draft_changed`. A Draft with no Required-unit selection is proposed with `revision: null`. Confirm selects the automatic reconciliation before it previews and accepts, so a failure after that selection might already have changed the Draft. An unverified result is reported as `outcome_unknown`, not as a retryable refusal.
- `list_plans`, `get_plan_decisions` `{ plan_id, limit? }` and `list_plan_snapshots` `{ plan_id }` read storage with the request's API key and answer Node's text (GRE-441). `list_plans` returns `{"plans": [{id, name, part_count, build_stale}]}` for the key's tenant, by name. `get_plan_decisions` returns the latest `limit` decisions, oldest first (default 40, at least 1), and `list_plan_snapshots` every snapshot summary, oldest first. `plan_id` and `limit` are read like Node's `asInt`: a number or numeric string, truncated. A `plan_id` that names no Build of the key's tenant answers `{"error": "plan_id required"}`, as Node does without `PRINT_PARTNER_MCP_PLAN_ID`. A storage failure answers `{"error": "Plans are unavailable"}`, `Plan decisions are unavailable` or `Plan snapshots are unavailable`. Like `GET /plans`, `list_plans` refuses a tenant with more than 64 Builds, which Node lists.
- `get_remaining` `{ plan_id }` and `get_printer_checkoff` `{ plan_id?, state?, integration_id? }` read storage with the request's API key and answer Node's text (GRE-1297). `get_remaining` summarizes the Build's Accepted Plan progress: printed, total and remaining units, the percent printed, whether it can be archived and the included Part count. It answers `Plan not found` for a number that names no Build of the key's tenant, `plan_id required` when `plan_id` is not a number, and Node's errors for an Accepted Plan that needs compatibility repair, is not initialized or is inconsistent. `get_printer_checkoff` lists the tenant's printer Checkoff links and undismissed unattributed prints, filtered by Build, link state and link integration. Each row's `host_name` is the fleet printer's name when its `printer_id`, or else its only printer on that integration, is in the fleet. `pending_verification` counts the listed links awaiting verification. An unknown `state` answers `invalid checkoff state`, and a storage failure answers `Printer Checkoff is unavailable`.

- `add_addon` `{ plan_id, source_name }` proposes a synced Source attachment, with compatibility warnings and suggested exclusions. Confirmation attaches the Source and merges the confirmed exclusions. The proposal returns the Source's stored name, and confirmation matches that name exactly, surrounding whitespace included; if no Source has that name any more, confirmation refuses rather than pick a similar one. `propose_update_build_brief` proposes requirements, Source contributions and compatibility findings; confirmation validates them and clears the selected draft when contributions were supplied. Both use the request's API key for every storage operation. A refusal that wrote nothing keeps the proposal pending. If the writer may have committed but its reply was lost, the proposal stays visible as `unknown` and cannot be confirmed again. Neither publishes or accepts a Plan.

Rust ignores optional addon documents over 1 MiB. An unparseable data-dir `merge_conflicts.yaml` falls back to the shipped copy. Confirmation writes its decision log in the same transaction as the change. Storage errors answer `Build planning is unavailable`. Rust also refuses confirmation when merging exclusions into a malformed or over-budget stored kit, and when the Source is already attached. Confirmation also refuses when the proposed canonical Source name no longer exists, instead of selecting a similar name. These are Rust-only refusals.

### `propose_create_build`

`propose_create_build` `{ name, request, urls?, idempotency_key? }` creates a Build together with its Build planning brief (GRE-1441). Each URL in `urls` becomes one evidence item. Rust classifies it the way Node's `classifyBuildUrl` does, and does not guess a role from the request text. No Source is created. The shared catalog's tool description still says the request is verbatim and normalized. Rust trims surrounding whitespace from `request` and stores no separate normalized form. That catalog is exported from Node's tool description (`npm run mcp:catalog -w @print-partner/server`), so regenerating it copies the same sentence until Node's description changes.

- **Propose** trims `name` and `idempotency_key` and removes surrounding whitespace from `request`, the way `PATCH /plans/{id}` stores a special request. It refuses a blank `name` or `request`, a `urls` value that is not a list of http or https URLs, a key that is not a string of at most 200 characters, a name another Build has, and the cases confirm refuses below. It stages the proposal without changing the Build.
- **Confirm** runs one transaction: it creates the Build, sets its special request, stores a new brief and records the key. The brief holds the request and those evidence items, and otherwise empty lists, in phase `preparing`. A GitHub URL, or a path ending `.git`, is a git repository (`kind` `mod`, `source_role` `addon`, `sync_status` `pending`). A Printables, MakerWorld, Thingiverse, Thangs, Cults3D or MyMiniFactory URL is a model page with `upload_required` true. Any other http or https URL is informational evidence. The stored `url` is the text that was sent. The evidence id is the first 20 hex digits of the SHA-256 of the normalized URL. A later entry with the same normalized URL is dropped. Rust does not guess requirements from the request; record them with `propose_update_build_brief`. The Build has no layers, no Working Plan and no Accepted Plan. It answers `{ "ok": true, "result": { "plan_id", "name", "idempotent_replay" } }`.
- **Idempotency.** `build_planning.create.<key>` stores the Build id. Confirming another proposal with the same key answers that Build with `idempotent_replay: true` without changing it, while it exists. If the Build was deleted, confirm creates a new one and moves the key.
- **Orders Module.** Propose and confirm both refuse while the Orders Module is off, as `PATCH /plans/{id}` refuses a special request. This is a provisional default until the owner decides.

A refused propose answers `{ "error": …, "code": … }` with `isError`. A refused confirm answers `{ "ok": false, "code": …, "detail": … }` and keeps the proposal pending. `detail` and `error` are plain messages; match on `code`.

| Code | When |
| --- | --- |
| `blank_name` | `name` is missing, not a string, or blank. |
| `name_too_long` | `name` is over 4,096 characters. |
| `blank_request` | `request` is missing, not a string, or blank. |
| `invalid_url` | `urls` is not a list, or an entry is not an http or https URL. |
| `invalid_idempotency_key` | `idempotency_key` is not a string, or is over 200 characters after trimming. |
| `name_taken` | Another Build has the name. |
| `orders_module_off` | The Orders Module is off. |
| `module_settings_invalid` | The stored Module settings don't parse, so the Orders state is unknown. |
| `idempotency_record_invalid` | The stored record for the key isn't a Build id. Use another key. |
| `unauthorized` | Storage refused the API key. |
| `unavailable` | Storage is busy, stopping or its queue is full. Nothing was written; try again. |
| `outcome_unknown` | The write may have committed. Confirm marks the proposal `unknown` and adds `outcome_unknown: true`; check `list_plans` before proposing again. The write seam does not produce it for this tool today. |
| `internal` | A storage failure. Nothing was written, and the cause is not returned. |

The last four codes are shared by MCP write tools that use the storage write seam (`pp-api/src/mcp/write_result.rs`).

### `propose_set_build_source_roles`

`propose_set_build_source_roles` `{ plan_id, roles: [{ evidence_id, role }] }` sets each named evidence item's role for this Build in its planning brief (GRE-1300 PR 1). `role` is `structural_base`, `overlay`, `addon` or `evidence`. A Source's Library role and the Build's layers do not change.

- **Propose** checks that `plan_id` is a JSON whole number from 1 up (Rust does not read numeric strings) and that `roles` is a list, then finds the Build's planning brief with the request's API key. It stages the proposal without changing the Build.
- **Confirm** trims each `evidence_id` and `role`, then runs one transaction: it sets the roles, checks that at most one evidence item is the structural base, clears the brief's selected draft and stores the brief. A later item for the same `evidence_id` wins. It answers `{ "ok": true, "result": { "updated", "evidence": [{ "id", "source_id", "source_role" }] } }`, listing the changed items in brief order. The planning phase returns to `preparing`.

A refused propose or confirm answers as for `propose_create_build`. Confirm refusals write nothing and keep the proposal pending.

| Code | When |
| --- | --- |
| `invalid_plan_id` | `plan_id` is missing or not a positive whole number. |
| `invalid_roles` | `roles` is missing or not a list. |
| `no_planning_brief` | The Build has no planning brief. `propose_create_build` creates one. |
| `planning_brief_invalid` | The stored brief does not parse. |
| `invalid_role_assignment` | A `roles` item is not an object. |
| `invalid_source_role` | A `role` is not one of the four. |
| `evidence_not_found` | An `evidence_id` names no evidence in the brief. The refusal names it in `evidence_id`. |
| `multiple_structural_bases` | More than one evidence item would be the structural base, counting roles already in the brief. |

It also answers the four shared codes above.

### `propose_assign_role_filament`

`propose_assign_role_filament` `{ plan_id, assignment }` saves one role's filament default, records it on the Build's planning brief, and gives the Build's included accepted Parts of that role the same filament (GRE-1435, GRE-1898). `assignment` is `{ role, inventory_kind, inventory_id?, requested_name?, requested_brand?, color_hex, substitution_confirmed?, spoolman_spool_id? }`. `inventory_kind` is `catalog`, `spoolman`, `custom` or `substitute`. The call does not accept a Plan or add an HTTP route.

- **Propose** checks that `plan_id` is a JSON whole number from 1 up and that the assignment has a non-blank `role`, one of the four kinds, and a `#` plus six hex digits. `custom` and `substitute` require `substitution_confirmed: true`. A non-blank `spoolman_spool_id` must be `spoolman:{integration}:spool:{n}` with `n` from 1 up, on a `spoolman` assignment whose `inventory_id` names the same integration. A `spoolman` assignment also requires Spoolman on for the Build: the user's setting under the Build's Plan override (`feature_toggles:plan:<plan_id>`), as Checkoff's deduction reads it. It then finds the Build's planning brief with the request's API key. It stages the proposal without making a network call.
- **Confirm** checks the assignment again. A `catalog` assignment must name a built-in catalog color, and `color_hex` must match that color. A non-blank `requested_name` must match the color's display name, and a non-blank `requested_brand` its product line, ignoring case. A `spoolman` assignment is checked for Spoolman on for the Build, then against one inventory read of its integration; the write transaction checks Spoolman for the Build again. `inventory_id` must be `spoolman:{integration}:filament:{n}`, Spoolman must hold filament `n`, and `color_hex` must match its color, ignoring case. A pinned spool must be an unarchived spool of that filament. Spoolman's labels join vendor, material and name, so `requested_name` and `requested_brand` are not compared for a Spoolman item. Confirm then runs one transaction: it replaces every `role_filaments` entry whose role matches, ignoring case and surrounding spaces, marks an existing `color_<role>` requirement satisfied (the role lowercased, with each run of non-alphanumeric characters becoming `_`), clears the selected draft, merges `role_filaments_<plan_id>`, and writes the same three filament columns on the included accepted Parts whose role matches. A second confirm for the same role replaces the first. The default is keyed by the role trimmed and lowercased (`primary` when blank), the key draft preparation reads, and any other key for the same role is removed. Node's assistant kept the role as typed, so a capitalized role never reached new Parts. The default and the Parts keep `filament_color_id` for a catalog or Spoolman item, `filament_custom_hex` for a custom or substitute color, and `spoolman_spool_id` for a pinned Spoolman spool, else null. An accepted Part's filament changes without a new revision, and Accept publishes the live value. It answers `{ "ok": true, "result": { "role", "updated_parts" } }`.

A refused propose or confirm answers as for `propose_create_build`. Confirm refusals write nothing and keep the proposal pending.

| Code | When |
| --- | --- |
| `invalid_plan_id` | `plan_id` is missing or not a positive whole number. |
| `invalid_filament_assignment` | `assignment` is missing or not an object with a role, a known kind, and a `#rrggbb` color. |
| `substitution_unconfirmed` | `custom` or `substitute` without `substitution_confirmed: true`. |
| `catalog_filament_not_found` | The catalog id names no built-in color. |
| `catalog_color_mismatch` | `color_hex` is not that color's hex. |
| `catalog_name_mismatch` | `requested_name` is not that color's display name. |
| `catalog_brand_mismatch` | `requested_brand` is not that color's product line. |
| `spoolman_module_off` | Spoolman is off for the Build, by the user's setting or the Build's override. |
| `spoolman_unavailable` | The integration the item names is missing or disabled, or its inventory cannot be read, so the item cannot be checked. |
| `spoolman_filament_not_found` | `inventory_id` is not a Spoolman filament id, or Spoolman holds no such filament. |
| `spoolman_color_mismatch` | `color_hex` is not that Spoolman filament's color. |
| `spoolman_spool_not_found` | The pinned spool is not an unarchived spool of that filament. |
| `module_settings_invalid` | A `spoolman` assignment, and the user's or the Build's stored feature settings don't parse, so the Spoolman state is unknown. |
| `no_planning_brief` | The Build has no planning brief. |
| `planning_brief_invalid` | The stored brief does not parse. |

It also answers the four shared codes above.

### `propose_import_build_inputs`

`propose_import_build_inputs` `{ plan_id, inputs: [{ source_id or url, kind?, title?, extract?, filenames?, derived_from_evidence_id?, branch? }] }` attaches evidence to the Build's planning brief: existing Library Sources by `source_id` (GRE-1300 part 2, GRE-1521), model pages and other links by `url` (part 3a, GRE-1526), and git repositories by `url` (part 3b, GRE-1533). Only a git repository the Library does not hold creates a Source. No Source is changed, and the Build's layers, drafts and Accepted Plan stay as they are.

- **Propose** checks `plan_id` and every input, then, with the request's API key, finds the brief, each Source, each `derived_from_evidence_id` and the Library Source for each git repository. It stages the proposal without making a network call.
- **Confirm** runs the same checks and then one transaction: each new git Source is created, one evidence item per input goes in the brief, the selected draft is cleared, and the brief is stored. The inputs of one confirm are attached together or not at all. Confirm makes no network call either: it does not fetch or sync a repository. Sync is a separate step (GRE-429). Each item's `id` is the first 20 hex digits of the SHA-256 of its normalized URL, as Node makes it. Importing an input the brief already holds, by that URL, updates its one item in place and keeps its `source_role`; Node drops the role there.
  - **A Source** gets the URL `printpartner:source:<id>`. `kind` defaults to `model_source`, and `title` to the Source's name. `input_kind` is `upload`, `sync_status` is `pending`, and a new item has no `source_role` until `propose_set_build_source_roles` gives it one. `filenames` are stored trimmed, without blanks or repeats. Rust does not scan the Source's folder for artifacts as Node does; `propose_rebuild_plan` reads the Source's files.
  - **A URL** is classified as Node's `classifyBuildUrl` does (`pp_services::build_url`). It must be http or https. The normalized URL is the trimmed URL without its fragment and without trailing slashes on its path; `url` keeps the value as sent.
    - A page on Printables, MakerWorld, Thingiverse, Thangs, Cults3D or MyMiniFactory, or one of their subdomains, is a model page: `kind` defaults to `model_source`, `input_kind` is `model_page`, and `upload_required` is `true`. Upload its files into a Source, then attach that Source by `source_id`.
    - A git repository is a `github.com` URL or one whose path ends `.git`. `kind` defaults to `mod`, `input_kind` is `url`, `source_role` is `addon` as Node stores it, and `sync_status` is `pending`. The item's `source_id` is a Library Source for that repository:
      - **Reuse.** A Library Source whose URL names the same repository is attached as it is; its branch is not changed. Two URLs name the same repository when they match after the clean-up `pp_services::build_url::repo_key` does: the normalized URL without `.git` or trailing slashes, and on `github.com` only `/owner/repo`, lowercased. So `https://github.com/Owner/Repo/` and `https://github.com/owner/repo.git` reuse one Source.
      - **Create.** Otherwise a git Source is created as `POST /sources` creates one: its URL is the normalized URL, through the same GitHub location rules, its branch is `branch`, or `main` when the input names none, and its kind is `github` or `git`. Rust does not ask the remote for its default branch (GRE-429). The name is the URL's last two path segments joined by `-`, without `.git`, as Node names it (on GitHub, the owner and repository), with the start of the evidence id and then a number added if the name is taken. Its id is in `created_source_ids`.
      - Importing the same repository again, by any URL that names it, updates its one git item in place and keeps the item's `id`, its `normalized_url` and any `source_role` the Build gave it.
    - Any other URL is a link: `kind` defaults to `informational_evidence` and `input_kind` is `url`.
    - `title` is stored when given. `derived_from_evidence_id` is checked but, as in Node, not stored on a URL's item.
  - **`extract`**, on any input, is stored trimmed with its SHA-256 as `content_hash` and the time as `retrieved_at`.
- It answers `{ "ok": true, "result": { "evidence": [{ "id", "kind", "source_id", "url", "upload_required" }], "created_source_ids": [...] } }`, one item per input in input order. `source_id` is `null` for a model page or link. `created_source_ids` lists each git Source this confirm created, once.

A refused propose or confirm answers as for `propose_create_build`. Confirm refusals write nothing and keep the proposal pending.

| Code | When |
| --- | --- |
| `invalid_plan_id` | `plan_id` is missing or not a positive whole number. |
| `invalid_inputs` | `inputs` is missing, not a list, or empty. |
| `invalid_input` | An input is not an object, has both or neither of `source_id` and `url`, or its `source_id` is not a positive whole number. |
| `invalid_url` | A `url` is not an http or https URL, or it is a git repository that `POST /sources` would refuse, such as a GitHub URL whose owner or repository is not a valid name. |
| `invalid_evidence_kind` | A `kind` is not `canonical_design`, `vendor_overlay`, `mod`, `component`, `model_source` or `informational_evidence`. |
| `no_planning_brief` | The Build has no planning brief. `propose_create_build` creates one. |
| `planning_brief_invalid` | The stored brief does not parse. |
| `source_not_found` | A `source_id` names no Library Source. A refused confirm names it in `source_id`. |
| `derived_evidence_not_found` | A `derived_from_evidence_id` names no evidence already in the brief. A refused confirm names it in `evidence_id`. |

It also answers the four shared codes above.

### `get_build_planning_state` and `get_plan_draft`

These two reads give an assistant the ids the Build planning write tools take: evidence and contribution ids for `propose_update_build_brief` and `propose_set_build_source_roles`, and the draft id, snapshot digest and Part ids for `propose_edit_plan_draft_parts` (GRE-1442). Both read with the request's API key through the same storage reads as `GET /plans/{id}/build-planning` and `GET /plans/{id}/drafts/{draft_id}`. Like those routes, they run on the storage writer queue today. Neither writes. `get_plan_draft` reads the brief and the draft in two transactions, so a write between them can pair a phase with a newer draft.

- `get_build_planning_state` `{ plan_id }` returns:
  - `brief`: the hydrated Build planning brief, as `GET /plans/{id}/build-planning` serves it, without the legacy `draft_review_blockers`.
  - `planning_phase`, `grouped_difference_count` and `difference_count`, as that route serves them.
  - `next_required_decisions`: what the owner still has to decide, in this order. Each item is `{ "kind": "upload", "evidence_id" }` for a model page whose files still need an upload, `{ "kind": "contribution", "contribution_id" }` for a Source contribution still proposed, or `{ "kind": "difference_group", "group_id" }` for a difference group with no resolution. Node answers prefixed strings such as `upload:<id>`; Rust answers objects.
- `get_plan_draft` `{ plan_id }` returns `{ "draft", "planning_phase"? }`. When the Build has a brief, `draft` is the draft the brief selects. It is `null` when the brief selects none, including a selection that hydration cleared because its Sources or resolutions moved (`planning_phase` is then `preparing`), and when the selected draft is gone (`missing_draft`). When the Build has no brief, `draft` is the Build's open draft (the last created, if there are several), or `null` if there is none. `planning_phase` is present only when the Build has a brief. The draft is the identity from `GET /plans/{id}/drafts/{draft_id}`, `{ draft_id, state, lifecycle_version, snapshot_digest, base }`, with that route's `parts`. Each Part carries `draft_part_id`, `relative_path`, `filename`, `included`, `quantity_effective`, `quantity_override` and its other fields. Node refuses a Build without a brief; Rust also serves a draft made in the Plan tab.

`plan_id` must be a JSON whole number from 1 up; Rust does not read numeric strings. Refusals answer `{ "error", "code" }` with `isError`:

| Code | When |
| --- | --- |
| `invalid_plan_id` | `plan_id` is missing or not a positive whole number. |
| `plan_not_found` | No Build of the key's installation has this id. |
| `no_planning_brief` | `get_build_planning_state` only: the Build has no brief. |
| `unauthorized`, `unavailable`, `internal` | Storage refused the key, was busy, or failed; the same codes as `propose_create_build`. A stored brief that is not valid answers `internal`. |

### `get_farm_status`

`get_farm_status` `{}` reports each configured printer from the farm projection `GET /api/v2/printers` serves ([Farm reads](farm-reads.md)), read with the request's API key, and from what the printer watchers last observed (GRE-1352). It sends no request to a printer: Node asked every printer host on each call, while Rust reports what its watchers know. Like that route, the read runs on the storage writer queue. It writes nothing.

It answers `{ printer_count, printers, state_counts, needs_filament_swap }`.

- Each printer has `id`, `name`, `state`, `not_connected_reason`, `integration_id`, `active_job`, `progress`, `eta_seconds`, `message`, `last_heard_at`, `idle_since`, `idle_for`, `filament_slots`, `filament_remaining_g`, `needs_filament_swap` and `empty_slots`.
- `state` is the watcher's answer: `idle`, `printing`, `paused`, `complete`, `error` or `offline`. It is `not_connected` when no watcher answered, with a `not_connected_reason`: `not_set_up`, `integration_missing`, `monitoring_disabled`, `monitoring_not_supported`, `awaiting_status`, or `host_state_unknown` when the host answered without a known state. It maps one to one onto the route's execution state, except that the route reads `complete` as idle.
- `active_job` is the file the printer reports and `progress` its whole percent, both only while printing or paused.
- `idle_since` is when the watcher saw the printer change into `idle`. It is `null` when this process did not see the change: after a restart, or when the printer missed polls just before it (an outage hides when the change happened). `idle_for` is then `unknown`; otherwise it reads `12m`, `2h 5m` or `3d 4h`. Both are `null` unless the printer is idle.
- `filament_slots` lists each slot with `slot`, `filament_color_id`, `label`, `empty` and `remaining_g`. Filament grams need Spoolman stock (GRE-1102), so `remaining_g` and `filament_remaining_g` are `null`, and a printer needs a filament swap only for an empty slot, listed in `empty_slots`. The watchers hold no ETA or message, so `eta_seconds` and `message` are `null`.
- `state_counts` counts each state, and `needs_filament_swap` lists `{ id, name, empty_slots }` for each printer with an empty slot.

The farm tools, `get_farm_status` and `get_print_stats`, are listed and callable only while the caller's Farm Module is on. Otherwise `tools/list` leaves them out, and calling one answers `{ "error", "code" }` with `isError`: `farm_module_off`, `module_settings_invalid` when the stored Module settings don't parse, or the shared storage codes `unavailable` and `internal`.

### `get_print_stats`

`get_print_stats` `{ hours? }` reports what was printed in the last `hours` (GRE-1352), read with the request's API key. `hours` defaults to 8 and must be a JSON number above 0 and at most 2160 (90 days); anything else, including a numeric string, answers `invalid_hours`. It writes nothing.

It answers:

- `window_hours` and `since`, the window's start in RFC 3339.
- `plates_sent`, `plates_completed`, `plates_failed`, `completion_rate` and `filament_consumed_g`, counted from every Checkoff burst job (`print_jobs`) in the window, and `by_printer` with the same counts per printer, its `printer_id` and `printer_name`. `completion_rate` is completed over completed plus failed, to three places, or `null` when none finished. Node counted only the newest 100 jobs; Rust counts them all, in one grouped read on the reader pool.
- `prints_seen_on_printers` `{ total, attached_to_plan, not_attached_to_plan, by_printer }`: prints the printers reported in the window. A Checkoff result with a reported outcome is attached to a Plan; an open unmatched print is not. Claimed and dismissed unmatched prints are not open and are not counted.
- `empty_window`: `true` when no plate was sent and no print was seen.
- `active_plans`: `{ "kind": "listed", "plans" }` with every Build that is not archived, by name, with `plan_id`, `plan_name`, `part_count` and `accepted_progress`. When the Build list cannot be read, it is `{ "kind": "unavailable", "reason" }`, with `too_many_builds` (more than 64 Builds, archived ones included) or `read_failed`, and the plate and print counts are still answered. A refused key refuses the whole call. Progress is `{ "kind": "ready", total_units, remaining_units, printed_units }`, the numbers `get_remaining` reports, `{ "kind": "empty" }` with no Accepted Plan, or `{ "kind": "unavailable", reason }`.

Like `get_farm_status`, it is listed and callable only while the Farm Module is on. Storage failures answer the shared codes `unauthorized`, `unavailable` and `internal`.

These tools are implemented in the Rust API behind both rebuild gates. Gateway entry-point proof waits on GRE-1333 and is tracked in [GRE-1386](https://linear.app/greetz/issue/GRE-1386/prove-addon-and-build-brief-mcp-tools-through-the-rust-only-gateway); for `propose_create_build` it waits on GRE-1353 and closes GRE-1441, and for `propose_assign_role_filament` it waits on GRE-1353 and closes GRE-1435. `propose_set_build_source_roles` and `propose_import_build_inputs` are proven through a running Rust-only `pp-server` by [`mcp_build_inputs.rs`](../rust/crates/pp-server/tests/mcp_build_inputs.rs) (GRE-1300), with an API key and no cookie: both tools propose and confirm on `/api/v1/mcp` and on `/mcp`. The default Node-backed runtime keeps its existing tools until the whole-runtime cutover in [ADR 0010](adr/0010-whole-runtime-cutover.md).

### `propose_edit_plan_draft_parts`

`propose_edit_plan_draft_parts` `{ plan_id, draft_id, parts: [{ part_id, included?, quantity_override? }], expected_snapshot_digest? }` changes which Parts an open Working Plan includes, and their quantities (GRE-1298).

- **Propose** reads the draft as the Plan tab does, with the request's API key, and stages the proposal without changing the Draft. The pending action's `params` hold `plan_id`, `draft_id`, the edits and `expected_snapshot_digest`: the given digest, or else the draft's current one. Propose needs no Build planning brief. Node refuses without one, but the edit never reads it.
- **Confirm** runs the Plan tab's Part edit (`PATCH /plans/{id}/drafts/{draft_id}/parts`, `WorkingDraftClient::edit`) against that digest. It answers `{ "ok": true, "result": { "draft_id", "snapshot_digest", "parts": [{ "part_id", "included", "quantity_override", "quantity_effective" }] } }` with the new digest and the edited Parts, then adds an `applied_action` entry to the Build's decision log. `quantity_override: null` clears an override. The edit never accepts or publishes a Plan.

Refusals have the shape described for `propose_create_build`, and confirm keeps the proposal pending.

| Code | When |
| --- | --- |
| `arguments_required` | `plan_id` or `draft_id` is not a positive whole number. |
| `not_found` | The key's account has no such Build or draft. |
| `not_open` | The draft is not open. |
| `parts_required` | `parts` is missing or empty. |
| `invalid_edit` | An edit has a wrong field type, a `quantity_override` that isn't a whole number from 1 to 10,000, or lists a Part twice. `parts` is not a list or has more than 10,000 edits. |
| `part_not_found` | A `part_id` is not in the draft. |
| `edit_empty` | An edit sets neither `included` nor `quantity_override`. |
| `invalid_digest` | `expected_snapshot_digest` is not 64 lowercase hex characters. |
| `draft_changed` | Confirm: the draft changed after the proposal, for example in the Plan tab. |
| `base_changed` | Confirm: the Accepted Plan changed after the proposal. |
| `accepted_baseline_required` | Confirm: the Build has no Accepted Plan to edit against. |
| `edit_refused` | Confirm: the edit refused for another reason. |

`unauthorized`, `unavailable`, `outcome_unknown` and `internal` apply as for `propose_create_build`.

### `propose_rebuild_plan`

`propose_rebuild_plan` `{ plan_id, idempotency_key? }` prepares the Build's Working Plan from its planning brief (GRE-1539; the storage operation is GRE-1518 and GRE-1736).

- **Propose** checks `plan_id` and `idempotency_key`, proves the API key and finds the brief. It stages the proposal without changing the Build. The brief is read again at confirm, so a role or a path choice set in between is used.
- **Confirm** runs one transaction. It requires exactly one evidence item with role `structural_base`. It removes a layer whose Source an earlier rebuild recorded in `managed_source_ids` when that Source is no longer `structural_base`, `overlay`, or `addon`. A layer the owner attached is not in that list, so it stays. It makes the structural base the base layer, then attaches each `overlay` and `addon` Source as an addon layer. Layers that already hold those Sources stay as they are.
- Resolved differences become excluded paths: `choose_source_b` leaves out Source A's `.stl`, `choose_source_a` leaves out Source B's, and `not_applicable` leaves out both. `include_both` excludes nothing. A confirmed contribution with responsibility `printable_parts` limits that evidence Source to its `path_scopes` (`mods/a/**` keeps paths under `mods/a/`). It prepares a draft from the layers the way `POST /plans/{id}/drafts/recompute` does, stores it, and records `draft_id`, `managed_source_ids` and `draft_source_revisions` in the brief. It never accepts or publishes a Plan.
- It answers `{ "ok": true, "result": { "draft_id", "snapshot_digest", "recompute", "sources": [{ "source_id", "role", "revision" }] } }`. `recompute` is `created`, or `existing` when the same key and the same Sources, revisions, and path choices found the draft an earlier rebuild made. The key is `idempotency_key`, or else the action id. Read the draft's Parts with `get_plan_draft`. The result lists no Parts (provisional).

Refusals have the shape described for `propose_create_build`. Every refusal leaves layers, drafts and brief unchanged, and confirm keeps the proposal pending.

| Code | When |
| --- | --- |
| `invalid_plan_id` | `plan_id` is not a positive whole number. |
| `invalid_idempotency_key` | `idempotency_key` is not a non-blank string of at most 143 characters. |
| `no_planning_brief` | The Build has no planning brief. |
| `planning_brief_invalid` | The stored brief does not parse. |
| `no_structural_base` | No evidence item has role `structural_base`. |
| `multiple_structural_bases` | More than one evidence item has role `structural_base`. |
| `unresolved_source_choices` | A difference group's resolution cannot be applied. That is a `custom` resolution, or an excluded `.stl` whose Source the brief's evidence does not name. The result adds `groups`, those group ids in the brief's difference order. |
| `source_not_found` | A role Source does not exist. The result adds `source_id`. |
| `layer_conflict` | The structural base Source is already attached as an addon layer. The result adds `source_id`. |
| `accepted_baseline_required` | The Build needs an Accepted Plan first. |
| `no_stls` | The Sources have no printable files. |
| `would_wipe` | The draft would have no Parts left. |
| `idempotency_conflict` | The key already made a draft that is not this rebuild, or one a later recompute abandoned. Use another key. |

`unauthorized`, `unavailable`, `outcome_unknown` and `internal` apply as for `propose_create_build`. Preparation reads the Source files inside the write transaction, so other writes wait while it runs.

Every other product tool still answers that it has no Rust port.

## To be written

- How to create and rotate the API key in the desktop app and the Docker image.
- How MCP access works when network sharing is on.
- The tool list and its schemas.
