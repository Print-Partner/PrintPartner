# Desktop rebuild flag

> **Plan status:** This page documents the existing `desktop_rebuild` flag and
> D-series work. Its screen sequence is historical; the current product target
> and replacement build order live in [Architecture](architecture.md) and
> [the UI target](product/ui-ux-target.md). GRE-621 keeps its existing D26
> web-app-shell scope and must not be re-scoped for the replacement UI. Track
> any missing Rust-only product entry point separately.

GRE-596 adds a build gate and a runtime opt-in for the desktop rebuild in GRE-595. Both default to off.

The `desktop_rebuild` Cargo feature is available on `pp-core`, `pp-contracts`, `pp-domain`, `pp-storage`, and `pp-desktop`. The desktop feature enables the core feature. The contracts feature exposes the new `build_identity`, `reconciliation`, `publication`, and `working_drafts` modules. Existing autosave contracts stay available in both builds.

GRE-498 adds the `pp-domain::features` registry and `pp-storage::features` tenant and Plan overrides behind `desktop_rebuild`. The storage client requires an explicit `AuthPolicy`. User choices are account preferences under the current one-account-per-tenant model. `SingleAccountDefault` maps sessions to the `default` tenant.

With `PP_DESKTOP_REBUILD` exactly `1` or `true`, health advertises `tenant_feature_toggles` and Settings shows tenant controls for farm, orders, Bambu, and Spoolman. All four default to off. Authenticated `GET` and `PATCH /settings/features` use the existing tenant `feature_toggles` setting at the root and `/api/v1`. Boolean patches save tenant choices. A `null` patch clears a choice to its default. Writes preserve unknown stored values and retry concurrent changes. Corrupt choices fail closed. Plan overrides retain precedence. With the flag off, the capability and controls are absent and these endpoints return 404 before reading or writing feature choices.

Plan feature controls on the Plan page use the same capability gate. Each feature offers account inheritance, enabled, or disabled. The effective value and its source are shown. Clearing an override sends `null` and resumes live tenant inheritance. Controls and feature reads are absent when the capability is absent.

When `PP_DESKTOP_REBUILD` is exactly `1` or `true`, Node printer queue suggestions honor each queued item's Plan overrides. Plan choices override user choices, then default to false. Profileless items inherit user choices. The global `get_farm_status` operation through MCP and the assistant stays user-scoped. Authenticated `GET` and `PATCH /plans/:id/features` are available at the root, `/api/v1`, and `/api/v2` only with these exact flag values. A patch value of `null` clears the Plan override and restores inheritance. Optional modules gain Node admission guards in separate slices.

With these exact flag values, Bambu and Spoolman inspection at `GET /api/v1/integrations/:id/status`, `GET /api/v1/integrations/:id/devices`, and `POST /api/v1/integrations/:id/test` requires the current tenant's matching `bambu` or `spoolman` choice to be enabled. Default or explicit opt-out returns 403 `FEATURE_DISABLED`. Malformed stored choices return 500 `FEATURE_TOGGLES_INVALID`. Admission runs after authentication and before status execution or adapter calls, including cached status reads. These tenant-global routes ignore Plan overrides. Saved connector metadata and editing remain available. Other integration types and flag-off inspection retain their existing behavior. This admission is Node's. The Rust-only runtime has no Bambu or Spoolman adapter yet: its `POST /api/v1/integrations/:id/test` answers 501 for those types without reading feature choices or contacting the host ([Printer-host reads](printer-host-reads.md)).

With these exact flag values, `POST /printer-send-queue` and its `/api/v1` alias check Bambu feature access when the linked host is Bambu. The selected owned Plan override takes precedence over the tenant's Bambu choice, then defaults to off. Admission follows Plan ownership and fleet-host resolution. Default or explicit off returns 403 `FEATURE_DISABLED`; corrupt tenant or selected Plan choices return 500 `FEATURE_TOGGLES_INVALID` before enqueue. Enabled Bambu retains the existing 400 unsupported-host response. This adds no Bambu transport. Moonraker, PrusaLink, and flag-off admission retain their existing behavior.

Dispatch and drain at both aliases preflight Bambu access before live-job reconciliation, artifact checks, queue writes, status probes, or job starts. Admission uses each stored item's Plan, never a request-body Plan. Missing or unowned Plans fail closed. Drain preflights the selected queued batch, respecting its printer filter and eight-item cap, before dispatching any host. Both routes recheck admission after asynchronous live-job reconciliation so concurrent revocation or newly queued items cannot bypass the check. A refused request preserves the stored queue and staged files for an explicit retry. Enabled Bambu retains the existing unsupported-upload response. Non-Bambu hosts and flag-off behavior remain unchanged.

The Node `printer-upload` worker also checks admission at execution time with these exact flag values, including jobs created before a feature choice changed. For a queue-owned upload, the stored job payload must match its queue item's printer and Plan before fleet or host lookup. A mismatch fails with `Stored printer upload does not match its queue item`. For Bambu, the payload must name a positive safe-integer Plan owned by the job's tenant. The job runner accepts numeric string Plan IDs after normalization. A missing or malformed Plan fails with `Pick a plan to bind this send (profile_id required)`. A deleted, nonexistent, or foreign Plan fails with `Profile not found` before feature reads.

After resolving the Bambu host, the worker reads the current tenant and selected Plan choices before adapter access. The Plan override takes precedence, then the tenant choice, then off. Default or explicit off sets the terminal job's `error` to `FEATURE_DISABLED: Feature bambu is disabled.`. Corrupt tenant or selected Plan settings set it to `FEATURE_TOGGLES_INVALID: Stored feature toggles are invalid.`. Unrelated Plan settings do not affect admission. The job runner reconciles the linked queue item to `error` and copies the same text into its `error` field, capped at 500 characters.

A worker refusal retains the queue-owned artifact for an explicit retry. Direct-upload artifacts follow the existing cleanup path. Enabled Bambu still fails with the existing `Direct Bambu upload is not available.` message and handoff guidance. Moonraker and PrusaLink transfers keep their existing behavior without reading feature choices. With the flag absent or any value other than exactly `1` or `true`, the worker keeps legacy behavior without feature reads or these Plan and queue-payload checks. The worker guard adds no Bambu upload transport.

Multipart staging still precedes queue admission. Every refusal removes the staged artifact, including feature denial and enqueue failure. Successful enqueue transfers artifact ownership to the queue. Cancellation removes that artifact through the existing cleanup path.

Enabling one connector type does not admit the other. Non-Bambu and non-Spoolman inspection remains available without reading feature choices.

With `desktop_rebuild`, Rust library callers can open storage with `WriterOwner::open_with_job_feature_admission(directory, limits)`. Every worker from that owner checks `Feature::Spoolman` before admitting a new `SpoolmanDeduction`. Ordinary `open` and `open_at` retain legacy behavior. Repeat the opted-in constructor on each reopen. Storage reads no runtime environment flag.

Admission resolves the stored job tenant and `PrinterUpload.profile_id` through the existing feature API's owned `build_profiles` Plan. Plan choices override user choices, then default to false. Missing or foreign Plans and corrupt settings return errors. Disabled admission returns typed `JobFailure::FeatureDisabled` before changing the job or renewing its lease. Existing effects can still be confirmed or reconciled after a disable. `Finish(None)` still requires primary printer receipts. This is Rust library enforcement only. No production Spoolman driver or desktop bootstrap wiring is added.

`CoreRuntime::start` reads `PP_DESKTOP_REBUILD` once at startup and stores the effective setting. `CoreRuntime::desktop_rebuild().enabled()` returns that setting. Only the exact values `1` and `true` enable it, and only when `pp-core` was compiled with `desktop_rebuild`. Missing, non-Unicode, empty, false, and unrecognized values produce false. The setting is process-local and does not change `desktop.toml` or persisted data.

D1 adds no routes, storage migrations, UI, or rebuild execution. `CoreRuntime::start` keeps the gateway and Node compatibility runtime even when both gates are enabled; the launchers use `start_for_launcher`, described under [Rust-only product entry point](#rust-only-product-entry-point). Future rebuild behavior must check the stored runtime setting and compile behind the Cargo feature.

Build the native shell with the core gate enabled using its existing staging prerequisites:

```sh
PP_DESKTOP_REBUILD=true cargo run --locked --manifest-path rust/Cargo.toml -p pp-desktop --features desktop_rebuild
```

The contracts and core gates can be tested without a native shell:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-contracts
cargo test --locked --manifest-path rust/Cargo.toml -p pp-contracts --features desktop_rebuild
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --lib
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --lib --features desktop_rebuild
```

Core tests isolate environment changes in child processes. They cover absent, disabled, invalid, and enabled values with and without the Cargo feature. The existing autosave consumer tests run in both contract builds. The Rust runtime proof checks the existing authenticated endpoints, source creation, response shapes, and shutdown with the runtime opt-in absent or false.

## Rust-only product entry point

`pp-server` and `pp-desktop` start through `CoreRuntime::start_for_launcher`.
With the Cargo feature compiled in and `PP_DESKTOP_REBUILD` exactly `1` or
`true`, that start runs Rust only:

- No Node child starts. First `prepare_data_dir` creates the data directory
  if it is missing and finishes a lease release this process failed earlier;
  it creates no schema or marker. Then startup runs in this order
  (`pp_core::rust_startup`):
  1. `WriterOwner::open_with_job_feature_admission` takes the data directory
     lease and makes the [ownership marker decision](storage.md#ownership-marker-decision),
     then opens the schema. A refused directory stops startup here, before
     release verification, recovery or admission, and stays byte-for-byte
     unchanged.
  2. `pp_compat::verify_bundle` verifies the release.
  3. `cutover::recover` runs without an ownership index: it removes stray
     cutover temporaries, and a cutover record refuses startup.
  4. Source-sync admission writes or reuses `source-sync-admission.json`.

  If step 2, 3 or 4 fails, the writer shuts down and releases the lease. The
  marker, and the schema it opened, stay: Rust owns the directory from step 1.
  The gateway then binds its usual loopback origin. The Node-backed start keeps
  its order: release verification, the lease, recovery, admission, the child.
- `rebuild_api::product_router` composes the read routers, the draft routers
  and the jobs router (including `/ws/jobs/{id}`) from that one owner, with the
  single-account auth policy. It passes the bundle's
  `community_manifests_dir()` to the Source reads.
- `pp-core` hands that router to `Gateway::start_rust_only`. `pp-gateway`
  takes an `axum::Router` and has no `pp-api` dependency. Host, Origin and the
  desktop session cookie are checked first. Every other admitted request that
  is not a gateway or asset path goes to the Rust routes, which do their own
  session or API-key authentication. Unknown paths get a Rust 404. Nothing is
  forwarded to Node. `Gateway::compat_forwards()` counts proxy attempts at the
  gateway's route-miss path: each request that no gateway, asset or Rust
  product route answered and that entered the compatibility branch. It stays
  at 0 in this mode.
- `POST /auth/logout` goes to the Rust routes first, which revoke the Rust
  session, then the gateway ends the desktop session and stops the runtime as
  it does today.
- Moonraker status (GRE-335, GRE-449): the gateway's status watchers poll the
  Moonraker hosts in the stored `integrations` setting (tenant `default`).
  `pp_storage::printer_hosts` projects that setting the way Node's private
  `/__desktop/moonraker-hosts` route does; both replay
  `fixtures/drivers/moonraker/host-snapshot-parity.json`. The gateway rereads
  the setting every second, so a changed, disabled or removed host replaces or
  cancels its one poller. A malformed setting stops every poller and the route
  answers 503 `Printer monitoring unavailable`.
  PrusaLink status (GRE-1175): the same watchers also poll the stored
  PrusaLink hosts, through the PrusaLink driver's Digest-authenticated status
  read, every 5 s (the web client's default status cadence; Moonraker polls
  every 2 s). Node reads PrusaLink status on demand, so the published status is
  the one Node's adapter reports, including its offline messages. An offline
  read backs off like a failed Moonraker poll and does not count as heard.
  PrusaLink rows come after the Moonraker snapshot and are watched only while
  the total stays within the 128-host cap; a PrusaLink row past the cap, or
  with an id a Moonraker row already has, is not watched.
  `GET`/`HEAD /api/v1/integrations/{id}/status` is the `pp-api`
  `printer_status` route. It authenticates like every Rust route, then answers
  the cached `PrinterHostStatus`; it never polls a printer on demand. An
  id that is not a watched Moonraker or PrusaLink host answers 404
  `Integration not found`; nothing goes to Node.
  An HTTPS host reports `offline` with `HTTPS printer monitoring is not
  available yet` until GRE-359, and a Moonraker host that redirects reports
  `offline` and stops polling. A PrusaLink redirect is the driver's
  `PrusaLink returned HTTP <status>` offline status, as in Node, and the
  poller keeps backing off. Hiding the window keeps polling; `suspend_monitoring`
  (sleep) stops every poller and `resume_monitoring` starts them again.
  Monitoring only reads: no Checkoff, queue or job writes and no sends.
  `Gateway::printer_observations()` is the read-only in-process port for later
  consumers such as farm reads and the filament writer; a host leaves it when
  its poller stops. As in Node, polling does not check the `farm` Module.
- Farm reads (GRE-1125): `GET /api/v2/printers`, `/api/v2/printer-checkoff`,
  `/api/v2/printer-checkoff/unattributed` and `/api/v2/printer-send-queue`
  answer from storage and the watchers' observation port, with separate
  execution and bed-clear facts, last-heard time and distinct not-set-up,
  manual and unreachable reasons. They write nothing. See
  [Farm reads](farm-reads.md).
- Match suggestions (GRE-1091):
  `GET /api/v2/printer-checkoff/unattributed/suggestions` proposes a Build and
  Plate for each unmatched print with confidence `exact`, `likely` or `none`
  and the evidence used. It is a read; nothing confirms a suggestion. See
  [Match suggestions](farm-reads.md#match-suggestions).
- Claim and dismiss (GRE-1182):
  `POST /api/v2/printer-checkoff/unattributed/{id}/claim` takes
  `{"profile_id": N}` and optional `selected_stl_basenames`, attributes the
  print's object names to incomplete Required units of that Build, and in one
  transaction creates its printer Checkoff link awaiting verify, marks every
  open print of that file on that integration claimed, records the printer's
  plan binding and appends `checkoff.unattributed_resolved`.
  `POST .../unattributed/{id}/dismiss` marks the print dismissed with the same
  event. Refusals keep Node's status and detail (400 `profile_id is required`,
  404 `Unattributed print not found`, 409 `Print already claimed`, `That print
  file is already assigned`, and the rest of `materializeProblem`) and write
  nothing. A Build that does not exist is a 500, as in Node (GRE-1217). Node's per-IP rate limit is not carried over because the routes are
  loopback-only. Tests: `pp-storage/tests/unattributed_claim.rs` and
  `pp-api/tests/unattributed_claim_http.rs`.
- Printer stream (GRE-1014): `GET /ws/printers` sends a snapshot of every
  configured printer, then each watcher transition in order with a strictly
  increasing sequence, heartbeats every 15 seconds, and resume by sequence.
  It writes nothing and never polls a printer. See
  [Printer stream](printer-stream.md).

  ```sh
  cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
    --features pp-core/desktop_rebuild,pp-storage/desktop_rebuild,pp-api/desktop_rebuild \
    -E 'test(ticket_t_4_watcher_proof)'
  ```
- Farm log (GRE-1126): `GET /api/v2/farm-log` pages the fleet's Checkoff
  bursts, burst undos and sends, newest first, and
  `POST /api/v2/farm-log/{id}/undo` undoes a logged burst through the burst
  undo. Storage appends an entry in the burst's and the undo's transaction;
  sends never offer undo. See [Farm log](farm-log.md).

  ```sh
  cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
    --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild \
    -E 'binary(farm_log) | test(farm_log)'
  ```

- Printer-host reads (GRE-360, GRE-1044): `GET /printers/{id}/capabilities`,
  `/files` and `/cameras` (root and `/api/v1`), and `POST /api/v1/integrations/{id}/test`, read a
  Moonraker or PrusaLink host on demand through the gateway's drivers. They are
  read-only on the printer: no upload or print start, and only PrusaLink's
  single Digest resend after a 401. Moonraker and PrusaLink both report the
  status capability, except a PrusaLink row the watchers do not poll (see
  above); Bambu reads as having no capabilities. See
  [Printer-host reads](printer-host-reads.md).

  ```sh
  cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
    --features pp-core/desktop_rebuild,pp-api/desktop_rebuild \
    -E 'test(ticket_t_21) | test(ticket_t_22)'
  ```

- The gateway listener is loopback only and passes the peer address to the
  Rust routes as `ConnectInfo`; they refuse a non-loopback peer. LAN sharing is
  not part of this mode.
- Shutdown drains and stops the gateway, drops its routes, then shuts the
  writer down. If tracked shutdown work is still running, the writer and its
  lock are kept until the process exits. The receipt reports
  `compat_reaped: true` because no child ran.
- `pp-core` owns a `RustStatus` channel for this mode: `starting`, then
  `ready` once the writer holds the lease and the gateway is serving, then
  `stopping` and `stopped` during shutdown. The sender lives in `CoreHandle`,
  so the channel stays open while the desktop holds a handle, including after
  shutdown. `CoreHandle::subscribe()` returns a `StatusReceiver`: its `Rust`
  variant carries this channel and its `Node` variant carries the supervisor's
  channel unchanged. `CoreHandle::compat_status()` returns the Node child's
  channel, or `None` in this mode. `RustStatus` has no `generation` or `pid`,
  because no Node child exists to supply them.
- The desktop tray maps the Rust states to the labels it already uses:
  "Service starting", "Service ready", "Service stopping" and "Service
  stopped". "Restart service" stays disabled, since there is no Node child to
  restart.
- `GET /__runtime` reports the Rust status and marks the compatibility backend
  unused:

  ```json
  {"proof_class":"headless_unsigned","rust":{"state":"ready"},"compat":{"state":"not_used"},"gateway":{"active_requests":0,"active_upgrades":0}}
  ```

  In Node mode the body is unchanged and has no `rust` key. The gate scripts
  that read `/__runtime` (`scripts/verify-rust-gateway/verify.py`,
  `rust/scripts/workflow-smoke.sh`, `rust/tests/*.py`) build `pp-server`
  without the feature, so they always see Node mode.

- `GET /health` lists each Module in `modules`, in this order: `farm` (farm
  and printers), `spoolman`, `orders`, `bambu` and `assembly_tracking`. Each
  entry is `{"state":"on","module":…}` or
  `{"state":"off","module":…,"reason":…}`. The reason codes are `default_off`
  (nobody turned it on), `turned_off` (the user chose off) and
  `setting_unreadable` (the stored choice does not parse, or could not be
  read in time, so the module stays off). `pp-storage::features::modules` reads the `default` tenant's
  `feature_toggles` and `build_tracking_assembly` settings on every request,
  so a change shows on the next `/health` without a restart. The body carries
  only these fixed keys and codes, never a stored value or parse error, so it
  holds no secrets, URLs or customer data. The read waits at most 100 ms for one of
  the 4 pooled readers. When it times out or fails, `/health` still answers
  200 with every other field and lists all five modules as
  `{"state":"off","module":…,"reason":"setting_unreadable"}`, so liveness
  never depends on settings storage and the UI's 8-second poll keeps showing
  the server as reachable. Treat `setting_unreadable` as "unknown, try
  again", not as the user's choice; the next request reads storage again.
  `modules` is optional in the generated `healthSchema`, as are the six
  members Node's `/health` never sends (`degraded_detail`, `single_user_auth`,
  `single_user_setup_required`, `owner_mapping_required`,
  `github_oauth_configured`, `discord_oauth_configured`), so one schema decodes
  both Node's body and the Rust-only body. Plan overrides do not change
  this list. Other Rust auth routers omit `modules`, so their body still
  matches Node's, and Node-mode `/health` comes from Node unchanged.

  ```json
  "modules":[{"state":"on","module":"farm"},{"state":"off","module":"spoolman","reason":"default_off"},…]
  ```

`CoreRuntime::start` and every other combination of gates keep the Node-backed
runtime, so existing flag-on Node tests are unchanged. With the feature off,
the gateway route table is pinned by a digest test and Plan reads still go to
the compatibility proxy. Use disposable data directories only: this mode does
not yet refuse a directory Node has used, and it writes Rust schema into the
directory it opens.

The `slice0_mount` tests check, for `PP_DESKTOP_REBUILD=1` and `true`:

- the process table, read from `/proc` on Linux and `ps` on macOS, shows no
  descendant of the launcher or `pp-server` process, at start and after the
  journey. With Node running, the same check finds the Node child, so it
  cannot pass on an empty table;
- a foreign `Host` gets `400 Invalid Host`, and a foreign `Origin`, or no
  `Origin` on a write, gets `403 Invalid Origin` from the gateway before the
  Rust routes see the request. The refused registrations write no account;
- the proxy-attempt count is 0 after the journey, including requests to paths
  no Rust route serves.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(slice0_mount)'
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail -E 'test(slice0_mount)'
```

The `health_modules` tests check the Module list through the same gateway:
every module is listed, stored changes show on the next request, and a canary
run plants secret-looking values in `feature_toggles`, `integrations`,
`default_spoolman_integration_id` and `build_tracking_assembly`, plus an
account email, a Plan name and the session token, and finds none of them, no
`://` and no host address in the body.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(health_modules)'
```

With `PP_JOURNEY_REPORT=1`, the feature-on pp-core gateway journey also writes
its count to `docs/parity/journey-route-misses.json`, which
`scripts/operation-inventory.py` quotes in its rollup. See
[Operation inventory](development/operation-inventory.md#journey-route-misses).

The launcher and `pp-server` tests need the built web assets and
`rust/bundle-manifest.json`, like the other runtime tests.

## Slice plan

The rebuild ships as small PRs to `main`, each under about 1.5k lines and checkable on its own. New product code compiles behind the `desktop_rebuild` Cargo feature, and anything user-visible also checks the runtime setting. D4 records media already shipped in #131 and adds no product code. D31 and D32 add CI or documentation only. The default build stays unchanged. The old desktop stack (#135 to #168, `desktop/rust-core`, and the reference branch `desktop/source-sync-local-current-20261008`) is reference only. It is not merged, and it is not closed or deleted until the slices cover it.

Each slice is done when:

- the existing suites pass with the feature off and on, and the default build is unchanged;
- a reviewer engine other than the author's scores it above 8 on the exact head, with all checks green.

Slices land in this order. Linear tracks each one under GRE-595. A slice is one Linear item and may ship as several PRs (D16 ships as four, D14 as five). Some modules already exist on main from earlier PRs (for example #129 to #131); a slice ports only the delta from the reference branch.

| Slice | Linear | Scope |
| --- | --- | --- |
| D1 | GRE-596 | Flag scaffold and pp-contracts additions (merged, #316) |
| D2 | GRE-597 | pp-core profile_interpreter |
| D3 | GRE-598 | pp-core review_observations, source_acquisition and uploads |
| D4 | GRE-599 | pp-source media |
| D5 | GRE-603 | pp-source retained_capture |
| D6 | GRE-605 | pp-storage auth and native_secrets |
| D7 | GRE-600 | pp-storage catalog core |
| D8 | GRE-601 | pp-storage catalog naming and tests |
| D9 | GRE-602 | pp-storage manifest_text and build_graph |
| D10 | GRE-604 | pp-storage build_graph manifest_options |
| D11 | GRE-606 | pp-storage required_units |
| D12 | GRE-607 | pp-storage working_drafts core, yaml and preparation |
| D13 | GRE-608 | pp-storage working_drafts save, manifest, rebase and siblings |
| D14 | GRE-610 | pp-storage uploads and capture_manifest (5 PRs) |
| D15 | GRE-611 | pp-storage jobs model and subscriptions |
| D16 | GRE-609 | pp-storage jobs engine (4 PRs) |
| D17 | GRE-613 | pp-storage source_scan |
| D18 | GRE-614 | pp-storage source_sync |
| D19 | GRE-612 | pp-storage plan_publication |
| D20 | GRE-615 | pp-storage checkoff_progress |
| D21 | GRE-616 | pp-storage read_model context and graph |
| D22 | GRE-618 | pp-storage read_model workflow and views |
| D23 | GRE-617 | pp-api auth HTTP |
| D24 | GRE-619 | pp-api drafts HTTP (list/read/edit/abandon; abandon GRE-988; reconciliation GRE-416; recompute/rebase/Accept GRE-417) |
| D25 | GRE-620 | pp-api builds HTTP and jobs |
| D26 | GRE-621 | Web and server wiring behind the flag |

Gap slices added in triage on 2026-10-13 cover commits that are not in the reference branch. The flag rule applies to D27 to D30. D31 is CI-only and D32 is a docs-only audit, so neither needs the flag.

| Slice | Linear | Scope | Lands after |
| --- | --- | --- | --- |
| D27 | GRE-622 | Plate layout and STL geometry parity (#161) | D10 |
| D28 | GRE-623 | ZIP acquisition label policy and bundle-probe SQLite lifetime (#149) | D4 |
| D29 | GRE-624 | Failed-Save intent retry and draft lifecycle replay (#145, #148) | D13 |
| D30 | GRE-625 | Source catalog metadata through Rust HTTP (#144) | D8, D23 |
| D31 | GRE-626 | Reconcile CI ratchet commits from #155 with main | D1 |
| D32 | GRE-627 | Coverage audit mapping #133 and #135 patches to slices | none; runs early, re-checked before cutover |

Turning `desktop_rebuild` on by default is a separate decision for the maintainer after D26 and D32.

### D12 observation core pull-forward

GRE-607 starts with `pp-storage::working_drafts::observation`, compiled only with
`desktop_rebuild`. This unit ports the read contracts, cancellation, preparation
limits, and cumulative budgets from the reference branch. It adds no executor,
adapter, routes, storage writes, or runtime wiring. Default builds do not expose
the module. Future user-visible callers must also check `PP_DESKTOP_REBUILD`.

`PreparationBudget::new` is public and returns `ReadResult`, validating limits
before accepting charges. Parser accounting methods are public so adapters and
future parsers can share the same budget. Byte counters saturate or use checked
addition, so oversized charges return `LimitExceeded` instead of overflowing.
Callers must propagate failures and discard the budget after a refusal. Raw
document reads and parser expansion retain separate total-byte caps.
Depth checks precede node and visit charges, matching entry accounting.

Read requests retain storage-private fields. External adapters can inspect them
but cannot create grants. This unit does not itself admit or perform any reads.

The read-only executor below uses D11's `required_units`. The observation
contract alone does not unblock GRE-604 D10b. The YAML parser and builder manifest
helpers are separate D12 units described below. The remaining D12 units are the
mutating executor and preparation. Those stay open on GRE-607.

Run its boundary and accounting proofs with:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test working_draft_observation
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --doc
```

### D12 YAML parser

The second GRE-607 unit ports `pp-storage::working_drafts::yaml`, the manifest
YAML reader, compiled only with `desktop_rebuild`. The module is crate-private
and feeds the builder manifest helpers. The optional `saphyr-parser` dependency
(pinned `=0.1.0`, with `arraydeque`) is enabled only by the feature, so default builds do not compile
it.

`Document::parse` charges the shared `PreparationBudget` for nodes, depth,
visits, and expansion, and checks cancellation. Each parsed node is charged once,
array separators count as two UTF-16 bytes, and each mapping tracks seen keys
in a hash set. Each mapping key charges the UTF-16 bytes of its entry copy and
its seen-key copy. Text coercions, including primitives and mappings, charge their
UTF-16 output bytes. The float matcher is compiled once. It accepts one document with
core-schema tags, `!`, anchors, and aliases. Duplicate keys, extra documents,
other tags, and tag/value mismatches return `InvalidDocument`. Scalars keep
JavaScript UTF-16 semantics: lone surrogate escapes such as `"\uD800"` survive
parsing, the private-use sentinel `U+E000` round-trips in every scalar style,
and keys follow JavaScript property order. `text`, `optional_text`, and
`truthy` apply JavaScript string and truthiness coercion. As with JavaScript
`join`, an array that reaches itself through an alias contributes an empty
string, so `project: &a [*a]` coerces to `""`.

#### Known differences

[GRE-841](https://linear.app/greetz/issue/GRE-841/p2-working-draft-yaml-remaining-flow-key-and-verbatim-tag-parity-gaps)
records flow-key and verbatim-tag documents that `Document::parse` and
js-yaml 5.4.3 do not treat the same way. The parity bar in the
[definition of done](development/definition-of-done.md) still requires a
match on real working-draft files and on every document Node's writer
emits. The forms below come from generated differential cases. The app
does not write them, and a person editing a draft does not produce
them. They stay unsupported.

Rust refuses these documents. js-yaml accepts them:

- An explicit empty flow key, such as `[? ]`, `[? : x]`, `[? , x]`, a
  `?` before the end of a flow line, or `[a, ? ]`. js-yaml returns the
  key `"null"`. `[? ]` loads as `[{"null":null}]`.
- An empty implicit flow key, or a flow colon with no space before a
  tab or a nested collection. `[:[]]` loads as `[{"null":[]}]`. The
  same spacing difference includes `{:[]}`, `{a:\tx}`, `{"q":\tx}`,
  `[a:[]]`, and a multi-line quoted flow key followed by a tab.
- A verbatim tag with no space before the following text, such as
  `!<tag:yaml.org,2002:str>b c1`.
- A line-broken empty tagged flow entry. The text `k: [!!str`, a line
  break, and `]` loads in js-yaml as `{"k":[""]}`.
- A property on the line after a flow mapping, then a colon. One shape
  is `- - {a: "a`, a continued line `   b", !!str`, and then `:x}`.

Rust accepts these documents. js-yaml rejects them:

- A comment with no space after a quoted key's colon, such as
  `{"q":# c`, a newline, and `}`, the same shape inside a flow
  sequence, and `{"q" :# c` with a space only before the colon. A space
  before `#` is a different document. js-yaml accepts that spaced form
  as one key whose value is null.
- A flow `?` followed directly by `,`, `]`, or `}`. Examples are `[?]`,
  `{?}`, `[x, ?]`, and `[!!str ?]`.
- A tab used as the separator in block structure. Examples are a tab
  before `- x` under a key, a tab between `-` and `:`, and a tab
  between `:` and `!!str`.
- A tag before an implicit key on its own line. One shape is the key
  `a`, then a line `  !<tag:yaml.org,2002:str> &x : `. js-yaml reports
  an unknown mapping tag.

`fixtures/parity/yaml-flow-properties/` records Node's results for
explicit empty flow keys, implicit empty flow keys, adjacent
verbatim-tag text, a block colon after a flow collection, a multi-line
quoted flow key followed by a tab, and a comment immediately after a
quoted key's colon. That corpus does not assert Rust parity.

The remaining D12 units are the mutating executor and preparation.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib working_drafts::yaml
```

### D12 builder manifest helpers

GRE-607 exposes five crate-private helpers through `working_drafts::observation`
for GRE-604 D10b. `parse_builder_manifest` reads part rules, option groups,
repository defaults, and their scalar or array shapes. `merge_builder_groups`
preserves earlier metadata, unions patterns, and preserves JavaScript property
order. `BuilderGlob` applies the Node glob and basename rules.
`parse_builder_hints` validates version 1 path hints and rejects unknown fields.
`infer_builder_siblings` delegates to the sibling inference already on main.

The model retains UTF-16 identifiers and the existing sibling group types.
Parsing uses the shared `PreparationBudget`, including rule and expansion
accounting. Both parsers check the document size before decoding. The sibling
wrapper takes the same budget and returns `ReadResult<BuilderGroups>`, matching
the landed inference rather than the reference branch's unbudgeted signature.
These helpers compile only with the default-off `desktop_rebuild` feature.
Draft application, the executor, and preparation remain separate
units. No user-visible behavior changes in this slice.

The contract is `loadManifestYaml`, `mergeOptionGroups`, and `matchKeyMatches` in
`web/apps/server/src/services/manifest-apply.ts`, hint validation in
`path-hints.ts`, and `inferSiblingFolderOptionGroups` in `repo-tree-summary.ts`.
Supplementary Unicode wildcard parity and lone surrogate patterns remain in
[GRE-641](https://linear.app/greetz/issue/GRE-641/align-builder-path-globs-with-node-utf-16-wildcard-semantics).
The current regex API matches UTF-8 input. That follow-up enables the maintained
UTF-16 matcher through Cargo configuration outside this helper slice.

Run the helper tests through the same imports D10b uses:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib working_drafts::manifest_tests
```

### D12 working-draft read executor

GRE-607 adds `WriterOwner::working_drafts_with_policy` behind the default-off
`desktop_rebuild` feature. The factory takes an explicit `AuthPolicy` and
returns `WorkingDraftClient`. Its `execute` method accepts a credential, profile
`PositiveId`, typed `Request`, cancellation flag, and queue wait. The requests
are `List`, `Read`, `Diff`, and `Workspace`.
They use the existing writer queue and one deferred transaction. Authentication
observes the actor without updating usage timestamps.

`Read` returns a `DraftDocument` with a validated `DraftIdentity`. `List` orders
identities by creation time and then ID. A missing or unowned profile returns
`ProfileNotFound`, and a missing draft returns `DraftNotFound`, as Node's
`PlanDraftWorkspaceService.read` and draft list route do. Draft loading reuses
the required-units reader, including its snapshot digest and selected
reconciliation validation. Document, diff, and workspace reads reject
incomplete rebase origins. `Workspace` reuses the required-units workspace
builder. No required-units reader logic changes.

`Diff` returns the complete input and Part diff against the draft's base
revision. Both the revision ID and Plan version determine `baseIsCurrent`.
Part rows sort by serialized ID as Node does. `DraftDocument` and `DraftDiff`
retain manifest strings as UTF-16 and serialize lone surrogates as JSON escapes.
Use their `into_json_body().into_bytes()` methods. The complete outcome uses
`Outcome::into_json_body()?.into_bytes()`.

This first core unit ports the read-only seam. The writes section below covers
Part edits and abandon. Preparation and recompute have their own sections. The
ordinary Save writer is GRE-861 (below). Rebase and siblings remain GRE-608.
There is no HTTP route or default-feature change.

The operation tests call the public client against Node-written databases.
Their literal expected JSON comes from `getPlanDraft`, `listPlanDraftIdentities`,
`diffPlanDraft`, and `PlanDraftWorkspaceService.read`. The UTF-16 case uses
`digestPlanDraft` and `diffPlanDraftSnapshot` directly because Node's SQLite
adapter cannot read the Rust PPJS blob encoding. The generator records the SQL
setup for each temporary database. It changes immutable identity or lineage
fields only after dropping the corresponding trigger in that temporary copy.

Regenerate the fixtures from the repository root with Node 22 and installed web
dependencies. The generator writes databases in the system temporary directory
and removes them when it finishes.

```sh
node --conditions=development --import ./web/node_modules/tsx/dist/loader.mjs \
  rust/crates/pp-storage/tests/fixtures/working-drafts-node.mjs web/apps/server \
  > rust/crates/pp-storage/tests/fixtures/working-drafts-node.jsonl
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test working_draft_core
```

### D12 working-draft writes

`WorkingDraftClient` has two write methods. Each one queues a `Write` command
on the existing single writer. The writer runs `abandon` in one immediate
transaction and `edit` in up to two. Each transaction has its own admission
deadline, and the writer checks cancellation before each transaction starts and
again before it commits.

- `edit` ports `PlanDraftWorkspaceService.editParts` and the repository's
  `editPlanDraftPartsBatch`. It returns the `required_units::Outcome` result.
  A changed edit clears the selected Required-unit reconciliation and writes
  the new snapshot digest and Part rows. An edit that changes no Part returns
  the current draft, even when the expected digest is stale. Both paths then
  select Required units with no decisions and the key `auto-<snapshot digest>`,
  as Node's `autoReconcile` does. The selection runs in a second transaction
  after the edit commits. A retry of the same edit finds the saved selection
  and writes nothing.
- `abandon` ports `PlanDraftWorkspaceService.abandon` and the repository's
  `transitionPlanDraft` for `abandon`. It returns `AbandonOutcome`. A retry
  with the same expected lifecycle version returns the abandoned draft and
  writes nothing.

Both methods return `profile_not_found` and `draft_not_found` as separate
outcomes. A stale snapshot digest or lifecycle version returns `draft_changed`
with the current workspace. A consumed or abandoned draft returns `not_open`.

`EditRequest` and `AbandonRequest` accept the JSON bodies that Node's
`parseEditPlanDraftPartsRequest` and `parseAbandonPlanDraftRequest` accept.
An edit body has either one `decision` or 1 to 10,000 `decisions`. Each
decision names at least one Part, and a batch edits each Part field at most
once. A quantity override is `null` or an integer from 1 to 10,000.

If the selection fails or is cancelled, the edit stays committed and `edit`
returns the error, as Node does. Node's route answers that error with HTTP 500
and `internal_error`. A retry of the same edit changes no Part, so it only runs
the selection again.

Resume has no Node route or service caller, so it is not ported.

The operation tests replay each Node case on a copy of the required-units
`unchanged` database. They restart the writer between calls and compare every
outcome and the draft, Part, reconciliation, and assignment rows with Node's.
Some cases run SQL between calls, for example to remove an injected selection
failure or to advance the accepted base.
The generator runs Node's service and contract parsers. Regenerate the
fixture from the repository root:

```sh
node --conditions=development --import ./web/node_modules/tsx/dist/loader.mjs \
  rust/crates/pp-storage/tests/fixtures/working-draft-writes-node.mjs web/apps/server \
  > rust/crates/pp-storage/tests/fixtures/working-draft-writes-node.jsonl
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test working_draft_writes
```

`rust/crates/pp-storage/tests/fixtures/mutate-working-drafts.py` runs the
read and write mutants against both test files. It copies the Rust workspace
into a temporary directory under `rust/target`, links the real `web` directory
beside it for compile-time includes, and builds there with its own
`CARGO_TARGET_DIR`, so it never changes the build artifacts in `rust/target`.

### D12 preparation prerequisites

GRE-607 adds the remaining manifest helpers and the accepted-base reuse check
behind `desktop_rebuild`. `optional_document` ignores missing, skipped, and
invalid optional manifests, but it checks cancellation first, and after an
invalid parse, so a cancelled attempt never reads as an absent manifest. `kit` projects stored kit selections through the
existing `KitManifest` parser. Both propagate preparation budget and cancellation
failures. `apply` preserves Node's document order, project filtering, selection
defaults, explicit group membership, group bounds, and exclusion precedence.
It accepts the shared `PreparationBudget`, charges each Part as an entry, and
charges each pattern comparison as a visit. Option-group merging, in `apply` and
in preparation's repository, hinted and inferred groups, charges a visit for each
group, variant, and path it merges and for each merge's final sort, plus the
entries each is compared against, or sorted, as `comparison_work`. `apply` copies
each document's groups one group, variant, and path at a time, charging a visit
before each copy. Builder observation merges the same way without a charge. A comparison keeps no memory after it
returns, so it does not charge the document-byte cap. It charges the separate
`comparison_work` counter instead. Before a comparison it charges the raw
pattern UTF-16 units plus the key's UTF-8 bytes, because normalization scans
the raw text before trimming. Each comparison builds its own `Glob`, so after
the comparison it charges the normalized pattern and key UTF-16 units plus the
glob matcher steps that `Glob` counted. The default and maximum is 2^28 units, so
comparisons at the 100,000-visit limit can average about 2,680 units each. One
comparison costs O(P + K²) units for a pattern of P units and a key of K units,
with K at most 4,096. Only the post-comparison charge can pass the limit, so an
attempt can exceed it by one comparison's normalized and matcher work. `kit`
reads stored kit JSON with `manifest_options::observation::stored_kit`, the
parser builder observation uses. It charges the raw bytes as a document and each
parsed value and object key as a node, and the last duplicate JSON key wins, as in
Node's `JSON.parse`. UTF-16 selection text uses the existing codec.

`read_model::reusable_draft_base` checks current freshness before validating the
complete accepted snapshot with the existing bounded readers. It propagates
integrity errors and checks preparation cancellation around those reads.
`catalog::naming::get` is crate-visible for the next preparation port's
transactional effective naming profile and digest. Its behavior is unchanged.
The preparation module below consumes these helpers.

The committed Node generator covers manifest application, optional documents,
stored kit selections, freshness, and accepted-base integrity. Regenerate its
literal outputs twice and compare them before replacing the fixture:

```sh
node --import ./web/node_modules/tsx/dist/loader.mjs --conditions=development \
  rust/crates/pp-storage/tests/fixtures/preparation-prereqs-node.mjs web/apps/server \
  > rust/target/preparation-first.jsonl
node --import ./web/node_modules/tsx/dist/loader.mjs --conditions=development \
  rust/crates/pp-storage/tests/fixtures/preparation-prereqs-node.mjs web/apps/server \
  > rust/target/preparation-second.jsonl
cmp rust/target/preparation-first.jsonl rust/target/preparation-second.jsonl
cmp rust/target/preparation-first.jsonl rust/crates/pp-storage/tests/fixtures/preparation-prereqs-node.jsonl
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib -E 'test(preparation_tests)' --test-threads=2
python3 rust/crates/pp-storage/tests/fixtures/mutate-preparation-prereqs.py
```

The mutation runner copies the workspace and uses a separate `CARGO_TARGET_DIR`.
It requires a passing baseline and counts only executed test failures as kills.

### D12 working-draft preparation

`working_drafts::preparation` is crate-private behind default-off
`desktop_rebuild`. It captures Source inputs and fingerprints, scans and merges
STLs, preserves accepted decisions, applies scopes and manifests, and returns
Parts and a planning digest without writing rows. Validated current accepted
Parts can bypass scanning. Its options use storage-local types, and reads use
storage-issued observation requests with the shared preparation budget.
Naming rules charge their definitions and per-path comparisons. Spoolman
defaults follow user and Plan feature choices; disabled or invalid choices drop
Spoolman references from new Parts without changing accepted decisions.
Recompute consumes it. Ordinary Save is GRE-861 (below). Rebase and siblings
remain GRE-608. Existing GRE-668, GRE-669 and GRE-670 follow-ups remain open.

Quantity JSON uses the existing `ryu-js` formatter at the shared Part encoding
boundary, including the bytes sorted and hashed by the planning digest.
Filename quantities retain JavaScript's binary64 rounding. For example,
`9007199254740993` becomes `9007199254740992`, `1000000000000000128` prints as
`1000000000000000100`, `1e20` prints as `100000000000000000000`, and `1e21`
prints as `1e+21`. This does not expand the stored integer range. Recompute
still refuses unsafe quantities atomically, preserving existing draft rows
and STL files. This code remains behind default-off `desktop_rebuild`; product
callers still check the `PP_DESKTOP_REBUILD` environment value at startup.

Preparation uses `pp_domain::build_identity::trim_js` for import-rule JSON,
rule and path normalization, blank naming markers, decimal `parseInt` prefixes,
role-default lookup and Spoolman color prefixes. JavaScript trims U+FEFF and
ordinary whitespace, and preserves U+0085. The prototype-slug decision remains
on GRE-697.

`working_draft_preparation_trim` compares public-client Parts against literal
Node 22.23.3 outcomes from disposable databases and STL files. Its two named
tests cover filtered, unfiltered and wrapped-JSON imports, naming and color
defaults. Cancellation and invalid U+0085 quantity prefixes preserve draft rows
and files. The storage module retains its default-off compile gate; product
callers retain the startup `PP_DESKTOP_REBUILD` environment check.

```sh
npx --yes --package=node@22 node --import ./web/node_modules/tsx/dist/loader.mjs --conditions=development rust/crates/pp-storage/tests/fixtures/preparation-trim-node.mjs web/apps/server > /persist/tmp/preparation-trim.jsonl
cmp /persist/tmp/preparation-trim.jsonl rust/crates/pp-storage/tests/fixtures/preparation-trim-node.jsonl
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test working_draft_preparation_trim
```

Tests compare literal Node fingerprints, Parts and digests, propagate resource
refusals, and cancel at every budget checkpoint. Regenerate the fixtures twice:

```sh
node --import ./web/node_modules/tsx/dist/loader.mjs --conditions=development rust/crates/pp-storage/tests/fixtures/working-draft-preparation-node.mjs web/apps/server > /persist/tmp/preparation-first.jsonl
node --import ./web/node_modules/tsx/dist/loader.mjs --conditions=development rust/crates/pp-storage/tests/fixtures/working-draft-preparation-node.mjs web/apps/server > /persist/tmp/preparation-second.jsonl
cmp /persist/tmp/preparation-first.jsonl /persist/tmp/preparation-second.jsonl
cmp /persist/tmp/preparation-first.jsonl rust/crates/pp-storage/tests/fixtures/working-draft-preparation-node.jsonl
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib -E 'test(working_drafts::preparation_tests)' --test-threads=2
python3 rust/crates/pp-storage/tests/fixtures/mutate-working-draft-preparation.py
```

The mutation runner copies the workspace into `/persist/tmp` with its own target
directory. Only an executed failing test counts as a kill.

### D12 working-draft recompute

`WorkingDraftClient::recompute` ports Node's `recomputePlanDraft` behind
default-off `desktop_rebuild`. Build the client with
`WriterOwner::working_drafts_with_observations`, which takes the Source reads
and preparation limits. A client from `working_drafts_with_policy` refuses
recompute. `RecomputeRequest::new` takes the `Idempotency-Key` header and the
route's `apply_manifest` flag. It trims the key as JavaScript does, refuses a
blank key with Node's message, and refuses a key over 160 UTF-16 units as the
route does. When `apply_manifest` is false, preparation keeps the current
inclusion and prefers the accepted Parts, as Node's service asks.

A recompute runs in three steps on the writer thread:

1. A read transaction resolves the actor and Plan. It returns the saved draft
   for a repeated key, or `idempotency_conflict` when that key named a rebased
   draft. It refuses a Plan that needs an accepted baseline.
2. Preparation runs outside any transaction with a fresh budget. No layers and
   no STLs become `no_layers` and `no_stls`. A request cancelled before either
   step returns its outcome fails as cancelled, as the write transaction does.
3. An immediate transaction with its own admission deadline checks the
   principal and the key again. A hidden Plan or a moved accepted base is
   `base_changed`. A changed input fingerprint is `inputs_changed`. It then
   copies quantity overrides, and inclusion when the draft is kept, from open
   drafts, as Node's `carryOpenDraftEdits` does. Before it loads an open draft,
   it charges the preparation budget for everything the load reads. That is
   one visit per draft, input, Part and decision, plus two for the selected
   reconciliation. Shared explicit column lists drive both the loaders and
   their cost queries. Each loaded column costs its byte length, including
   identifiers and metadata. Each assignment also costs 1,024 comparison units
   and uses no visit. A 200,000-assignment draft fits the default limits; two
   such open drafts exhaust the default grant. Before their bytes and other
   work, the 268,435,456-unit comparison cap permits at most 262,144 assignment
   rows across carried drafts. Each table load retains its 200,000-row cap,
   and structural work stays capped at 100,000 visits.
   On the Linux calibration VM, two runs of five alternating release trials
   measured a median 1,929.5 ms for a 200,000-assignment carry and 12.8 ms
   without a carry. The difference is 9,583 ns per assignment. The slowest
   carry, 2,431.6 ms, projects to about 3.2 seconds for the default grant's
   assignment ceiling.
   This is a work bound; elapsed time depends on the machine and scheduler.
   Refusing two drafts with 200,000 assignments took 2,381.8 ms in one run.
   Carry loading and budget refusal hold the writer lock for that whole
   duration. GRE-706 must size the caller's wait and UX for multi-second
   lock occupancy; there is no elapsed-time guarantee.
   Each cost query streams indexed rows and sums individual column lengths
   without concatenation, aggregation or sorting. The open-draft ID list,
   the cost scan, and the loader all check cancellation between rows. JSON
   parsing and serialization also check cancellation. Large JSON arrays
   retain raw rows and validate one at a time, so cancelling does not free
   a whole parsed object tree. Cap-sized carry tests assert cancellation
   and unchanged rows; they do not bound wall-clock latency (a prior
   `<50 ms` release sweep had little headroom under load). SQLite hooks
   remain a test-only dependency. A limit or cancellation refusal rolls
   the transaction back.
   It abandons
   every open draft, increments their lifecycle versions, inserts the draft,
   inputs and Parts, and returns `created` with the saved snapshot.

`RecomputeOutcome::into_json_body` writes Node's result kinds.
`RecomputeOutcome::WouldWipe` and `RebaseOutcome::WouldWipe` both serialize
`{"kind":"would_wipe"}`. Preparation drops a reported row whose key does not
end in `.stl` before the `no_stls` check, matching Node's `scanRepo`. After
that check `prepare` cannot return `would_wipe`, and Node cannot either.

`WorkingDraftClient::recompute_and_select` ports Node's
`PlanDraftWorkspaceService.recompute`. It runs the recompute above and, after
a created or existing draft, selects Required units as `edit` does. The
selection has no decisions, uses the key `auto-<snapshot digest>`, and runs in
its own immediate transaction with a fresh admission deadline. A draft that
already has a selection is not selected again; a read transaction returns its
workspace. Either path returns `RecomputeWorkspaceOutcome::Selected` with the
`required_units::Outcome`. Every other recompute result is `Refused`.
The selection checks cancellation before it starts and before it commits, but
it does not use the preparation budget, as `edit` does not. If the selection
fails or is cancelled, the recompute stays committed and the method returns
the error. Node's route answers that error with HTTP 500 and `internal_error`.
A retry with the same key finds the saved draft, so it only runs the selection
again.

The operation tests replay each Node case on a copy of the required-units
`unchanged` database and compare outcomes and the `plan_drafts`,
`plan_draft_inputs`, `plan_draft_parts`, reconciliation and assignment rows
with Node's. The selecting cases run Node's service: created then replayed,
existing, an injected selection failure then a retry, and a draft selected
before the recompute. Creation times are
compared by format. Cases that change the database between the transactions
run their SQL after Node's preparation returns. The Rust test reads run it when
the first STL scan starts. After that scan, preparation still reads settings,
Source rows, and whether the Plan exists. Only the hidden-Plan case changes one
of those, and it only turns off Spoolman defaults, which the fixture does not
set. Both sides therefore prepare the same Parts.
Two cases (GRE-479) start from an accepted Part with curated choices: a role
override, filament color, custom hex, Spoolman spool and notes, on top of the
fixture's quantity override and exclusion. Their setup rewrites the accepted
revision's Part, its digest (Node's `digestPlanRevisionParts`) and its legacy
projection Part, so the revision still validates. A recompute that applies the
manifest and one that keeps the draft both copy every choice to the new draft.
Rust-only tests cover cancellation, a budget refusal, open drafts whose Parts,
selected reconciliation or any loaded column exceed the budget when their edits
are carried, a cancelled or unauthenticated request, a client without
observations, and cancellation during the selection. That test holds the
selection with a slow trigger, cancels, compares the rows with Node's committed
recompute, and then retries. Regenerate the fixture twice:

```sh
node --import ./web/node_modules/tsx/dist/loader.mjs --conditions=development rust/crates/pp-storage/tests/fixtures/working-draft-recompute-node.mjs web/apps/server > /persist/tmp/recompute-first.jsonl
node --import ./web/node_modules/tsx/dist/loader.mjs --conditions=development rust/crates/pp-storage/tests/fixtures/working-draft-recompute-node.mjs web/apps/server > /persist/tmp/recompute-second.jsonl
cmp /persist/tmp/recompute-first.jsonl /persist/tmp/recompute-second.jsonl
cmp /persist/tmp/recompute-first.jsonl rust/crates/pp-storage/tests/fixtures/working-draft-recompute-node.jsonl
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test working_draft_recompute --test-threads=2
cargo nextest run --release --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test working_draft_recompute -E 'test(cap_sized_assignment_selection) | test(two_cap_sized_drafts) | test(cancellation_before_recompute_of_a_cap_sized)'
node --import ./web/node_modules/tsx/dist/loader.mjs --conditions=development rust/crates/pp-storage/tests/fixtures/working-draft-recompute-node.mjs web/apps/server --large-carry
python3 rust/crates/pp-storage/tests/fixtures/mutate-working-draft-recompute.py
```

The mutation runner copies the workspace into `/persist/tmp` and reuses
`CARGO_TARGET_DIR` when supplied. Run it separately from other Cargo commands
when you reuse a target directory. Only an executed failing test counts as a kill.

### D13 ordinary Save writer

GRE-861 ports `WorkingDraftClient::save` behind default-off `desktop_rebuild`
(#1063, `ccc32252`). Callers build `save::SaveCommand` with the session or API
key, Plan profile, `SavePlanChoicesRequest`, and idempotency key (trimmed;
non-empty; at most 200 UTF-16 units). The client needs observations from
`WriterOwner::working_drafts_with_observations`. Service callers also require
`PP_DESKTOP_REBUILD` exactly `1` or `true`. There is no HTTP route in this
slice. Manifest, rebase, and siblings remain GRE-608.

A Save runs on the writer thread:

1. An immediate transaction admits the principal, resolves preparation and
   Required-unit reconciliation, applies Plan choices, publishes through
   `AuthenticatedPublicationContext`, closes the expected open draft when
   present, and writes the atomic `ApplyPlanDraftReceipt`. Stale, missing, or
   colliding inputs refuse without writes. A publication failure rolls the
   snapshot, reconciliation, and Source closure back together.
2. After commit, capture builds the published authority. If that capture fails,
   the receipt and closed-draft IDs stay committed and surface as
   `CommittedSaveCaptureFailure`; a retry with the same key replays the
   receipt without a second publication. Cancellation or a preparation budget
   limit before commit leaves the database unchanged.

Focused proofs live in `working_draft_save` (atomic receipt and exact-key
replay, changed-payload refusal, publication rollback, post-commit capture
retry, Source budget and cancellation). Default builds omit the module; run
package checks with and without `desktop_rebuild`:

```sh
cargo check -p pp-storage --manifest-path rust/Cargo.toml --locked --no-default-features
cargo check -p pp-storage --manifest-path rust/Cargo.toml --locked --features desktop_rebuild --tests
cargo nextest run -p pp-storage --manifest-path rust/Cargo.toml --locked --features desktop_rebuild \
  -E 'binary(working_draft_save)'
```

### D29 failed ordinary-Save intent (client)

GRE-811 retains an immutable failed ordinary-Save command (payload and
idempotency key) in `PlanWorkspaceContext` for deliberate retry. GRE-812 keeps
that intent per Plan across Build navigation: switching Plans does not apply one
Plan's failed intent to another, and ordinary `preparePlan` / navigation does
not automatically resend a definitive rejection. Deliberate Retry uses
`retryPlanSave`. Web-only; no new Rust `desktop_rebuild` surface. Focused proofs:
`failed_save_survives_navigation_without_resend`,
`failed_save_is_isolated_by_plan` in `PlanSelection.integration.test.tsx`.

```sh
npm run test -w @print-partner/web -- src/context/PlanSelection.integration.test.tsx src/context/PlanWorkspaceContext.test.tsx
```

### D29 failed special-request and manifest Save intent (client)

GRE-813 retains failed Build special-request and kit-manifest payloads for
deliberate retry. Failed manifest saves keep the exact kit body from the
refused request; a later retry does not rebuild from newer UI selections until
the user makes a new edit. `BuildSaveFlushContext` allows multiple flush
producers per Build so special-request and manifest autosave can register
independently. `retryPlanSave` replays retained ordinary-Save intent from
GRE-812, then retries auxiliary failures in first-failure order (whichever
kind failed first runs first when both failed). A failed retry does not stop
the rest: every retained save runs, then the first failure is rethrown. When a
manifest failure is retained, `retryPlanSave` skips its own Plan prepare,
because the manifest retry prepares with `applyManifest` once its PUT lands.
Web-only; no new Rust `desktop_rebuild` surface.

GRE-991 keys the retained kit by Build. `KitManifestOptions` remounts per Build,
so the editor mounted for a Build adopts its refused kit. It shows those
selections with the error status, and it owns the retry that `retryPlanSave`
runs. Since GRE-814, Leave-Build flushes no longer resend it. The hook detects a refused PUT from the PUT
itself, so a failed Plan refresh on an earlier save does not hide a later PUT
failure. Focused proofs: `failed_manifest_save_keeps_exact_payload` and
`remounted_editor_owns_retained_manifest_retry` in
`useKitManifestAutosave.test.ts`; `failed_special_request_retry_preserves_order`
in `PlanSpecialRequestField.test.tsx`;
`retry_with_failed_manifest_prepares_the_plan_once` in
`PlanWorkspaceContext.test.tsx`.

```sh
npm run test -w @print-partner/web -- src/hooks/useKitManifestAutosave.test.ts src/lib/planAuxiliarySaveIntent.test.ts src/components/PlanSpecialRequestField.test.tsx src/context/BuildSaveFlushContext.test.tsx src/components/KitManifestOptions.navigation.test.tsx src/context/PlanWorkspaceContext.test.tsx
```

### D29 flush honors deliberate retry (client)

GRE-814 separates an ordinary flush from a deliberate Retry in
`BuildSaveFlushContext`. Producers register a flush that takes a mode.
`useFlushBuildPageSaves` runs `"flush"`. Leave-Build guards, Build switching,
imports, editor unmounts and hidden-tab flushes all use it. It settles pending
edits and waits for in-flight saves. When a special-request or manifest save
was refused, it rejects with that failure and sends nothing. Navigation guards
therefore keep the Build open, as they do for a definitive ordinary-Save
rejection in `preparePlan`. `useRetryBuildPageSaves` runs `"retry"`, which
resends the exact retained payload and then settles pending edits. The
special-request and kit-manifest Retry buttons, and `retryPlanSave` through
`retryFailedAuxiliarySaves`, are deliberate retries. GRE-815 wires
`useRetryBuildPageSaves` into the Parts and command actions. This change is
web-only and adds no new Rust `desktop_rebuild` surface. Focused proofs:
`flush_does_not_resend_failed_intent` and
`explicit_retry_flushes_retained_request` in `BuildSaveFlushContext.test.tsx`.

```sh
npm run test -w @print-partner/web -- src/context/BuildSaveFlushContext.test.tsx src/hooks/useKitManifestAutosave.test.ts src/components/KitManifestOptions.navigation.test.tsx src/components/PlanSpecialRequestField.test.tsx
```

### D29 explicit retry actions (client)

GRE-815 wires the deliberate Retry into the Plan page and the command palette
through one hook, `useFailedSaveRetry`. The Plan page shows its recovery alert
and "Retry save" when the selected Build has a failed ordinary Save or a
retained special-request or kit-manifest refusal. The alert shows that
failure's message. The palette lists "Retry failed save" first under Actions in
the same cases. It closes the palette, then reports "Saved" or the refusal
message. Retry calls `retryPlanSave` only when an ordinary Save failed, because
with nothing retained it prepares a new Plan draft. Otherwise it resends only
the retained auxiliary saves through `retryFailedAuxiliarySaves`. It then runs
`useRetryBuildPageSaves` to settle the mounted editors. If the resend is
refused, Retry still settles the mounted editors, through
`useFlushBuildPageSaves`, so unsent edits are saved but the refusal is not sent
a second time. A refused kit-manifest Retry keeps the manifest retained under
the newest refusal message. Retry never calls `preparePlan`. The Plan page Retry uses `aria-disabled` while a save runs, so
keyboard focus stays on it and a refused retry keeps the alert. The
auxiliary-save store is now observable (`useFailedAuxiliarySaveMessage`) and
records each refusal's message. This change is web-only and adds no new Rust
`desktop_rebuild` surface. Focused proofs: `parts_retry_uses_retained_save` in
`PartsPage.recovery.test.tsx` and `command_retry_does_not_create_new_intent` in
`CommandPalette.retry.test.tsx`, plus
`plan_retry_resends_retained_manifest_with_failed_ordinary_save` and
`manifest_retry_refusal_updates_retained_message`.

```sh
npm run test -w @print-partner/web -- src/pages/PartsPage.recovery.test.tsx src/components/CommandPalette.retry.test.tsx src/lib/planAuxiliarySaveIntent.test.ts src/components/PlanSpecialRequestField.test.tsx src/hooks/useKitManifestAutosave.test.ts
```

### D29 draft lifecycle replay and cancelled selection

GRE-816 adds regression proofs for behavior already on main; it changes no
production code. `abandon` writes only the `plan_drafts` row and starts no
Required-unit selection. Retrying it with the same lifecycle version returns
the same `ready` identity, before and after a later Save, and writes nothing. A
Save that still names the open draft refuses with `draft_changed` and writes
nothing; a Save without a source publishes and leaves the abandoned draft as it
was. When the selection after `edit` is cancelled, the edit stays committed
and unselected, survives a writer restart, and a retry of the same edit only
selects. Default builds omit the target.

```sh
cargo nextest run -p pp-storage --manifest-path rust/Cargo.toml --locked --features desktop_rebuild \
  -E 'binary(working_draft_lifecycle_replay)'
```

### D29 provider recovery with retained intent (client)

GRE-817 drives the real `PlanWorkspaceProvider`, router and Plan page against
fixture endpoints. A refused file choice stays pending, and the Plan page's own
`preparePlan` used to flush it, which resent the refusal each time the page
mounted. With the rebuild health capability (`PP_DESKTOP_REBUILD` set to `1` or
`true`), `preparePlan` now refuses from the retained intent instead. Leaving
and reopening the Plan route sends nothing, and one Retry click publishes the
original request and key once. Flag-off keeps the earlier flush. Focused
proofs: `provider_recovery_never_auto_retries_failed_save` and
`provider_explicit_retry_publishes_once` in
`PartsPage.provider-recovery.test.tsx`, plus a flag-off control.

```sh
npm run test -w @print-partner/web -- src/pages/PartsPage.provider-recovery.test.tsx
```

### D4 media reconciliation

GRE-599 reconciles media already shipped in #131. The `pp-source` media library, the `pp-source-media` CLI, and all unconditional #131 exports remain available in the default build. This slice adds a historical audit and documentation. It adds no feature behavior, host, runtime API, or dependency.

Run the read-only audit from the repository root:

```sh
python3 rust/scripts/audit-d4-media.py
```

The audit uses fixed revisions and does not follow the current `HEAD`:

- Landed #131 is `1beaa1cc09622611505b6470402027053db10d91`.
- The reference is `d73b6d8d250b84e22b37262a0c2720d4f777a0ca`.
- The audited snapshot is `2e3cac927a1a82cf2d1a276f9564772f6cd67ee4`.

Within `rust/crates/pp-source/`, the inventory covers `src/media.rs`, `src/bin/pp-source-media.rs`, `tests/media.rs`, `tests/direct_model_binding.rs`, `tests/support/process.rs`, and `src/archive.rs`. It also covers the union of paths under `rust/tests/source-media/` at all three revisions. Every audited file must equal its #131 blob. The shared media test and CLI equal the reference. All nine probe files equal the reference except `verify-zip64.py`.

The direct-model binding test and its process helper are intentional #131 additions absent from the reference. `archive.rs` is a dependency already reconciled in #131. Its reference version lacks `read_bound_entry`. The audit checks the landed archive blob identity without classifying the archive implementation as D4. The pinned `lib.rs` module prefix and Cargo feature section also confirm unconditional media and an empty `desktop_rebuild` feature.

On success, the audit prints the commit IDs, each file's blob identity and reference delta, and unified reference-to-audited diffs. The three media hunks and the probe changes have these reasons:

- Model reads use #131's `local_header_offset`, `inflated_bytes`, and `archive::read_bound_entry` to bind model bytes to the validated ZIP64 payload. `readBytes` includes the inflated model length. Preserve `direct_model_redirect_is_rejected_before_publish_and_valid_models_still_convert` in `tests/direct_model_binding.rs`, which rejects redirected reads before publication and still converts valid models.
- Grammar compilation reports `3MF grammar regex is invalid` instead of an unnamed unwrap panic. Missing attribute or entity captures return `MediaError::Invalid`. The regex patterns remain identical to the reference.
- `radix_number` returns `NaN` for a non-digit instead of unwrapping a failed digit conversion.
- `verify-zip64.py` sends the inflated budget through `archiveLimits` and asserts `archive-limit`. The reference sends that archive budget through media `limits` and does not assert the error code.

Exit 0 means all identity checks passed. Exit 1 reports a file, inventory, or gate mismatch. Exit 2 reports missing Git, missing historical objects, or a Git command failure. Failures go to stderr without a success summary. A checkout without the pinned history must fetch history containing those full commit IDs before rerunning. The audit never fetches, checks out files, writes files, or runs Cargo. Blob equality proves historical file identity, not a fresh probe execution. Later media changes do not invalidate this fixed audit.

The existing full source suites cover media, direct-model binding, and archive behavior with the feature off and on:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-source
cargo test --locked --manifest-path rust/Cargo.toml -p pp-source --features desktop_rebuild
```

D5 retained capture and D28 ZIP acquisition label policy remain separate slices. GRE-784 adds the synchronous supplied-import client; the rest of D3 source acquisition remains deferred. Local selection is outside this audit. This audit introduces no user-visible path and needs no runtime gate. Future rebuild callers must compile behind `desktop_rebuild` and check `PP_DESKTOP_REBUILD` for exactly `1` or `true`.

### D7 catalog core

GRE-600 adds explicit catalog requests behind `desktop_rebuild` to the existing
`SourceCatalogClient`, writer queue and transaction-owned catalog state.
`CreateCatalogSource` and `UpdateCatalogSource` normalize GitHub locations;
creation requires model URLs for Printables, MakerWorld and Thangs.
Legacy `Create` and `Update` retain their existing behavior in both builds.
`SourceActivity` reads tenant-scoped events with a limit between 1 and 100.
Category requests return typed categories and trees, and numeric category updates
retain each finite selector's success or failure after deduplication.
JSON numeric selectors and activity IDs retain their exact JSON numbers. Only
integer selectors represent mutable IDs. Native finite floats normalize to integer
selectors only within the JavaScript safe integer range; other floats retain their
failure echoes. GitHub owner and repository segments reject delimiters and dot
segments before normalization. Category validation returns typed input errors with
the feature enabled and retains legacy error messages with it disabled. These are
intentional safety corrections to the D7 reference. Storage errors stay distinct.
`catalog_access` and `catalog_key_access` bind an explicit authentication policy.

With the feature enabled, import-rule bodies have an 8 MiB limit; other requests
retain the 1 MiB limit. The default build retains its 1 MiB limit for all requests.
The existing schema, JSON validation, writer deadlines, work leases and protected
delete audit commits remain unchanged. Naming additions and broad catalog tests
belong to D8. Sync configuration versions, reservation proofs and upload guards
remain deferred to the slices that introduce their consumers.

No runtime, HTTP or UI caller selects these operations. Future user-visible callers
must compile behind `desktop_rebuild` and check the stored `PP_DESKTOP_REBUILD`
setting for exactly `1` or `true` before using them.

### D8 naming preview

GRE-601 adds `PreviewNaming` behind `pp-storage/desktop_rebuild`. The request
accepts a relative path and an optional typed partial profile. Missing override
fields inherit tenant global defaults. Explicit null fields and unknown fields
are rejected. Preview uses the existing catalog authentication policy and writer
transaction without saving naming settings or accessing files.

Folder rules take priority over markers. Markers use descending UTF-16 length
within each path segment. Slug stripping follows configured marker order and
restores the original name if stripping empties it. Quantity captures retain the
reference's numeric-prefix parsing, minimum of one, and nullable nonfinite result.
The default build excludes this request and retains existing naming behavior.

The rebuild CI gate runs the feature-on catalog suite. This storage unit adds no
runtime, HTTP or UI caller. Future user-visible callers must also check
`PP_DESKTOP_REBUILD`. Tests for later source-work and capture APIs remain deferred.

### D8 naming error classification

GRE-601 classifies naming failures behind `pp-storage/desktop_rebuild` using
existing catalog requests. Invalid profiles submitted to `SaveNaming` or
`SaveGlobalNaming` return `CatalogFailure::Input` with the existing validation
message. `GetNaming` and `SaveNaming` return `CatalogFailure::NotFound` for
missing or foreign-tenant sources. Authentication still runs before source
lookup and profile validation.

`GetNaming` classifies errors projecting a stored override as
`CatalogFailure::InvalidStoredNaming`, retaining the original error as its cause.
Source lookup and global-setting reads happen outside that classification, so
SQL failures retain their original type. Invalid global settings still fall back
to defaults without repairing storage. `SaveNaming::UseDefaults` can replace a
corrupt source override while preserving unrelated metadata.

The catalog tests exercise validation, missing sources, corrupt stored overrides,
SQL abort rollback, normalization, digest equivalence, global fallback, tenant
isolation and persistence after restart. Default-build error text and naming
behavior stay unchanged. This unit adds no user-visible caller. Future callers
must also check `PP_DESKTOP_REBUILD`.

The remaining reference `catalog/tests.rs` cases need APIs absent from main:
issued-token retirement and incarnation proofs, repeated local-scan completion,
captured-claim activation, orphan-source-work reaping, and pending/resolved lease
release across expiry and cancellation. Those tests depend on the matching
source-work state and job capture/settlement APIs. Port them with those owners
when the APIs land; do not emulate them in D8 or start GRE-602/D9 here. GRE-601
remains In Progress until its final slice.

### D6 auth scheduling

GRE-605 adds `WriterOwner::rebuild_auth_with_policy(policy, lifetimes)` behind
`desktop_rebuild`. This explicit library constructor returns `RebuildAuthClient`.
The existing `auth` and `auth_with_policy` constructors retain their existing pool
and default session/key lifetimes with the feature off or on. No runtime caller
selects the rebuild client. D23/D26 must check the stored `PP_DESKTOP_REBUILD`
setting before selecting it.

The rebuild client admits at most 34 flows and sends only password hashing and
verification to two KDF workers. SQL stages run through the storage writer;
status, session resolution, logout and key operations can progress while both KDF
workers are occupied. Ready auth work has a separate queue and alternates with ordinary writer work,
so it cannot consume ordinary write capacity. Full KDF queues retry without
blocking that writer. A login that loses a credential comparison retries once
by reloading the same user and re-verifying the supplied password outside the writer. Shutdown rejects late KDF
results. Credential compare-and-swap checks remain in the shared SQL executor.
`AuthLifetimes::shorter` supports shorter sessions and optional key expiry.

The port preserves main's `User::has_password`, HMAC key hashing, hostile-cost
checks and legacy constructors. The reference's subscription revalidation calls
remain deferred to D15, where subscriptions are introduced. Native-secret and
durable-authority support is the other D6 PR. Neither PR completes GRE-605 until
both clear the coordinator's exact-head Grok Build gate.

### D5 retained capture

GRE-603 ports the full reference `pp-source::retained_capture` module behind `desktop_rebuild`, including its eight embedded tests. Deliberate fixes beyond the reference track allocation cleanup receipts, reject malformed frozen layouts, and validate inventory and transfer paths before I/O or staging. Minimal support adds `local_selection::InputFile`, inventory and capture helpers, pinned-identity directory operations, and optional `getrandom`. Transfer atomically refuses existing import operations and creates destination files exclusively. Failed transfers can leave staging. The separate 20-test reference suite passed locally and is excluded from this slice; a compact public test covers lifecycle, reopen, inventory, transfer, malformed reopen, entry limits, path collisions, and existing-operation sentinel preservation. Current root, ancestor, path, and ACL protections remain in place. No runtime callers, routes, or CLI commands use these APIs. Later integration remains deferred and must check `PP_DESKTOP_REBUILD` for exactly `1` or `true`. GRE-603 stays In Progress.

Final slot cleanup rechecks the pinned directory identity during allocation rollback, candidate abort, frozen removal, and deletion recovery. A slot replaced before that check returns an error and preserves the replacement. This is an identity check before unlink, not an atomic guarantee against concurrent replacement between the check and unlink.

## Directory layout resolver

GRE-344 adds `pp_core::layout` behind the default-off `desktop_rebuild` Cargo feature. `DirectoryLayout::resolve` takes a platform, home path, and `LayoutEnvironment` snapshot. It returns store, auxiliary app data, config, logs, and cache paths without filesystem access. `store` retains `~/.print-partner` outside portable mode, as required by [ADR 0005](adr/0005-desktop-product-identity.md). The existing database and `desktop.toml` belong to `store`. Outside portable mode, `app_data` holds auxiliary state. Portable mode places both `store` and `app_data` in `<root>/data`.

The desktop launcher selects the resolver's `store` only when its own `desktop_rebuild` Cargo feature is compiled and `PP_DESKTOP_REBUILD` is exactly `1` or `true`. With either gate off, it retains the legacy absolute-home check and `~/.print-partner` store. The debug-only `--data` override wins verbatim and skips environment and home lookup, including for a relative path. With both gates on, a present `PP_PORTABLE_DIR` skips home lookup even when the root is invalid. An invalid portable root returns the resolver error without a home fallback. Without a portable root, the enabled selector obtains home once and passes it to the resolver. The server continues to require `--data`. No data migration is included.

macOS auxiliary app data and config use `~/Library/Application Support/com.poitee.printpartner`. Logs use `~/Library/Logs/com.poitee.printpartner`, and cache uses `~/Library/Caches/com.poitee.printpartner`.

Linux auxiliary app data uses the `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, and `XDG_CACHE_HOME` roots. Missing or empty values default to `~/.local/share`, `~/.config`, `~/.local/state`, and `~/.cache`. Each path appends `com.poitee.printpartner`; logs also append `logs` under the state directory. A nonempty relative XDG value returns an error rather than choosing a directory relative to the process working directory.

`LayoutEnvironment::from_env` reads those variables, `APPDATA`, `LOCALAPPDATA`, and `PP_PORTABLE_DIR`. A supplied portable root takes precedence on every platform and appends `data`, `config`, `logs`, and `cache`. An empty portable root returns an error. Selected roots and the home path must be absolute and contain no parent traversal. The resolver preserves non-Unicode paths and does not canonicalize paths or follow links.

The launcher consumes only `store`. Preferences, origin state, the database, recovery, and service logs keep following that selected directory. Portable service logs remain `<root>/data/logs`. The resolver's sibling `<root>/logs` path is unused. Auxiliary directory integration, browser preference capture and restore, and installed artifact receipts remain open. The resolver stays independent of the runtime gate. The launcher selector uses `DesktopRebuild::from_env()`.

Unix store admission runs before runtime ownership cleanup or lock creation, and before standalone Rust writer database preflight. Public `pp_storage::lease::prepare_directory` callers use the same admission checks. New directories use mode 0700, restricted further by the process umask. Existing stores must belong to the effective user, grant that user read, write and search permissions, and deny group and other write permissions. Admission preserves existing modes, including legacy 0755 stores, and resolves data-directory aliases. It refuses unsafe stores without changing their permissions or contents. This shared runtime protection applies with the rebuild gate off and on, including `pp-server --data`.

Before recursive creation, admission checks existing input prefixes and their resolved ancestry, including the full working-directory ancestry for relative paths. It checks canonical ancestors again after creation. Ancestors must belong to root or the effective user and deny group and other write permissions unless they have the sticky bit. This permits trusted aliases and sticky temporary directories, including the macOS `/tmp` alias. The store itself keeps the stricter owner and mode requirements. Unsafe ancestors fail with `Unsafe data directory ancestor` before lock creation or database preflight.

Store admission checks permissions on an open directory descriptor and holds it while it resolves the canonical path. It compares that descriptor's device and inode with the canonical directory, then rechecks the canonical leaf's owner and permissions. Replacement at this checkpoint fails with `Data directory was replaced` before lock creation, ownership cleanup or database preflight. Existing aliases remain supported.

Writer admission keeps that descriptor through OS-lock acquisition and opens `printpartner.lock` relative to it before database preflight. After taking that lock, Rust reserves `.desktop-lease` with a regular file to exclude standalone Node. An existing Node lease directory refuses admission without owner-marker inspection or recovery. Rust rechecks directory identity and permissions before and after locking. An observed replacement fails with `Data directory was replaced`. A lock created during a replacement race belongs to the admitted directory; refusal closes the lock without path-based cleanup.

The standalone Rust writer acquires `printpartner.lock` before its first read-only database preflight and retains the same admitted directory descriptor through storage ownership. It rechecks identity and permissions immediately before preflight, refusing an observed replacement before database capture, temporary-copy creation or SQLite open. It checks resolved ancestor permissions before each lock acquisition. `PP_STORAGE_CORE_LOCK=1` tells the supervised Node child to validate its inherited descriptor against `printpartner.lock`. The standalone Node lease protocol remains unchanged. See [Storage](storage.md) for shutdown, crash recovery, and unsupported directory operations.

These Unix checks remain snapshots, not path confinement. Replacement during preflight capture or after the OS-lock checkpoints, transient replacement restored between checks, ancestor replacement, ACL policy, standalone Node admission, and Windows runtime permission policy remain open.

Windows auxiliary app data and config use `<APPDATA>/com.poitee.printpartner`. Cache uses `<LOCALAPPDATA>/com.poitee.printpartner`; logs append `logs` to that local directory. Missing or empty variables fall back independently to `~/AppData/Roaming` and `~/AppData/Local`. Nonempty relative or parent-traversing values return a named error. The legacy store remains `~/.print-partner`. Windows ignores XDG values; macOS and Linux ignore Windows values.

These are pure path mappings using the host's `Path` semantics. The Linux tests supply absolute Linux paths to exercise the Windows mapping policy. They do not parse Windows drive or UNC paths on Linux, resolve Windows Known Folders, or establish a Windows build or launcher receipt. Rust entry points still contain Unix-specific APIs.

Portable path tests run on Linux, including the macOS and Windows mappings:

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core --lib --features desktop_rebuild --no-tests=fail -E 'test(layout::tests::ticket_t_12)'
```

### Origin recovery and library preferences

CoreRuntime replaces a valid saved port only when binding returns `AddrInUse`. It holds the replacement listener while publishing a private same-directory file, syncing the file, checking the old file identity and content, renaming, and syncing the directory. Node starts only after publication succeeds. Missing configuration retains hard-link publication without clobbering an existing name. Corrupt and unsafe configuration still fails closed. `origin_change()` returns an optional immutable `OriginChange::PersistedPortInUse` with nonzero old and new ports. Its display notice contains no launch token or credentials.

`CoreHandle` offers typed per-field updates and reads for `BrowserPreferences` under the existing storage lease. The versioned `browser-preferences.json` sidecar is independent of the port. It stores optional sidebar collapse, theme, and supported printer poll interval values. Unknown fields, invalid values, unsupported versions, unsafe files, and oversized files fail without overwrite. Updates preserve other fields and unrelated portable-store bytes. Shutdown closes command admission and settles queued callers before releasing the lease.

`CoreHandle` also offers typed per-field reads and updates for `DesktopSettings`, the six user paths the compatibility child reads from its environment: `PP_EXCHANGE_DIR`, `DOCKER_SOCKET`, `PP_COMPOSE_FILE`, `SLICER_BAMBU_DIR`, `SLICER_ORCA_DIR` and `SLICER_PRUSA_DIR` (the `Owner::DesktopSettings` entries in `pp_compat::settings`). They live in a versioned `desktop-settings.json` sidecar, so `desktop.toml` keeps only the port. A stored path must be absolute and free of `..`, control characters and more than 4 KiB. It does not have to exist yet. Unknown fields, unsupported versions and invalid paths fail without overwrite. Core reads the file once at startup and passes the stored values to the child at every spawn, so a change takes effect the next time Print Partner starts. If the file cannot be read, the app still starts without those settings and leaves the file untouched. The spawn refuses any name that is not a desktop setting, so credentials cannot reach the child this way.

The child always runs with `NODE_ENV=production`, the same as the Docker image (an `Owner::Fixed` entry). Without it the server would put password reset links in the forgot-password response, serve `/auth/dev-login` once single-user or multi-user sign-in is on, and turn on the OpenAPI docs UI. `web/apps/server/src/desktop-child-env.test.ts` runs the app with exactly the fixed child environment and checks all three.

With `desktop_rebuild` compiled and `PP_DESKTOP_REBUILD` exactly `1` or `true`, the shell shows that credential-free notice and copies saved sidebar, theme, and printer poll values into the replacement origin before the page reads them. It writes those same keys back to `browser-preferences.json`. Other localStorage keys and IndexedDB stay on the previous origin. The desktop gateway cookie name includes the port, so a replacement opens the one-shot bootstrap URL. The host-only `pp_session` cookie is not scoped to a port, so the shell does not clear it or add a login step. With either gate off, the shell keeps the previous behavior: no notice and no preference script. Focus, tray, and second-instance window behavior stay in GRE-368. These operations do not relocate or rewrite product databases.

## Review observations

GRE-598 adds `pp_core::review_observations` behind `desktop_rebuild`. Library callers can observe an accepted storage snapshot for available input roots, missing STL artifacts, and accepted thumbnail signatures. Reads share entry, path, byte, and elapsed budgets and support cancellation. Source inventory reads use pinned directory handles. The small `pp-source::observation` helper is gated by the source crate's new feature, enabled by the core feature.

An optional filament provider reads bounded JSON over HTTP, confines redirects to its original origin, and resolves only its configured integration's filament identifiers. Provider reads reuse the checked `pp-net` address policy and pin DNS answers before connecting. Cloud metadata destinations are rejected. Provider failures fall back to catalog-only lookup. Cancellation returns an error. No application routes or startup paths call these library methods, so runtime behavior remains unchanged. Later drafts API integration must check the stored `PP_DESKTOP_REBUILD` setting.

The reference acquisition and upload modules land in smaller slices on top of D5 retained capture, the D14 upload journal and D15 import jobs.

GRE-784 adds `pp_core::uploads::SourceImports` behind `desktop_rebuild`. `SourceImports::new` opens the owner's upload journal with its import quota. `directory_input` names every regular file below a supplied directory in path order. It skips names that start with `.` and refuses links and special files. `admit` hashes the named files from that directory under the request's input limit, measures stored bytes under the storage `repos` directory, and admits the operation once per key. A replay of the same request returns the stored operation. `get` returns the journal operation. A refusal before admission writes no rows and changes no files. The client never claims or runs the import. GRE-787 adds the runner and verifies published artifacts on `get`. Captured preparation stays in GRE-785. No route, CLI or startup path calls these methods, so they need no `PP_DESKTOP_REBUILD` check yet.

GRE-785 extends that client with captured preparation. `SourceImports::new` also opens a policy-aware import worker. `preflight_capture` measures stored bytes under `repos` and returns storage's immutable preflight. `admit_captured` admits a `PreparedCapturedAdmission` once per key. `prepare_frozen_claim` inspects a retained frozen capture, correlates its manifest and inventory with the journal, and claims only an exact admitted match that still has a current Source proof. Manifest/journal mismatch and a missing Source refuse with journal rows and non-database files unchanged. The client still does not copy retained inputs into owned import directories (GRE-786). No route, CLI or startup path calls these methods, so they need no `PP_DESKTOP_REBUILD` check yet.

GRE-786 adds the input-copy stage. `write_retained_inputs` takes a claimed frozen capture and copies its retained files into `repos/.pp-imports/{job_id}` through directory capabilities with `O_NOFOLLOW` and `O_EXCL`. Case-folded path collisions are rejected before any destination file is opened. An existing destination operation directory is refused without overwriting. On success the journal advances to `OwnedInputReady` and the frozen capture is removed. The client still does not publish or activate (GRE-787). No route, CLI or startup path calls these methods, so they need no `PP_DESKTOP_REBUILD` check yet.

GRE-787 adds publish and activate. `work_frozen` claims an exact admitted frozen capture. `work_operation` claims a job by id, and `work_next` claims the next supplied import. Each runs the claimed attempt through the journal phases and stops after the `Through` phase it is given (`OwnedInput`, `Published`, `Activated` or `Settled`). From `OwnedInputReady` the client re-verifies the owned inventory, prepares media, and materializes an immutable Source revision within the reservation. It then records `Published` with the artifact. `Activate` runs in storage under the claim's generation and fence, and `Cleanup` removes staging and the owned directory before it settles the job. A stopped attempt keeps its job until restart recovery requeues it under a new generation and fence. Until then a retry claims nothing and changes no row or file. After recovery the retry resumes at the journaled phase. It does not recopy input, republish, or record a second activation, and it returns the recorded receipt. A captured import that is still `Admitted` is refused without its frozen capture and is not failed, so `work_frozen` can still run it; once the retained capture is consumed a deterministic failure reaches `Fail` and `Cleanup` instead of requeueing forever. A refused call leaves `repos/.pp-imports` untouched. The snapshot selection takes its byte budgets from the operation's prepared reservation and refuses files it cannot classify instead of silently omitting them. `get` now verifies a published artifact against its stored revision. No route, CLI or startup path calls these methods, so they need no `PP_DESKTOP_REBUILD` check yet.

GRE-788 adds `pp_core::source_acquisition::SourceAcquisitions` behind `desktop_rebuild`. It owns a `SourceImports` client and an ingress gate that starts closed. `admit_captured` refuses while ingress is closed and writes no journal row. `recover_before_accepting` closes ingress, finishes interrupted vault deletions, and runs each frozen retained capture through `work_frozen` to `Settled`. An exact admitted match is published, activated and cleaned up. A capture whose journal row is owned or later, or which has no journal row, is removed. A capture that conflicts with its journal row, and any candidate or malformed capture, stays byte-for-byte unchanged with its journal row; other captures still settle. A malformed slot does not stop the frozen captures beside it from settling. While any capture remains, the call returns `Retained captures require repair (N remaining): <capture id>: <cause>; ...`, naming every remaining capture with its cause (for example a conflicting journal row, a candidate that was never frozen, a malformed slot, or an import job still held by a stopped attempt until restart recovery requeues it), and ingress stays closed. Otherwise it opens ingress and returns the number of captures reconciled. Journal-only work whose retained capture was already consumed is not resumed here. GRE-789 adds bounded acquisition work on top of this gate. `SourceAcquisitions::new` takes a positive `capacity`, and `acquire(CaptureRequest, cancelled)` first reserves the operation key. A key that another acquisition holds, from preflight until its operation is recorded, is refused with `Source acquisition already in progress for this key`. Keys are reserved across tenants, so two tenants using one key at the same moment see the later one refused. Then the request is preflighted: a key whose operation is already recorded replies with that operation as recorded, which may not be settled yet, without reading input or writing anything, even when every slot is held. Otherwise the request takes one capacity slot. With ingress closed it is refused with `Source acquisition ingress is closed`, and with every slot held with `Source acquisition capacity exhausted`. A refused request reads no input and writes no capture, journal row or job. An accepted request writes its files into an owned candidate, binds the capture manifest to the preflighted admission, and freezes it; any failure before the freeze removes the candidate, and a candidate that cannot be removed closes ingress until recovery repairs it. A worker thread then admits the frozen capture and settles it through `work_frozen`; the slot is held until the operation is recorded, and the returned `AcquisitionReply::wait` yields that recorded operation. `recover_before_accepting` refuses with `Source acquisitions are active` while any key is reserved, and leaves ingress as it was. GRE-790 adds `shutdown(budget)`, which returns an `AcquisitionDrain { idle, interrupted, pending }`. Shutdown closes ingress for good. Later acquisitions are refused with `Source acquisition is shutting down` before preflight, including replies for recorded keys, so they read no input and write nothing. Recovery on that instance is refused with `Source acquisitions are shut down`. One budget covers every in-flight acquisition, not one per worker. An acquisition still capturing its input finishes the capture unless its caller cancels it. Every worker is told to stop. A worker that admitted its capture but has not settled it stops without failing the import. It leaves the journal row admitted and the frozen capture retained, records its key in `interrupted`, and its reply is `Source acquisition interrupted by shutdown; retained for recovery`. Shutdown returns when no key is reserved or the budget runs out. Keys still in flight are then listed in `pending` and `idle` is false. Nothing replays interrupted work automatically. A new instance's `recover_before_accepting` settles a retained capture, after a restart when a stopped attempt still holds the import job. A worker stopped after its capture became owned input resumes through import job restart recovery instead. No route, CLI or startup path calls these methods, so they need no `PP_DESKTOP_REBUILD` check yet.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild --test source_import_client
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild --test source_import_files
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild --test source_import_recovery
```

### D6 native-secret migration

GRE-605 ports the reference native-secret migration protocol behind `desktop_rebuild`.
The storage owner exposes an explicit `native_secret_migration()` library client.
Opening storage does not create its receipt table; the first admitted preparation
creates it transactionally. Preparation preserves the legacy `github_pat` setting
and returns secret material plus a stable native-store locator. The caller must
write and verify the native-store copy before confirming. Confirmation revalidates
the physical storage owner, receipt generation, row identity and value before
atomically deleting the legacy setting and recording completion. Reopening resumes
the same receipt. The receipt contains no token bytes.

This slice adds no native keychain provider or startup caller. Future user-visible
migration must check the stored `PP_DESKTOP_REBUILD` setting before requesting
preparation or confirmation. Auth scheduler and authority changes are a separate
D6 PR; GRE-605 remains In Progress until both parts pass the coordinator gate.

The same D6 foundation ports durable authority receipts for sessions and routed
API keys. Receipts retain credential digests, tenant, subject and authentication
policy, and revalidation rejects changed policy or revoked credentials. Physical
owner admission checks the storage identity. These internal APIs compile behind
the feature, and the D16 jobs engine stores and revalidates them (see below).

### D15 jobs model and subscriptions

GRE-611 adds the reference job model behind `desktop_rebuild`. `CompletedResult` holds
each job kind's public result. A job document can carry `public_result`, and a checklist
export effect can carry a completion claim. Decoding checks each claim against the
payload, effect intent, tenant binding, accepted Plan basis and, once the job succeeds,
its confirmed receipt and public result. Empty fields are omitted, so existing documents
encode unchanged. Two deliberate differences from the reference follow Node: basis
identifiers must be positive safe integers, and the `extract-source-docs` result stores
an error count.

The job stream registry starts each stream with its job's snapshot and queues newer
committed versions for the same tenant and job. A stream closes when the job finishes,
its 16-frame mailbox overflows, its credential expires or is revoked, or the owner stops.
At most 128 streams are live. The snapshot changes that expose `public_result` landed
with D16 (b); the engine and auth revalidation calls landed with D16 (c).

`fixtures/parity/jobs-model/generate.mjs` records Node's parsed accepted-plate and
direct-3mf results and its verdict on accepted-basis seals. Node builds its other job
results inside `InProcessJobRunner`, which built-in type stripping cannot load, so Rust
tests check those shapes as literals. The generator imports zod:

```sh
npm --prefix web ci --ignore-scripts
node fixtures/parity/jobs-model/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib jobs::
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test jobs_model_parity
```

### D16 jobs engine: enqueue and claim

GRE-609's first PR wires original authority into the job engine behind
`desktop_rebuild`. With the feature off, the engine is unchanged. No runtime caller
uses it yet; future callers must also check `PP_DESKTOP_REBUILD`.

Enqueue records who asked. A session or routed-key job stores a sealed reference to
that credential as `_authority`: the session digest or key id and hash, the admitted
tenant and subject, and the policy. The raw secret is never stored. A physical-owner
job stores `_authority: null`. Documents written before this change have no
`_authority` field. They stay that way and are never upgraded.

Claims check that authority. `WriterOwner::job_worker_with_policy(policy, admission)`
returns a worker that revalidates each job's original credential under `policy`.
A revoked or expired credential, a changed policy, tenant or subject, or a missing
legacy authority fails the job with `_authority_refusal` (reason, phase `claim`,
time and generation). The refusal commits and the claim continues with the next job,
so a refused entry never blocks healthy work. A plain `job_worker` claims
physical-owner jobs. When it reaches a job with an original credential, it returns
`AuthorityFailure::PolicyRequired` after committing the refusals and expiry
recovery already done in that transaction. `claim_kind(kind)` claims only one
admitted kind. `authorize(lease)` returns the live attempt's revalidated tenant and
subject without changing the job. A queued job's snapshot reports progress `0`, and
another tenant's read returns `JobAccessFailure::NotFound`.

Claims reserve individual Sources with D16 (d), described below. Upload, local scan
and Source Sync paths arrive with D14, D17 and D18.

`fixtures/parity/jobs-engine/generate.mjs` starts jobs in Node's
`InProcessJobRunner` under web's tsx loader and records the enqueued snapshot and the
other-tenant read. The durable engine's queued message, "Waiting for a compatible
worker", deliberately differs from Node's "Queued". Node keeps no queue across restarts
and never revalidates the enqueuing credential, so the claim tests follow the reference
engine's `job_authority_recovery.rs`:

```sh
npm --prefix web ci --ignore-scripts
node fixtures/parity/jobs-engine/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test jobs_engine_parity --test job_authority_claims
```

### D16 jobs engine: recovery and public results

GRE-609's second PR revalidates authority on every worker operation, behind
`desktop_rebuild`. Heartbeat, progress, effect admission, effect confirmation, finish
and fail all check the attempt's original credential first, under the worker's policy.
A refusal records `_authority_refusal` with phase `worker_advance`, fences the
attempt, commits, and then returns the typed `AuthorityFailure`. An attempt with no
effects fails. An attempt with effects waits for reconciliation. Physical-owner
attempts are not checked, and an attempt whose document lost its `_authority` is
refused as a missing original.

A refused `ConfirmEffect` whose receipt matches the admitted effect keeps that
receipt as observed evidence. The effect stays unconfirmed, and a document may hold an
observed receipt only while the job is waiting for reconciliation after a refusal. A
receipt that does not match is rejected and nothing is written. Reconciliation can
confirm the observed receipt only with the same receipt. It cannot deny the effect, and
an observed receipt never proves an uploaded-only print or a resolved printer failure.
`ConfirmSucceeded` reconciliation and `Finish` refuse jobs that carry a checklist
completion claim, which only the checklist finalizer settles.

`WorkerOperation::FinishPublic { artifact, result }` finishes like `Finish` and stores
`result` as the job's `public_result`. The result kind must match the job's kind.
Snapshots now report `public_result` as `result`, never the internal receipt, and add
`created_at`. `AtomicJobClient::read_public` returns one snapshot.
`AtomicJobClient::list_retained` returns the tenant's retained snapshots with Node's
filters (`LegacyListFilter`: status, `since` in milliseconds against finish time or
else update time, and profile) and Node's newest-first order. Ties keep enqueue order.
Quarantined and unreadable rows are skipped. Turning HTTP query strings into
`LegacyListFilter` is GRE-795 (`docs/gre-795-jobs-http.md`). Read-one HTTP at
`/jobs/{id}` and `/api/v1/jobs/{id}` is GRE-796 (`docs/gre-796-jobs-http.md`).
Job WebSocket admission at `/ws/jobs/{id}` is GRE-798 (`docs/gre-798-jobs-http.md`).
Snapshot delivery and close reasons on that socket are GRE-799 (`docs/gre-799-jobs-http.md`).
Reconnect after a lagged mailbox, and owner shutdown of live sockets, are GRE-800
(`docs/gre-800-jobs-http.md`).
No event is lost or repeated when a socket subscribes while the job is
committing. That proof is GRE-511 (`docs/gre-511-job-stream.md`).
Composing those routes on the opted-in rebuild router is GRE-821
(`docs/gre-821-runtime-jobs.md`).
Restart, resubscription, polling fallback, tenant and registry bounds for those
streams are proved together by GRE-410 (`docs/gre-410-job-streams.md`).
The browser job row follows that socket snapshot when health reports
`tenant_feature_toggles`, and keeps HTTP polling otherwise. That wiring is
GRE-822 (`docs/gre-822-job-context.md`).
The Plan review cache follows an open draft's revision when health reports
that same capability and the draft base is current, and keeps the existing
review query otherwise. That wiring is GRE-823
(`docs/gre-823-workspace-reads.md`).
A finished import scan refreshes that source's queries when health reports
the same capability, and a finished plan publication refreshes that plan
only. Without the capability, a finished source job still refreshes every
source, profile, and plan review. That wiring is GRE-824
(`docs/gre-824-job-invalidation.md`).
GRE-825 runs that browser wiring against a disposable server. One Plan review,
one draft Save, and one job reconnect run with the opt-in unset and again with
`PP_DESKTOP_REBUILD` exactly `1`. The unset run keeps the existing review, Save,
and socket plus `GET /jobs/{id}` poll. Optional feature controls stay absent.
The opted-in run uses the same review and Save after health reports
`tenant_feature_toggles`. Feature controls render and stay off. A dropped job
socket is replaced by the next snapshot, and the row does not poll. The proof
does not turn the flag on by default and does not cut production data over.

```sh
npm run build --prefix web
node --test --test-concurrency=1 web/apps/web/test/browser/gre-825-desktop-rebuild.isolated.mjs
```

The command runs `desktop_rebuild_flag_off_keeps_existing_flow` and
`desktop_rebuild_opt_in_save_and_reconnect`. Run it from the repository root
after that build. `npm run test:browser` from `web/` runs the same file.

A rebuild job row ends when its socket closes 1008 for a reason other than
`Job not found`, including `Session expired` and `Session revoked`. Those
session reasons also call `notifyEngineUnauthorized`. An ordinary drop still
reconnects, and the flag-off poll stays. That close handling is GRE-1001
(`docs/gre-1001-job-session.md`).

`fixtures/parity/jobs-engine/generate.mjs` now also runs Node jobs to the end and
records the finished snapshot and `listJobs` under each filter. The Rust test finishes
the update check with Node's literal result and asserts the public read and each list.
Stored times have whole-second precision, so the test waits one second between the two
finishes. Node's snapshots have no `created_at`. The worker-advance and
observed-evidence tests port the reference `job_authority_recovery.rs`, because Node
never revalidates a running job:

```sh
node fixtures/parity/jobs-engine/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test jobs_engine_parity --test job_authority_claims
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib jobs::
```

### D16 jobs engine: streams, shutdown and Source work

GRE-609's third PR wires the D15 stream registry into the engine and gives the catalog
the Source-work state that later slices settle, behind `desktop_rebuild`. With the
feature off, the engine and catalog behave as before.

`AtomicJobClient::subscribe(credential, job_id)` resolves the credential, reads the
tenant's job and, once that transaction commits, opens a stream that starts with the
job's snapshot. Another tenant's job returns `JobAccessFailure::NotFound`. Every job
save inside a jobs transaction is collected, and the last committed version of each
job is published after the commit. If the commit outcome is unknown, every live stream
closes. Each auth write, through either `auth_with_policy` or
`rebuild_auth_with_policy`, runs under the registry's admission gate and then
revalidates every live stream. A stream whose credential no longer resolves to the
same tenant and subject closes as revoked before it can deliver a queued frame. If
revalidation itself fails, every stream closes. `WriterOwner` shutdown stops the
registry first: live streams close as `OwnerStopped`, and a subscription that commits
during shutdown returns `JobAccessFailure::Stopped`.

The catalog's Source-work state now has a random incarnation and an issued high-water
mark, and each lease carries a `SourceWorkProof` (incarnation and token). A second
lease on a Source that already has live work fails with `SourceBusy`. A lease that is
dropped while the writer queue is full is recorded as orphaned, and the writer frees
its Source before the next envelope. `classify_issued` and `retire_issued` tell
active, retired, never-issued and other-incarnation tokens apart; D17 and D18 call
them when a scan or Sync settles its reservation. D16 (d) adds claim-time Source
reservation, described below.

`fixtures/parity/jobs-engine/generate.mjs` now also records the events a tenant
listener receives while each Node job runs. The Rust test subscribes before the claim
and asserts that the stream ends with Node's events. Node's running event says
"Running…" where the durable model says "Running". On failure, Node reports progress
100 and the handler's error, and the durable worker keeps the last progress and
reports "Job failed". Node never revalidates a listener, so the stream authority and
shutdown tests port the reference `job_transport.rs`:

```sh
node fixtures/parity/jobs-engine/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test jobs_engine_parity --test job_transport
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib catalog::
```

### D16 jobs engine: claim-time Source reservation

GRE-609's fourth PR reserves an individual Source when a worker claims its job,
behind `desktop_rebuild`. `claim`, `claim_kind`, `claim_import`, and
`PendingClaim::receive` return `ClaimedAttempt` with the job, attempt lease, and
optional Source-work lease.
The default build keeps the existing tuple return and behavior.

`source_binding` maps ImportScan, ExtractSourceDocs, and SuppliedSourceImport to
their stored Source, including targeted import claims and cancelled import cleanup
claims past the attempt limit.
A missing Source, including a Source in another tenant, fails the queued job with
"Source is unavailable for this job" and a `source_unavailable` history entry.
The claim does not increment attempt or generation. Untargeted claims continue to
healthy work, while `claim_import` returns `None` after failing the targeted job.
A busy Source leaves the queued job unchanged. `claim_import` returns `SourceBusy`
so its caller can retry, while untargeted claims skip the busy Source.

`ClaimPlan` carries a pending `SourceReservation` through the database commit.
Only `TransactionDecision::Claimed` activates its token and creates the Source-work
lease after commit. The lease remains live after cancellation or attempt expiry
until its holder releases or drops it. An abandoned claim reply drops its lease.
The existing orphan queue handles releases that cannot enter the writer queue.

CheckSourceUpdates claims have no initial Source-work lease. Their workers acquire
individual leases through `begin_source_work`. D18 (a) replaced Sync's scoped
Source bridge: Sync reserves each target's Source when it claims the target, and
`begin_source_work` refuses a Sync attempt. Import settlement belongs to
D14 (d), local scan settlement to D17, and Sync settlement to D18. Claim-time
import reservation and targeted `SourceBusy` errors follow the reference
`source_binding`, `claim`, and `claim_import` in
`desktop/source-sync-local-current-20261008` at `d73b6d8d`,
`rust/crates/pp-storage/src/jobs.rs:2431`, `:2588`, and `:3372`.

Node has no durable claims or Source reservation counterpart. These tests port the
reference `durable_jobs.rs` reservation cases. The committed Node generator and its
literal-result parity tests remain unchanged. The test-only `claimed_attempt!`
pattern retains the Source-work lease while matching either build's return type.
`rust/scripts/migrate-claimed-attempt.py` reruns the mechanical tuple migration.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test durable_jobs --test supplied_imports
node fixtures/parity/jobs-engine/generate.mjs --check
```

### D14 uploads: supplied-import job arms

GRE-610's first PR adds the job kind the upload journal will own, behind
`desktop_rebuild`. With the feature off, the engine is unchanged. The journal, its
schema arrives in D14 (b), and the journal and capture paths in (c) to (e).

`JobKind::SuppliedSourceImport` carries `Payload::SuppliedSourceImport { project_id,
operation_key, input_version }` and reserves `source:{project_id}`. Only the upload
journal enqueues it: `AtomicJobClient::submit` refuses the payload with "Supplied
imports require atomic domain admission". The public job API never shows one. A public
read or stream of an import returns `JobAccessFailure::NotFound`, the retained list
skips it, and its saves are never published to a stream. The internal `Get` still
reads it.

`ServerWorkerClient::claim_import(job_id)` claims that queued import and no other job.
If its original authority is refused, the refusal commits with phase `targeted_claim`
and the typed `AuthorityFailure` is returned, instead of moving on to the next job. An
import attempt advances only by heartbeat and progress; the journal owns its other
phases. Cancelling an import, or recovering a cancelled one after a restart, returns it
to the queue with `cancel_requested` set, so its owner can reclaim it and release its
input. As a deliberate difference from the reference, the 100-attempt limit does not
apply to these cleanup claims, and recovery never fails a cancelled import for its
attempt count; other import attempts and other job kinds keep the limit.

Node has no supplied-import kind. `fixtures/parity/jobs-engine/generate.mjs` records
that Node refuses to start one ("Unsupported job kind: supplied-source-import") and that
a job it never started cannot be read, streamed or listed. The Rust parity test asserts
that a durable import looks the same to its own tenant. The claim, advance and
cancellation cases port the reference engine:

```sh
node fixtures/parity/jobs-engine/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test jobs_engine_parity --test supplied_imports --test job_authority_claims
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib jobs::subscriptions::
```

### D14 uploads: import, Source revision and preclaim schema

GRE-610's second PR installs the upload journal's tables behind `desktop_rebuild`,
with no callers until D14 (c). The SQL is the reference's
`uploads/schema.sql`, `remote_sources/schema.sql` and
`remote_sources/preclaim39.sql`, unchanged. The reference's provenance columns move
from Rust into `remote_sources/columns38.sql`.

Main keeps the reference's numbering, so D17's Source scan can stay at 40:

| Step | Installs |
| --- | --- |
| 37 to 38 | the provenance columns on `projects`, `source_revisions` and `source_docs`, `source_import_operations`, `source_import_quota`, and the Source revision attempt, artifact and observation journal with its immutability triggers |
| 38 to 39 | `source_preclaim_refusals` and its cursor triggers |

The reference installed the import tables at 36. Main never used 36, so they arrive
with the 38 step and the validator threshold moved from 36 to 38. A database at 36
holding them is refused. Columns that already exist are kept, as in the reference,
and the validator then checks their definitions.

Opening a database below 39 runs in this order:

1. Back up the database as opened, before any write.
2. Restore missing common tables and objects and repair tenant mismatches. This
   pass seeds nothing.
3. Install each pending step.
4. Seed the starter profiles.

Repairs run before the steps because the 38 step's provenance triggers refuse the
repair's writes to `source_revisions`. Each step through 46 (`LAST_COPIED_STEP`)
writes `backups/pre-schema{N}.db`, where N is the step's target version. Before
the first step writes, the open names its rollback copy in the run marker
`backups/pre-schema-run`; a restart of an interrupted upgrade reads the marker
and retention keeps that copy. The marker goes once the upgrade commits, or when
an open finds nothing to upgrade. Each step validates its objects
and columns, and then commits them with its version stamp in one transaction. For a database at 37 the
first backup, `pre-schema38.db`, is the untouched input. `pre-schema39.db` is taken
after step 2, so it can include that pass's repairs. An
interrupted step leaves its input version intact, so each attempt takes a fresh copy
rather than reusing an existing file. The legacy upgrade to 37 (35 without the
feature) still reuses an existing backup, because its earlier migrations do not
commit atomically. With the feature, that backup is named
`pre-schema{from}-to-{current}.db`, as Node names its upgrade backups, so a
database restored at another version gets its own copy. Upgrading from 35 therefore
writes `pre-schema35-to-46.db`, `pre-schema38.db` and later step backups, and
`SchemaReady.backup` names the first. A
failed backup stops the upgrade before the step writes. Backups of a WAL database no
longer leave the temporary copy's `-wal` and `-shm` files behind.

Opening checks the installed objects against the SQL text for the stored version,
and refuses objects from a later step. It also refuses an orphaned quota row,
partial revision or document provenance, an invalid observation, an invalid preclaim
refusal, and a cursor shared by a refusal and an observation. D14 (c) adds the checks
of the journal documents against their durable jobs. Without the feature, storage
stays at schema 35 and refuses a schema-39 database.

Node's schema stops at 34 and has none of these tables, so there is no Node literal
to match. The parity target is the reference SQL. The Node schema-parity tests
compare Node's catalog with Rust's after omitting these objects and columns, the
same way they omit the Plan Apply admissions. The upgrade tests run the real owner
from 35, 37 and 38:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib -- uploads:: tests::ticket_t_7_migrations
```

### D14 uploads: upload journal and admission

GRE-610's third PR adds the upload journal and admits supplied files and zips into
it, behind `desktop_rebuild`. Nothing claims or runs an admitted import yet; the
import phases arrive in D14 (d) and captured inputs in D14 (e).

`WriterOwner::imports(policy, quota)` returns an `ImportClient` for the owner's import
quota, at most 64 GiB. Every client of one owner must name the same quota.
`ImportClient::admit` takes a credential, an `Admission` (operation key, an existing
Source or one to create, `Input::Files` or `Input::Zip`, the reserved bytes and the
input and prepared limits), the expected input files, the bytes already on disk, and
the `accounting_epoch` read before measuring them. In one writer transaction it:

1. refuses a stale epoch ("Import accounting changed; retry admission");
2. checks the key, each file's relative path and digest, duplicates, at most 10,000
   files and the input limit;
3. returns the prior operation for a replay of the same request by the same actor,
   and refuses any other reuse of the key ("Import idempotency conflict");
4. checks the target, the limits (input at most 256 MiB, prepared at most 1 GiB, and a
   reservation of at least input + 3 × prepared + 16 MiB, within the quota), and that
   the bytes on disk plus every unsettled reservation plus this one fit the quota
   ("Owner import quota exceeded");
5. creates the Source if asked, enqueues the internal `SuppliedSourceImport` job with the
   admitting authority, and writes the journal entry and its reservation.

The entry is validated before it is written, so an entry whose input paths differ from
its files, or whose files are not of kind `input`, is refused rather than committed.
The reference wrote such an entry, and every later read of its key failed.
`ImportClient::get` returns an entry to the actor that admitted it. Each successful
admission advances the epoch.

Opening storage now checks each journal entry: its columns against its document, the
document's state and bindings, its job (live or archived) against its Source, key and
input version, a settled entry against a terminal job, and its quota row against its
reservation and retained bytes.

Node writes supplied files with `writeUploadedFiles` and has no journal, quota or
idempotency. `fixtures/parity/uploads/generate.mjs` records its limits and, for each
of 22 file sets, either its refusal or the paths it wrote. The journal matches
Node's 256 MiB and 10,000-file limits and acceptance of those path cases.
Admission rewrites backslashes as `/`, drops leading slashes, and collapses empty
and `.` segments before validation and hashing. It preserves `..` segments for
rejection, even when resolving them would leave a path inside the upload root.
Duplicate detection uses the normalized path without regard to case. Its error
matches Node's `Duplicate uploaded file path: <original spelling>` for the second
file. Earlier parity coverage omitted empty and dot segments and mapped duplicate
diagnostics to the generic `Duplicate file` error.

Successful fixtures also record a SHA-256 digest of the input-order file metadata
using Node's resolved paths, declared sizes, the test hash `b` repeated 64 times,
and kind `input`. Rust compares its admitted paths and requested digest with these
literal values. The hash is synthetic metadata, not the sparse probe file's
content hash. Other refusal diagnostics, quota, idempotency, startup, and codec
cases follow the reference `source_import_codec.rs` and its serializer fixture.
These admission changes remain behind default-off `desktop_rebuild`. User-visible
rebuild behavior also requires `PP_DESKTOP_REBUILD`.

```sh
node fixtures/parity/uploads/generate.mjs --check
umask 022
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test upload_journal
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test import_phases
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets --features desktop_rebuild
```

### D14 captured identifiers and payload policy

GRE-766 decodes a capture identifier and the fixed file or ZIP payload policy
behind `desktop_rebuild`. A supplied files or ZIP input stays version 1. A
payload that carries the fixed policy decodes as version 2. `ImportClient::admit`
still accepts only a supplied input: a captured payload does not deserialize as
an admission, and this slice writes no captured journal row. Manifest inspection
and captured admission stay on later D14 slices.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib uploads::tests::capture_payload
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets --features desktop_rebuild
```

### D14 captured binding

GRE-767 binds a captured file or ZIP payload to its actor, operation key, target,
admission limits, and requested inventory. `CapturedInputV2::bind` stores the
policy, target, and manifest digests. `validate` rejects a changed component.
Repeated inputs produce the same digests. The payload decodes as version 2.
Supplied inputs stay version 1, and `ImportClient::admit` still accepts only
those. Refusal leaves the admitted journal row and non-database files unchanged.

### D14 capture manifests: inspect, serialize, and journal resolve

GRE-768 adds `inspect_capture_manifest` and `bind_capture_manifest` behind
`desktop_rebuild`. A closed v1 wire document carries capture id, operation key,
actor, target, digests, admission limits, and requested inventory.
`inspect_capture_manifest` returns paths and `max_input_bytes` only after
recomputing `requested_digest`, `policy_digest`, and `target_digest`, validating
the payload policy, and checking that payload paths match `requested_files`.

GRE-769 adds `resolve_capture_manifest`. It runs the same integrity checks, then
compares actor, operation key, target, admission limits, inventory, and digests
with an admitted journal `Operation`. Exact rebound bytes must match the
presented document. A mismatch or integrity failure refuses without writing;
journal rows and non-database files stay unchanged.

### D14 captured preflight admission

GRE-770 adds `ImportClient::preflight_capture` and `preflight_capture_target`
behind `desktop_rebuild`. Preflight authenticates the credential, checks
admission limits, and — for a fresh key — checks that the bytes on disk plus
every unsettled reservation plus this reservation fit the owner import quota
(`Owner import quota exceeded`), then validates the create or existing target
basis. It returns an immutable `PreflightedCapture` and does not publish files
or enqueue a job. A matching prior captured journal entry may be returned as
`replay` without re-checking owner-wide quota; a supplied prior with the same
key refuses with an idempotency conflict. Refusal and success both leave
journal rows, non-database files, and the accounting epoch unchanged.

### D14 captured prepare and admit

GRE-771 adds `PreflightedCapture::prepare` and `ImportClient::admit_prepared`
behind `desktop_rebuild`. `prepare` binds the capture manifest (GRE-768/769)
into a `PreparedCapturedAdmission` with `Input::Captured`. `admit_prepared`
submits writer `Admit` under `AdmissionAuthority::Preflighted`, re-admits the
preflight credential in the write transaction (`Capture authority changed` on
mismatch), revalidates the target basis, checks captured inventory and binding,
and either returns an exact prior operation (same actor and `intent_digest`) or
writes one journal row, quota reservation, and supplied import job
(`input_version` 2). Credential-path `admit` still refuses `Input::Captured`.
A stale basis after preflight refuses without changing journal rows or
non-database files.

### D14 capture correlation and resolved claims

GRE-772 adds `ImportClient::correlate_capture_manifest` and
`ServerWorkerClient::claim_resolved_import` behind `desktop_rebuild`. Correlation
reads the journal and durable jobs only: it returns `Absent`,
`ExactAdmitted(ResolvedCapturedClaim)`, `ExactOwnedOrLater`, or
`MismatchRepairRequired` without admitting or charging. A resolved claim is
revalidated in the claim transaction against the admitted operation, inventory,
captured job payload, and Source; a changed inventory or other mismatch refuses
with `Captured import requires repair` and leaves queued job rows unchanged.

GRE-772 also makes correlation verify the manifest digests before it reads the
journal, so tampered bytes refuse with an error instead of returning `Absent`.
`Absent` also requires the retained inventory to match the manifest. A changed
inventory returns `MismatchRepairRequired`, so recovery does not re-admit a
capture whose files changed. Correlation and refused claims leave journal rows,
job rows, and the accounting epoch unchanged.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib uploads::tests::capture_preflight
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib uploads::tests::capture_admission
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib uploads::tests::capture_correlation
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets --features desktop_rebuild
```

### D14 uploads: import phases

GRE-610's fourth PR runs an admitted import through its phases, behind
`desktop_rebuild`. It moves journal state only. The files are written by the import
runner in pp-core (D3), and captured inputs arrive in D14 (e).

`ServerWorkerClient::import_phase(lease, phase)` applies one `uploads::Phase` to the
live attempt from `claim_import`, in one writer transaction:

| Phase | Requires | Result |
| --- | --- | --- |
| `Read` | a live attempt | the entry, unchanged; the only phase that does not advance the accounting epoch |
| `Owned(input)` | `admitted` | `owned_input_ready`, with `.pp-imports/{job_id}` holding exactly the admitted files |
| `Published(artifact)` | `owned_input_ready` | `published`, with the artifact bound to the Source and within the reservation |
| `Activate` | `published`, and the Source configuration version from admission | a complete Source revision, its attempt and its manifest artifact. If the Source still matches the admission basis, it points at the revision, is marked up to date, takes the suggested import rules when it still has its default rules, and gets a new document index (`activated`). Otherwise the revision stays inactive (`conflict`) |
| `Fail` | no receipt | `cancelled` if the job was cancelled, otherwise `failed` |
| `Cleanup` | `activated`, `conflict`, `failed` or `cancelled` | the reservation settles, the stored bytes stay retained, and the job ends `succeeded`, `failed` or `cancelled` |

GRE-853 reuses one complete Source revision when a later import publishes the same
upstream key, manifest, and locator. The revision keeps the first provenance; each
import keeps its own configuration, observation, input, and producer on
`source_revision_attempts`. An existing non-complete key returns
`uploads::IncompleteRevisionKey` and does not replace the revision or its attempt.

A cancelled attempt may only read, fail and clean up ("Import cancelled"). Every
phase but `Read` appends a Source observation with the attempt's generation and
fence, the receipt's revision and activation, and the cancellation flag.

Each phase first revalidates the job's original authority with
`jobs::revalidate_source_authority`. If it is refused,
`jobs::commit_source_authority_refusal` fails the job with phase `source_writer`, an
`authority_refused` observation records the fenced attempt, both commit, and the
phase returns the typed `AuthorityFailure`. These are the helpers' first production
callers. A targeted claim refused before the job's first attempt now writes the
entry's `source_preclaim_refusals` row; one refused after an earlier attempt writes an
`authority_refused` observation with that generation and no fence. A generic claim
skips an import whose entry is still `admitted`, because only the owner that holds
its input can run it. A generic claim of a recovered import, whose entry has left
`admitted`, records that same observation for the recovered generation and no fence.
The job journal records the refusal once, with phase `claim`, and the claim moves
on. Retrying the generic claim writes no second observation.

A job can end before its `Cleanup` phase: an authority refusal, a missing Source or
the attempt limit fails it. `uploads::settle_terminal` then settles the entry, and
nothing else runs. An unfinished entry becomes `failed`, or `cancelled` if its owner
asked, and its reservation is released as `Cleanup` releases it. The job, the
Source and the observations stay as they are. It acts only on a terminal job with no
fence or worker and an unsettled entry, so a live attempt keeps its import, and a
repeat changes nothing. Two callers use it. The owner's `Cancel` of the ended job
settles it with their current credential and advances the accounting epoch. At
startup, after expired attempts are recovered, the storage owner settles every
such entry, including those of archived jobs. The reference has no such path: its
refused imports keep their reservation.

As in the reference, `uploads::source_reserved` makes Source deletion return
`active_work` while an import of the Source holds an unsettled reservation.
Storage written before that guard can hold an unsettled entry whose Source is
gone. The same startup pass fails its queued job with `source_unavailable`, as a
claim would, and then settles the entry. A terminal job with a missing Source
also settles. Only a non-terminal, non-queued job with a missing Source refuses
to open.

Opening storage checks each preclaim refusal against its entry and job: it must be
the entry's first and only observation, the entry must be `admitted`, or settled as
`failed` or `cancelled` to match the cancellation flag, and the job must have failed
at generation 1 from a targeted-claim refusal at generation 0, with the same
cancellation flag. The check also runs from the other side: an entry whose job
records a targeted-claim refusal at generation 0 and whose observation cursor is
above 0 must have its preclaim row, so opening storage without it fails with the
typed `uploads::MissingPreclaimRefusal` ("Missing Source preclaim refusal"). Storage written before targeted claims journaled
the refusal left the cursor at 0 and has no row; it still opens.

Node writes supplied files in one call and has no journal, attempts or
observations, so the tests port the reference `source_import_operation.rs` storage
assertions and its `Postprocessing` wire values:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test import_phases --test upload_journal
node fixtures/parity/uploads/generate.mjs --check
```

## D9 build graph receipt

GRE-602 adds the read-only first half of `pp_storage::build_graph` behind
`desktop_rebuild`. Authenticated `List`, `Read`, and `ListLayers` commands run
through the existing writer queue and return typed summaries and ordered layers.
`List` rejects more than 64 builds, shares one graph budget across summaries,
and checks cancellation between builds.
`ListLayers` caps results at 1,024 layers and checks cancellation during iteration.
Summary facts reuse the accepted graph reader and freshness calculation.
Corrupt freshness inputs return explicit unavailable freshness and integrity
progress for that build while retaining healthy builds in lists.
Summaries validate PPJS metadata losslessly. Existing scalar snapshot projections
report `manifest_text` integrity failures for non-scalar metadata; lossless public
snapshot serialization remains deferred.

GRE-602 also adds `Create`, `Touch`, `SetBase`, `AttachAddon`,
`ReplaceAttachment`, `Detach`, and `Delete` through the same authenticated queue.
Mutations use deadline-aware writer admission and immediate transactions. Typed outcomes distinguish missing builds,
sources, and layers from duplicate names and attachments. Names reuse the gated
`pp-domain::build_identity::BuildName`, also reexported by `pp-contracts`
through `pp-api-types`; IDs use `pp_domain::PositiveId`.
Create and Touch return boxed summaries. Attachment changes mark
`config_modified_at`, and returned layers retain the 1,024-layer cap. Delete
removes tenant-scoped production, filament, and plan feature settings and defers foreign keys
so accepted graph references can cascade together. Errors, cancellation, and
budget exhaustion roll back the transaction.

With `desktop_rebuild`, graph reads report `Failure::BudgetExhausted` separately
from stored integrity failures. `BudgetResource` identifies the shared
200,000-row or 64 MiB batch budget, or the 1,000,000 Plate overlap comparison cap.
The 64 KiB stored-row text cap remains an integrity check. Accepted reads also
propagate resource exhaustion instead of embedding it in context errors. The
feature-off reader retains its existing error classification.

D9 is complete with the [final coverage and integration audit](desktop-rebuild-d9-audit.md).
Required-unit planning integration belongs to GRE-606 D11. Manifest option observation is
GRE-604 D10b, described below.
No runtime caller is added. Future user-visible callers must also check
`PP_DESKTOP_REBUILD` for exactly `1` or `true`.

## D10a manifest model receipt

GRE-604 D10a adds `pp_storage::manifest_options::KitManifest` behind `desktop_rebuild`.
The pure model parses request and stored JSON, normalizes selection variant IDs,
projects typed selections, and emits JavaScript-compatible JSON with UTF-16 text
and ordered objects. Input errors emit lossless JSON detail bodies.
The codec enforces 8 MiB, 64 levels, and 100,000 nodes.
Focused codec unit tests and public model integration tests run in the desktop gates.

## D10b manifest observation receipt

GRE-604 D10b adds `pp_storage::manifest_options::observation` behind `desktop_rebuild`.
GRE-644 slice 1 wires its build graph manifest commands, described below. HTTP routes remain deferred.

`capture` reads the build, its stored kit, and its ordered source layers in one transaction.
`basis_matches` repeats the capture, so a later transaction can reject a stale observation.
`observe` builds the response of Node's `buildPlanManifestBuilder` in
`web/apps/server/src/services/plan-manifest-builder.ts` through storage-issued `DraftReads` requests:

- It reads each checkout manifest and STL inventory. It filters paths with the source's import rules and sorts them as `listStlRelativePaths` does.
- A source without declared option groups infers groups from shipped and custom path hints, then from sibling folders.
- Groups merge in layer order and keep JavaScript property order. Each variant names the sources whose scanned parts it matches.
- Stored kit selections take precedence. Repository and community manifest defaults fill the other groups in layer order.

As in Node, an unreadable or invalid manifest reports `exists: false` with the fallback YAML.
Invalid UTF-8 decodes with replacement characters. An invalid or unreadable custom hints file fails the observation.
A source without a checkout path is skipped. A source whose checkout root the adapter refuses is omitted.

Adapter reads, manifest and hint parsing, and STL scans charge the caller's `PreparationBudget`.
The stored kit and each source's import rules charge their bytes as documents.
Both also charge every JSON node and its depth to the same budget. Import rules charge one rule per accepted path.
Import rules over the JSON byte, depth, or node limit refuse the observation.
Malformed or non-array import rules give an empty rule set, as in Node.
A malformed stored kit falls back to an empty kit, as in Node.
A stored kit over the JSON byte, depth, or node limit refuses the observation.
Path filtering and pattern matching check cancellation.
Each source returns a SHA-256 stamp of its reads, so a caller can compare two observations.
`validate_known_selections` returns the `manifestSelectionsInputError` messages for a kit save.

The tests compare the response bytes with `rust/crates/pp-storage/tests/fixtures/manifest-builder-node.json`.
`manifest-builder-node.mts` in the same directory regenerates that file from the Node services.
Builder globs normalize backslashes to slashes, lowercase both inputs, and trim
ECMAScript whitespace. They check normalized equality, then match the entire path.
A pattern without a slash also matches against the basename.
Only `*` and `?` are wildcards. They consume zero or more UTF-16 code units and
exactly one unit, respectively, excluding LF, CR, U+2028 and U+2029.
Slashes have no special wildcard restrictions. All regex punctuation is literal.
Literal comparison follows ECMAScript non-Unicode `/i` canonicalization after
lowercasing. Uppercase expansions and non-ASCII-to-ASCII uppercase mappings do
not fold. Lowercasing still maps Kelvin sign to `k` before comparison.
Lone surrogate pattern units are preserved, and `?` consumes half a supplementary character.

The matcher uses two indices and one saved `*` retry point instead of a regex.
It has O(path units × pattern units) worst-case work and O(path units + pattern
units) normalization storage. `BuilderGlob::new` normalizes a pattern once for
all paths. The matcher has no `PreparationBudget` parameter. The observation
checks cancellation once on entry and before each path, so cancellation waits
for at most one path's match, even when no paths were scanned or a variant has no parts.
This resolves the backtracking and UTF-16 parity defects in GRE-642 and GRE-641.

Regenerate or verify the Node differential corpus with
`node rust/crates/pp-storage/tests/fixtures/manifest-globs-node.mjs` or append `--check`.
The generator reads the actual `matchKeyMatches` and `globToRegExp` functions.
Rust case mapping comes from std, while Node uses ICU.
`manifest-case-node.mjs` writes, or with `--check` verifies, `manifest-case-node.jsonl`.
It pins each UTF-16 unit's non-unicode `/i` class and each scalar's `toLowerCase` result.
`builder_glob_case_mappings_match_node_tables` compares Rust against those tables.
The Desktop contracts workflow runs both `--check` commands on Node 22 and 24, so
an ICU or Unicode upgrade on either side fails CI instead of drifting.
Run the ignored `builder_glob_backtracking_probe` test in an exclusive landing slot
with `--release -- --ignored --nocapture` to check the 10 ms release bound.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib manifest_options
```

## Map one existing account

Build with the `desktop_rebuild` Cargo feature and start with `PP_DESKTOP_REBUILD=1` or `PP_DESKTOP_REBUILD=true`.
Both gates must be enabled. Node receives the account mode through the verified parent setup pipe.
A child environment variable cannot enable mapping.

Stop every process that uses the data directory before configuring a mapping.
Upgrade the copy with the existing unmapped browser or server first.
Mapped startup requires the exact current `schema_version` in `app_settings` and performs no data or schema migration.

Create `desktop-account.json` in the data directory with this exact shape.
Replace both account IDs with the existing user's `users.id`.

```json
{"version":1,"kind":"existing-account","user_id":"account-a","tenant_id":"account-a"}
```

Set its permissions to `0600` and its owner to the user who runs the desktop process.
The file must be a regular file, at most 4096 bytes, with no symlink at its path.
Unknown fields, malformed JSON, missing users, and unequal user and tenant IDs fail startup before product database writes.
The mapping selects exactly one account. It never selects the first row or changes stored tenant ownership.
A self-hosted first account can have the stored ID `default`, which claims its legacy `default` tenant.
Select that account explicitly with this mapping, only when a matching `users.id = 'default'` row exists.

```json
{"version":1,"kind":"existing-account","user_id":"default","tenant_id":"default"}
```

A missing mapping still refuses populated account databases, including a claimed `default` account.
Mapping another user ID onto the shared `default` tenant remains unsupported and fails before product database writes.
For supported legacy data in shared single-user-auth copies, use the [offline ownership copy](#copy-legacy-default-ownership-into-an-account-mapped-desktop) below.

Core pins mapping presence and the SHA256 of its bytes under the storage lease for the full runtime lifetime.
Every compatibility child checks the same pin before product startup.
A changed, removed, or newly created file rejects compatibility restarts until you stop and restart the full runtime.
Edit the file while the full runtime is stopped, then restart to select a different account.
Each authenticated request reads the selected user's current email, display name, password presence, and administrator flag.
The session provider is `desktop`. A nonadmin account stays nonadmin, and deletion rejects subsequent authenticated requests.

Mapped mode uses the isolated Source filesystem policy and runs tenant workers only for the selected account.
It skips global starter seeding, Source ownership repair, legacy export and manifest migration, outcome conversion, Advisor backfill, and shared transfer artifact sweeping.
It does not modify product rows outside the selected tenant to make them accessible.
Backup routes, including restore, are unavailable while mapped.
Replacing the database file, including through a symlink alias, rejects subsequent authenticated requests and stops scheduling tenant work.
Stop the runtime before restoring or replacing a copy, then restart for fresh admission.
Live external in-place database replacement is unsupported. Use the existing storage lease rather than writing around it.

With both gates enabled, an empty copy without a mapping retains the synthetic desktop owner.
An existing copy with users requires a valid mapping.
With either gate disabled, existing users remain refused and the original synthetic setup protocol remains unchanged.

## Copy legacy default ownership into an account-mapped desktop

GRE-353 adds an offline copy command for legacy mutable Plans, accepted Plan/Source history, and supported exports. Stop the source server and desktop first. The command reserves the source with `acquireDataDirectory`, reads its current SQLite snapshot including committed WAL data, and leaves its product database and files unchanged. Ownership lease records can change during reservation and release.

Run this command on Linux with Python 3 available. Publication uses libc `renameat2` with `RENAME_NOREPLACE`. The destination must be new, its parent must already exist, and it must not overlap the source. Existing files, directories, empty directories, and symlinks at the destination all cause refusal. The tool builds a private staging directory beside the destination and publishes the complete copy atomically. Caught errors remove staging and publish nothing. Abrupt termination can leave a private `.ownership-copy-*` directory. Power-loss durability is not guaranteed.

From `web/apps/server`, inspect the stopped source with the exact runtime gate enabled:

```sh
PP_DESKTOP_REBUILD=1 npx tsx --conditions=development src/ownership-migration-cli.ts inspect /absolute/source > inventory.json
```

The inventory contains a deterministic full-database `source_digest`, table names, row identifiers, claim placeholders, and an `exports` list of artifact paths, SHA-256 digests, Plan IDs, and current tenant IDs. It contains no credentials, settings values, or document contents. Review every row identifier against your source data. Create a separate manifest with this shape:

```json
{
  "version": 1,
  "source_digest": "COPY_THE_64_CHARACTER_DIGEST_FROM_INVENTORY",
  "selected_user_id": "account-a",
  "claims": [
    { "table": "build_profiles", "rowid": 1, "user_id": "account-a" },
    { "table": "build_profiles", "rowid": 2, "user_id": "account-b" }
  ],
  "exports": [
    {
      "path": "tenant-default/profile-1-Plan/checklist/checklist-CONTENT_SHA256.html",
      "sha256": "COPY_THE_ARTIFACT_DIGEST_FROM_INVENTORY",
      "profile_id": 1,
      "user_id": "account-a"
    }
  ]
}
```

Include exactly one claim for every inventory row, including independent rows assigned to other users. Each owner and the selected user must already exist in `users` and must differ from `default`. The selected user identifies the desktop account to open. Claims can distribute unrelated Plans among multiple existing accounts. The tool does not infer ownership, clone shared dependencies, or change IDs. A Plan, its Source relationships, parts, and progress must have consistent owners. Conflicting assignments, dangling references, missing or extra claims, duplicate claims, and destination uniqueness collisions cause refusal. Machine and filament profile references from unchanged foreign assignment rows must also resolve to the same owner.

Apply the reviewed manifest to a new destination:

```sh
PP_DESKTOP_REBUILD=1 npx tsx --conditions=development src/ownership-migration-cli.ts apply /absolute/source /absolute/new-desktop /absolute/manifest.json
```

The copy contains a private `desktop-account.json` v1 mapping with `user_id` and `tenant_id` equal to `selected_user_id`. Existing desktop admission rules still apply. Rows outside the default tenant retain their content and owners. Stored `projects.local_path` values under each Source's own repository tree are rebased to the destination, including foreign rows, so the copy never depends on the original directory. The global `default` `app_settings.schema_version` stays at `default`. The tool compares every staged table against the expected source rows before committing so trigger side effects cannot silently change unrelated data.

The copy supports default rows in `build_profiles`, `projects`, `profile_layers`, `parts`, `print_progress`, `source_docs`, `source_notes`, `plan_decisions`, `printer_profiles`, `process_profiles`, and `filament_profiles`, plus `source_revisions`, `plan_revision_input_sets`, `plan_revision_inputs`, `plan_accepted_input_sets`, `plan_revisions`, `plan_revision_parts`, `required_units`, `plan_revision_required_units`, `plan_revision_required_unit_sets`, `accepted_plate_revisions`, `accepted_plate_heads`, `accepted_plates`, and `accepted_plate_units`. Every supported default row needs its own explicit claim. It discovers tenant tables from SQLite rather than assuming this list covers the schema. Any populated default tenant table outside that list causes refusal, except the global schema version. Bundled starter printer, process, and filament profiles are supported and require explicit claims. Their scalar fields and resolved configuration JSON retain their original bytes. Imported or externally backed profiles cause refusal when `source_path`, `raw_json`, `raw_ini`, or `compatible_printers` is nonempty. This is not a whole-application ownership migration. Do not remove product data merely to bypass refusal.

The tool refuses default settings other than `schema_version`, drafts, printer queues, opaque project metadata and imported-path lists, nonempty decision parameters other than `{}`, and populated decision result JSON. It also refuses stored sessions, password reset tokens, and Plan share capabilities. Share bundles contain ownership references outside the supported row graph. No startup path or HTTP endpoint invokes it.

Filesystem support covers regular files in `repos/<project id>` and `sources/<project id>` for known projects. Symlinks and special files cause refusal. A nonempty `projects.local_path` must be an absolute path inside `repos/<its project id>`; external paths are refused. Every Source revision must have a safe `<its project id>/revisions/...` locator and an existing regular snapshot tree. Relative part and document paths must contain no parent segments, empty segments, backslashes, or external locators. Unknown top-level artifacts and native storage cause refusal. Standard `uploads`, `upload-staging`, `staging`, `backups`, `thumbnails`, `previews`, `thumbs`, and `covers` directories are accepted only when empty. Exports support the existing `profile-<Plan id>-<slug>/checklist/checklist-<sha256>.html`, `profile-<Plan id>-<slug>/direct-3mf/direct-<16 hex digest>.3mf`, and `profile-<Plan id>-<slug>/kit/<slug>-<timestamp>-<sha256>.print-partner-kit.zip` layouts. Files can be directly under `exports` for legacy default data or under the existing tenant directory. Each file needs an `exports` manifest claim with its exact path, digest, Plan ID and named owner. The owner must agree with the Plan's explicit row claim or existing tenant; foreign exports cannot be reassigned. Omitting `exports` remains valid when no export files exist. Unmapped files, wrong scopes, unsupported layouts and target collisions fail closed. Default exports move into `tenantExportDirectory` for their mapped owner; foreign exports keep their tenant scope. There is no copied `tenant-default` fallback. The tool preserves bytes and validates content-addressed filenames, without rewriting embedded export content. Accepted STL publications under `accepted-stl/profile-<Plan id>/<revision-<id> or empty>/<all or missing>/<color or color_dir>/content-<sha256>` and accepted Plate publications under `accepted-plates/profile-<Plan id>/revision-<id>` are copied after their tree is checked against the bundle or manifest. For a nonempty STL publication, each loose file must match its `accepted-stl.zip` entry by name, size and CRC-32. An empty STL publication has no files; the copy skips it, and the export recreates it the next time it opens. A crashed STL export can leave its `.tmp-XXXXXX` staging directory beside the `content-*` folders. The copy checks its Plan, revision, selection and grouping like a publication, then leaves it in the source and does not copy it. A crashed Plate export can leave a `.revision-<id>.tmp-XXXXXX` staging directory beside `revision-<id>`. The copy checks its Plan and revision, then leaves it in the source in the same way. Older export layouts and other temporary publications remain unsupported. Ownership markers, leases, `.desktop.lock`, `printpartner.lock`, and the source desktop mapping are excluded from the copy. Socket files outside those excluded ownership paths cause refusal.

The staged copy suspends ten known triggers inside its transaction after checking their definitions against the current migrations. These cover Part acceptance invalidation, immutable updates for Plan revisions, Plan revision Parts, Required units, Required-unit maps and map headers, plus immutable updates for Plate revisions, Plates and Plate units and Plate head ownership updates. It restores them before validation and verifies that the complete schema matches the source. All historical revision and input digests, existing Required-unit maps and owner relationships are checked. Every supported history row must fit the production operational limit of 64 KiB across its stored text columns, measured as UTF-8 bytes in SQLite. Builds with accepted history also receive the operational text budget check for their tenant ID, name, order number, special request and archive timestamp. Ordinary mutable Builds without accepted history retain their existing supported text values. These limits are checked again after owner reassignment in staging. Current revisions require complete Required-unit maps. Retained older revisions may lack maps only when no partial mapping or created-unit state exists. Current accepted Plans must pass the production operational reader and tracked Part byte verification in both source and stage. Valid format-1 input history retains its existing `uninitialized` operational result; it is not promoted to format 2. Plans whose working Part edits or deletions cleared the accepted revision pointer are also supported when their positive Plan version and validated history remain. The retained revision numbers must form the complete sequence from 1 through the Plan version, with each revision after the first pointing to its immediate predecessor and retaining tracked publication provenance with format-2 inputs. The inferred current revision must have a complete Required-unit map, positive unique projection IDs, valid stored Part booleans and consistent quantities between 1 and 10,000. Retained progress at inferred-current Part/unit coordinates must pass the production raw-boolean and assembly/completion checks. Other historical unit coordinates retain their existing ignored status. Historical projection IDs may refer to deleted working Parts for these dirty Plans; any surviving projection must still belong to the same Plan and owner. Their retained accepted-input pointer must match the latest historical revision, including its acceptance timestamp; legacy revisions must have no input pointer. The copy preserves their null pointer, working edits and `compatibility_dirty` operational result across restart. It does not select a historical revision or repair acceptance. A zero version with history or a positive version without history causes refusal. IDs, digests, tokens, actor identities and timestamps are preserved.

Accepted Plate database state is supported independently of export trees. Every retained Plate revision must have a valid historical Plan basis, a complete historical Required-unit map, and a valid layout digest, geometry, counts and associations. Raw placement values, pinned booleans and historical Part inclusion booleans must be valid. Plate revision numbers for each tenant and Build must be the complete sequence from 1 through the maximum retained number. An existing head must target that maximum revision. Headless history and stale Plan bases remain supported. Nonnull undo provenance must target the immediate predecessor on the same Plan basis. Null undo provenance remains valid. Source and staging must return equal results through the production Plate reader before publication. Tests verify supported Plate history through two reopens of both source and destination.

Plate admission also caps each history validation pass at 1,000,000 potential overlap comparisons across all retained revisions and owners. A Plate with `n` placed units charges `n * (n - 1)`, reserving both the legacy and current layout validation attempts even when the legacy digest matches. Unplaced units charge zero because neither attempt compares them for overlap. They still undergo the existing layout and history validation. The preflight scans stored unit identities and placement before full row materialization or layout reads. It stops at the first unit that pushes the cumulative charge above the limit and raises `OwnershipPlateComparisonBudgetError` with numeric `limit` and `charged` fields. The charge is a conservative bound, not a measurement of comparisons performed. There is no CLI or environment override.

Inspect runs one history validation pass and one set of production head reads, for at most 2,000,000 overlap comparisons. Apply runs three history validation passes, on the source, the source with owner claims, and the staged copy. It also reads production heads once on the source and once on the stage, for at most 5,000,000 overlap comparisons. Each head set is a subset of the validated history, so each contributes at most one additional history budget. These bounds cover the pairwise overlap loops, not total runtime, memory, row counts or other validation work. An oversized source fails before staging. A refusal leaves no published destination, removes any private stage and preserves source product bytes. Keep the source intact if it exceeds this limit; the ownership copy does not prune history or raise the budget.

The source digest is rechecked after reservation, after the SQLite backup and before publication. Product file hashes and export inventory are rechecked after copying. Any database change requires a new inventory and reviewed manifest. The command exits with status 1 on refusal and status 0 with a JSON receipt on success. Both inspect and apply require `PP_DESKTOP_REBUILD` to be exactly `1` or `true`.

## GRE-644 manifest commands, slice 1

`build_graph::ManifestOptionsCommand` adds `ReadKit`, `SaveKit`, and
`ReadBuilder` through `BuildCommand::Manifest`, behind `desktop_rebuild`.
`BuildOutcome::Manifest` returns typed outcomes. Use `into_success_body()` for
lossless JSON bytes, including UTF-16 selection IDs.

`WriterOwner::build_graph_with_observations` accepts the existing `DraftReads`
adapter and `PreparationLimits`. Capture authenticates and reads the graph in
a reader transaction, then releases that reader before adapter calls. Two
observations share one preparation budget and must have equal source stamps.
The queued command authenticates again and checks `basis_matches` inside its
immediate write transaction. Save validates selections in that transaction,
then writes the kit setting and configuration timestamp together. Cancellation
before commit rolls both back. Save refuses stale inputs without retrying;
builder reads retry once. Kit reads need no filesystem adapter.

This adapts the reference's command location to main's separate
`manifest_options` model. The builder outcome holds serialized bytes because
main keeps its builder model internal. Stored malformed kits read as empty,
missing Plans win over invalid save input, and omitted save fields reset to
empty defaults, as Node does. SQL read errors propagate instead of becoming
empty kits as in the reference.

Unlike the reference, selection validation runs inside the write transaction.
Unlike both Node and the reference, save rejects unknown group and variant IDs,
as required by this slice. D10b's validator continues to check cardinality;
the command adds unknown-ID checks without changing `observation.rs`.
Kit refusal texts and Plan id handling follow Node, as the GRE-451
`manifest-http` parity corpus checks: an id that names no Plan is 404.
The GRE-451 `manifest-checkouts-http` corpus replays builder reads and kit
saves over real Source checkouts through the production `SourceReads`, with
the unknown group and variant refusals declared as corrections.
Save observes attached checkouts even when selections are omitted, as the
reference does, so source failures and stale stamps still refuse that save.
Node only builds option groups when the update supplies selections.
Two observations share a budget instead of receiving separate budgets as in
the reference.

The public command tests cover Node response parity, save/read persistence,
stamp changes, database changes before writer admission, bounded builder retry,
unknown selections, cancellation, missing and foreign Plans, adapter failures,
and rollback when the freshness update fails. The Node fixture follows D10b's
JSON and generator pattern. Regenerate it from `web`:

```sh
npx tsx --conditions=development ../rust/crates/pp-storage/tests/fixtures/manifest-commands-node.mts apps/server > ../rust/crates/pp-storage/tests/fixtures/manifest-commands-node.json
```

Storage commands landed without HTTP. GRE-801 registers gated ReadKit on the
Build router (`docs/gre-801-manifest-http.md`). GRE-802 registers gated SaveKit
(`docs/gre-802-manifest-http.md`). GRE-803 registers gated ReadBuilder
(`docs/gre-803-manifest-http.md`).

## D11 required units (GRE-606)

`required_units` compiles only with `desktop_rebuild`. It has a model, readers,
and one write operation.

The model provides planning and selection digests, canonical decisions, exact
predecessor matching, required-unit assignments, conflicts, surplus tokens, and
selection basis rows. It keeps `DraftPart` manifest metadata separate from scalar
JSON. Planning hashes preserve UTF-16 ordering and lone surrogates in manifest
fields. Predecessor units are sorted by prior index before validation, as in Node.
Target, predecessor, token, and decision errors use Node's messages and order.

The readers verify stored data on every read, as Node's repository does:

- `storage::draft` loads a Plan draft and checks its snapshot digest. A selected
  reconciliation must exist for the same draft and be finalized, or the read
  fails with "Plan draft Required-unit selection is corrupt".
- `storage::saved` checks a finalized reconciliation's canonical JSON, digests,
  decision rows, and assignment rows.
- `storage::base` reads the accepted revision's Required-unit set and checks its
  coordinates, Object names, progress, orphan units, and mapping digest.
- `view::workspace` builds the draft workspace and its diff against the base
  revision. Diff rows sort by their serialized ID, as in Node.

Corrupt rows return Node's messages for the same checks, with two known gaps.
Selection basis rows share one message, "Required-unit selection basis is
invalid". Some malformed stored result JSON returns a different message than
Node, although both refuse the row. GRE-648 tracks both gaps.

The operation ports Node's `savePlanDraftRequiredUnitReconciliation` and the
`reconcile` path of `PlanDraftWorkspaceService`. Get a client with
`WriterOwner::required_units_with_policy`. `ReconciliationClient::reconcile`
queues one command on the single writer, as `build_graph` does. The writer runs
it in one immediate transaction. It checks cancellation before the transaction
starts and again before it commits, so a cancelled request writes nothing.
The expansion and insert loops also check cancellation once per assignment and
once per inserted decision and assignment row, so a cancelled selection stops
inserting and rolls back promptly. There is no timing target. Unexpected
transaction failures surface Node's
`Plan draft update failed` on the client, with the source retained through
`.context`, matching the publication client and Node's reconcile route.
Authentication failures and admission outcomes (`Cancelled`, `QueueFull`,
`Stopped`, `OutcomeUnknown`) stay typed and unwrapped.

Selection refuses a draft before it expands assignments when the sum of part
quantities is above 230. The error is "Reconciliation row limit exceeded".
230 is ten times the largest assignment list in the checked-in fixtures:
`edit-selected` in `working-draft-writes-node.jsonl` stores 23 assignments.
The constant is `storage::ASSIGNMENT_CAP`. Chad checks it against plans 14–23
before this feature is turned on. Raise that one constant if a real plan would
be refused. Node still expands a draft of this size. A quantity above 10,000
on one Part is still "Required-unit target quantity is invalid", which is
checked before the cap.

- `ReconciliationRequest` has the contract's snake-case fields. It rejects
  unknown fields, a snapshot digest that is not 64 lowercase hex characters,
  more than 10,000 decisions, and two decisions for the same draft Part.
- `ReconciliationCommand::new` trims the idempotency key as JavaScript `trim`
  does and requires 1 to 160 UTF-16 units, as the Node route does.
- A session records the user ID as the actor. An API key records `tenant:<id>`
  and updates the key's last use time.
- `Outcome` serializes with Node's `kind` values and workspace shape.
- A retry with the same actor, key, and payload returns the saved selection,
  including rows Node wrote. If the draft is still at the expected v1 digest
  with no selection, the retry selects the saved reconciliation again. A retry
  with a different payload returns `idempotency_conflict`.

GRE-416 mounts the operation as `PUT /plans/{id}/drafts/{draft_id}/reconciliation`
on the draft router (see [D24 drafts HTTP](#d24-drafts-http-gre-619)). The local
`Decision` and request types exist because `pp-storage` has no dependency on
`pp-contracts`.

Tests open Node-written databases in
`rust/crates/pp-storage/tests/fixtures/required-units/`. Each operation test
compares the outcome with the Node result in the matching JSON file, or with
literal digests from Node's service, and checks the stored rows. The
`required-unit-fixture` binary runs one request twice, with a restart between
runs, and prints both outcomes and the database before and after:

```sh
PP_RECONCILIATION_SESSION=required-unit-fixture-secret \
  cargo run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild,test-unmarked-writer \
  --bin required-unit-fixture -- DIRECTORY PROFILE_ID DRAFT_ID IDEMPOTENCY_KEY REQUEST_JSON
```

Run the tests with:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib required_units -- --test-threads=2
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test required_unit_operations -- --test-threads=2
```

The T-61 acceptance target (GRE-416) replays the 24 seeded Node histories in
`fixtures/parity/required-unit-histories/node.json` (inclusion and quantity
edits, valid and invalid decisions in shuffled order, stale digests, key replays,
changed payloads and a restart) through the public clients, checks every refused
request left the draft, decision and assignment rows unchanged, checks surviving
units keep their predecessor's token, and checks a second tenant cannot read,
replay or change the first tenant's selection. Active production and running
operations refuse publication with every membership row unchanged in the
publication parity cases `production-active` and `running-export`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
  --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -E 'test(ticket_t_61_required_units) | test(reconciliation_)'
```

## D19 publication model and production links, GRE-612 PR 1

`pp-storage::plan_publication` is internal and requires `desktop_rebuild`.
The feature remains off by default. This slice ports publication assignments,
preparation, revision digests, accepted projection validation, production setting
validation, and Required-unit token remapping from the source-sync reference.
The optional `pp-contracts` dependency supplies `UnmappableLink`; the boundary
check rejects this dependency if it becomes required or reachable by default.

PR 2 adds the transaction entry described below. PR 3 adds `PublicationClient`,
authenticated admission, schema installation, and writer `Envelope` dispatch.

`node fixtures/parity/plan-publication/generate.mjs` regenerates literal Node
results in `fixtures/parity/plan-publication/node.json`; `--check` detects drift.
It requires Node 22.18 or newer and `npm --prefix web ci --ignore-scripts`.
The module hooks transform TypeScript and select development package exports,
so the real database probe needs no server or package build.
The fixture header records the generating Node version. The generator imports
`preparePlanPublication`, `digestPlanRevisionParts`, `assessCheckoffRemap`, and
`requiredUnitObjectName`. It extracts and executes the repository's inline
accepted revision check, `projectionPlanningFieldsMatch`, and private
`applyCheckoffRemap` and `strictProductionState` methods with row and setting
adapters. It does not copy their implementations. The transaction cases below also import
the real repository and SQLite database layer.

Rust tests compare literal digests, progress, assignment coordinates, object
names, remap mappings, and persisted settings. Shared projection cases compare
acceptance and rejection. Production validation accepts Checkoff coordinates
owned by another profile, matching Node. Remapping treats an ownerless queue item
as related when any part belongs to the profile or no referenced parts still exist,
even when its state is `done`. Node's exact error messages are retained in the
fixture; tests separately assert the Rust reference's more specific diagnostics.
Node's inline projection check has no progress-validation counterpart. Both
implementations remap Checkoff and send-queue coordinates by Required-unit token
(GRE-677): a kept unit follows its token to its new index, and a dropped or
excluded unit refuses the remap. The Node transaction cases
`mapped-remap-index-change` and `mapped-remap-index-0` record this. Direct Rust
tests cover progress corruption, excluded or missing tokens, and changed indexes.


## D19 publication transaction and job conflicts, GRE-612 PR 2

`publish_in_transaction` mirrors Node `AppRepository.applyPlanChanges` and takes
an existing `&Transaction`. D13 save can call it inside its own transaction with
an authenticated tenant and actor, profile and draft IDs, admitted and execution
requests, an idempotency key, and a Required-unit token allocator. It returns a
publication `Outcome`. The caller commits success or rolls back errors.

The transaction verifies the accepted projection, selected reconciliation and
live selection basis before publishing immutable revisions, Parts, input sets,
Required-unit mappings, progress, accepted pointers, and a replayable receipt.
`plan_publication/schema.sql` supplies the admission table and immutability
triggers for PR 3's schema installation. PR 2 adds no startup migration or client.
The default build remains unchanged.

`jobs::publication_conflicts` and `printer_upload_conflicts` move here from D16
because publication must check execution jobs inside the same transaction.
Exports use their profile ID. Uploads and deferred starts also resolve Checkoff
coordinate ownership. All terminal jobs, including uploaded-only uploads, are
skipped. An active deferred start bound to an uploaded-only parent still blocks
publication and validates the parent upload binding. The supplied-source import
arm waits for GRE-610's payload variant.

The generator records 26 real SQLite transactions covering receipt replay,
already-applied drafts, idempotency conflicts, changed drafts and bases, pinned
Source inputs, archived Builds, a corrupt accepted pointer, reconciliation
reasons, active production without remapping, and Checkoff links, resolved units,
and ownerless queues, with and without their optional unit arrays. Rust restores
each database on Node's schema and compares literal outcomes and publication
rows, normalizing only publication time and the field casing of receipts and
production counts.

Preparation sorts assignments by Part and unit index before validating them, as
Node does. Preparation and the transaction's Build check measure the 64 KiB
operational text limit as Node does: the UTF-8 length of Node's named Part and
Build fields. A lone UTF-16 surrogate counts as the 3 bytes of U+FFFD. Rows the
transaction inserts are still checked by the sum of all their text columns. The
Build check runs after the archived check, so an archived Build returns
`build_archived`. A set accepted revision with a nonpositive Plan version returns
`accepted_baseline_required`.

Node `applyPlanChanges` does not check durable jobs, so its running-export case
publishes. Rust returns the exact job conflict list and leaves publication rows
unchanged. Both runtimes publish with a terminal uploaded-only upload.

Rust keeps Node's Checkoff remap refusal reasons for deleted Parts, missing match
keys, and indexes at or past the new quantity. After those checks, both runtimes
write coordinates by Required-unit token and refuse if the referenced token was
dropped or excluded (GRE-677).

This rule applies whenever a kept token's index changes. `Selection::PriorCompletion`
renumbers units with no quantity change. In the index-1 fixture both runtimes move
the coordinate to the kept token's new index. At index 0 both refuse the dropped
token and write nothing. Index 2 and a cross-Part filename rename refuse with
Node's quantity and match-key reasons. Fixtures record Node's literal outcomes,
and Rust compares them exactly.
The prior-completion fixtures change the STL, keep the quantity at 3 and accept
prior completion with units 1 and 2 completed. Both runtimes refuse a link at
index 0, whose token is dropped, and move links at indexes 1 and 2 to 0 and 1.

## D19 publication client and writer admission, GRE-612 PR 3

With `desktop_rebuild`, `WriterOwner::publication_with_policy` validates the
deployment's `AuthPolicy` before constructing a `PublicationClient`.
`SingleAccountDefault` resolves sessions to tenant `default`; `AccountTenant`
resolves sessions to the account's tenant. Both retain the account as the receipt
actor. Invalid policy fails before queue admission or writes. The policy-less
`publication()` helper is available only in unit tests. `PublicationCommand`
trims keys with ECMAScript whitespace rules and admits 1 to 160 UTF-16 units.
`normalized_http` keeps the original admitted request separate from a request
normalized by a caller. The writer resolves the authenticated tenant and actor
inside the publication transaction. Request data cannot select either identity.

`PublicationClient::apply` uses the bounded writer queue. Before admission,
cancellation returns `required_units::AdmissionFailure::Cancelled`, a full queue
at its deadline returns `QueueFull`, and closed or poisoned admission returns
`Stopped`. After admission, the caller waits for the result even if cancelled;
a lost writer reply returns `OutcomeUnknown`. The writer uses its dequeue
transaction deadline and the shared bounded SQLite lock admission path. It
commits applied or replayed receipts and rolls back unsuccessful publication.
Its token allocator uses operating-system randomness.

The feature installs and validates the admission table and immutable triggers
at schema 37. Main never uses version 36: the reference's version 36 import
tables arrive with D14's version 38 step. Reopening checks schema text and
orphan admissions. Without the feature, storage remains at schema 35 and refuses
a newer database.

D13 save, GRE-608, can call `AuthenticatedPublicationContext::resolve` and
`publish_in_transaction` on its own transaction, using `PublicationRequests::new`
for admitted and execution requests. That caller owns commit or rollback. It
must not enqueue `PublicationClient::apply` while holding its transaction.
D13 integration remains GRE-608's caller work.

Eight literal fixtures run the actual Node Apply route and workspace service
against fresh SQLite databases. They cover missing, whitespace-only and
UTF-16-sized keys, embedded BOM preservation, identical replay receipts,
session actor attribution, tenant isolation and unexpected-error text. The
fixture supplies authenticated request context; authentication middleware
itself is covered by the auth tests. Rust compares these literals through its
client, queue and transaction executor with deterministic fixture tokens. A
separate owner test proves schema installation, random token generation,
writer dispatch, shutdown and replay after reopen.

GRE-665 item 1 applies because this public client can return errors. Invalid
keys now use Node's `Request is invalid`; unexpected publication errors use
Node's `Plan draft update failed`. The Node error fixture injects a SQLite
trigger failure through the real route. Internal transaction callers retain
the original diagnostic error. Authentication failures retain the auth module's
existing errors. Queue admission has no Node counterpart; direct Rust tests
check the supplied reference behavior, including cancellation after admission.

### D17 source scan: observation and settlement

D17 (a), GRE-613, ports `source_scan` document records, observations, and
settlement from `desktop/source-sync-local-current-20261008`. The
`desktop_rebuild` feature gates the module and the jobs commands
`ObserveLocalScan` and `SettleLocalScan`. Settlement rechecks the original
credential, Source reservation, configuration, activation, and path. It replaces
the document index and records a confirmed effect receipt in one transaction.
PDFs remain pending for extraction. Settlement does not finish the job or retire
its Source token. Callers must complete, reconcile, release or drop `source_work` promptly.

The three-PR slice plan assigns the complete D17 scope as follows:

| Slice | Work | Estimated added/changed lines |
| --- | --- | ---: |
| D17 (a) | Document types, observation/settlement, journal SQL, jobs commands/clients/outcomes, `classify_issued`/`proof` callers | 600 |
| D17 (a) | Settlement, authority and reservation tests; Node generator and golden; docs | 650–850 |
| D17 (b) | Complete, inspect, inspect-held and reconcile commands/clients/outcomes; both transaction decisions; `retire_issued`/`consume_retired` callers | 780 |
| D17 (b) | `revalidate_source_authority`, `commit_source_authority_refusal`, idempotent End/release, remaining reference catalog tests | 400–500 |
| D17 (c) | Schema 40, `backups/pre-schema40.db`, validation inside the migration transaction, migration tests and docs | 150–200 |

D14 (b), #553, supplies schema 39 and the Source configuration and document
provenance columns. D17 (c) installs the scan journal at schema 40 through the
production migration. The scan tests open `WriterOwner` and use that schema
without ad-hoc tables or column alterations. The default build stays at schema 35.
Source Sync remains D18, GRE-614.

`fixtures/parity/source-scan/generate.mjs` runs Node's
`services/source-docs-index.ts`, which calls `services/source-docs-scan.ts`.
Four cases cover empty input, markdown/PDF indexing, ignored directories and
Unicode paths. Rust tests assert the committed Node rows and counts. Node stores
24 hex characters of the content hash; the reference Rust journal uses full
SHA-256. The generator also records full Node crypto hashes for Rust input.
The probe sorts paths by JavaScript code units for the settlement contract and
records inventory/index digests from Node crypto. Durable settlement, Source
reservations and authority checks have no Node counterpart. The tests adapt the
observation/settlement portion of the reference catalog test
`repeated_local_scan_completion_adopts_the_exact_terminal_result` and its Source
lease lifetime checks. D17 (b) ports exact terminal-result adoption and release
acknowledgement. New tests exercise stale observations, revoked authority,
invalid documents and transaction rollback.

Regenerate or verify the Node results with:

```sh
node fixtures/parity/source-scan/generate.mjs
node fixtures/parity/source-scan/generate.mjs --check
```

D17 (b) adds `CompleteLocalScan`, `InspectLocalScan`, `InspectHeldLocalScan` and
`ReconcileLocalScan`. Completion commits the public result and journal digest
before retiring the Source token. A repeated completion adopts only the exact
terminal request and result. Held inspection recovers settlement evidence after
a lost reply. Reconciliation retains the effects and records the failure before
retiring the reservation. Release acknowledges already retired tokens and keeps
the proof if acknowledgement fails.

GRE-703 items 2–4 bind settlement to the journaled reservation incarnation and
token, test retired proofs and cancellation after observation, and replace the
raw UNIQUE error with `LocalScanFailure::AlreadyObserved`. D17 (c) addresses
GRE-703 item 1 by moving the test harness onto the real migration. It also removes
the client schema prerequisite notes for GRE-707 item 6.
`mark_local_failure`, `revalidate_source_authority` and
`commit_source_authority_refusal` retain their tests; this schema slice adds no
production callers for them. GRE-610 (D14 import Phase) calls the two authority
helpers. `mark_local_failure` keeps its `dead_code` expectation until GRE-614
(D18 source_sync) calls it. `commit_source_authority_refusal` reuses
`AuthorityRefusalReason::of` and saves one refusal.

The Node generator now also executes the local `syncProjectById` completion and
failure paths. Rust compares four terminal document-index results and two
failure-metadata cases against Node. Durable completion, inspection and
reconciliation have no Node counterpart. Tests port the remaining reference
completion and release checks, and add refusal, rollback and lost-reply cases.

### D17 source scan: schema 40

D17 (c) completes GRE-613 behind `desktop_rebuild`. The 39 to 40 step installs
`source_scan_executions` and its receipt index from `source_scan/schema.sql`.
`IF NOT EXISTS` preserves the reference catalog text and lets the step validate
an existing table and refuse drift without replacing it.

The upgrade keeps D14 (b)'s order:

1. Back up the input before any write. From 39, this is `backups/pre-schema40.db`.
2. Restore common objects and repair tenants without starter seeding.
3. Back up each pending step, install and validate it inside one transaction, then
   stamp and commit its version. The scan validator runs before the 40 stamp.
4. Validate the installed schema, then seed starters.

Preflight validates the scan journal for the stored version. A database below 40
with scan objects is refused. Reopening at 40 validates the journal without a new
backup. A fresh install creates no backups. The default build refuses schema 40.

Upgrade tests cover 35, 37, 38 and 39 to 40, logical backup contents from 39,
a blocked backup with unchanged input, and a drifted existing journal. The direct
initializer test bypasses preflight to prove that step validation rolls back the
new index and leaves version 39 intact. The same drift is also refused through
`WriterOwner` preflight. Logical comparisons include the SQLite catalog and rows,
because SQLite's backup API can rewrite header counters.

### D18 source sync: batch open and target claim

D18 (a), GRE-614, ports the first part of `source_sync` from
`desktop/source-sync-local-current-20261008` behind `desktop_rebuild`. A Sync
attempt opens a batch, then claims its targets one at a time. The default build
is unchanged and stays at schema 35.

| Slice | Work | Estimated added/changed lines |
| --- | --- | ---: |
| D18 (a) | Schema 41; `open`, basis validation, target claim with Source reservation, ordinary failures, `halt_job`; Cancel and worker Fail halts; bridge removal; tests, Node probe, docs | 1,600 |
| D18 (b) | Target validation, `settle`, `fail` with `mark_local_failure`, `acknowledge`, `adopt`, `halt_after_error` and `HaltSourceSync`; recovery of retained evidence; tests and docs | 1,450 |
| D18 (c) | `finish`, `inspect`, `inspect_retained`, the recovery read types, the journal-owner guards for generic Finish, effects and reconciliation, claim and advance refusals, the prune guard; tests and docs | 1,200 |

`open_source_sync` seals the batch the first time. An explicit selection keeps
payload order. A selection of all Sources orders them by `last_synced_at`, then
id, as the reference does; Node's `listProjectIds` uses table order. A later open
revalidates the sealed basis and returns claims from earlier attempts to pending.

`claim_next_source_sync_target` claims the first target that is not settled, in
one writer transaction. A missing Source fails with Node's `Source not found`. A
non-Local Source fails as `unsupported_kind`. Neither failure takes a reservation.
A Local target records the attempt, the Source observation and a new reservation
token. The token is activated after the commit, as claim-time reservation does.
The reference uses three round trips for this: next, `BeginSyncTarget` and bind.
A busy Source returns `SourceBusy` and leaves the target pending. A claim before
open is refused. The returned `SyncTargetLease` holds the Source until it is
released or dropped, so callers must not keep it longer than the target's work.

Cancel, a worker `Fail` and a revoked original credential halt the batch. Pending
targets become not-run. A claimed target is kept for reconciliation, and the Job
becomes `reconciliation_required`. Otherwise the Job is cancelled or failed. A
revoked credential records a Source-writer refusal. Until D18 (b), a claimed
target cannot be settled. GRE-854 rejects generic `Finish`, `FinishPublic`,
`BeginEffect` and `ConfirmEffect` for Sync before authority revalidation.
These calls leave the Job, batch, target, history and Source reservation unchanged.
Heartbeat and progress updates remain available, and worker `Fail` still halts
the batch. Non-Sync jobs and builds without `desktop_rebuild` keep their existing
generic worker behavior. GRE-775 stores one successful target and releases its
reservation. GRE-776 records one local read failure, the Source failure
observation and progress, and leaves that reservation unacknowledged so the
failure stays retained evidence. GRE-777 acknowledges that retained failure when
the presented observation matches the claim. GRE-778 adopts an already-
acknowledged successful result when the live Source observation still matches
the claim capture, and refuses mismatched provenance without writing.
GRE-779 ports `halt_after_error` and `HaltSourceSync` so a caller Source
operation error closes through the existing batch halt path. The detail is the
Job recovery message. Retained failures stay as recorded. A claimed target is
kept for reconciliation. A stale attempt is refused without writing.
GRE-780 ports `inspect` and `InspectSourceSyncTarget` so the authorized worker
reads one live Sync target without writing or changing reservation ownership.
Claimed targets report `Pending`; acknowledged successes and failures return
their stored evidence. A foreign tenant row or Job mismatch is refused without
writing.
GRE-781 ports recovery read types and `inspect_retained` through
`inspect_source_sync_recovery` so a credential-scoped caller validates retained
target/result/observation bindings and reads each disposition without adopting
or deleting evidence. A corrupt Source binding, incomplete success capture,
batch-phase mismatch or stale version is refused without writing.
GRE-782 ports `finish`, `validated_terminal_aggregate` and
`FinishSourceSync` so a Sync-owned finish succeeds only after every target has
a valid acknowledged terminal result. The aggregate is stored on the batch.
Exact retry returns the same aggregate without writing. An unsettled target is
refused without writing.

GRE-857 preserves a Sync Job's completed Source-writer refusal evidence when
Cancel marks the Job for cancellation during reconciliation. The refusal
observation, target failure code and detail, recovery message, and generation
remain unchanged.
A second Cancel writes nothing. Repeating the same authority-refusal halt also
writes nothing, including when the first halt failed the Job without a claim.
The regression tests cancel twice and reopen storage to check the saved evidence.
GRE-855 applies the same Source-writer refusal and batch halt during
`open_source_sync`, heartbeat, progress and worker `Fail`. A recognized refusal
commits before returning its typed authority error. It fences the attempt even
before a batch exists. An existing batch halts with pending targets marked
not-run, or requires reconciliation when claims or retained effects remain.
Open-time refusal preserves a prior attempt's claim evidence instead of resetting
it to pending. Unrecognized errors still roll back. Tests reopen storage to check
that the refusal, batch and target evidence persist. Cancel preserves a completed
worker-advance halt and its refusal evidence.

The reference schema numbers Source Sync 42 because its 41 validates manifest
columns, which main does not have. Here the 40 to 41 step installs
`source_sync_batches` and `source_sync_targets` from `source_sync/schema.sql`,
following D17 (c): back up to `backups/pre-schema41.db`, install and validate in
one transaction, then stamp 41. Preflight refuses Sync objects below 41.

`fixtures/parity/source-sync/generate.mjs` runs Node's `InProcessJobRunner` Sync
job over a missing id and a Local Source. The Rust test asserts the target order
and Node's literal failure. Node has no durable batch, Source reservation or
authority check. The claim-before-open, restart-recovery and ordinal tests adapt
the reference `pp-core/tests/source_sync_local.rs` cases. The halt, refusal,
busy-Source and bridge tests are new. The generic-worker regression checks every
effect kind and both Finish APIs after a target claim, with the original session
valid and revoked. It compares database snapshots and verifies that the same
Source reservation still prevents other work.
The non-Sync regression confirms that generic effects and Finish still work.

```sh
node fixtures/parity/source-sync/generate.mjs --check
umask 022
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib source_sync::
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test job_feature_admission
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --test durable_jobs
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test durable_jobs
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets --features desktop_rebuild
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib uploads::tests
```

### D20 Checkoff progress

GRE-615 adds `pp-storage::checkoff_progress`, compiled only with
`desktop_rebuild`. `WriterOwner::checkoff_progress()` returns a client whose
`apply` runs one request in one immediate writer transaction. No routes call it
yet; D24 and D26 wire HTTP behind `PP_DESKTOP_REBUILD`.

The contract is `db/accepted-plan-progress.ts`, `setAcceptedPrintedCounts` in
`db/repository.ts`, the progress and assembly routes in `routes/parts.ts`, and
the import route in `routes/plan-drafts.ts`. `apply` returns the Node status
and JSON body for each outcome:

- Completion by Required-unit token marks that unit and every lower unit
  printed. Clearing it clears that unit and every higher unit, including their
  assembly.
- Assembly by token sets one unit. Assembling an unprinted unit changes nothing.
- Part coordinates (`/parts/:id/progress` and `/parts/:id/assembled` bodies)
  resolve to a token against the current accepted snapshot.
- Import sets each listed Part's printed prefix. It checks every row before it
  writes any, and reports how many Parts changed.

Every write first checks the expected accepted basis, then archival. Unit
completion and assembly then check that each Part has one progress row per
unit. Import skips that check, as Node does. Only a successful write commits,
so a refusal leaves the database unchanged, including API key usage. An import for a Plan the caller cannot see returns 404
`progress_target_not_found`. This differs from Node on purpose. Node throws a
plain error and returns 500, but its route already returns 404 for a Plan it
does not serve.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test checkoff_progress
```

### Domain event log, GRE-1088

The 41 to 42 step installs `domain_events` and its index. Checkoff
writes, including GRE-420 verify, bursts and burst undo, and Accept append an
event in their own transaction when they change a row. The step writes no
step copy of its own; the copy taken at open covers it. See
[Domain events](domain-events.md).

### Domain event stream, GRE-1162

`GET /api/v1/events` serves the log as Server-Sent Events from `read_router`,
so the Rust-only runtime mounts it. It resumes from `Last-Event-ID`, answers
401 without a credential, ends the stream when the session is revoked, and
stops feeding a client whose bounded queue stays full for 10 seconds; that
connection closes at the server's next write, or after 30 seconds in which the
client accepts no bytes. Shutdown draining ends open streams and does not wait
for them. See
[Domain events](domain-events.md#http-stream).

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --lib events::
```

### Printer Checkoff verify and dismiss (GRE-420)

GRE-420 adds two `checkoff_progress::Request` variants, still compiled only with
`desktop_rebuild` and still called by no route. Each runs in the same immediate
writer transaction as the other Checkoff writes.

- `Request::Verify { body }` takes the `POST /printer-checkoff/verify` body:
  `link_id`, `decisions` and optional `additional_decisions`. It ports
  `verifyPrinterCheckoff`, `verifyAcceptedPrint` and
  `applyAcceptedUnitDecisionsInternal`. In one transaction it reads the link
  from the `printer.checkoff_links` setting, checks the decisions against the
  link's pending units and the current accepted snapshot, marks confirmed units
  printed, records the resolved units, imported-extra results, state and
  `units_marked` on the link, and appends one `print_jobs` row with one
  `print_job_parts` row per decision. It returns Node's status, `detail`, and
  `{ link, units_confirmed, units_rejected, outcomes }` body.
- `Request::Dismiss { body }` ports `dismissHostFailedLink`: a `host_failed`
  link becomes `dismissed`.

Links are parsed, normalized, moved to the end and trimmed to 200 exactly as
`services/printer-checkoff-store.ts` writes them, so the stored setting matches
Node byte for byte after a write, except for lone surrogates (below). Only an explicit authenticated request can
verify. A link that a status watcher still holds in `watching` refuses with 409,
and a repeated decision or a second link for an already printed unit cannot
count a unit twice: the first refuses because the unit is no longer pending, the
second records the outcome with `units_confirmed: 0`.

One correction differs from Node on purpose. Node writes each Part's confirmed
units before it checks the next Part's left-fill rule, then returns 400 and
commits the earlier Parts. Rust checks every Part first, so a refused burst
writes nothing. The parity fixture keeps this case as its last step and the test
asserts the one progress row Node left behind.

Text is cut at UTF-16 units like Node's `.slice`. When a cut splits a surrogate
pair, Node stores a lone surrogate escape. Rust strings cannot hold one, so
storage reads a lone surrogate in the stored links as U+FFFD and writes U+FFFD
where its own cut splits a pair. The rest of the setting still loads; before
this, one such string made every link in the tenant unreadable. The fixture
seeds a lone surrogate and cuts a note across a pair to cover both directions.

Not ported here: the route itself and its webhooks, unattributed-print claiming
after verify, the Spoolman deduction Node runs after the response (and the
storage-owned deduction intent GRE-411 adds), file assignments, remote-file
recheck, and the monitor that moves links to `awaiting_verify`. Those stay on
Node until their whole operation moves. Manual complete moved with GRE-1169
(below).

The Node fixture replays the services on a copy of
`rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db` with
synthetic links; generated IDs and times are normalized.

```sh
cd web && node --conditions=development --import tsx ../fixtures/parity/printer-checkoff-verify/generate.mjs --check
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --no-tests=fail -E 'test(ticket_t_61_checkoff)'
```

### Manual complete (GRE-1169)

With `desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true`, the Checkoff
router mounts Node's `POST /printer-checkoff/{id}/manual-complete` (and its
`/api/v1` and `/api/v2` aliases) behind the same loopback, Host, Origin and
credential gate as the bursts.
`pp-storage::checkoff_progress::verify::manual_complete` runs in one writer
transaction and matches Node:

- 404 `Tracked print not found` when the link id is unknown.
- 409 `This print is monitored by its printer host` unless the link's
  `integration_id` is `manual:<printer_id>`.
- 409 `Tracked print is no longer waiting to finish` unless the link is
  `watching`.
- Otherwise the link becomes `awaiting_verify` with `host_outcome: success`,
  `saw_active: true`, `last_progress: 100` and a fresh `completed_at`, moves
  to the end of the list before the terminal-history trim, and the response is
  `{ link }`. Nothing is checked off; the operator still verifies the print.

Node's handler has no side effects after the write, so this route owns the
whole operation. Single-link verify and dismiss stay on Node: they deduct
Spoolman stock, send webhooks and claim unattributed prints after the write.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test checkoff_single_http
```

### Checkoff bursts (GRE-1090)

With `desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true`, the gateway mounts
`POST /printer-checkoff/verify-batch` and `POST /printer-checkoff/verify-batch/undo`
at `/` and `/api/v2` (`pp-api/src/checkoff.rs`, admission through
`accepted::Gate`: loopback, exact Host and Origin, session or API key). These
routes exist only in the Rust-owned runtime: with the flag on, Rust holds the
ownership marker and `.desktop-lease` for the data directory and standalone Node
is refused (see Writer admission above), so no Node verify, monitor or claim
writes `printer.checkoff_links`, progress or outcomes beside a burst. A burst
does not run Node's unattributed-print claim or Spoolman deduction after verify;
those are GRE-1169 and GRE-411 (deduction intent).
MCP `propose_verify_printer_checkoff` applies through the same burst
(GRE-1170, below).

A burst body is `{ "idempotency_key": "...", "beds": [{ "link_id", "decisions",
"additional_decisions"? }] }`: 1 to 200 characters of key and 1 to 50 beds, each
naming a different link and shaped like a `POST /printer-checkoff/verify` body.
`pp-storage::checkoff_progress` runs every bed through the GRE-420 verify in one
writer transaction. The left-fill rule counts units that any bed of the burst
confirms, so a Part's units spread over several beds may come in any bed order.
The first bed that refuses rolls the whole burst back and
returns its status with the detail prefixed by `beds[i]: `. Success returns
`undo_token` and one result per bed.

The receipt is written in the same transaction under the
`printer.checkoff_bursts` setting: the key, the SHA-256 of the request with
object keys sorted, the response, and the link, progress and outcome rows undo
needs. A repeat with the same key and request returns the stored response before
any other check and writes nothing, also after a restart. The same key with a
different request returns 409. The newest 100 receipts are kept. A dropped
receipt leaves the SHA-256 of its key in `printer.checkoff_bursts_retired` (the
newest 5,000), and replaying that key returns 409 instead of running again, so an
undone burst cannot be counted again by a late retry. A replay of an undone burst
that still has its receipt (GRE-1190) returns 409 with a detail that starts
`burst_undone:`, writes nothing and appends no event, so a client is never told
a burst applied when its progress was undone. The receipt stays as it was, apart
from the `undone_at` mark undo wrote. To apply the same beds again the client
sends a new idempotency key.

Undo takes `{ "undo_token" }` and is a compensating write: it restores each
link at its original position, the progress rows and the outcome rows the burst
wrote. It refuses with 409 and a reason when any of them changed since, when
the burst was already undone, and when anything that can depend on the burst's
units changed after it: the burst stores the SHA-256 of every progress row of
the Parts it decided, the outcome rows that name its units, and every stored link
whose units or resolved units name them, and undo compares it. A later verify of
another link that records a unit the burst printed, or a later unit that filled
from the burst's unit, therefore blocks undo. Unrelated activity, such as a new
print job, a new link, or a dismiss of a link without the burst's units, does not. A receipt
without that hash or its unit list (written before they existed) refuses with
409 instead of undoing on weaker checks. Undo on a Build archived since the burst refuses like
every other progress write (409 `Archived Plan Progress cannot be changed`). An unknown token is 404. No printer, webhook or
Spoolman call happens in either transaction.

#### Reconcile by operation id (GRE-1170)

`GET /printer-checkoff/verify-batch?idempotency_key=...` (also at `/api/v2`,
same admission) answers what a key settled to without resending the body. The
key is read like the burst's key (JS-trimmed, 1 to 200 characters), and it must
appear exactly once, otherwise the answer is 400. The read writes no Checkoff
state; as on every route, an API key's `last_used_at` may be recorded.

| State | Body | Meaning |
| --- | --- | --- |
| `applied` | `{idempotency_key, state, applied_at, undone_at: null, receipt}` | The burst committed; `receipt` is the stored response |
| `undone` | the same with `undone_at` | The burst committed and was undone |
| `expired` | `applied_at`, `undone_at` and `receipt` are null | The burst committed, its receipt was trimmed, and the key cannot run again |
| `unknown` | `applied_at`, `undone_at` and `receipt` are null | The key is in neither list, but the retired list is full (5,000 keys), so a key older than the newest 5,100 bursts may have been dropped. The lookup cannot tell whether it ran, and replay would treat it as new |
| 404 `burst_not_found` | problem body | The key is in neither list and no key was ever dropped, so no burst used it and nothing was applied. Resending the same request with the same key is safe |

#### MCP `propose_verify_printer_checkoff` (GRE-1170)

The Rust MCP (`/mcp`, `/api/v1/mcp`, API key always required) ports Node's
`propose_verify_printer_checkoff` with the same name and schema
(`{link_id, decisions}`). The propose step reads the link in a writer
transaction that changes no Checkoff state and refuses as Node does: `link_id
and decisions are required`, `Checkoff link not found`, or `Checkoff link is
not awaiting verification`. Nothing is written until `confirm_apply`.
`confirm_apply` runs the proposal as a one-bed burst with the idempotency key
`mcp:<action_id>`, so it shares this burst's receipt, replay, undo and farm log.
It answers `{ok: true, result: <burst receipt>}`, which carries `undo_token`.
Node answered with the single-verify body. A refused burst answers
`{ok: false, status, detail}`, writes nothing and stays pending for a retry or
dismiss.

The action id is the operation id. `confirm_apply` for an action this session
no longer holds (applied already, or proposed before a restart or in another
session) looks up `mcp:<action_id>`. If that burst exists, the call answers
`{ok, reconciled: true, action_id, detail, result, undone_at}` instead of
Node's unknown-action error: `ok` is true while the burst stands, and false
once it was undone or its receipt expired. Otherwise, including an `unknown`
lookup, the call answers Node's unknown-action error. If the lookup cannot run
(the writer is busy or past its deadline, or authentication fails), the call
answers `{"error": "Checkoff is unavailable"}` and the action is not reported
as unknown.

The replacement UI's Today screen confirms one bed, or units picked on several
beds as one burst (one request, one Undo), through the generated client (`verifyCheckoffBurst`, `undoCheckoffBurst`) and resends a
lost send with the same key; see `web/apps/ui/README.md` (Today).

`web/packages/contracts/src/checkoff-burst.ts` holds the request schema and the
`P` (`plateBed`) and `F` (`partFillBeds`) helpers, which both produce this
request shape. `verifyBatchRequest` merges beds for the same link, keeping both
unit decisions and imported-extra `additional_decisions`, and leaves out beds with
nothing to decide (a plate still `watching`), which the route would refuse.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild,pp-api/desktop_rebuild -E 'test(verify_batch)'
npm --prefix web exec -w @print-partner/contracts -- vitest run src/checkoff-burst.test.ts
```

### Manual Checkoff bursts (GRE-1191)

The Build Checkoff tab records what was printed or assembled with no printer
link. With `desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true`, the
Checkoff router mounts two writes at `/` and `/api/v2`, and the Accepted Plan
reads mount one read at `/api/v2` only. All three are Rust-only; Node has no
route at these paths, so the gateway manifests do not list them.

- `GET /api/v2/plans/{id}/checkoff/units` returns `{ profile_id, basis, parts }`:
  the included Parts in the order of `GET /plans/{id}/checkoff`, each with its
  quantity, printed and assembled counts and every Required unit's `token`,
  `unit_index`, `object_name`, `printed` and `assembled`. `basis` is the
  Accepted Plan basis a burst sends as `expected`, or `null` for a Build with
  no Accepted Plan (`parts` is then empty). A Build the caller cannot see is 404.
  Each unit also carries `plate`, `{ plate_id, ordinal, printer_name }` for the
  head Plate revision's Plate it is placed on, and the body carries that
  revision as `plate_revision_id` (GRE-1121, for the Checkoff tab's `P` key).
  `pp-storage::plates::head_plates` reads them in the same transaction as the
  units. Both are `null` when the head revision stamps an older Accepted Plan
  or there is none; a unit the head lists as unplaced has `plate: null`. A head
  revision whose layout fails its integrity checks fails the read with 500,
  as `GET /plans/{id}/plates` does.
- `POST /plans/{id}/checkoff/bursts` takes `{ idempotency_key, expected, units }`:
  a key of 1 to 200 characters after trimming, the basis, and 1 to 1,000
  `{ token, printed?, assembled? }` entries, each setting at least one flag and
  naming a different token. Unknown fields are refused. `expected` for another
  Build than the path is 404 `Plan not found`.
  `pp-storage::checkoff_progress::manual` applies the burst in one writer
  transaction. Printed units fill from the left, as the single-unit writes do:
  `printed: true` prints that unit and every lower unit of its Part, and
  `printed: false` clears that unit and every higher unit and their assembly. A
  burst that prints a unit at or above one it clears on the same Part is 400.
  Assembling a unit that is not printed after the burst is 400. A token that is
  not on an included Part of the Accepted Plan is 404, so counts never pass the
  Part's units. A stale basis is 409 `Accepted Plan changed; reload and retry`,
  and an archived Build is 409 like every progress write. The first refusal
  leaves the database unchanged.
- Success returns `{ idempotency_key, undo_token, profile_id, printed_added,
  printed_removed, assembled_added, assembled_removed, parts }`, where `parts`
  holds each named Part's quantity and new counts. The receipt is stored in the
  same transaction under the `checkoff.manual_bursts` setting with the request's
  SHA-256 (object keys sorted), the progress rows as they were before, and the
  SHA-256 of what can depend on the changed units (the same state hash as
  printer bursts). Retention and replay follow printer bursts: the same key and
  request returns the stored receipt and writes nothing, also after a restart;
  the same key with another request is 409; a key whose receipt was dropped
  (newest 100 kept) is 409 from `checkoff.manual_bursts_retired`. A replay of a
  burst that was undone is 409 `This Checkoff burst was undone; send the change
  again as a new burst`, so a late retry never counts it again.
- `POST /plans/{id}/checkoff/bursts/undo` takes `{ undo_token }` and restores
  the progress rows the burst changed. It refuses with 409 and a reason when the
  Accepted Plan basis changed since the burst, when any progress row of the
  touched Parts, an outcome naming a touched unit, or a printer Checkoff link
  naming one changed since (`Checkoff changed since this burst: a later change
  on its Parts depends on it`), and when it was already undone. A token from
  another Build's path, or an unknown one, is 404.

Each Part a burst or undo changes appends one `checkoff.progress_changed` event
in the same transaction ([Domain events](domain-events.md)). Neither write
touches a draft, a Required-unit ID, a printer Checkoff link, a print job, the
send queue or Spoolman, so Checkoff never passes the Accept gate and never
deducts or reserves stock. The generated client names the three routes
`readBuildCheckoffUnits`, `recordManualCheckoffBurst` and
`undoManualCheckoffBurst`.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test checkoff_manual_bursts
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test checkoff_manual_bursts_http
```

### Print-file classification (GRE-1212)

`pp_domain::print_file::classify` (feature `desktop_rebuild`) is the Rust port
of Node's `classifyPrintFileBytes`. The file-assignment routes in GRE-1210
will use it to decide what a print file is from its bytes alone. It returns the
classification (`gcode`, `bgcode`, or `3mf` with `toolpath_package`,
`slicer_project`, `model_package` or `unsupported`), the SHA-256 and size of
the bytes, or one of Node's ten rejection reasons with Node's wording.
`next_action` and `is_print_ready` match `printFileNextAction` and the
contract's `isPrintReady`.

It reads files of up to 256 MiB. For 3MF it walks the ZIP central directory
only, with Node's caps (2,000 entries, 1,024-byte names, 64 MiB per entry,
256 MiB in total, depth 16), and refuses unsafe names and symlink entries. It
never inflates an entry or parses XML. Binary G-code is checked structurally
(header, version, checksum type, block chain) and never decoded.

`fixtures/parity/print-file-classification/cases.json` holds 85 inputs and
the Node classifier's answers: the Node unit-test inputs plus truncated 3MF and
binary G-code, ZIP64 records, hostile ZIP entries (including names with a leading byte-order mark, which Node's `TextDecoder` drops) and G-code edges.

```sh
cd web && node --conditions=development --import tsx ../fixtures/parity/print-file-classification/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-domain --features desktop_rebuild --test print_file_parity
```

### Print-file assignment storage (GRE-1217)

`Request::AssignFile` in `pp-storage::checkoff_progress` (feature
`desktop_rebuild`) is the create path of Node's
`materializeAcceptedPrinterLink`. It is what `POST
/printer-checkoff/file-assignments` records once the route has parsed the
request and read the file. GRE-1220 mounts it (next section). In one
writer transaction, against the Build's Accepted Plan:

- The plan revision must be the one the operator saw, or 409 `The Accepted
  Plan moved on; reload the Build and choose the file again`.
- Every confirmed unit must be an incomplete Required unit of an included
  Part, picked once, and every object mapping must point an unmatched or
  ambiguous object at a distinct confirmed unit. Otherwise 409 `That print
  file does not map to an incomplete Required unit in this Build`.
- No watching or awaiting-verify link may already hold the same file name
  (basename, any case) on that integration, or 409 `That print file is already
  assigned`.
- The link is created watching with the confirmed units (each with its
  Required-unit object name), the unmatched names as `unlabeled_names`, the
  remote path, identity and classification, and, with `retain_unmatched`, the
  unmatched objects (or the file itself) as pending imported extras. With
  `completed` it is created awaiting verify with Node's host-completed fields.
  Node applies that patch in a second transaction; here it is the same write.
- The response is `{ link, attribution }`, with Node's camelCase
  `attribution` object.

`checkoff_progress::attribution` is now the one port of
`resolveAcceptedPrinterAttribution` (per-name outcomes, the positive-only rule
that drops fuzzy file-name matches, the file-name fallback),
`confirmAcceptedPrinterUnits` and `applyConfirmedObjectMappings`. The
unattributed-print claim uses it too. `pp_domain::gcode_objects::match_object_name_exact`
is the matcher without its fuzzy step.

A Build that does not exist is a 500 for both claim and assignment, because
Node's operational read throws `Profile not found` and both routes answer that
with a plain 500. The claim answered 409 `Accepted Plan operational state is
not initialized` before.

`fixtures/parity/printer-file-assignment/node.json` replays 18 create commands
through Node's repository (and its completed patch) on a copy of the
required-units fixture: matches by Required-unit name and by file name,
confirmed and mapped units, every refusal the fixture can reach, kept extras, a
completed print and a missing Build. The Rust test replays them and compares
every response and stored row.

```sh
cd web && node --conditions=development --import tsx ../fixtures/parity/printer-file-assignment/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test printer_file_assignment --test unattributed_claim
```

### Print-file assignment routes (GRE-1220, GRE-1255)

`pp_api::file_assignments` serves `POST /printer-checkoff/file-assignments`,
`POST /printer-checkoff/file-assignments/preview` and
`POST /printer-checkoff/file-assignments/upload` at the root and under
`/api/v1`, where Node registers them, behind `desktop_rebuild` and
`PP_DESKTOP_REBUILD`. Each mount has Node's windows per route: 20 a minute
for the preview and assignment with their 1 MiB JSON limit, and 10 a minute
for the upload.

- Storage parses the body in Node's `parsePrintFileRequest` order and resolves
  the printer as `resolveAssignmentPrinter` does: a fleet printer by id, its
  host or `manual:<id>` with `tracking: "manual"`, or, with no `printer_id`,
  the unmanaged printer (`manual:unmanaged`). The assignment route then checks
  `plan_revision_id`, `unit_tokens` and `object_mappings`; the preview reads
  the Build's Accepted Plan and builds `suggestPrintFileAttribution`'s
  suggestion and match review. Every refusal keeps Node's status and detail.
- The route reads the file. A `snapshot_token` is trimmed and needs a
  `remote_path` and no `upload_token`, or 400 `A snapshot token requires its
  original printer file path`. It redeems the classification and identity
  captured for exactly that Build, printer, path and file name while the
  printer's linked integration is unchanged; anything else is 409 `This file
  preview expired or its source changed. Cancel and reopen the file.` A
  redeemed file is not re-observed for drift, and a successful assignment
  spends the token. Node redeems and records in one synchronous step; here the
  assignment reserves the token inside the storage call, so a concurrent
  assignment of the same token is refused, and an admitted write spends it
  even if the request is dropped. An outcome that is unknown after admission (a
  lost writer reply or `CommitUnknown`) spends it too, so the operator reopens
  the preview instead of recording the file twice. Any other answer, a refusal
  or a credential failure such as an expired session, wrote nothing and puts the
  token back. A reserved token keeps its place in the 128-token limit, and the
  writer transaction checks
  the printer's host fingerprint again, so a settings change after the route's
  read refuses with the same 409.
- The upload route authenticates the caller first, as Node's `onRequest`
  hooks do, so a bad credential is 401 before the rate window counts it or
  the body is read. It takes `multipart/form-data` (else 415 `Send the print
  file as multipart/form-data`) under Node's busboy limits: one file part of
  up to 64 MB (413 `That print file is larger than the 64 MB PrintPartner
  will take as an upload`, 400 `Upload one print file at a time` for a
  second), and four fields cut at 64 KiB (a fifth is 400 `That upload could
  not be read`). A part is a file when it has a file name or an
  `application/octet-stream` type, as busboy decides, and a part named after
  an `Object.prototype` member is 400 `That upload could not be read`, as
  `@fastify/multipart` refuses it: when the form ends, so a later files,
  fields or size limit answers first, and without that part's own size limit.
  Node has no cap on the whole body; here it
  is 65 MiB, and a body past it that is still inside the file part is the
  413 (the unreadable 400 when that part is named after an
  `Object.prototype` member), anywhere else the unreadable 400. Answers still differ from Node for
  requests the web client never sends: an empty multipart body (the
  unreadable 400 here, `profile_id is required` in Node), a field part typed
  `application/json` (read as text here, JSON-parsed in Node), a file name
  sent only as `filename*=` (multer does not decode it), and a malformed or
  oversized JSON body (415 here, Fastify's 400 or 413). It then checks
  `profile_id`, the `file` part, its file name (what follows the last `/` or `\`,
  unless that is `.` or `..`, trimmed) and extension in Node's order,
  reads the Build's Accepted Plan (a missing Build is a 500,
  as for the preview), classifies the bytes off the async workers (409 with
  Node's rejection detail), and holds the classification for 15 minutes under
  a new `upload_token`. The answer is the preview's body with that token.
  `print_file::UploadStore` is Node's `pendingUploads`: it keeps only the
  classification and identity (`size_bytes`, `sha256`), since the bytes
  classify the same way every time, but counts their size toward Node's
  128 MB cap, so a third full upload is 503 `PrintPartner is holding as many
  uploaded print files as it will; record one of them, then try again`.
- An `upload_token` is redeemed only for the Build it was uploaded for. An
  unknown, expired or foreign one answers Node's 409 `That upload is no longer
  available; upload the print file again`. The assignment reserves it as it
  reserves a snapshot: a successful or unknown outcome spends it, and a
  refusal releases it, so the operator does not upload the file again after
  a stale plan revision. Node reads the upload without reserving it; here a
  concurrent second assignment of the same token is refused, while a preview
  still reads it.
- A `remote_path` without a snapshot on a Moonraker or
  PrusaLink host answers 501 instead of recording the print unread, because
  this runtime does not read a printer's file itself yet. A printer tracked by
  hand, or a host type Node has no file reader for, records the print unread
  as Node does, and an unread 3MF is refused with Node's 409.
- The assignment itself is `Request::AssignFile` (previous section).

`checkoff_progress::print_file::SnapshotStore` is Node's
`printer-file-snapshots.ts` for this process: one capture at a time, at most
128 tokens, 15 minutes each. The files/content route calls
`SnapshotStore::begin` with `print_file::file_source` read before and after
it asks the printer, `Capture::push` for each chunk it streams, and
`Capture::finish` at EOF, which classifies the bytes and makes the token
redeemable. A capture dropped before EOF releases the store and leaves no
token. The files/content route does not capture yet; GRE-1214 passes one store
to both routers and replaces its 501.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test file_assignments_http
```

### Accepted Plate workspace read (GRE-1189)

`pp-storage::plates` serves Node's `GET /plans/{id}/plates`
(`routes/accepted-plates.ts`, `readAcceptedPlateWorkspace`) in one deferred
read transaction. `pp-api::plates::plates_router` serves it at `/` and
`/api/v2` behind `accepted::Gate` when `PP_DESKTOP_REBUILD` is `1` or `true`.

`read_router` mounts it with every other Plate route; see
[Accepted Plate routes in Rust](#accepted-plate-routes-in-rust-gre-419).

The read answers as Node does. A Build the caller cannot see is 404
`profile_not_found`; an empty Accepted Plan, or one with no Required units, is
`{"kind": "empty_plan"}`; a dirty or uninitialized compatibility state is 409
`accepted_state_unavailable`. With no head, or a head revision stamped with
another Accepted Plan basis, the answer is `setup` with the Required units and
`expected_plate_revision_id` (`null` without a head). Otherwise it is `ready`:
the head revision's layout, read in either stored layout format and checked
against its counts, tenant and layout digest, with placed units per Plate,
`unplaced` units, and `unassigned` Required units. `printers` lists the
`printer.fleet` rows that Node's `acceptedPrinter` accepts (trimmed id, name
and model, a positive build height, margins smaller than half the bed),
sorted by UTF-8 id. Stored Plate data that contradicts its revision, and a
stored Plate id that is not a generated `plate_` id, which Node's contract
parser rejects, are 500 `internal_error`. A read writes nothing.

### Accepted Plate edits (GRE-1184)

`pp-storage::plates` also serves the Plate-revision edits from Node's
`routes/accepted-plates.ts`: `PATCH /plans/{id}/plates/{plateId}/units/{token}` (move),
`PATCH .../units/{token}/pin`, `POST .../units/{token}/unplace`,
`POST .../units/{token}/transfer` and `POST /plans/{id}/plates/restore`.
`pp-api::plates::plates_router` serves them at `/` and `/api/v2` behind
`accepted::Gate` when `PP_DESKTOP_REBUILD` is `1` or `true`.

`read_router` mounts them with every other Plate route; see
[Accepted Plate routes in Rust](#accepted-plate-routes-in-rust-gre-419).

Each edit is one immediate writer transaction. It checks, in Node's order, the
target Printer (transfer only, 422 `printer_not_found`), that the Build is
visible and its Accepted Plan matches the request's `expected` basis (409
`accepted_plan_changed`, or `accepted_state_unavailable` while compatibility is
dirty or uninitialized), that the Build is not archived (409 `plan_archived`),
and that the head is `expected_plate_revision_id` (409
`plate_revision_changed`). It then reads the head revision's layout in either
stored layout format and checks its counts, tenant and digest. A change is
validated with the current clearance rule (422 `outside_build_area`,
`overlapping_units` or `invalid_units`), written as a new immutable revision
with the next revision number, and the head moves by compare-and-set from the
expected revision. The same transaction appends `plates.revision_changed`. An
edit whose layout digest equals the head's returns the head and writes nothing.
Refusals write nothing. Stored Plate data that contradicts its revision is 500
`internal_error`, as Node's `AcceptedPlateIntegrityError`.

A move takes any trimmed Plate id of at most 200 characters; pin, unplace and
transfer take only generated `plate_` ids, as Node's routes do. A transfer
leaves the unit unplaced on the target Plate, or on a new random `plate_` Plate
for `target_printer_id`, and drops a source Plate it empties. Restore accepts
only the revision the head's arrange replaced (otherwise 422
`unit_not_found`), writes its layout as a new revision and returns the
workspace.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -p pp-storage -p pp-api -E 'test(ticket_t_61_plates) | binary(plates_http)'
```

### Accepted Plate arrange (GRE-1200)

`pp-storage::plates` serves Node's `POST /plans/{id}/plates/arrange`
(`arrangeAcceptedPlates` in `services/accepted-plate-workspace.ts`) as
`Request::Arrange`. The body is `expected`, `expected_plate_revision_id` and
`mode`, which is `unplaced` or `all`; anything else is 400
`invalid_request`. `read_router` mounts it with the other Plate routes.

Arrange is one immediate writer transaction. It checks, in Node's order, that
the Build is visible (404 `profile_not_found`), that it has an Accepted Plan
(an empty Plan, or one with no Required units, answers `{"kind": "empty_plan"}`),
that a head revision exists, that the request's basis is current (409
`accepted_plan_changed`) and that the head is `expected_plate_revision_id` (409
`plate_revision_changed`). It then packs the layout, checks archive state (409
`plan_archived`) and validates the result with the clearance rule.

- `unplaced` keeps each manual or pinned unit where it is and packs every other
  unit of that Plate around them. Units that overflow go to new Plates for the
  same Printer, with Node's deterministic `initial_plate_id`.
- `all` repacks every unit of each Printer, split into buckets by the Build's
  stored Production setup rules (`production_setup:<id>` in `app_settings`,
  read by `production::setup::stored_rules`). It reuses the Printer's existing
  Plate ids in order, clears manual placement and pins, and records the replaced
  revision as `undo_from` so restore can return to it.

A unit that cannot fit its Printer is 422 `unit_too_large` with `token` and
`printer_id`. A packed layout whose digest equals the head's returns the
workspace and writes nothing. Otherwise the new revision, the head move and
`plates.revision_changed` commit together, and the response is the workspace
with each Plate's units in packing order, as Node answers.

`fixtures/parity/plates-arrange/node.json` is Node's answer for 28 cases in 13
phases (refusals, pinned and manual units, an unplaced unit, archive, a missing
head, a foreign Plan, several Printers, grouping rules and keep-together rules).
Regenerate it with `node fixtures/parity/plates-arrange/generate.mjs` and check it
with `--check`. `ticket_t_61_plates_arrange_matches_node` replays every case and
compares responses, changed tables, events and final Plate rows.

### Accepted Plate initialize (GRE-1202)

`pp-storage::plates` serves Node's `POST /plans/{id}/plates/initialize`
(`initializeAcceptedPlates` and `publishAcceptedPlatesInternal`) as
`Request::Initialize`. The body is `expected`, `expected_plate_revision_id`
(`null` before any Plate revision) and `assignments`, a list of
`{token, printer_id}` where `printer_id` is `null` or trimmed text of 1 to 200
UTF-16 units. Anything else is 400 `invalid_request`.

Initialize is one immediate writer transaction. Node reads, measures and
publishes in three steps; Rust holds the writer while it reads the STL files,
so no other write can land between the checks and the publish. In Node's
order it checks:

1. The Build is visible and has an Accepted Plan (404 `profile_not_found`,
   `{"kind": "empty_plan"}`, or 409 `accepted_state_unavailable`), and the basis
   is current (409 `accepted_plan_changed`).
2. The expected head. A head on an older Accepted Plan counts as the setup
   head. A mismatch with no current layout is 409 `plate_revision_changed`.
3. The assignments: 422 `unknown_unit_token`, `duplicate_assignment`,
   `missing_assignment` (an empty list) or `unassigned_units`, each with
   `tokens` in UTF-8 order. Units left out of the list stay unassigned.
4. The Printers in the stored fleet: 422 `printer_not_found` or
   `missing_printer_geometry` with `printer_ids`. While the head does not match,
   each of these refusals is 409 `plate_revision_changed` instead.
5. A replay: when the head moved but the stored layout is exactly what these
   assignments pack to with the stored dimensions, the answer is the current
   workspace and nothing is written. Otherwise 409 `plate_revision_changed`.
6. Each assigned unit's STL, by `plates::artifacts`: found inside its Source
   snapshot one segment at a time, case-insensitively, without following
   links, and checked against its accepted SHA-256. A failure is 409 with
   Node's reason as the code (`missing`, `unsafe_path`, `ambiguous_case`,
   `symlink`, `not_file`, `empty`, `io_error`, `digest_mismatch`, `legacy` or
   `untracked_source`) and `token`, 409 `invalid_stl`, or 422
   `degenerate_geometry`. The route limits are 10,000 units, 256 MiB of
   distinct STL bytes and 5,000,000 triangles counted once per unit (413
   `limit_exceeded` with `limit`). An STL shared by several units is read once.
7. Packing: each Printer in UTF-8 id order packs its units in Plan order,
   split by the Production setup's grouping buckets, onto Plates with Node's
   deterministic ids (422 `unit_too_large` with `token` and `printer_id`).
8. The publish: 409 `plan_archived`, then the clearance rule. A layout equal
   to the head of the same Accepted Plan writes nothing. Otherwise it writes
   a new revision, creates or moves the head, and appends
   `plates.revision_changed`. The answer is the workspace with units in packing
   order and `arrange_undo_revision_id` `null`, as Node answers.

STL geometry (`production::geometry`) moved from `pp-core` to `pp-storage` for
this; `pp-core::production::geometry` re-exports it.

One case differs from Node on purpose: a replay that names a unit the stored
layout does not hold is 409 `plate_revision_changed`, where Node throws and
answers 500. A Printer whose bed is past the stored Plate range (more than
2,147,483,647 µm) cannot be packed in the Plate types, so its units are only
checked against that bed while every other Printer packs, and a unit that does
not fit, on that Printer or another, is still 422 `unit_too_large`. After that an archived Build is 409
`plan_archived` and any other Build is 422 `outside_build_area`, the codes
Node gives for the same requests.

`fixtures/parity/plates-initialize/node.json` is Node's answer for 44 cases in
18 phases. Each phase writes box STLs into the pinned Source snapshot and
seeds SQL that re-pins those Parts to the files' SHA-256 and restamps the
revision digest with Node's `digestPlanRevisionParts`, because the fixtures pin
digests whose files do not exist. Regenerate it with
`node fixtures/parity/plates-initialize/generate.mjs` and check it with `--check`.
`ticket_t_61_plates_initialize_matches_node` replays it. The `plates::artifacts`
unit tests cover the limits, a linked file, a linked snapshot directory, an
ambiguous case and paths outside the repositories. The walk starts from
`repos/<locator>` as stored, not the canonical snapshot path the accepted read
checks, so a snapshot directory replaced by a link is refused as Node refuses it.

### Accepted Plate routes in Rust (GRE-419)

`pp_core::rebuild_api::read_router` mounts `pp_api::plates::plates_router`, so
with `desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true` the Rust-only start
(`rebuild_api::product_router`) serves every Plate route Node registers, at
`/` and `/api/v2` as Node does:

- `GET /plans/{id}/plates`
- `POST /plans/{id}/plates/initialize`, `.../arrange` and `.../restore`
- `PATCH /plans/{id}/plates/{plateId}/units/{token}` and `.../pin`
- `POST /plans/{id}/plates/{plateId}/units/{token}/unplace` and `.../transfer`

They move together because they all write the same `accepted_plate_*` tables.
A request is one storage transaction on the single writer. Nothing falls back
to Node, and no Plate route is left on Node beside a Rust one. Bodies follow
Node's limits: 8 MiB for initialize (`MAX_BULK_JSON_BODY_BYTES`) and 1 MiB for
the other writes (`MAX_JSON_BODY_BYTES`). A larger body is 413 `Request body too
large` before storage runs. The flag-off default still serves these routes
from Node, and the gateway manifests still name Node as their owner. Making
Rust the default is the gateway cutover (GRE-593).

`plates_http` drives initialize, its replay, arrange, the body limits and the
flag through the router.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -p pp-storage -p pp-api -E 'test(ticket_t_61_plates) | binary(plates_http)'
```

### Production setup in Rust (GRE-1209)

GRE-421's first part. `pp_storage::plates` serves Node's `GET` and
`PATCH /plans/{id}/production-setup` as `Request::ProductionSetup`, and
`pp_api::plates::plates_router` mounts both at `/` only, because Node
registers no `/api/v2` alias for them. With `desktop_rebuild` and
`PP_DESKTOP_REBUILD=1` or `true`, Rust alone runs the command that writes
`production_setup:<id>`. Rust already deletes the key with its Build. Node's
duplicate still copies it to the new Build's own key until the duplicate part
of GRE-421 moves, so the two runtimes never write the same key.

The route checks in Node's order:

1. The id is `Number(id)` as a positive integer (so `0x1` and `" 1 "` name
   Build 1), else 400 `invalid Build id`.
2. The Build must exist in the tenant, else 404 `Build not found`. An archived
   Build still saves, as in Node.
3. The PATCH body must be one `productionSetupCommandSchema` command, else 400
   `Invalid production setup` with Zod's `issues`: the same paths and
   messages, in Zod's order.
4. A `set_route` that changes a route already chosen is 409
   `production_method_locked` when a Printer Checkoff link of this Build that
   is not dismissed names, after the slicer's instance suffix, a Required unit
   the next package makes. The next package is the custom selection's available
   tokens, or every incomplete unit. When the Accepted Plan state cannot be
   read, it is 503.

A stored setup that fails `productionSetupSchema`, or names another Build, reads
as Node's default. A saved setup is Zod's output: trimmed text, unknown keys
dropped, and the schema's key order. Node retries its setting compare-and-set
and answers 409 after eight conflicts. Here the read, the lock check and the
write share one immediate transaction on the single writer, so no conflict can
occur. The body limit is Node's 8 MiB (`MAX_BULK_JSON_BODY_BYTES`), and a body
that is not JSON is 400 `Invalid JSON body`.

`fixtures/parity/production-setup/node.json` is Node's answer for 82 cases in
22 phases. `ticket_t_61_build_lifecycle_production_setup_matches_node` replays
the 78 that reach storage and compares the status, the body, the changed tables
and the stored text, then reopens the writer. The other four are the
`/api/v2` alias and Fastify's body parser, which `plates_http` covers at the
router. Regenerate the golden with
`node fixtures/parity/production-setup/generate.mjs`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -p pp-storage -p pp-api -E 'test(ticket_t_61_build_lifecycle) | binary(plates_http)'
```

### Build header edit and archive in Rust (GRE-1231)

GRE-421's second part. `pp_api::builds::build_router` serves Node's
`PATCH /plans/{id}` and `POST /plans/{id}/archive` at `/`, `/api/v1` and
`/api/v2`, both as `BuildCommand::Update` in `pp_storage::build_graph::header`
(an archive is an update with only `archive` set). With `desktop_rebuild` and
`PP_DESKTOP_REBUILD=1` or `true`, Rust alone writes a Build's name, special
request, `archived_at` and `last_used_at` through these routes. Draft headers
and Accept's header copy stay where GRE-1207 put them.

A PATCH checks in Node's order:

1. `archived: false` is 400 `Cannot unarchive; duplicate the archived template
   instead`, before the Build is looked up.
2. The id is `Number(id)` as a positive integer (through `plates::build_id`),
   and the Build must be the tenant's, else 404 `Profile not found`.
3. On `/api/v1`, a Build whose accepted progress is unavailable gets the legacy
   refusal before anything is written.
4. A `special_request` key needs the `orders` feature: 403 `FEATURE_DISABLED`,
   or 500 `FEATURE_TOGGLES_INVALID` when the stored toggles do not read.
5. A string `name` is trimmed. Blank is 400 `Profile name is required`. A name
   another Build has is the Build rename rule Accept uses:
   409 `{"code":"duplicate_build_name","name":...}` (Node answered 400
   `Profile already exists`). The web client's `engineFetch` shows that 409 as
   `Profile already exists: <name>`, the message Node gave. A `name` that is not a string is ignored.
6. `special_request` is `String(value)` trimmed, and blank or `null` clears it.
   When `String()` throws (an object with its own `toString` member, or an
   array holding one), any rename stays saved and the answer is Node's: 500
   `Internal Server Error` beside an archive, else 400 `Cannot convert object
   to primitive value`.
7. `archived: true` archives as below, then `touch_last_used: true` sets
   `last_used_at`.

As in Node, each step writes as it goes: a rename saved before an archive
refusal stays saved. The answer is the Build summary on the mount's contract.

An archive keeps an archived Build as it is. Otherwise every Part of the
accepted Plan needs one progress row per unit (Node's `assertCompleteProgress`,
through `checkoff_progress::complete_rows`; a missing row is 500 `Accepted Plan
data is inconsistent`), and the Plan must have units and none remaining (else 400 `Archive only when print remaining is
0`). Compatibility-dirty or uninitialized progress is 409, and an integrity
failure is 500 `Accepted Plan data is inconsistent`.

`fixtures/parity/build-header/node.json` is Node's answer, with
`PP_DESKTOP_REBUILD=1`, for 58 requests in 14 phases on copies of
`accepted-plate-node.db`. `build_header_patch_and_archive_match_node` replays
it through `build_router`: 51 cases match Node's status, body and changed
tables, after the replay removes GRE-1100's `customer` and `due_date` from
each Rust summary (Node's summary has neither, and the replay requires both to
be present and null). Seven are declared corrections that write nothing: the two
duplicate-name renames (409 above), a blank name beside an archive (400, where
Node answered 500), a PATCH or archive body that is not JSON under
`Content-Type: application/json` and an empty archive body under that type
(400 `Request is invalid` without Fastify's parser codes), and a `null` PATCH
body (400 `Request is invalid`, where Node answered 500). An archive otherwise
ignores its body. Like the other Rust routes, it does not reproduce Fastify's
415 for a body under a content type Fastify has no parser for, so that request
archives. The web client always sends `{}` as JSON.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --no-tests=fail -p pp-api --test build_header_http
```

### Variant dimensions and selection in Rust (GRE-1221)

GRE-421's sixth part (GRE-1221). `pp_api::builds::build_router` serves Node's
`GET /plans/{id}/variant-dimensions` (with `HEAD`) and
`POST /plans/{id}/variant-selection` at `/`, `/api/v1` and `/api/v2`, as
`BuildCommand::Variants` in `pp_storage::build_graph::variants`. With
`desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true`, Rust alone writes
`plan_variant_selection_<id>` and the base Source's import rules for a
selection. The import-rules write is `catalog::save_import_rules`, the same
code `PUT /sources/{id}/import-rules` runs, so both routes store the same
normalized rules and mark the same Plans modified.

The read takes the Build, its base Source (the lowest `layer_order` base
layer), that Source's `local_path` and the stored selection in one read
transaction. It then reads `print-partner.manifest.yaml` through the
production `SourceReads` adapter, outside any transaction, as the manifest
builder does. Dimensions are empty when the Build has no base Source, the
Source has no checkout, the manifest is missing or unreadable, or the manifest
does not load (Node's `loadManifestYaml` throws, for example on an empty
`selections` list, or a mapping with its own `toString` key where Node calls
`String()` on it). Otherwise each entry of `variant_dimensions` whose value is
a list keeps its strings and numbers in JavaScript property order; a list
`variant_dimensions` reads as index keys, a `__proto__` entry is dropped (Node's
assignment calls the prototype setter), and a non-finite number is `null` in
JSON. The stored selection is returned as `JSON.stringify(JSON.parse(text))`,
or `{}` when it is missing, empty or not JSON.

A selection checks in Node's order:

0. Under `Content-Type: application/json`, an empty body, a body that is not
   JSON, or one Fastify's parser refuses as prototype poisoning (a `__proto__`
   key, or a `constructor` key whose object has a `prototype` key) is 400
   `Request is invalid` before the id is read, as Fastify answers before Node's
   route runs. Like the other Rust routes, a body under another content type is
   read as JSON rather than refused with Fastify's 415.
1. The id is `Number(id)` as a positive integer (so `0x3`, `3e0` and `" 3 "`
   name Plan 3, through `plates::build_id`), else 404 `Profile not found`,
   before the body is read. The dimensions read does the same.
2. The body is `{ selection?: Record<string, string>, source_id?: positive safe
   integer }` (no body or `null` is `{}`), else 400 `Invalid variant
   selection`.
3. Without a base Source, 400 `No base source on plan`; a `source_id` other
   than the base Source, 400 `Source must match the plan's base source`.
4. Each selected dimension, in JavaScript property order, must be a dimension
   (400 `Unknown variant dimension: <name>`) and its value must equal
   `String(option)` for one option (400 `Unsupported option for variant
   dimension: <name>`). `String()` is JavaScript's, so `2.0` is `"2"` and
   `.nan` is `"NaN"`.

One immediate writer transaction then rechecks the Build and its base Source,
stores the selection as `JSON.stringify` writes it, and saves each non-empty
value plus `/` as an import rule. The stored selection and the response keep a
lone UTF-16 surrogate, but an import rule carries U+FFFD in its place, so
`imported_paths` stays readable by Rust's `serde_json` readers of that column;
Node stores the escape. The refusal detail for an unknown dimension does the
same. A read failure other than a missing or
unreadable file, such as an exhausted `PreparationLimits` budget (each
copied dimension name and text option, aliases included, is charged to it), is 503
`source_observation_failed`, as for the manifest builder; Node has no limits.

`fixtures/parity/variants-http/node.json` is Node's answer for 78 requests on
one database. `variants_flat_v1_v2_match_node_responses_and_changed_tables`
in `ticket_t_10_fixtures` replays every case through `build_router` over the
production `SourceReads`: 74 match Node's status, body and changed tables, and
four bodies Fastify's parser refuses (poisoned, not JSON, or empty under
`application/json`, including on a missing or non-numeric Plan) are declared
corrections (400 `Request is invalid` without Fastify's parser codes);
and after each of the 18 saves the stored selection text and the Source's
`imported_paths` equal Node's byte for byte. Regenerate the golden with
`node fixtures/parity/variants-http/generate.mjs`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --no-tests=fail -p pp-api -E 'test(variants_flat)'
```

### Plan features and decision append in Rust (GRE-1243)

GRE-421's seventh part (GRE-1243). `pp_api::builds::build_router` serves Node's
`GET` and `PATCH /plans/{id}/features` and `POST /plans/{id}/decisions` at `/`,
`/api/v1` and `/api/v2`, as `BuildCommand::PlanFeatures` and
`BuildCommand::AppendDecision`. With `desktop_rebuild` and
`PP_DESKTOP_REBUILD=1` or `true`, Rust alone reads and writes
`feature_toggles:plan:<id>` through these routes and alone appends
`plan_decisions` rows. With the flag off the router mounts nothing and Node
keeps both. `GET /plans/{id}/decisions` stays in the planning router
(GRE-1119), and `/settings/features`, the tenant layer, stays Node's.

A features request checks in Node's order:

0. The credential is checked first, as Node's `onRequest` hook does. Then a
   PATCH body Fastify's parser refuses (empty, not JSON, or a `__proto__` or
   `constructor.prototype` key under `application/json`) is 400 `Request is
   invalid`. A decision append refuses those bodies at the same point.
1. The id must match `/^[1-9]\d*$/` and be a safe integer, and a PATCH body
   must be an object whose keys are all features (`farm`, `orders`, `bambu`,
   `spoolman`) and whose values are all booleans or `null`. Otherwise the
   answer is 400 `Invalid plan feature input` once the credential checks. So
   `0x1`, `01` and `1.0` are 400 here, unlike the `Number()` ids of the other
   Build routes.
2. The Plan must be the tenant's, else 404 `Plan not found`.
3. The tenant layer `feature_toggles` and the Plan layer must both be JSON
   objects whose feature keys hold booleans, else 500
   `FEATURE_TOGGLES_INVALID`.

A patch applies in registry order (`farm`, `orders`, `bambu`, `spoolman`):
`null` removes the key, a boolean replaces it in place or appends it. Layers
are read as Node's `parseFeatureLayer` reads them
(`pp_storage::source_scan::node_json::RawObject`): unknown keys, JavaScript
property order and each number's source text (`1.50`, `1E+2`, `-0`,
`12345678901234567890`) survive, and strings are re-escaped as
`JSON.stringify` writes them. One immediate writer transaction checks the
Plan, reads both layers and writes the Plan layer's text even when it is
unchanged or empty, so an emptied layer is `{}` and the row is never deleted.
The read and the write share that transaction, so Node's 409 compare-and-set
conflict cannot occur. `FeatureClient`'s Plan-scope `set` writes the same
way; its tenant-scope `set` keeps sorted keys and deletes an empty row, as
before, because Node owns that operation. The answer is Node's
`{profile_id, overrides, effective}`, where `effective` gives each feature's
`enabled` and its `source` (`plan`, `user` or `default`).

A decision append reads the id with `Number()`, so `0xc` and `12.0` name Plan
12. A missing, foreign or non-numeric Plan is 404 `Profile not found` before
the body is read. `kind` defaults to `user_note` when it is absent or `null`
and must be `applied_action`, `dismissed_action`, `user_note` or `choice`,
else 400 `Invalid kind`. `actor` is `assistant` only for exactly that string,
else `user`. `params` (default `{}`) and `result` (default `null`) are stored
as `JSON.stringify` writes them. Node hands `action_type`, `label`, `summary`
and `rationale` to better-sqlite3 unchecked, with only `null` falling back to
`""` or `null`, and Rust binds them the same way:

- A number is bound as a double that the TEXT column stores as text: `5` is
  `"5.0"`, `1e21` is `"1.0e+21"` and `-0` is `"0.0"`. A lone surrogate becomes
  U+FFFD.
- An array is spread into the parameter list, so `["x"]` stores `"x"`, and
  `[]` beside `["a","b"]` shifts the later values into the neighbouring
  columns as Node does. An object is a named-parameter bag that binds nothing.
- A boolean, or an array element that is not a string, number or `null`, is
  400 `SQLite3 can only bind numbers, strings, bigints, buffers, and null`. A
  second object is 400 `You cannot specify named parameters in two different
  objects`. Any count other than eleven parameters is 400 `Too few parameter
  values were provided` or `Too many parameter values were provided`;
  a twelfth bindable value stops binding there, before any later field is
  checked. A NOT NULL failure is 400 with SQLite's message.

The answer is the inserted row as Node's `mapPlanDecision` returns it. The
append writes only `plan_decisions`: no publication row or blocker.

Both routes follow Fastify's `text/plain` parser, which hands the route the
body as a string. A `text/plain` feature patch is therefore 400 `Invalid plan
feature input` whatever it says, and a `text/plain` decision has no fields, so
it appends the defaults, as `{}` does. Any other content type is read as JSON,
as on the other Rust Build routes.

`fixtures/parity/plan-features-decisions/node.json` is Node's answer for 114
requests on one database, run with `PP_DESKTOP_REBUILD=1` except four with it
unset. `plan_features_decisions_flat_v1_v2_match_node_responses_and_changed_tables`
in `ticket_t_10_fixtures` replays them through `build_router` merged with
`planning_router`, as `pp-core` composes them. 102 match Node's status, body
and changed tables. Eight bodies Fastify's parser refuses are declared
corrections (400 `Request is invalid` without Fastify's codes). The four
flag-off cases reach a router built with the flag off, which mounts nothing:
404 and nothing written, where Node answers its own 404 `Plan not found` for
features and still appends the decision. After each of the 34 writes, the
Plan's stored layer text or the decision rows it added equal Node's byte for
byte, and every 200 body also has Node's key order (error bodies come from the
shared problem builder, which writes `code` before `detail`). A malformed body
sent with an unknown session is 401 and writes nothing. Regenerate the golden with
`node fixtures/parity/plan-features-decisions/generate.mjs`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --no-tests=fail -p pp-api -E 'test(plan_features_decisions) | test(plan_feature_ids)'
```

### D21 accepted read JSON bodies, GRE-616 PR 1

With `desktop_rebuild`, `AcceptedRead`, `Snapshot` and `Batch` each have an
`into_json_body` method. It returns the same bytes as the derived `Serialize`
output, except for Part manifest metadata. Each accepted `Part` now carries its
`PartManifestMetadata`, and the body writes `requirement` and `optionGroupId`
from those UTF-16 units. Text that is not valid Unicode keeps each unpaired
surrogate as a `\uXXXX` escape, as `JSON.stringify` does. The derived output
writes `null` for that text. No routes call these methods yet; D23 to D26 wire
HTTP behind `PP_DESKTOP_REBUILD`.

D22, GRE-618, lifts the temporary snapshot refusal for non-scalar manifest
text by moving Review to the same lossless carrier, as described below.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib read_model::json_body
```

### D21 published capture, GRE-616 PR 2

With `desktop_rebuild`, `read_model::capture_published` reads the Accepted Plan
and its Build context in one transaction after a committed Save. It fails when
the Build is missing, when the Accepted Plan is not ready, or when the snapshot
Plan version is older than the Save receipt. Integrity failures propagate
unchanged. The D13 Save port (GRE-608) is its first caller.

Two other reference differences are not ported, because main is already
correct:

- Accepted progress. The reference counts units inline. Main uses
  `views::progress`, which gives the same counts: units of included Parts, and
  the units not yet completed. A test pins these counts in both profile
  summaries.
- Naming folder rules. The reference omits `functional_class` when it is unset,
  which changes the effective naming digest. Node `digestEffectiveNaming` writes
  `"functional_class":null`, and main does the same. Porting the change would
  mark every Build with such a rule stale. A test pins the Node digest.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib read_model::capture_published_tests
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --test ticket_t_61_read_model
```


### D22 Review JSON and workflow

GRE-618 completes the workflow and views delta from
`desktop/source-sync-local-current-20261008`. Workflow already matches that
reference, so its resolver stays unchanged. Main retains its catalog and
collation fallbacks instead of the reference's panic behavior.

With `desktop_rebuild`, `views::review_json` returns `ProjectReviewJsonBody`.
Its `into_bytes` preserves `requirement` and `option_group_id` from each Part's
UTF-16 manifest metadata, including unpaired surrogates as JSON escapes.
`summary_json` returns an optional `ProjectReviewSummaryJsonBody` with its own
`into_bytes`. Ready and empty reads have summaries; unavailable reads do not.
The summary uses scalar fields and does not serialize Part metadata.
Accepted snapshot reads now admit valid non-scalar metadata. Invalid PPJS
encodings, projection mismatches and digest failures remain refusals.

Default builds retain `views::review` and `views::summarize_review`. The fixture
binary selects the matching API at compile time. No HTTP routes or runtime
callers are added. Future product callers must check `PP_DESKTOP_REBUILD` and
send these JSON bytes directly, without parsing them through `serde_json::Value`,
which cannot hold unpaired surrogates.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild
```

### D23 auth HTTP wiring, GRE-617 PR 1

Main's `pp-api` `auth/http.rs` and its `auth_http` tests are newer than
`desktop/source-sync-local-current-20261008`. They keep the reset delivery
window, health `degraded_detail`, `hasPassword` and the no-panic rewrites. No
handler code is ported back from the reference.

`pp-api` gains a `desktop_rebuild` feature. `auth_router` accepts an
`AuthBackend`: the existing pooled `AuthClient`, or, with the feature, D6's
`RebuildAuthClient`. Existing callers pass `AuthClient` unchanged.

With `desktop_rebuild`, `pp_core::rebuild_api::router(rebuild, owner, routes)`
is the single place that registers Rust auth routes. It returns `None` before
creating any client unless the stored `DesktopRebuild` setting is on. When it
is on, it selects `rebuild_auth_with_policy` with the caller's lifetimes, as D6
requires. Default builds do not compile the module.

The desktop gateway does not mount this router. The desktop process holds the
storage lease that `WriterOwner::open` needs, and Node owns those writes until
cutover. Gateway mounting belongs to D26.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-core
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-core --features desktop_rebuild
```

### Password-change response parity, GRE-873

`fixtures/parity/auth-password-change/` records Node's public
`POST /auth/change-password` status and body together with whether the old and
new passwords still authenticate. The corpus covers missing cookies, wrong
current passwords, valid changes, malformed fields, and nonempty invalid or
revoked cookies (including empty bodies). Rust resolves the session before
field validation so those stale cookies return
`401 {"detail":"Authentication required"}` instead of a 400 field error.
Password and session policy stay unchanged. The focused suite requires
`desktop_rebuild`.

```sh
node fixtures/parity/auth-password-change/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test auth_password_change_parity
```

### D24 drafts HTTP, GRE-619

`pp-api` `drafts/http.rs` compiles with `desktop_rebuild`.
`DraftHttpConfig::new` enables the router only when
`PP_DESKTOP_REBUILD` is exactly `1` or `true` at process start. Otherwise
`draft_router` and `admit_draft_routes` mount nothing and every draft path
answers 404.

Landed replacement slices for the superseded #635 monolith:

| Slice | Linear | Scope |
| --- | --- | --- |
| a1 | GRE-939 | Loopback Host/Origin admission and credential presentation ([#969](https://github.com/poitee/PrintPartner/pull/969)) |
| a2 | GRE-944 | Request target, body size, and body-read timeout bounds |
| b | GRE-940 | List and workspace reads |
| c | GRE-941 | API-key `ResolveKey` so `lastUsedAt` commits; foreign-tenant keys 401 |
| d | GRE-942 | Part edits with Node conflict shapes |
| e | GRE-988 | Abandon with storage lifecycle token |
| f | GRE-1093 | Accept preview through Accept's own checks |
| g | GRE-416 | Required-unit reconciliation (`PUT .../reconciliation`) |

With both gates on, `draft_router` serves these paths at the root and
`/api/v2`:

| Method | Path | Storage |
| --- | --- | --- |
| GET | `/plans/{id}/drafts` | `WorkingDraftClient` list |
| GET | `/plans/{id}/drafts/{draft_id}` | workspace read |
| GET | `/plans/{id}/drafts/{draft_id}/accept-preview` | read-only Accept preview |
| PATCH | `/plans/{id}/drafts/{draft_id}/parts` | `edit` with typed `EditRequest` |
| PUT | `/plans/{id}/drafts/{draft_id}/reconciliation` | `ReconciliationClient::reconcile` with typed `ReconciliationRequest` |
| POST | `/plans/{id}/drafts/{draft_id}/abandon` | `abandon` with typed `AbandonRequest` |
| POST | `/plans/{id}/drafts/save` | draft-only autosave, `WorkingDraftClient::autosave` (GRE-418) |
| GET | `/plans/{id}/drafts/header` | Build header and Accepted base before any draft (GRE-1120) |
| GET | `/plans/{id}/drafts/{draft_id}/header` | draft header beside the Build's header (GRE-1197) |
| PUT | `/plans/{id}/drafts/header` | draft header save, `WorkingDraftClient::save_header` (GRE-1197) |

`admit_draft_routes` buffers each body before routing, disables Axum's 2 MiB
default, and applies the guard's limits: 16 KiB request target, 1 MiB for
reads, 8 MiB for PATCH and PUT, 5 s body timeout, 64 admission permits. Loopback
transport is required. Host must match the configured listener; forwarded and
`x-forwarded-*` headers are refused. Session-cookie writes (PATCH/PUT/POST) need a matching
Origin; GETs and API-key writes may omit it, but any Origin sent must match.
A foreign or `null` Origin, or `sec-fetch-site: cross-site`, returns 403.
Ambiguous credentials return 401, or 403 on an Origin-less write. Exactly one
of session cookie, `Authorization: Bearer`, or `x-print-partner-api-key` is
accepted. API keys use the configured tenant, not a request header.

List returns `{ profile_id, drafts: [{ draft_id, state, lifecycle_version,
snapshot_digest, base }] }`. Workspace returns the storage workspace JSON.
Missing profile or draft returns 404 with Node `detail` and `code`
(`profile_not_found` / `draft_not_found`).

PATCH bodies are typed `EditRequest`. Invalid JSON, unknown fields, empty
decisions, zero part IDs, and out-of-range quantities return 400
`invalid_request` before any mutation. Successful edits return 200 with the
workspace. Conflicts match Node `plan-drafts.ts`: 409 is `{ code }` plus
`workspace` only when one exists (`draft_changed`, `base_changed`,
`not_open`, `accepted_baseline_required`, `idempotency_conflict`) with no
synthetic conflict `detail`; 422 `domain_error` is `{ code }` only. 404 and
503 keep their Node `detail`. Server failures use
`{ detail: "Plan draft update failed", code: "internal_error" }`.

`POST /plans/{id}/drafts/{draft_id}/abandon` takes
`{ expected_lifecycle_version }` (`AbandonRequest`). Success returns 200 with
the draft identity (`state: abandoned`, lifecycle advanced). A retry with the
same expected lifecycle version is idempotent (`Ready`). A stale token returns
409 `{ code: "draft_changed", workspace }` with no synthetic `detail`; a
consumed or already-closed draft that is not the idempotent retry returns
409 `{ code: "not_open", workspace }`. Invalid bodies return 400
`invalid_request` before mutation. Server failures use the same
`Plan draft update failed` detail as edit. The desktop gateway does not mount
this router; D26 does.

`PUT /plans/{id}/drafts/{draft_id}/reconciliation` (GRE-416) saves and selects
a Required-unit reconciliation, as Node's route of the same path does. The
`Idempotency-Key` header is required: a missing key, or one that is blank or
longer than 160 characters after trimming spaces and tabs, returns 400
`invalid_request` before the body is read. The body is
`{ expected_snapshot_digest, decisions }` (`ReconciliationRequest`):
`select_exact_predecessor`, `accept_prior_completion` or `replace` decisions,
at most 10,000, one per draft Part. A malformed body returns 400 before any
write. The route reuses `WorkingDraftClient`'s writer and auth policy through
`WorkingDraftClient::reconciliation`, so the selection runs in one immediate
transaction on the single writer and nothing is sent to Node.

- 200 returns the draft workspace with its `reconciliation` summary. The
  selection commits with one `plan.draft_changed` domain event
  ([Domain events](domain-events.md)).
- A retry with the same actor, key and payload returns the stored selection and
  writes nothing, before the snapshot digest is checked. The one exception is
  Node's repair: an open v1 draft whose selection was detached gets that
  selection back, and the repair still writes no domain event. The same key with
  another payload returns 409 `{ code: "idempotency_conflict" }`.
- Edit's conflicts apply: 409 `draft_changed`, `base_changed`, `not_open` or
  `accepted_baseline_required`, 422 `domain_error`, and 404
  `profile_not_found` or `draft_not_found`. When another connection holds
  the SQLite write lock past the writer's admission deadline, the route answers
  503 `transaction_unavailable`, as edit does.
- A decision that does not apply to the draft makes Node's service throw, so
  both routes answer 500 `Plan draft update failed`. The transaction rolls
  back every reconciliation, decision and assignment row and writes no event.


### Draft recompute, rebase and Accept (GRE-417)

The draft router also serves Node's three remaining draft writes under the
same flag, at the root and under `/api/v2`. Each needs an `Idempotency-Key`
header (trimmed, 1 to 160 characters). A request without a usable key gets 400
`invalid_request` before its body is read. Each request runs on the
`WorkingDraftClient` writer, and nothing is sent to Node.

- `POST /plans/{id}/drafts/recompute` takes `{ apply_manifest: boolean }` and
  runs `WorkingDraftClient::recompute_and_select`. It abandons every other open
  draft, creates the new draft, and selects Required units as an edit does. It
  answers with the workspace. A replay with the same key returns the same
  draft and writes nothing. `no_layers`, `no_stls` and `would_wipe` are 422
  `{ code }`. `accepted_baseline_required`, `base_changed`, `inputs_changed` and
  `idempotency_conflict` are 409. A missing Plan is 404 `profile_not_found`.
- `POST /plans/{id}/drafts/{draft_id}/rebase` takes `RebaseRequest` and runs
  `WorkingDraftClient::rebase_and_select`. A rebased or existing draft is selected
  and answered as recompute's is. As in Node's `rebaseFailure`, a source whose
  lifecycle or digest changed is 409 `draft_changed` with the source's workspace,
  and a source whose state differs from `expected_source_state` is 409
  `not_open` with its workspace. `merge_conflicts`
  is 422 `{ code, conflicts }`. `base_unchanged` is 409, alongside recompute's
  409s and 422s.
- `POST /plans/{id}/drafts/{draft_id}/apply` (Accept) takes `ApplyRequest` and runs
  `PublicationClient::apply` through `WorkingDraftClient::publication`. 200 is
  the receipt `{ profile_id, draft_id, revision_id, plan_version,
  draft_lifecycle_version, revision_digest, required_unit_mapping_digest,
  applied_at }`, also for a replay with the same key or an already consumed
  draft. When Accept answers `reconciliation_required`, the route runs Node's
  `prepareForApply` and retries once under the same key with the prepared
  draft's revision. That means checking the draft against the request,
  answering 409 `not_open`, `draft_changed` or `base_changed` with its
  workspace, and otherwise selecting Required units. `draft_changed`,
  `base_changed` and `not_open` from Accept carry the draft's current workspace.
  `reconciliation_required` is 422 `{ code, reason }`. `production_active` is
  423 with `checkoff_link_count` and `send_queue_item_count`.
  `checkoff_remap_unsafe` is 422 with `unmappable`. `build_archived` and
  `token_allocation_failed` are 422 `{ code }`. Rust's `execution_conflict`,
  which Node has no counterpart for, is 409 `{ code, operations }`, and so is
  `duplicate_build_name` (`{ code, name }`), a draft header naming another
  Build ([Draft header fields](draft-headers.md#accept)). A missing Plan
  is 404 `profile_not_found` and a missing draft is 404 `draft_not_found`.
  Only Accept publishes. Recompute, rebase and autosave never write an
  Accepted Plan.
  Before it publishes, Accept saves the snapshot its preview promises: one
  `plan_snapshots` row, `source` `pre_apply`, named `Before accepting draft
  <draft_id>`, in the same transaction. It holds the Accepted Plan being
  replaced. Its `layers` come from that revision's `plan_revision_inputs`
  (type, Source id, current Source name and order), not from `profile_layers`:
  Accept publishes the working layers, so they already match the new Plan. The
  payload has Node's `capturePlanSnapshotPayload` shape plus
  `accepted_plan_revision_id` and `accepted_plan_version`. Accepted revisions
  record no kit manifest, Source tag or branch, so `kit_manifest`, `tag`,
  `branch` and `recipe_summary` are `null`, and restoring the row brings back
  the replaced Plan's Source layers while keeping the current kit manifest and
  Source refs. The first Accept, a replaced revision with no input set, one
  with an input whose Source row is gone, and one whose migrated input set
  holds `legacy:<id>` layers have nothing restore can place as recorded, so
  they save none, and the preview reports `saved_before_accept: false` for
  them. An empty input set is a Plan with no Source layers; its snapshot has
  `layers: []`, which restores that. A replay, an already consumed draft and
  every refusal save none either. Beside the snapshot, Accept saves the Build's
  header as it stood in `plan_snapshot_headers` (schema step 45), and a draft
  header row is copied to the Build
  ([Draft header fields](draft-headers.md#accept)).

Contention past the writer's admission deadline is 503
`transaction_unavailable`. Any other storage failure is 500
`Plan draft update failed`, as it is for edit. Every draft write in these
operations appends `plan.draft_changed` in its own transaction
([Domain events](domain-events.md)).

`POST /plans/{id}/progress/import` (Node's accepted Progress import) is mounted
on the Checkoff router (`pp_api::checkoff`). It takes `{ expected, rows }` and sets
each listed Part's printed count against the Accepted Plan the caller
observed, with the 8 MiB body limit Node's bulk routes use. A body for
another Plan is 404 `progress_target_not_found`. A stale basis is 409
`stale_accepted_plan`. Storage's other refusals keep their Node detail and code.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -p pp-storage -p pp-api -E 'test(ticket_t_61_publication)'
```

With `desktop_rebuild` and `PP_DESKTOP_REBUILD=1|true`, the Rust-only start
(`CoreRuntime::start_for_launcher`) serves recompute, rebase, Accept and
progress import through `rebuild_api::product_router`, which merges the draft
router into the read router that carries Checkoff. The flag-off default still
serves those four from Node. Making Rust the default is the gateway cutover (GRE-593).

### Draft-only autosave (GRE-418)

`POST /plans/{id}/drafts/save` writes Plan choices into the observed open
draft, or into a new final-state draft when none is open, with one
Required-unit reconciliation whose row is the durable receipt, all in one
immediate transaction. It needs an `Idempotency-Key` like the routes above,
replays before the stale-base check, refuses a changed payload under a reused
key, and never publishes. [Plan autosave](plan-autosave.md) has the contract.

This is a separate operation from Node's `POST /plans/:id/save`, which
publishes. That path stays on Node with its publishing semantics until UI
slice 6 moves the browser to draft-then-Accept, and no Rust router mounts it,
so Node and Rust never write the same operation. Node has no route at
`/plans/{id}/drafts/save`, which is why the gateway manifests do not list it.

### Draft header fields (GRE-1197)

`PUT /plans/{id}/drafts/header` saves a Build's name, order number, customer
and due date into the observed open draft, or into a new draft when none is
open, in one immediate transaction. It needs an `Idempotency-Key`, writes
nothing to the Build, and Accept publishes it.
`GET /plans/{id}/drafts/{draft_id}/header` reads the draft's header beside the
Build's current one. [Draft header fields](draft-headers.md) has the contract,
schema steps 43 and 45, the draft paths that carry the header forward, and the
Accept copy.

`GET /plans/{id}/drafts/{draft_id}/accept-preview` reports what accepting the
draft's current revision would change, and writes nothing. It runs in one
deferred read transaction on the writer. Storage owns the checks:
`plan_publication::check_draft` and `check_selection` are the same functions
Accept (`publish_in_transaction`) runs before it writes, in the same order.

- A draft whose Parts and inputs equal its accepted base returns 200
  `{ kind: "no_change", revision, snapshot }`, but only when Accept, sent
  without `remap_checkoff_links`, would pass every check. Otherwise the preview
  returns Accept's refusal, such as `reconciliation_required`,
  `production_active` or `execution_conflict`. A changed draft reports active
  production and running operations as checks in `changes` instead.
- Otherwise 200 `{ kind: "changed", revision, changes, snapshot }`. The diff
  and the `no_change` decision use the Parts Accept would publish: a Part with
  a base takes `filament_color_id`, `filament_custom_hex` and
  `spoolman_spool_id` from its live accepted Part, as Accept does, and a new
  Part keeps its own. `changes` holds `fields` (each changed Part field, `old`
  and `new`), `parts_added`,
  `parts_removed`, `inputs` (changed Source input fields; each value is
  `{ kind: "text" | "integer" | "flag", value }` or `null`), `required_units`
  (`added`, `removed`, and `printed_affected`: printed units that Accept removes
  or whose Part changes), `plates.cleared` (the accepted Plates Accept clears),
  `stock` (`off`, or per Spoolman spool the unprinted unit count before and
  after), `header` (the Build header fields the draft changes, `old` and
  `new`), `duplicate_orders` (other unarchived Plans with the order number
  the Build has after Accept),
  `production` (active checkoff links and queued sends, with any link Accept
  could not remap), and `blocked_by` (running operations Accept refuses on).
- `revision` is `{ draft_id, expected_snapshot_digest,
  expected_lifecycle_version, expected_base }`, the fields Accept's request
  expects. Accept refuses a stale one with `draft_changed` or `base_changed`.
- `snapshot` is `{ saved_before_accept, detail }`. `saved_before_accept` is
  true when Accept will save the `pre_apply` snapshot described above, that is
  when the accepted Plan's revision has an input set whose layers are all
  `base` or `addon` with an existing Source. It is false, with a detail
  saying no snapshot is saved, on a Build's first Accept, when the accepted
  revision has no input set (a `legacy` revision), when one of its inputs
  names a Source row that is gone, and when its input set was migrated by
  schema v18 and holds `legacy:<id>` layers, which Node's restore would turn
  into addons with no base.
- A refusal is the one Accept would return, as an RFC 9457 problem
  ([Problem responses](problem-responses.md)): 404 `draft_not_found` for a
  missing draft, otherwise 409 with no `detail`, Accept's outcome kind as
  `code`, and its fields as extension members, for example
  `{ code: "already_applied", receipt }` for a draft Accept already consumed,
  `base_changed`, `inputs_changed`, `build_archived`, or
  `{ code: "reconciliation_required", reason }`. Accept's idempotency-key
  lookup has no preview counterpart, since a preview sends no key.
- Refusals come in Accept's order, through the same storage functions:
  `already_applied`; then the Plan (`build_archived`) before the draft
  (`draft_not_found`, `not_open`), so a missing draft on an archived Plan is
  `build_archived`; then `draft_changed`, `accepted_baseline_required`,
  `base_changed`, `inputs_changed`, and `reconciliation_required`; then, for
  `no_change` only, `production_active`, `checkoff_remap_unsafe` and
  `execution_conflict`. Token allocation can still fail inside Accept's
  write; a preview cannot predict that.

Draft rows carry Parts and inputs only, so Plan name, order number, and other
Plan header fields never appear in `fields`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(accept_preview)'
```

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --lib
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --lib drafts::http::tests -- --test-threads=2
```

### D30 catalog HTTP reads, GRE-804

`pp-api` gains `catalog`, compiled only with `desktop_rebuild`.
`CatalogHttpConfig::from_startup` keeps the router unmounted unless
`PP_DESKTOP_REBUILD` is exactly `1` or `true`. While it is unmounted, `GET` and
`POST /sources` answer 404 and do not open the catalog.

With both gates on, `GET /sources` and `GET /sources/{id}` are served at the
root and `/api/v1`. List returns `{ "sources": [...] }`. Detail returns that
caller’s catalog `SourceSummary`. A missing or non-canonical id returns 404
`Source not found`. The session cookie or one API key is required. API keys use
the configured tenant, not a request header. The same loopback, host, and
origin checks used by draft admission apply. Create, patch, and delete stay
unregistered. The desktop gateway does not mount this router; D26 does.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --test catalog_http
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test catalog_http
```

### D26 runtime wiring (a), GRE-818

`pp_core::rebuild_api::clients(rebuild, owner, policy, lifetimes)` composes the
opted-in auth, draft, and Build storage clients from the same `WriterOwner` and
stored `DesktopRebuild` flag that D23's auth router already uses. It returns
`None` before creating any client unless the flag is on. Catalog is not in the
bundle: GRE-819 mounts it with `admit_catalog_routes(config, owner)`, which
builds a credentialed catalog client per request. `RebuildHttpClients::close`
drops the owned handles; storage shutdown stays with `WriterOwner`. This slice
does not mount routes or change production ownership.

```sh
cargo check --locked --manifest-path rust/Cargo.toml -p pp-core --no-default-features
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild \
  --lib application_host_ -- --test-threads=2
```

See `docs/gre-818-rebuild-clients.md`.

### D26 runtime wiring (b), GRE-819

`pp_core::rebuild_api::read_router` mounts auth, Plan builds, and catalog from
one `WriterOwner` when the stored rebuild flag is on. It uses the GRE-818 client
bundle plus `admit_catalog_routes(config, owner)`. Aliases share that owner.
`RebuildReadOwner::admit` fails closed for unknown names. Flag off returns
`None` before clients or mounts. Draft HTTP is GRE-820 (below); cutover remains
GRE-593.

```sh
cargo check --locked --manifest-path rust/Cargo.toml -p pp-core --no-default-features
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild \
  --test rebuild_read_http -- --exact --test-threads=2
```

Focused tests: `rebuild_read_aliases_use_one_owner`, `unknown_read_owner_is_refused_before_execution`.

### D26 runtime wiring (c), GRE-820

`pp_core::rebuild_api::draft_router` mounts completed draft list/read/edit/abandon
routes from one `WriterOwner` when the stored rebuild flag is on. It uses the
GRE-818 client bundle's auth and draft handles. Root and `/api/v2` aliases share
that executor. `RebuildDraftOwner::admit` fails closed for unknown names. Flag
off returns `None` before clients or mounts. Rust draft failures stay on the
Rust path with no Node fallback. Cutover remains GRE-593.

```sh
cargo check --locked --manifest-path rust/Cargo.toml -p pp-core --no-default-features
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild \
  --test rebuild_draft_http -- --exact --test-threads=2
```

Focused tests: `draft_aliases_and_workers_share_executor`,
`rust_draft_failure_never_falls_back_to_node`.

See `docs/gre-820-draft-mutations.md`.

### D26 web app shell, GRE-621

The web app reads the rebuild flag from one place,
`desktopRebuildEnabled` in `web/apps/web/src/lib/desktopRebuild.ts`. It is true
only when health advertises `tenant_feature_toggles`, which Node does only for
`PP_DESKTOP_REBUILD` set to exactly `1` or `true`. `JobContext`, `planReview` and
`PlanWorkspaceContext` call it in place of their copies of the same check.

While the flag is on, `AppLayout` sets `data-desktop-rebuild` on `<html>` and
removes it when the flag turns off or the shell unmounts. Rebuilt UI styles
(GRE-1008 onward) scope under `:root[data-desktop-rebuild]`, which also covers
dialogs and other portals. With the flag off the root gets no attribute and the
shell markup is unchanged. This slice adds no tokens, screens or routes.

```sh
cd web/apps/web
npx vitest run src/lib/desktopRebuild.test.ts src/layout/AppLayout.desktopRebuild.test.tsx
```

## Filament grams per send (GRE-1165, part a: GRE-1237)

Schema step 46 adds the Rust-only table `print_link_grams`, one row per tenant and
Printer Checkoff link. A link is one send of a file to a printer. Its row holds
the grams that send uses (`grams`, positive), the number of Checkoff units the
send prints (`units`, positive), where the grams came from (`basis`:
`estimated` or `measured`), the slicer's grams per millimetre of filament
(`grams_per_mm`, when the file states both), and the length the printer
reported using at finish (`measured_mm`). Step 46 takes a `pre-schema46.db` copy like every
step through `LAST_COPIED_STEP`, and the table is validated against
`print_grams/schema.sql` like the other side tables. Node has no such table and
never writes it.

The owner decided on 2026-11-04 to record the slicer's estimate at send and
replace it with the measured value when the print finishes. Printers report
millimetres, and the staged file is deleted after upload, so the conversion
factor is kept at send (Alfala, 2026-11-07):

- `pp_storage::print_grams::record_estimate` writes the estimate and its
  `grams_per_mm`. A later estimate replaces it only until a length is measured.
- `pp_storage::print_grams::record_measured` stores `measured_mm`. With a
  `grams_per_mm`, the grams become `measured_mm × grams_per_mm` and the basis
  `measured` (`Recorded`). Without one, the estimate stays (`NoFactor`). A link
  with no estimate is left alone (`NoEstimate`). Nothing calls it
yet: the send path writes the estimate in GRE-1238, and the status watcher
writes the measured value in GRE-1165's last part.

The Spoolman shortfall read (`GET /plans/:id/filament-shortfalls`) now reads
grams from this table instead of `print_jobs.filament_consumed_g`, which
nothing writes. A send's grams are spread evenly over its `units`, so a send
whose units are only partly checked off still costs each checked-off unit its
share. Each Part averages its five most recent sends that have a checked-off
unit for it (`print_job_parts` rows with the send's `link_id`). A Part with any
measured send uses only measured sends and reads `measured`; a Part with only
estimates reads `estimated`. The read no longer requires
`print_jobs.status = 'completed'`, which nothing sets either.
