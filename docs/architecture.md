# Architecture

Print Partner plans and tracks layered STL kit builds. One person runs a small print farm, turns customer orders into printable work, and records what physically came off each printer. This page is the target: one Rust core, one new UI, no Node at runtime.

Status: accepted for implementation on 2026-11-02. Chosen by an arena of three independent architecture designs judged by two reviewers, with every factual claim below measured against `main` at `a8d6323a`. [Architecture arena rationale](#rationale) records what was rejected and why. [ADR 0008](adr/0008-replace-the-old-screens.md) records the UI decision this design depends on.

## Target

One Rust core runs in two products:

- **Desktop app.** `pp-desktop` opens the new UI in a native window on a loopback origin. The window gets no native IPC for domain work. "Share on network" serves the same UI to phones, tablets and other computers, off by default, with login or device pairing. See [Desktop app](desktop.md) and [Network sharing](network-sharing.md).
- **Docker image.** `pp-server` runs the core headless for one user, serving the same UI and API. Docker is the only headless mode; there is no launchd, systemd or Windows Service mode. See [Docker image](docker.md).

Both are built from the same router and the same database. npm and Vite stay as the UI build tool and nothing more.

Only one process writes to a data directory at a time, enforced by an OS advisory lock. See [Storage](storage.md).

Platforms ship in this order: macOS, then Windows and Linux before 1.0. All three are release targets, and the Windows port is scheduled early rather than at the end (see [Cross-OS](#cross-os)).

Out of scope, by decision: multi-user accounts, hosted SaaS, Postgres, and the West3D order and farm features.

## Where the project actually is

Measured, not estimated:

| Measure | Value |
| --- | --- |
| Rust non-test lines, all crates | 110,191 |
| `pp-storage` alone | 51,824 in 85 files, 47% of the tree |
| Node non-test lines (routes, services, db, mcp, adapters, lib) | about 72,500 |
| Routes in `pp-gateway/operations.json` | 781, **every one owner `compat`** |
| Routes the gateway itself owns | 1 (`POST /auth/logout`) |
| Crates excluded from the Windows CI job | 8 of 15 |
| `docs/` top-level entries | 74, of which 36 are per-ticket `gre-*.md` |

Two facts follow that shape the whole plan.

First, **the strangler has not taken ownership of a single route.** The gateway proxy, the operation ownership manifest and the per-route cutover machinery in `pp-gateway/src/ownership/cutover.rs` all work and are tested, but nothing has moved through them. Rust domain code exists behind `desktop_rebuild` and is exercised by tests, not by users.

Second, **no shipped binary serves the Rust API.** `pp-core/src/rebuild_api.rs` defines the routers, but outside tests the only reference is the module declaration. `pp-server` still requires `--node` and waits for the Node child before reporting ready. This is why mounting the API is the first slice in every plan below.

The Rust tree is not small: at 110,191 non-test lines it is roughly half the Node server it replaces, and `pp-storage` is nearly half of that. The crate question is really a question about `pp-storage`.

## Module map

The end state is seven product crates plus one local-platform crate, and the rule that makes them hold together is that **only the engine may link SQLite** and **the API may not reach the network**.

```text
pp-desktop ──> pp-app ──> pp-api ──> pp-engine ──> pp-model
                  │                     ^
                  └──> pp-connect ──────┘
                  └──> pp-engine
                  └──> pp-os
pp-app also builds the pp-server binary.
```

| Crate | Interface: what a caller must know | Hides |
| --- | --- | --- |
| `pp-model` | Plain data and pure functions. Domain values (IDs, quantities, digests, versions), wire DTOs, the port traits the engine needs, and pure algorithms: G-code object parsing, plate packing, the 3MF writer, import rules, the profile parser. | Nothing stateful. Deep because the algorithms are large and the interface is types and functions. |
| `pp-engine` | `Engine::open(dir, config, ports)`, `query(Query) -> View`, `execute(Command, Actor) -> Receipt`, `subscribe(Topics) -> EventStream`, `shutdown(deadline)`. | Schema and migrations, the single writer and read pool, the advisory lock, durable jobs with claims and recovery classes, drafts and autosave, publication with immutable Required-unit allocation, checkoff, Source snapshots, Sync, backups, auth storage, secrets, the printer status hub, the send queue, the operation journal. |
| `pp-connect` | Constructors returning port implementations, plus one rule: every network call goes through one outbound guard. | Moonraker, PrusaLink, Bambu MQTT, Spoolman, git, native slicers, notifiers, mail. |
| `pp-api` | `router(engine, exposure, auth) -> axum::Router`, and the MCP service. | Route tables, the event WebSocket, MCP over rmcp, sessions and API keys, Host and Origin checks, rate limits, body limits, asset serving, the MCP tool catalog. |
| `pp-app` | `App::start(config) -> AppHandle`, plus `origin`, `share_on_network(bool)`, `subscribe`, `shutdown`. Builds the `pp-server` binary. | Config and desktop settings, the data directory per OS, building adapters, the loopback listener and the optional LAN listener, TLS, mDNS, bounded drain, redacted logs. |
| `pp-desktop` | The Tauri app: window, tray, autostart, single instance, notifications, updater. | Tauri config, platform quirks, the Linux WebKitGTK environment layer. |
| `pp-os` | `open_no_follow`, `create_private_dir`, `owned_by_current_user`, `DataDirLock`, `shutdown_signal`, `restrict_to_owner`. | `libc` and `windows-sys`. On Unix, `openat`/`fstatat`/`geteuid` and device-plus-inode identity. On Windows, `CreateFileW` with `FILE_FLAG_OPEN_REPARSE_POINT`, owner SID checks, and `filesystem` locking. |

### Allowed edges, enforced

| Crate | May depend on | Must not |
| --- | --- | --- |
| `pp-model` | nothing | tokio, rusqlite, reqwest, axum, tauri |
| `pp-engine` | `pp-model` | reqwest, MQTT, git, axum, tauri, any socket crate |
| `pp-connect` | `pp-model` | rusqlite, `pp-engine`, axum |
| `pp-api` | `pp-engine`, `pp-model` | `pp-connect`, tauri |
| `pp-app` | `pp-api`, `pp-engine`, `pp-connect`, `pp-model`, `pp-os` | tauri |
| `pp-desktop` | `pp-app`, `pp-model` | rusqlite, `pp-connect` |
| `pp-os` | nothing | any `pp-*` crate |

`rust/scripts/check-boundaries.py` enforces this and asserts `package_names == set(ALLOWED)`, so **creating, renaming or deleting a crate requires editing that file in the same PR**. It currently checks only `pp-*` workspace edges; the per-crate third-party deny lists above (only the engine names `rusqlite`, only `pp-os` names `libc` or `windows-sys`) are a real addition, not a declaration.

That deny list is what turns the project's rules into compile errors:

- **One writer.** Only `pp-engine` can write SQL, so no other crate can open the database.
- **Explicit sends.** A printer send is a `Command` that only `Actor::User` may construct, so an assistant or a background job cannot send a print.
- **One Accept gate.** Every Plan change is a `Command` that produces a proposal; only a user `Command` accepts it.

### Nesting, not one flat enum

`pp-storage/src/lib.rs` is 989 lines and its writer thread funnels every operation family through a single `enum Envelope` with 16 variants (line 107): WorkingDraft, WorkingDraftWrite, BuildGraph, NativeSecrets, Features, Publication, RequiredUnits, Uploads, Checkoff, Read, Catalog, CatalogRequest, Jobs, Setting, Auth, WriterControl. Every new family adds an arm to one file.

The engine interface must **nest per family** (`Command::Plans(plans::Command)`, and the same for `Query` and `View`), so adding an operation edits a family module rather than one growing enum. Without this the consolidation rebuilds the current collision point at larger size.

Within `pp-engine`, modules own their tables: `plans`, `accepted`, `checkoff`, `sources`, `printers`, `stock`, `exports`, `backup`, `auth`, `settings`, `jobs`, `store`. Cross-module calls go through `pub(crate)` functions, never SQL against another module's tables. An ast-grep rule (the repo already ratchets ast-grep) flags a table name used outside its owning module.

### Seams: only where two adapters exist

Real seams, each with at least two concrete adapters:

| Port | Adapters |
| --- | --- |
| `PrinterDriver` | Moonraker (covers Annex K3 and Micron), PrusaLink, Bambu status-only, and a recorded-traffic simulator. **This trait does not exist yet:** `pp-drivers` is concrete modules and `prusalink.rs` imports types from `moonraker_driver`. It is a seam to extract, not one that exists. |
| `FilamentStock` | Spoolman, plus a fixture adapter for tests |
| `SourceFetch` | git over HTTPS, local directory and ZIP, plus a fake |
| `SlicerHandoff` | OrcaSlicer and Bambu Studio. There is no Prusa slicer adapter in Rust or Node; do not claim one. |
| `Notifier` | Home Assistant, generic webhook, Discord |
| Mail | SMTP and a capture fake |

Deliberately **not** seams:

- **Storage.** SQLite is the only adapter, so a storage trait would be hypothetical. Tests open real SQLite in temporary directories (`pp-storage/tests/common/scratch.rs` uses `tempfile`; there is no in-memory mode). A `PlanRepository` trait with one implementation would be the same mistake.
- **The HTTP and MCP surfaces** are two real adapters over the engine, which is why the engine interface is commands and queries rather than per-route functions.

`pp-contracts` is deleted. It is a facade: 970 lines that mostly `pub use pp_api_types::*`, and its `autosave.rs` is one line. Delete it and nothing reappears in callers except import paths. Note the direction: `pp-contracts/Cargo.toml` depends on `pp-api-types`, so any plan that keeps the facade and absorbs the types is backwards.

## How the UI is delivered

One origin, three hosts. `pp-api` serves the built assets and the API from the same origin in every mode, so the desktop window, a browser on the Docker host and a phone on the LAN load one bundle over HTTP. There is no second transport.

- **Desktop.** `pp-desktop` starts `pp-app` in process, binds a loopback listener on a random port, and points the window at it. The webview keeps zero Tauri commands for domain work (`main-no-native-commands`). Notifications, tray state and file reveal run in the shell process off the event stream.
- **Docker.** `pp-server` serves the same router and assets, always requires sign-in, runs non-root, and keeps the data directory on a volume. TLS is the job of a reverse proxy; the repo ships a `Caddyfile`.
- **Share on network.** A second listener, off by default, bound only when the user turns it on. Every request needs a session or a paired device token; Host and Origin must match an allowlist; MCP always needs an API key, loopback included. Discovery is mDNS; optional TLS uses a per-install CA.
- **Types.** Rust is the source. DTOs carry `schemars` derives, and the existing `web/scripts/rust-contracts/generate.mjs` pipeline emits TypeScript and Zod validators into `web/packages/api`. One derive feeds three consumers: the TS client, MCP `inputSchema`, and request validation. CI regenerates and fails on drift. Only the UI is generated; nothing hand-edits `web/packages/api`.

### API style

- REST JSON for reads and commands. Rust handlers currently register both root and `/api/v1` aliases for the old screens; the root aliases go away with them.
- Plan changes are **proposals**: stage, then accept or reject. The UI and MCP share one server-side queue, so the Accept gate has one implementation rather than one per screen.
- Writes carry an expected revision and a stale write returns a conflict, which fixes the fast-click lost-edit class of bug at the API instead of in UI timing.
- **MCP** keeps Node's tool names and input schemas, because external clients (the cursor plugin, West3D, Chad's own connector) depend on them. The current catalog is 86 tools, not 89: 89 specs minus 6 `ui_` tools, plus 3 meta tools.
- **Stdio MCP** is a bridge that forwards to the running instance over loopback HTTP with a key, so stdio never opens the database and the single-writer rule holds.
- One authenticated WebSocket event stream with topics (`printers`, `jobs`, `plans`, `queue`, `stock`). On connect the server sends a snapshot, then ordered deltas with a per-topic sequence number. A reconnect with the last sequence gets the missed deltas or a fresh snapshot. This generalizes the existing jobs socket rather than adding a second one, and GRE-1014's printer stream becomes the `printers` topic.
- Printer sends are never triggered by an event stream message. They are explicit commands from a user action.

## Node retirement

Every Node operation family, with its Rust owner and current status. Status is for `main` at `a8d6323a`.

Source: the 55 non-test route modules in `web/apps/server/src/routes`.

| Family | Routes | Rust owner | Status |
| --- | --- | --- | --- |
| Auth, sessions, API keys | auth, auth-route-model, auth-types, api-key-management | `pp-engine::auth` | partial, served behind the flag |
| Feature admission | tenant-features | `pp-engine::settings` | partial |
| Sources, manifests, imports, docs | sources, source-docs, source-cover-route-model, manifest, repo-manifest, phase-manifest-route-model, imports | `pp-engine::sources` | partial, no git yet |
| Plans, drafts, parts, profiles | plans, plan-drafts, plan-role-assignment-model, plan-summary-presenter, parts, profile-library | `pp-engine::plans` | partial, reads and draft mutations served |
| Build workflow and planning projections | within plans | `pp-engine::plans` | **missing, GRE-415** |
| Accepted plans, plates, checkoff read | accepted-plates, accepted-plates-route-model, accepted-state-detail, accepted-export-job-errors, accepted-filament-failure | `pp-engine::accepted` | partial, read only |
| Exports and production setup | exports, production-setup | `pp-engine::exports`, `pp-model` | missing |
| Printers, queue, printer checkoff, Bambu | printers, printer-route-model, printer-send-queue, printer-checkoff, printer-checkoff-route-model, bambu-connect | `pp-engine::printers`, `pp-connect` | missing |
| Slicers | slicer-handoff, slicer-instances | `pp-connect::slicers` | partial, discovery only |
| Integrations | integrations, spoolman-proxy, webhooks, discord-digest | `pp-connect::stock`, `pp-connect::notify` | missing |
| Jobs | jobs, job-route-inputs | `pp-engine::jobs` | mostly done |
| Backups | backups | `pp-engine::backup` | partial, no restore activation |
| Settings, admin, legal, logging, metrics, health | settings, settings-route-model, legal, logging, metrics, health | `pp-engine::settings`, `pp-api` | partial |
| App update check | within settings | `pp-desktop` updater | missing |
| Sharing | shares, reference-sharing | `pp-engine` | missing, see ADR 0004 |
| Assistant HTTP actions | assistant | none by decision | AI reaches Print Partner through MCP only. Note the MCP catalog imports `ASSISTANT_TOOL_SPECS` from these files, so deleting them must keep the catalog. |
| Board | board | none | drop, single-user |
| API surface and core | api-v1, api-v2, api-v1-extensions, core-routes | `pp-api` | replaced by the Rust router |

No candidate table covered this fully; the families easiest to forget are imports, parts, the manifest routes, production setup, and the assistant/MCP coupling above.

### What happens to Node code

| Path | End state |
| --- | --- |
| `web/apps/server` | Deleted after cutover. Maintenance only before that. |
| `web/apps/web` (old screens) | Frozen now, deleted with the old screens. |
| `web/packages/contracts` | Deleted; replaced by generated `web/packages/api`. |
| `web/packages/domain` | Rules (3MF, plate packing, import rules, checkoff matching) move to Rust. Presentation helpers used by three.js stay in the UI package. |
| `pp-compat` | Deleted with Node. It exists only to supervise the Node child. |
| `pp-gateway` proxy and `ownership/` | Deleted with Node. Listener, assets and rate limit move to `pp-api` earlier. |
| `slicer-sidecar` | Not in the 1.0 image. Native handoff covers desktop. See open questions. |

## Cross-OS

The Windows port is provably incomplete today, by the project's own CI: the `rust-windows` job excludes 8 of 15 crates, with the comment "Unix-only crates; remove each exclusion when its port ticket is fixed."

The blockers, by file:

| Location | Unix-only use | Fix |
| --- | --- | --- |
| `pp-storage/src/lease.rs:8` and 70-99, 164, 216, 303-351, 401 | `DirBuilderExt`, `MetadataExt`, `OpenOptionsExt`, `openat`, `fstatat`, `unlinkat`, `O_NOFOLLOW`, `O_DIRECTORY`, `geteuid`, `dev()`/`ino()` | `pp-os`. Note that the lock file itself is opened with `openat` at line 303-347; a library lock helper alone does not fix this. |
| `pp-storage/src/schema.rs:7` | unconditional `os::unix::fs::DirBuilderExt` | `pp-os::create_private_dir` |
| `pp-source/src/directory.rs:9-10` and the capability walk | `MetadataExt`, `openat`, `O_SEARCH`, `geteuid`, `PATH_MAX` | `cap-std` and `cap-fs-ext` give the same no-escape guarantee on Windows and remove the most code. |
| `pp-core/src/state_file.rs`, `review_observations.rs` | `O_NOFOLLOW`, `O_NONBLOCK`, device/inode identity | `pp-os` |
| `pp-server/src/main.rs:3` | unconditional `OpenOptionsExt`, `tokio::signal::unix` | `pp-os::shutdown_signal` |
| `pp-desktop/src/focus.rs` | Unix socket for single-instance focus | `tauri-plugin-single-instance` |
| `pp-compat/*`, `pp-gateway` proxy | Node process groups, `kill(-pgid)`, `/proc`, Unix sockets | Not ported; deleted with Node. |

Beyond compiling, Windows needs: a data directory decision (`~/.print-partner` on macOS and Linux, an OS-conventional path on Windows), long-path handling, refusal of reparse points, reserved names (`CON`, `NUL`) and case-insensitive collisions in Source archives, the WebView2 evergreen bootstrapper in the installer, and a verified answer on whether mDNS can share UDP 5353 with the Windows responder.

Linux: Tauri 2 uses webkit2gtk-4.1, which brings GTK3 and two advisory ignores that only upstream can clear. Set `WEBKIT_DISABLE_DMABUF_RENDERER=1` when it is unset (this is the value the repo's smoke test and e2e config use), strip proxy variables for the loopback origin, and keep a non-WebGL fallback for 3D previews because WebKitGTK can report a working WebGL2 context on a slow software path.

Packaging:

| OS | Package | Signing | Updates |
| --- | --- | --- | --- |
| macOS | `.app` in a `.dmg`, arm64 and x86_64 | Developer ID, notarize and staple in CI | Tauri updater, signed manifest |
| Windows | NSIS installer, per user, WebView2 bootstrapper | Authenticode (route open) | Tauri updater |
| Linux | `.deb` and AppImage | Signed checksums | Tauri updater for AppImage only |
| Docker | Multi-arch, non-root | cosign | Pull a new tag; pre-upgrade backup at startup |

Every update path relies on two rules: a verified pre-upgrade copy, and refusing a database newer than the binary without a restart loop.

## Data invariants and cutover

### Invariants, each with a test

1. **One SQLite file, same format.** Rust opens the database Node created. Migrations are additive only: new tables and new nullable columns. No drop or rename while a supported downgrade target can still open the file.
2. **`tenant_id` stays.** The product is single-user, but the format keeps `tenant_id='default'`. Engine code treats it as a constant. Do not remove it.
3. **Immutable IDs.** Plan, Plate and Required-unit IDs are never renumbered. Required units come only from the publication allocator. Import keeps rowids; path rebase changes stored paths, not identities.
4. **Autosave.** The ADR 0006 text contract holds: Unicode scalar values, unpaired surrogates rejected. Ordinary autosave is one command with a durable receipt. A stale write conflicts and loses nothing.
5. **Recovery.** Jobs are claimed and fenced, with a named recovery policy per kind. Printer sends never retry automatically. An uncertain send is reconciled against the printer, never resent.
6. **One writer.** The advisory lock is held from before preflight until after close.

Each invariant gets a test that fails when the invariant breaks, not a statement in a document: import an ID-set comparison, a byte-for-byte schema replay, an autosave conflict fixture, a restart-during-upload recovery case, and a downgrade-refusal case.

### The flag is a migration, not a config change

`pp-storage/src/schema.rs:12-15` sets `CURRENT_VERSION = 41` with `desktop_rebuild` and `35` without, and the version lives in `app_settings`. Because the older build refuses a database newer than it can read, **turning the flag on performs an irreversible 35 to 41 upgrade.**

There are three schema states, not two: Node's, Rust-at-35 and Rust-at-41. So the flag flip needs the same treatment as cutover: a verified backup first, a refusal path in the older build, and a stated rollback window. AGENTS.md currently forbids turning the flag on by default; removing the flag and removing Node should happen in the same change, so there is never a period where two runtimes both claim the same data directory.

### Cutover

The old screens are replaced, so nothing needs to run Rust and Node against the same live database. The per-route cutover machinery loses its consumer. It works and is tested, so deleting it is a scope decision, not a bug fix; the design keeps it until cutover and deletes it with Node.

Cutover runbook:

1. Stop Node. Take a Node backup archive. Keep the original read-only for the rollback window.
2. Copy the data directory to a new volume.
3. Start Rust on the copy in dry-run import: migrate, rebase stored paths without changing content or IDs, write an import report.
4. Diff real plans per family (Required units, Plates, checkoff state, artifact hashes) between Node's read model and Rust's, on the copy. Any difference blocks.
5. Start Rust for real. Point the MCP connector at it.
6. Rollback means restarting Node on the untouched original; changes made in Rust after step 5 are lost unless exported. This is the stated cost.

Node stays as a CI data oracle until that diff passes, which the "verify first" decision permits.

## Migration sequence

Each slice is one PR under about 1,500 changed lines, behind `desktop_rebuild` until cutover, and independently mergeable. Crate moves are renames: reviewers check `git diff -M --stat` and the definition of done counts non-rename lines.

**Ordering principle:** platform and contracts first, then the runtime, then families; reorganize crates while the runtime is being built, never in front of it.

| Milestone | Contents |
| --- | --- |
| M0 Decide and record | ADRs, `CONTEXT.md`, this page, and the Linear cancel and re-scope list. |
| M1 Foundations | `pp-os` with its Unix adapter and the `lease.rs`/`schema.rs` call sites moved; `pp-contracts` folded into `pp-api-types`; `pp-perf` to `rust/tools`; contracts generation from Rust with a CI drift check; fix the boundary checker's edge map. |
| M2 Windows-clean core | `pp-os` Windows adapter; storage, source, api and server on Windows CI; remove the eight CI exclusions one crate at a time. **Moved early on purpose:** every later slice adds file handling, so porting later multiplies the work. |
| M3 Mount the runtime | The first slice no candidate had: mount `pp-api` behind the flag so a shipped binary serves it (GRE-621, with prerequisites GRE-794 and GRE-800). Without it every screen slice is blocked. |
| M4 Collapse the crates | `pp-model`, `pp-engine`, `pp-connect`, `pp-api`, `pp-app` created by moving code family by family, nesting `Command`/`Query`/`View` per family. Runtime keeps working throughout; no behavioural change allowed in these PRs. |
| M5 Plans through checkoff | Autosave, publication, build lifecycle, read model, Required units, plates, checkoff. The families every UI concept needs first. |
| M6 Sources | Uploads, scanner, catalog, git fetch, watcher, documents. |
| M7 Farm, exports, stock, slicers | Printer status and drivers, send queue, Spoolman, 3MF and STL writers, slicer handoff. |
| M8 MCP, auth, sharing, admin, backups | Tool dispatch on engine commands, the shared proposal queue, auth completion, LAN pairing and admission, admin, backups. |
| M9 Docker and cutover | Rust image, import, rehearsal, the data diff, production cutover. |
| M10 Release and retire Node | Signed builds per OS, then delete `pp-compat`, the gateway proxy, `web/apps/server`, the old screens, and the Node toolchain from CI and the devcontainer. Remove the flag together with Node. |

### Tickets to cancel or re-scope

Cancel or re-scope because they serve the old screens or the strangler: GRE-621's web wiring half (keep the mount, drop the old-screen wiring), GRE-1062, GRE-1066 and its open siblings, GRE-452 and GRE-343 (keep only "the SPA reconnects after a core restart"), GRE-1065, GRE-448 (Rust-only shutdown), GRE-654 and GRE-655 (no `pp-compat` on Windows), GRE-1060 and GRE-1061 (artifact and MCP parity, not route shapes), GRE-1013 (keep generic rename only), GRE-986 (single-user), and the UI v11 screen tickets that describe the old shape. GRE-1008 (tokens) and the contract tickets stay.

New tickets needed: the M3 mount, the `pp-os` crate, the per-family command nesting, the operation journal, a recorded printer simulator adapter, the hand-written replacement for the 3,976-line `generated.rs`, per-crate deny lists in the boundary checker, the docs history move, and per-crate AGENTS.md.

## Repo and docs

```text
AGENTS.md                 short: product, rules, map, checks
CONTEXT.md                glossary
docs/
  architecture.md         this page
  adr/                    decisions
  product/                UI and UX target, domain pages
  operations/             docker, storage, network sharing, release, cutover
  development/            ci, building, definition of done
  history/                gre-*.md receipts, triage, audits
rust/
  AGENTS.md               crate rules, how to add an operation
  crates/pp-*/            each with AGENTS.md: interface, owns, does not own, tests
  tools/                  xtask, pp-perf
web/
  AGENTS.md               generated types only, tokens, evidence rule
  apps/app                the new UI
  packages/api            generated, never hand-edited
```

Today `docs/` has 36 `gre-*.md` files at its top level and `desktop-rebuild.md` is 2,637 lines, which agents load by accident. Moving ticket receipts to `docs/history/` keeps the record and gives both people and agents a short path to the current design.

Three rules for agents, to be enforced rather than documented:

- Root `AGENTS.md` stays under about 120 lines.
- Every crate has an `AGENTS.md` stating its interface in a few lines and its sharp edges.
- `just guidance` checks that the files exist and stay short.

## Open decisions

Recorded, not decided. Each is a risk-and-churn call rather than a technical one.

1. **Crate consolidation timing.** This design collapses to six or seven deep crates, but it does it in M4, while the runtime is being built, not before it ships. The alternative is to keep today's graph and consolidate after Node retires, which risks never happening. If the churn is unacceptable, M4 slips and the `Envelope` collision point stays.
2. **Cutover posture.** Whole-runtime copy with a stated rollback window, versus keeping the tested per-route cutover for a while longer.
3. **Docker slicers.** `slicer-sidecar` is a 418-line Python container that keeps a second language runtime in the product. Drop it from 1.0, or keep it as an optional external container outside the image.
4. **Windows signing route,** which sets the lead time for a signed release.
5. **`GET /ws/printers` (GRE-1014).** Already approved; no candidate's redesign of it should land without a fresh decision.

Settled by measurement, so not open: nothing outside MCP calls the REST API (the cursor plugin is MCP-only and Home Assistant is outbound-only), so REST shapes are free to change and only MCP paths and tool names need stability.

## Rationale

Three designs were written independently, then judged by two reviewers against the same criteria: depth, Node retirement completeness, PR-sized migration, cross-OS realism, data invariants, and docs navigability.

They agreed on the biggest call: the per-route strangler loses its purpose once the old screens are replaced, because there is no longer a half-migrated route anyone calls. They disagreed on the crate graph, and the judges split on it. One judge picked the six-crate engine on end state; the other picked the thirteen-crate graph on buildability, and said explicitly that it was not the better end state, only the better starting order.

This design takes the six-crate shape because it is the only one a contributor or an agent holds in one page, and because it makes the project's four hard rules into compile errors rather than conventions: one writer, explicit sends, one Accept gate, and no domain IPC. It takes the losing candidate's ordering because a crate reorganization in front of every domain slice blocks all of them, and because a plan that cannot start its first slice tomorrow is not a plan. It takes the third candidate's rigor: a named test per invariant, a four-OS packaging table, and a complete cancel list.

Rejected explicitly:

- **Keep 14 crates.** `pp-contracts` fails the deletion test outright, domain logic is spread across four crates, and `pp-gateway` mixes transport with printer code. The structure costs locality and buys nothing the tests use.
- **One crate per operation family.** The best locality, but it opens with about 50,000 lines of relocation before anything ships and turns every cross-family transaction into an interface negotiation. Publication reads Source revisions and allocates Required units in one write; families do not split cleanly along transactions.
- **A storage trait or a repository per table.** SQLite is the only adapter. A seam with one adapter is a hypothetical seam, and tests already use real SQLite in temp directories.
- **Per-OS `cfg` blocks in each crate** instead of `pp-os`. That is what main has now, and it is why eight crates are excluded from Windows CI. One crate with two adapters puts every `unsafe` platform call in one reviewable place.
- **Tauri IPC for domain calls.** Docker and phones need HTTP anyway; two transports double the test surface and let the desktop drift from the shared UI.
- **GraphQL or OpenAPI as a second description** of the same types. One generator, plus `schemars` for MCP `inputSchema`, covers every consumer.
- **A native Rust UI** (egui, Iced, Slint). It loses CSS-level polish and HTML accessibility, and phones need a web UI regardless.
- **Keeping Node as a sidecar** for unported features. It leaves a Node runtime in the artifacts, which is the one outcome the goal rules out.
