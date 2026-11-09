# Draft header fields

A draft can propose a new Build name, order number, customer and due date. The
proposal autosaves into the durable draft, and only Accept copies it to the
Build. GRE-1197 built the storage, the draft write and the draft read; GRE-1207
built the Accept copy, its preview and schema step 45 ([Accept](#accept)).
Everything here is Rust-only, behind `desktop_rebuild` and
`PP_DESKTOP_REBUILD=1`.

## Schema 43

The 42 to 43 step installs three side tables from
`pp-storage/src/working_drafts/header_schema.sql`, validates them, and stamps 43
in one transaction. It alters no table Node uses.

| Table | Key | Columns |
| --- | --- | --- |
| `plan_draft_headers` | `draft_id`, cascades with its draft | `name` (1 to 4096 characters), `order_number`, `customer` (null or 1 to 4096), `due_date` (null or a real `YYYY-MM-DD` day), `header_version` (1 to 2147483647), `updated_at` |
| `build_profile_details` | `profile_id`, cascades with its Build | `customer`, `due_date`, `updated_at` |
| `plan_draft_header_receipts` | tenant, Plan, actor and hashed `Idempotency-Key` | `payload_digest`, `draft_id` (cascades with its draft), and the saved header and version, never updated |

`build_profile_details` holds the Build's customer and due date. Part A creates
it empty; Accept writes it ([Accept](#accept)). GRE-1100 and GRE-1095 read the due date
and customer from it.

Step 43 takes a `pre-schema43.db` copy before it writes, like every step
through `LAST_COPIED_STEP`. The open now writes the run marker
`backups/pre-schema-run` before the first step: it names the run's rollback
copy, one name per line, a restart reads it and retention keeps every copy it
names, and the marker goes once the upgrade commits. A marker naming anything
but `pre-schema*` copies is refused before any write. An open from 41 with no
marker still skips retention, because a release before 43 wrote no marker (see
[Domain events](domain-events.md#schema-42)). That open may be continuing an
older release's upgrade, so its marker also names the copies 38 to 41 it finds:
if it is interrupted too, the retry runs retention but keeps that older run's
untouched input.

Preflight refuses these tables in a database below 43, and every open checks
their SQL against the reference. The Node schema-parity comparison omits all three
tables, as it omits the other Rust-only tables.

## Write: `PUT /plans/{id}/drafts/header`

The body is a `SaveDraftHeaderRequest`:

```json
{
  "expected_base": {"revision_id": 1, "plan_version": 1},
  "expected_draft": {"draft_id": 2, "state": "open", "lifecycle_version": 0,
    "snapshot_digest": "...", "base": {"revision_id": 1, "plan_version": 1}},
  "expected_header_version": null,
  "header": {"name": "Bracket kit", "order_number": "PO-7",
    "customer": "Acme", "due_date": "2026-12-01"}
}
```

Every field is required; `null` clears an optional one. An `Idempotency-Key`
header is required. Storage trims text as JavaScript trims, then checks each
field. One immediate transaction then:

1. Looks up the key's receipt before any stale check. A retry with the same
   payload (Plan, expected base, draft and header version, and trimmed
   header) replays the first answer's receipt beside the current header read
   and writes nothing. The same key with any other payload is refused with 409
   `idempotency_conflict`.
2. Checks the Accepted base (`base_changed`, `build_archived`,
   `accepted_baseline_required`) and the observed draft (`draft_changed`).
3. With an open draft: a header equal to the stored one leaves the header and
   events alone and answers as saved. Otherwise the stored `header_version` must equal
   `expected_header_version` (`null` for no row), or the save is refused with
   409 `header_changed` and the current header. A write bumps the version and
   emits `plan.draft_changed`.
4. With no open draft: prepares a new draft from the Plan's sources against the
   Accepted base, as autosave does, and writes header version 1.
5. Stores the key's receipt: the draft and the header it now holds.

A 200 answers `DraftHeaderSaved`: the `receipt` (Plan, draft and saved header)
and the `header` read below. Invalid fields answer 422 `invalid_header` with
`field` and `reason` (`text_length` or `calendar_date`). Nothing reaches
`build_profiles`, Plan revisions or Required-unit IDs.

## Read: `GET /plans/{id}/drafts/{draft_id}/header`

Returns `DraftHeaderView`: `draft_id`, the Build's current header (`build`:
name and order number from `build_profiles`, customer and due date from
`build_profile_details`) and the draft's proposal (`draft`). `draft: null` means
the draft leaves the header unchanged.

The workspace read (`GET /plans/{id}/drafts/{draft_id}`) keeps its body. Its
bytes are pinned to Node's by the draft read, rebase, recompute and
Required-unit parity fixtures, and Node has no header, so the header has its own
read beside it. Both routes are in the generated `@print-partner/rust-client`
(`readDraftHeader`, `saveDraftHeader`).

## Read without a draft: `GET /plans/{id}/drafts/header`

Returns `BuildHeaderView`: the Build's current header (`build`, as above) and its
Accepted base (`base`: `revision_id` and `plan_version`, `null` and `0` before
the first Accept). The Plan tab reads it before any draft exists, shows the
header, and names that base when its first header save creates the draft. It
writes nothing. An unknown Plan is 404. The generated client calls it
`readBuildHeader` (GRE-1120).

## The Plan tab (GRE-1120)

The replacement UI's Plan tab (`web/apps/ui/src/builds/BuildPlan.tsx`) uses only
these routes through the generated client:

- The header form saves a field when it loses focus or on Enter, with
  `saveDraftHeader`. It names the open draft and the header version it read,
  so a change made elsewhere is refused with `header_changed`, and the tab
  rereads. A blank name is refused in the form and never sent.
- Each Part's Included box and quantity override save as one
  `autosaveDraft` decision. Writes run one at a time, and each names the draft
  the previous one left. While an Included save is queued or running, the
  box shows its value and the next click toggles from that; a refused save
  shows the saved value again.
- Review and accept opens the gate. It reads `previewDraftAccept`, shows the
  snapshot note, the header's old and new values, Part changes, printed
  Required units affected, cleared Plates, stock impact and duplicate orders,
  and sends the preview's `revision` without `draft_id` as the `acceptDraft`
  body with a new `Idempotency-Key`. A refusal shows its reason and previews
  again. Accept stays off until that preview arrives, and the refused changes
  stay visible meanwhile. If it fails, the old changes are hidden and Accept
  stays off, with "The preview failed", until Check again succeeds. The next Accept
  uses a new key. Accept stays off, with the reason,
  while the preview lists running work in `blocked_by` or checkoff links and
  queued sends in `production`: the tab sends `remap_checkoff_links: false`,
  so Accept would refuse with `production_active`.
- A lost answer or a 5xx, such as 503 `outcome_unknown`, is not a refusal.
  Header, Part and Accept writes resend it with the same `Idempotency-Key`
  and the body of the first attempt, even if the Plan was read again in
  between, and the core answers a repeat with the first receipt. If the outcome is
  still unknown, the gate keeps its key and preview, so the next Accept
  replays the same request instead of starting a new one.
- Once the draft writes settle, the saved header replaces every field not
  typed in since the last save was sent. Typing in one field while another
  saves is kept, and a field changed back to its old value while a save runs
  is sent too.
- Review and accept opens the gate only after every draft write has landed,
  so the preview, and the revision Accept sends, include the edit made just
  before the click. Start a draft does nothing while a save is running,
  because that save already starts the draft.
- The Rust MCP router's `propose_apply_plan_draft` (under
  `PP_DESKTOP_REBUILD`) accepts through the same Rust preview and Accept, so
  it copies the draft header too (GRE-1236, below). The desktop gateway still
  answers `/mcp` and `/api/v1/mcp` with 403 before any product route, with
  or without the flag (`pp-gateway/src/lib.rs`), so a desktop client does not
  reach it yet. Node's own MCP ignores the draft header.
- Offline, every field is read-only and every button is `aria-disabled` with
  the reason. With the Orders module off, the order number, customer and due
  date fields are hidden and the tab says why.

## Draft paths that carry the header

Every new draft row goes through `insert_snapshot`, which takes an `Origin`:
`New`, `Draft(id)` or `Rebased(source)`. Every origin but `New` carries that
draft's header row forward. The argument has no default, so a new draft path
must choose.

| Path | Header source | Test that fails without the carry |
| --- | --- | --- |
| Recompute | the open draft it abandons | `gre_1197_autosave_keeps_and_recompute_carries_the_draft_header` (pp-api) |
| Rebase | the source draft | `rebase_carries_the_source_draft_header` (`working_draft_rebase.rs`) |
| Save (publishing) | the observed draft | `save_carries_the_source_draft_header` (`working_draft_save.rs`) |
| Autosave | none: it writes into the open draft in place, or creates a draft when none is open | `gre_1197_autosave_keeps_and_recompute_carries_the_draft_header` checks the row survives |
| Header save | writes its own row | `gre_1197_header_save_creates_a_draft_when_none_is_open` |
| Restore as draft ([GRE-1094](build-history.md#restore-a-snapshot-as-a-draft)) | the snapshot's `plan_snapshot_headers` row, checked as a header save checks it (empty optional text reads as none), as header version 1, through `insert_snapshot` with `Origin::Snapshot` | `restore_as_draft_creates_a_draft_that_accept_publishes_without_rewinding_evidence` (pp-api) |

Passing `Origin::New` in recompute or Save, or dropping the carry for
`Rebased`, makes the matching test fail; this was checked by hand for this change.

## Accept

Accept (`POST /plans/{id}/drafts/{draft_id}/apply`) of a draft with a
`plan_draft_headers` row copies it to the Build inside Accept's transaction,
after the new revision is published: name and order number to
`build_profiles`, customer and due date to `build_profile_details` (inserted,
or updated in place). A draft with no header row changes neither, so an Accept
without a header writes exactly what it wrote before. A replay of the same
Accept answers its receipt and writes nothing, and a refused Accept writes no
header. Under `PP_DESKTOP_REBUILD` the Rust gateway serves Accept, so no Node
route writes these columns in the same flow.

Before its selection, which writes, Accept refuses a header whose name another
Build of the tenant already has, the rule a Build rename checks: 409
`{ code: "duplicate_build_name", name }`. The Accept preview returns the same
refusal from the same point. It also refuses a header that gives the Build an
order number another unarchived Build holds: 409
`{ code: "duplicate_order", order_number, builds }`, where `builds` lists each
holder's `profile_id`, `name` and stored `order_number`
([Order authority](#order-authority-gre-1095)).

The preview (`GET .../accept-preview`) lists the header fields the draft
changes in `changes.header`: `{ field, old, new }` for `name`,
`order_number`, `customer` and `due_date`, `null` for an empty field. A
draft whose Parts and inputs equal its base but whose header differs is
`changed`, not `no_change`. `duplicate_orders` lists the other unarchived
Builds holding the order number the Build has after Accept. On a ready
preview these can only be duplicates the Build already shares.

A header save changes neither the snapshot digest nor the lifecycle version,
so the preview's `revision` also carries `expected_header_version`: the
draft's `header_version`, or `null` without a header row. Accept compares it
with the stored version in its revision gate and answers `draft_changed` when
they differ, including when a header row exists and the request omits the
field. A header saved after the preview therefore never reaches the Build
unreviewed. The field is only a precondition. It is not part of the request
digest, because the admission row Accept shares with Node stores no header
version, so a replay of the same key answers the existing receipt.

Rust MCP (`/mcp`, `/api/v1/mcp` under `PP_DESKTOP_REBUILD`) accepts through
the same preview and Accept (GRE-1236). `propose_apply_plan_draft`
`{ plan_id, draft_id }` first runs Node's guards with the caller's API key on
the brief as the Build planning read hydrates it: the brief must exist and
select `draft_id` (hydration drops the selection when the Sources, their
pinned revisions or the resolutions moved), and its source choices must
resolve (Node's `resolvedSourcePathExclusions`: no `custom` resolution, and
every excluded `.stl` path names a Source in the brief's evidence). It then
reads the Accept preview and writes nothing. When the preview needs a
Required-unit reconciliation and the draft has none selected, it proposes the
action with `params.revision: null` and returns the `reconciliation` reason;
`confirm_apply` then selects the automatic reconciliation, as Node's
`prepareForApply` and the Rust HTTP Accept do, and accepts the revision it
previews after the same checks. When Accept would run, it proposes an action whose
`params.revision` is the preview's `revision` without `draft_id`, and its
result carries the `preview`. When Accept would refuse, it proposes nothing
and returns the refusal as `preview_result`. It also proposes nothing, and
returns the `preview`, while the preview lists running work in `blocked_by`
or checkoff links and queued sends in `production`. `confirm_apply` of that action
sends exactly that revision to Accept with the action id as the idempotency
key and `remap_checkoff_links: false`, after running the brief guards again, so a
header saved after the proposal is refused with `draft_changed`. Success
answers Node's shape: `apply_result` is `{ kind: "applied", receipt }`, also
for a replay or a draft already accepted, with Node's camelCase receipt keys, beside `build_workflow` (or `null`) and
`planning_phase: { kind: "applied", draft_id, revision_id }`. A refusal keeps
the action pending, as Node does. Only this tool and `get_build_workflow` have Rust ports; every other
product tool still answers that it has no Rust port.

Two limits remain:

- **Save.** The publishing Save in Rust (`WorkingDraftClient::save`) has no
  HTTP route; Node serves `POST /plans/:id/save`, and Node's Accept never
  reads `plan_draft_headers`. Rust Save publishes the source draft's header as
  it stands in Save's transaction, because Node's `SavePlanChoicesRequest`
  carries no header version. Whatever routes Save to Rust must add a
  client-observed version first.
- **Old web client.** `applyPlanDraft` in the old UI
  (`web/apps/web/src/api/endpoints/planDrafts.ts`) sends no
  `expected_header_version`, so its Accept of a draft that has a header row
  answers `draft_changed`. The replacement UI's Plan tab creates header rows
  and accepts them through the gate ([The Plan tab](#the-plan-tab-gre-1120)).

## Order authority (GRE-1095)

One order is one Build. An order number belongs to at most one unarchived
Build of a tenant. `pp-storage/src/order_authority.rs` owns the rule:

- **Normalization.** Two order numbers are the same order when they match
  after trimming JavaScript whitespace, collapsing each inner whitespace run
  to one space, and lowercasing. `" PO-7 "`, `"po-7"` and `"PO-7"` are one
  order. `"#1001"` and `"1001"` are two, because punctuation is kept. A blank
  or missing order number holds no order. The replacement UI's Builds list
  groups duplicates with the same rule (`normalizeOrderNumber` in
  `web/apps/ui/src/builds/BuildsList.tsx`).
- **Enforcement.** Accept is the only Rust write of
  `build_profiles.order_number`. It checks the draft header's order number
  inside its immediate transaction, before anything writes, and refuses with
  `duplicate_order` when another unarchived Build holds it. Two Accepts of
  one order on different Builds therefore serialize, and the second is
  refused. The preview and Rust MCP's `propose_apply_plan_draft` and
  `confirm_apply` run the same check, so MCP proposes nothing for such a
  draft and a stale proposal is refused at confirm and stays pending.
  Archived Builds and other tenants hold nothing.
- **Existing duplicates.** Copied Node data can already hold two unarchived
  Builds with one order. Nothing merges, renumbers or blocks them. A Build
  keeps an order it already holds, in any spelling, through Accept. The
  Accept preview's `duplicate_orders` and the Builds list's "Duplicate order"
  mark report them. Archive one of them to resolve it.

The rule covers Rust Accept and Rust MCP under `desktop_rebuild` and
`PP_DESKTOP_REBUILD`. Node writes order numbers without it: Node's Accept
with the flag off, `POST /plans/:id/duplicate` (copies the order number),
and kit import. Duplicating a Build as a detached experiment, promoting it,
and order import are not built yet.

There is no schema step: a unique index would need a normalized column and
would refuse the existing duplicates.

Tests: `order_authority_*` in `pp-storage` (normalization, `claim`) and in
`pp-api/src/drafts/http/tests/order_authority.rs` (HTTP refusal and preview,
the concurrent race, existing duplicates across restart, MCP).

## Schema 45

When Accept saves its `pre_apply` snapshot of the replaced Plan, it also saves
the Build's header as it stood in `plan_snapshot_headers`, keyed by that
snapshot's id, so a restore (GRE-1094) can bring the header back. Node's
snapshot payload is unchanged. The 44 to 45 step installs the table from
`pp-storage/src/plan_publication/snapshot_header_schema.sql`, takes
`pre-schema45.db`, and alters no table Node uses.

| Table | Key | Columns |
| --- | --- | --- |
| `plan_snapshot_headers` | `snapshot_id`, cascades with its `plan_snapshots` row | `name`, `order_number`, `customer`, `due_date`, `captured_at` |
