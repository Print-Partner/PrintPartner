# Plan autosave

## Runtime ownership

Two different operations exist, and each path has one meaning in every runtime:

- `POST /plans/:id/save` is Node's publishing Save, described in the rest of this page. It stays on Node, with its publishing semantics, until UI slice 6 moves the browser to draft-then-Accept. No Rust router serves `/plans/:id/save` with the flag on or off, so Node remains the only writer of that operation.
- `POST /plans/{id}/drafts/save` is the Rust draft-only autosave (GRE-418). It exists only with the `desktop_rebuild` Cargo feature and `PP_DESKTOP_REBUILD=1` or `true`, and Node has no route at that path. It writes Plan choices into a working draft and never publishes: Accept (`POST /plans/{id}/drafts/{draft_id}/apply`) is the only path that writes an Accepted Plan.

### Rust draft-only autosave

The body is `SavePlanChoicesRequest`, the same shape Node's Save takes: `expected_base`, `expected_draft` (the open draft the browser observed, or `null`), `remap_checkoff_links` and the final `decisions`. The `Idempotency-Key` header is required, trimmed, and 1 to 160 characters; without it the route answers 400 `invalid_request` before reading the body.

`WorkingDraftClient::autosave` runs one immediate SQLite transaction:

1. It reads the receipt for the key first. A replay with the same payload returns the stored receipt and the draft's current workspace and writes nothing. A reused key with a different payload is 409 `idempotency_conflict`. Both happen before the stale-base check.
2. It checks the Accepted base and the observed draft. A stale base is 409 `base_changed`; an observed draft that is no longer open with the same snapshot digest, or a `null` observation while a draft is open, is 409 `draft_changed`.
3. It applies the final choices to the observed draft, or creates a final-state draft when none is open, then runs one Required-unit reconciliation. That reconciliation row is the durable receipt: it stores the key and a digest of the payload.
4. It appends `plan.draft_changed` for each draft write, then commits.

Any failure rolls every write back, including the draft creation. Draft and Part IDs stay as they were; autosave never closes or abandons the observed draft and never writes `plan_revisions`, `plan_apply_requests` or `build_profiles`.

200 is `{ receipt, workspace }`, where `receipt` is `{ profile_id, draft_id, reconciliation_id, snapshot_digest, saved_at }` and `workspace` is the draft workspace read after the commit, so it is never older than the receipt. Refusals use Node's codes: 404 `profile_not_found`; 409 `accepted_baseline_required`, `base_changed`, `draft_changed`, `inputs_changed` and `idempotency_conflict`; 422 `build_archived`, `part_not_found`, `part_ambiguous`, `no_layers` and `no_stls`; Required-unit refusals as a Part edit answers them. Writer contention is 503 `transaction_unavailable` and any other storage failure is 500 `Plan draft update failed`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -p pp-storage -p pp-api -E 'test(ticket_t_61_autosave)'
```

## Node publishing Save

Ordinary file and quantity edits use one `POST /plans/:id/save` request. The browser sends stable file identities, final choices, the accepted Plan base, and the exact open draft it observed, or null. It keeps newer local choices visible while the request runs.

The server owns the complete save. It prepares a command-owned draft with the final choices, reconciles Required units once, and publishes through the existing native `applyPlanChanges` operation. It does not update accepted revisions in place. The SQLite transaction rolls back draft creation, reconciliation, publication and lifecycle changes together on failure.

A retry sends the same original command and idempotency key. The browser retains that failed ordinary-Save request (payload and identity) until a deliberate retry succeeds or the user discards pending edits; it does not invent a new key for the same failed intent. Retention is per Plan: switching Builds keeps each Plan's failed intent without applying it to another Plan or automatically resending it. Navigation and ordinary `preparePlan` leave a definitive rejection in place; only deliberate `retryPlanSave` (Retry save) resends it. A definitive rejection stays available for deliberate Retry until a successful prepare with applyManifest consumes it; that applyManifest prepare continues without replaying the rejected payload so later plain prepares do not stick it. The server checks the durable receipt before testing the current Plan base. A reused key with a different payload fails. Existing draft work is copied rather than abandoned before success; only the explicitly selected original draft closes after publication succeeds.

The response contains the commit receipt, a full authoritative Review, the profile summary, and closed draft IDs. The server captures the Review snapshot and profile summary together before asynchronous filament enrichment. A receipt may refer to an earlier commit on replay, but the returned Review must not predate that receipt.

The browser hydrates both Review variants from this result, retains newer local intent, and rejects older responses. It invalidates other screens without fetching every screen before reporting Saved. Source recovery and explicit draft operations retain their existing APIs and safety rules.

## Design decision

Two designs were compared: composing the native draft/publication operations, and directly applying accepted-plan deltas. Native composition is the base because it retains the existing history, completion, production and Checkoff protections. The delta design contributed applying choices in memory before the first draft insertion and capturing one consistent read snapshot.

Direct accepted-row mutation was rejected because it bypasses immutable revision and Required-unit ownership rules. Blindly chaining the old endpoints was rejected because it reconciles twice and ordinary recompute abandons open drafts. The independent design check agreed with native composition plus final-state insertion and consistent response capture.

## Verification

### Typed quantities

Grid, table, and mobile quantity controls share the same editor. Typing stays local until Enter or blur; the plus and minus buttons step from the typed value. Escape cancels unfinished typing. Empty values, fractions, and numbers outside the existing 1–10,000 limit do not save and show an inline error. An incoming save result does not replace an unfinished entry.

For a browser check against an isolated database, start these in separate terminals from `web/`:

```sh
node --conditions=development --import tsx scripts/plan-save-benchmark.mts --serve
```

```sh
VITE_DEV_API_TARGET=http://127.0.0.1:5182 npm run dev -w @print-partner/web -- --host 127.0.0.1 --port 5176
```

Then run `node scripts/check-plan-quantities.mjs`. It checks actual save responses, typing during a delayed response, both buttons, Enter and blur, invalid input, and reload persistence in grid, table, and mobile views. It restores the fixture's original quantity and writes desktop/mobile screenshots to a temporary folder. These commands do not use the live database.

### Save integrity and latency

Real SQLite tests cover atomic rollback, existing draft preservation, progress, source changes, committed retries, changed payloads and stale bases. Route tests require one reconciliation and confirmed state. Client tests delay responses and exercise rapid choices, retry identity, cache hydration and Build navigation.

Latency is measured from the click to confirmed Saved, separately from immediate checkbox feedback. The target is at least 95 percent of repeated single-file and folder saves below two seconds, without reversals or lost persistence. Local tests and live-host measurements are reported separately; a passing local benchmark is not proof of deployed latency.

Run the repeatable server benchmark from `web/`:

```sh
node --conditions=development --import tsx scripts/plan-save-benchmark.mts
```

It creates an isolated temporary database with 350 initially selected files across seven tracked sources, compares 30 single-file and 30 ten-file saves per path, checks returned choices, and fails if the new path's 95th percentile exceeds two seconds. Add `--serve` to start that fixture on loopback port 5182 for browser testing. It does not use the production database.

The September 6 local run measured new-path p95 server times of 385 ms for single files and 279 ms for ten-file batches, compared with 660 ms and 485 ms for the old sequence. These are local server timings, not network or browser timings.

An isolated development-browser pass observed 19 of 20 single-file/folder saves below 0.9 seconds; one single-file sample took 3.45 seconds. Timings include browser-control and polling overhead. No checkbox reversals or error alerts occurred, and three quick reversals retained the final selection. Live-host latency still needs measurement after deployment.

## Database statement reuse

Live verification after PR55 measured nine click-to-Saved samples at 1.54 to 2.44 seconds, with no observed checkbox reversals. Only four were below two seconds. Eight supplied server timing records showed the save command averaging 1209 ms and accepted snapshot/summary reads averaging 251 ms. Filament lookup averaged less than one millisecond. The two-second live target was not met.

To profile the save endpoint against the isolated fixture, excluding the old multi-request path:

```sh
node --conditions=development --import tsx scripts/plan-save-benchmark.mts --single-request-only --profile
```

The command prints the path to a `save.cpuprofile` file. Profiling starts after fixture setup and captures 30 single-file and 30 ten-file saves. Import the file into a JavaScript CPU profiler to inspect the call tree.

The local profile identified repeated SQL construction and statement compilation in the per-part and per-unit insert loops. The save now prepares each repeated insert once within its operation and reuses it for each row. Row order, returned identities, transaction boundaries, reconciliation, native Apply and verification remain unchanged. Nullable geometry booleans bind explicit SQL values so unknown geometry stays `NULL`, distinct from `false`.

With profiling enabled in both runs, single-file p95 fell from 235 to 164 ms and ten-file p95 from 237 to 164 ms. Sampled time inside `savePlanChoices` fell from 8.80 to 4.34 seconds across the 60 saves. These are local measurements; deployment and live click-to-Saved acceptance must still be verified. A real SQLite regression checks that the seven repeated insert statements are prepared once per save rather than once per part or unit, while existing tests retain rollback, retry, source-change and completed-progress coverage.
