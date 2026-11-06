# Draft header fields

GRE-1197 part A. A draft can propose a new Build name, order number, customer
and due date. The proposal autosaves into the durable draft, and only Accept
copies it to the Build. Part A is storage, the draft write and the draft read.
Part B, the Accept copy into `build_profiles` and `build_profile_details`, is a
separate ticket. Everything here is Rust-only, behind `desktop_rebuild` and
`PP_DESKTOP_REBUILD=1`.

## Schema 43

The 42 to 43 step installs two side tables from
`pp-storage/src/working_drafts/header_schema.sql`, validates them, and stamps 43
in one transaction. It alters no table Node uses.

| Table | Key | Columns |
| --- | --- | --- |
| `plan_draft_headers` | `draft_id`, cascades with its draft | `name` (1 to 4096 characters), `order_number`, `customer` (null or 1 to 4096), `due_date` (null or a real `YYYY-MM-DD` day), `header_version` (1 to 2147483647), `updated_at` |
| `build_profile_details` | `profile_id`, cascades with its Build | `customer`, `due_date`, `updated_at` |

`build_profile_details` holds the Build's customer and due date. Part A creates
it empty; Accept writes it in part B. GRE-1100 and GRE-1095 read the due date
and customer from it.

Step 43 takes a `pre-schema43.db` copy before it writes, like every step
through `LAST_COPIED_STEP`. The open now writes the run marker
`backups/pre-schema-run` before the first step: it names the run's rollback
copy, a restart reads it and retention keeps that copy, and the marker goes once
the upgrade commits. A marker naming anything but a `pre-schema*` copy is
refused before any write. An open from 41 with no marker still skips retention,
because a release before 43 wrote no marker (see
[Domain events](domain-events.md#schema-42)).

Preflight refuses these tables in a database below 43, and every open checks
their SQL against the reference. The Node schema-parity comparison omits both
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

1. Answers a retry of a key that created a draft with that draft's header, or
   refuses `idempotency_conflict` if the header differs.
2. Checks the Accepted base (`base_changed`, `build_archived`,
   `accepted_baseline_required`) and the observed draft (`draft_changed`).
3. With an open draft: a header equal to the stored one writes nothing and
   answers as saved. Otherwise the stored `header_version` must equal
   `expected_header_version` (`null` for no row), or the save is refused with
   409 `header_changed` and the current header. A write bumps the version and
   emits `plan.draft_changed`.
4. With no open draft: prepares a new draft from the Plan's sources against the
   Accepted base, as autosave does, and writes header version 1.

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

## Draft paths that carry the header

Every new draft row goes through `insert_snapshot`, which takes the draft
whose header it carries (`header_from`). A new caller must name it, so a
draft path cannot skip the choice silently.

| Path | Header source | Test that fails without the carry |
| --- | --- | --- |
| Recompute | the open draft it abandons | `gre_1197_recompute_carries_the_open_draft_header` (pp-api) |
| Rebase | the source draft | `rebase_carries_the_source_draft_header` (`working_draft_rebase.rs`) |
| Save (publishing) | the observed draft | `save_carries_the_source_draft_header` (`working_draft_save.rs`) |
| Autosave | none: it writes into the open draft in place, or creates a draft when none is open | `gre_1197_autosave_keeps_the_draft_header` checks the row survives |
| Header save | writes its own row | `gre_1197_header_save_creates_a_draft_when_none_is_open` |
| Restore as draft (GRE-1094, not built) | the restored revision's header, through `insert_snapshot` | GRE-1094 adds its test |

Replacing `header_from` with `None` in recompute, rebase or Save makes the
matching test fail; this was checked by hand for this change.
