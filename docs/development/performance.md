# Performance baseline

`scripts/perf-baseline/baseline.py` measures the Node runtime and the Rust-only runtime on the same synthetic data, so the owner's slow operations have a number before budgets are set (GRE-388). It records raw samples; it sets no budget and gates nothing. [`pp-perf`](../../rust/crates/pp-perf/README.md) keeps the Measurement record and the 10% regression comparison for later captures.

## Run it

Build the web assets, the bundle manifest and a release `pp-server` with the rebuild feature, then run the harness:

```sh
npm --prefix web ci
VITE_PRINT_PARTNER_DESKTOP=1 npm --prefix web run build
python3 rust/scripts/build-desktop-manifest.py --web web --node "$(command -v node)" \
  --output rust/bundle-manifest.json --commit "$(git rev-parse HEAD)"
cargo build --manifest-path rust/Cargo.toml --locked --release -p pp-server
python3 scripts/perf-baseline/baseline.py --binary rust/target/release/pp-server --out /tmp/pp-baseline
```

`--out` must be a new or empty directory. It receives `samples.json` (every raw sample plus machine, artifact, toolchain, dataset and method metadata), `summary.md` (the table below) and `logs/` (each server's stderr). A run of 10 rounds takes about 25 minutes. `--runs`, `--idle-secs`, `--plan-iterations`, `--list-repeats` and `--export-repeats` change the counts; `--help` lists them. The harness's own tests:

```sh
python3 -B -m unittest discover -s scripts/perf-baseline -p 'test_*.py' -v
```

`just ci --only perf-baseline-tests` runs the same tests. No hosted job runs them yet.

## What it does

1. Generates the synthetic installation with seed 388 (`npm --prefix web run fixture:synthetic`, see [Synthetic installations](../storage.md#synthetic-installations)). Its "Synthetic Build Anchor 100u15p" has 100 Required units on 15 Plates, the shape of Build 22.
2. Starts five Moonraker simulators on `127.0.0.1`. They answer the recorded status body and 404 for every other path, so nothing can start a print. A Node-runtime start creates one printer per simulator through `POST /api/v1/integrations`; Rust has no route that creates printers. That seeded directory is the copy source for every run.
3. Runs 10 rounds. Each round starts each runtime once, Node first in odd rounds and Rust first in even rounds. Each start gets a fresh copy (`copy.mts`), a fresh `pp-server` process from the same binary, and a fresh login:
   - Node: `PP_DESKTOP_REBUILD` unset. The Rust gateway supervises the Node child and proxies every product request to it, so Node numbers include that loopback hop. This is the product path today.
   - Rust: `PP_DESKTOP_REBUILD=1`, with the Rust ownership marker written into the copy and the single account registered on it, as [Agent and contributor workflow](agent-workflow.md#replacement-ui-on-a-rust-only-core) describes. Registration is not timed; the 5 s settle before the idle window lets its password hashing finish.
4. Before each start it waits, up to `--quiet-timeout`, for the 1-minute load average to fall to `--max-load`, and records the load it starts at.

Requests use one keep-alive HTTP/1.1 connection per runtime, as a browser tab does. Where both runtimes serve a path, both get the same request. Otherwise each runtime gets what its own UI sends: the old web app (`web/apps/web`) on Node, and the replacement UI's generated client routes (`rust/crates/pp-api/src/client_contract.rs`) on Rust.

| Measure | Node request | Rust request |
| --- | --- | --- |
| App open | spawn to the origin line, plus the first `GET /api/v2/plans` | same |
| Idle memory and CPU | 5 s settle, then 30 s of process-tree CPU ticks and Linux PSS (`smaps_rollup`) | same |
| Thumbnails | per Part of the 100-unit Build: cold is the placeholder `GET /parts/:id/thumbnail`, `GET /parts/:id/mesh` and `POST /parts/:id/thumbnail` with a 256 px PNG; warm is `GET /parts/:id/thumbnail` | per Part of that Build: `GET /sources/{id}/thumbnail?path=` for the Part's STL. Cold is the first read, which renders the PNG; warm is the same GET from the cache |
| Plan change | `POST .../drafts/recompute`, `PATCH .../parts`, `POST .../apply` under `/api/v2/plans/{id}`; also `POST /plans/:id/save` | the same three, plus `POST .../drafts/save` and `GET .../accept-preview` |
| Checkoff session | per unit `PATCH /parts/:id/progress`, then `GET /plans/:id/checkoff` per burst | per burst `POST /api/v2/plans/{id}/checkoff/bursts`, then `GET .../checkoff/units` |
| Exports | `POST /jobs/export-stl-pack` or `/jobs/export-checklist-html`, `GET /jobs/:id` every 20 ms until done, then the download | `GET /api/v2/plans/{id}/exports/stl-pack` and `GET /api/v2/plans/{id}/exports/checklist`, each from the request to the last byte |
| Printer event | a simulator turns `printing` into `complete`; poll `GET /api/v1/integrations/{id}/status` every 50 ms | same |
| Today refresh | All Production's reads at once: `/printer-checkoff`, `/printer-checkoff/unattributed` | Today's reads at once: `/api/v2/tasks`, `/api/v2/printer-checkoff`, `/api/v2/printers` |

What the numbers leave out:

- No browser, and no React work. On Node the WebGL render is not timed, so a cold thumbnail is the server's share only: the placeholder, the mesh and the uploaded PNG. On Rust, `GET /sources/{id}/thumbnail` draws the PNG, so the cold time includes that render.
- The old Builds list does not request thumbnails. The replacement Builds list requests `GET /sources/{id}/thumbnail` for the first Source of each row on screen, after `GET /api/v2/plans` returns. Those image requests are not part of the Builds list rows. The thumbnail rows time the Parts of one Build. Node sends what the old app sends on that Build's Sources and Review steps. Rust sends `GET /sources/{id}/thumbnail` for each of those Parts.
- The printer event row times the gateway's Rust Moonraker watcher in both modes; it polls every 2 s, so the row shows where the event fell in that cycle. It is not a Node-versus-Rust comparison.
- Rows marked "different requests" have no ratio. Checkoff on Node is one request per unit and on Rust one per burst; Today and All Production are different screens.
- When a server closes an idle keep-alive connection, the request is sent again once and only that attempt is timed. `http_retries` in the samples counts these: 0 on Node and 3 on Rust in the baseline below.
- p95 is nearest-rank. Below 20 samples it is the maximum, and the table says "(max)". Per-request rows pool samples from all rounds.
- The four export rows time one transfer each: Node's job, its 20 ms poll and the download, or one Rust GET read to the last byte. They leave out the replacement UI's check. That check is the same GET aborted at the headers, after the core has opened and verified the files, and the browser download sends the GET again. The `check request` and `check then download` rows time that pair. They run after the four row GETs, so those files are already in the page cache. The cold time to the first byte is the `until headers` sample of the first GET.

## Large Plan projection lookup (GRE-1955)

`required_units::view::diff` links each draft Part to the base Part named by
`baseRevisionPartId`. The original lookup scanned all prior Parts for every
current Part. The 1,003-Part synthetic shape therefore performed quadratic
JSON-field lookups in both the in-transaction reconciliation workspace and the
autosave response workspace. The request-local index builds a map for integer
Part IDs and keeps the first row for duplicate IDs, matching the old
`Iterator::find` behavior. If prior IDs or a base reference are not integers,
the lookup falls back to the old JSON equality scan. The workspace projection,
its validation/errors, transaction rollback, reconciliation response, and
cross-request refresh behavior remain intact; no projection is skipped or
cached.

A fresh service-harness comparison used unchanged `9a4798e54f9b2ec012667e8cdd08967821247e7e`
and the candidate built from the same source revision plus this lookup change.
Each had two warmups and 30 measured samples on a disposable fixture with 1,003
Parts (150 Included, 853 excluded), 1,003 accepted assignments, and 1,003
Required units. Each measured save changed one Included Part from quantity 1 to
2, then Accept produced 1,004 assignments. All 30 operations and resets on each
binary passed checks for stable prior `(Part, unit-slot, token)` identities, a
unique new unit, quantities, one Plan-version advance, and receipt/revision
agreement.

Times below are client request durations through response-body read and JSON
parse, in milliseconds; p95 is nearest-rank. The mutation core is autosave plus
Apply. The service loop is autosave, Accept preview, Apply, and sequential
draft/header refresh requests; fresh-draft setup, resets, SQLite readback, and
browser rendering are outside the timer.

| Operation (n=30 per binary) | Unchanged median / p95 / max | Indexed median / p95 / max |
| --- | ---: | ---: |
| Draft autosave | 223.3 / 233.3 / 235.8 | 161.6 / 177.4 / 239.9 |
| Accept preview | 179.3 / 220.5 / 223.6 | 179.7 / 206.1 / 290.2 |
| Apply | 333.4 / 363.7 / 377.5 | 335.9 / 422.9 / 472.9 |
| Mutation core (save + Apply) | 558.8 / 586.2 / 607.1 | 502.0 / 583.8 / 712.8 |
| Service loop | 743.2 / 789.6 / 811.9 | 690.9 / 779.6 / 1,007.1 |

The candidate's median autosave was 61.6 ms lower (27.6%), median mutation
core 56.8 ms lower (10.2%), and median service loop 52.3 ms lower (7.0%). The
mutation-core p95 was nearly unchanged and the service-loop p95 was 10.0 ms
lower (1.3%). The slowest candidate service-loop sample included a 239.9 ms
save, 290.2 ms preview, and 472.9 ms Apply; the candidate's maximum was higher.
The median improved, but full-loop p95 moved only 10.02 ms (1.3%) and the
candidate maximum was higher. The fixed run order (unchanged first, candidate
second) was not interleaved or randomized. The loop's two refresh reads were
measured sequentially to avoid overlapping measured requests, whereas the UI
schedules them concurrently and does not wait for them before settling. These
are synthetic service-loop timings, not user-visible measurements or timings on
Chad's Plan; they set no performance budget or SLA.

### Paired browser workflow

A local browser run compared the Node UI, unchanged Rust at
`9a4798e54f9b2ec012667e8cdd08967821247e7e`, and candidate Rust at
`4e86df263de0390d6542c78e9c41789ab9306371`. Each series had two warmups and 30
measured edits on the same synthetic 1,003-Part fixture; the browser retained
each UI's normal refresh scheduling and used accepted-and-settled state as the
completion condition. All 90 measured edits passed identity, persistence, and
version checks. Times are milliseconds; p95 is nearest-rank.

| Browser boundary | Node median / p95 / max | Unchanged Rust median / p95 / max | Candidate Rust median / p95 / max |
| --- | ---: | ---: | ---: |
| First input → accepted settled | 833.4 / 951.4 / 957.4 | 1,283.9 / 1,424.9 / 1,611.3 | 1,086.3 / 1,304.0 / 2,427.4 |
| Enter → accepted settled | 803.3 / 921.5 / 931.1 | 1,241.4 / 1,388.7 / 1,575.3 | 1,055.4 / 1,274.5 / 2,356.8 |

Candidate Rust improved the first-input median by 197.6 ms (15.4%) versus
unchanged Rust, and was faster in 27/30 corresponding samples. It remained
253.0 ms (30.4%) slower than Node at the median and slower in all 30 samples.
Its first-input p95 was 352.6 ms higher than Node's; its maximum was 1,470.0 ms
higher. **The local “no slower than Node” gate remains unmet.**

The median phase measurements against unchanged Rust were: autosave HTTP
244.5→165.8 ms, save-response-to-Review-enabled 149.2→99.2 ms, post-save
four-read critical span 117.3→72.1 ms, and Accept-click-to-settled 494.3→464.9
ms. These are browser wall-time intervals, not CPU time inside the lookup.

The slowest candidate measured sample (index 29) took 2,356.8 ms from Enter to
accepted-settled and passed the correctness checks. Its measured intervals
included 373.2 ms autosave HTTP, a 239.8 ms post-save four-read critical span,
471.8 ms from Review click to enabled Accept preview (426.7 ms preview HTTP),
and 1,018.7 ms from Accept click to settled (755.9 ms Apply HTTP and 261.9 ms
after the response). The recorded ready-action gaps totaled 178.6 ms; this is
automation timing, not human review dwell. The two post-Accept reads overlapped
with a 3.0 ms critical span. These intervals overlap and should not be summed.
The outlier was not CPU-profiled, and host load was not measured, so its cause
is unknown; do not attribute it to host load or rendering alone.

## Baseline, 2026-11-08

Captured 2026-11-08 on a shared Linux host: Intel Xeon E5-2650 v3, 12 logical CPUs, 31 GiB, kernel 7.0.0-42. `pp-server` was a release build with `desktop_rebuild` from a checkout of main at `a71591e4c` (sha256 `87bd1e32…5bc6`). The samples record `checkout_dirty: true`: the harness and these docs (`scripts/perf-baseline/`, `scripts/local-ci.py`, `docs/`) were not yet committed. No file under `rust/` or `web/` differed from `a71591e4c`. Toolchain: Rust 1.99.0, Node 24.21.0. Dataset digest `41d97179…44ad1`, seed 388. Ten rounds per runtime, `--max-load 16 --quiet-timeout 180`. Other agents' builds kept the 1-minute load at run start between 8.4 and 22.1 for the whole run (Node 8.4 to 20.0, Rust 11.0 to 22.1, from `load_at_start`), so both runtimes ran on a busy machine and the spread is wide; compare medians. Raw samples and all metadata: [performance-baseline-2026-11-08.json](performance-baseline-2026-11-08.json).

| Measure | Node median | Node p95 | Rust median | Rust p95 | Rust/Node (median) | n (Node/Rust) | Note |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Process start to origin line | 10530 ms | 14678 ms (max) | 5344 ms | 10402 ms (max) | 0.51x | 10/10 |  |
| First Builds list (`GET /api/v2/plans`) | 458 ms | 753 ms (max) | 584 ms | 759 ms (max) | 1.28x | 10/10 |  |
| App open to usable Builds list | 11137 ms | 15204 ms (max) | 5972 ms | 11052 ms (max) | 0.54x | 10/10 |  |
| Builds list, warm | 407 ms | 702 ms | 632 ms | 947 ms | 1.55x | 100/100 |  |
| Idle memory, PSS of process tree | 354.6 MiB | 377.3 MiB (max) | 69.5 MiB | 71.1 MiB (max) | 0.20x | 10/10 |  |
| Idle CPU, process tree (% of one core) | 1.02 % | 2.03 % (max) | 0.43 % | 0.60 % (max) | 0.43x | 10/10 |  |
| Thumbnails cold, whole Build | 2525 ms | 4649 ms (max) | - | - | - | 10/0 | not available in Rust |
| Thumbnail cold, per Part | 119 ms | 219 ms | - | - | - | 200/0 | not available in Rust |
| Thumbnails warm, whole Build | 626 ms | 896 ms | - | - | - | 30/0 | not available in Rust |
| Thumbnail warm, per Part | 30.0 ms | 51.4 ms | - | - | - | 600/0 | not available in Rust |
| Plan change: open draft (recompute) | 99.8 ms | 189 ms | 71.9 ms | 176 ms | 0.72x | 60/60 |  |
| Plan change: edit Part quantity | 54.9 ms | 86.6 ms | 29.7 ms | 110 ms | 0.54x | 60/60 |  |
| Plan change: autosave | - | - | 32.5 ms | 80.2 ms | - | 0/60 | Node has no draft-only autosave |
| Plan change: Accept preview | - | - | 12.0 ms | 22.4 ms | - | 0/60 | Node has no Accept preview |
| Plan change: Accept | 90.6 ms | 137 ms | 29.9 ms | 79.5 ms | 0.33x | 60/60 |  |
| Plan change: open + edit + Accept | 242 ms | 421 ms | 134 ms | 379 ms | 0.55x | 60/60 |  |
| Plan change: whole flow as each UI sends it | 243 ms | 422 ms | 185 ms | 480 ms | - | 60/60 | different requests per runtime; see the notes |
| Plan change: Node one-request Save | 239 ms | 388 ms | - | - | - | 60/0 | Node only; Rust has no `/plans/:id/save` |
| Checkoff burst, write + screen refresh | 451 ms | 750 ms | 220 ms | 329 ms | - | 150/150 | different requests per runtime; see the notes |
| Checkoff session, 15 bursts, 100 units | 6934 ms | 13192 ms (max) | 3510 ms | 4360 ms (max) | - | 10/10 | different requests per runtime; see the notes |
| STL pack export, first in run | 1482 ms | 5122 ms (max) | - | - | - | 10/0 | not available in Rust |
| HTML checklist export, first in run | 382 ms | 592 ms (max) | - | - | - | 10/0 | not available in Rust |
| STL pack export, repeated | 983 ms | 6400 ms (max) | - | - | - | 10/0 | not available in Rust |
| HTML checklist export, repeated | 354 ms | 483 ms (max) | - | - | - | 10/0 | not available in Rust |
| Printer event to status route | 1361 ms | 1597 ms (max) | 573 ms | 1553 ms (max) | - | 10/10 | the Rust gateway watcher in both runtimes; 2 s poll |
| Today refresh reads | 9.6 ms | 24.0 ms | 1310 ms | 1806 ms | - | 50/50 | different requests per runtime; see the notes |
| Today read `GET /api/v2/tasks`, within the refresh | - | - | 1304 ms | 1804 ms | - | 0/50 | Rust only; Node has no task feed |
| Memory after the workload, PSS | 438.6 MiB | 472.3 MiB (max) | 73.3 MiB | 74.7 MiB (max) | 0.17x | 10/10 |  |
| Processes in the tree | 3 processes | 3 processes (max) | 1 processes | 1 processes (max) | - | 10/10 |  |
| Machine 1-minute load at run start | 11.7 | 20.0 (max) | 15.1 | 22.1 (max) | - | 10/10 |  |

What the numbers say:

- App open: Rust reaches a usable Builds list in about 6.0 s, Node in about 11.1 s. Most of the Node time is starting the Node child before the origin line.
- Memory: the Rust-only process uses about 70 MiB PSS idle and 73 MiB after the workload; the Node runtime's three processes use about 355 MiB and 439 MiB.
- Builds list: Rust is slower, 584 ms against 458 ms for the first list and 632 ms against 407 ms warm, on 22 Builds. This is the only shared read where Rust lost.
- Today: `GET /api/v2/tasks` takes about 1.3 s and is almost all of Today's refresh. Node has no task feed to compare it with.
- Plan change: the three steps both serve (open draft, edit, Accept) take 134 ms on Rust and 242 ms on Node. Rust's autosave adds 33 ms and Accept preview 12 ms. Node's one-request Save takes 239 ms.
- Checkoff: Build 22's 15 bursts take 3.5 s on Rust and 6.9 s on Node. Node sends one request per unit, so this compares the two workflows, not the same request.
- Rust cannot do thumbnails, the STL pack or the HTML checklist yet. Node's cold thumbnails take 2.5 s for the 20 Parts of the 100-unit Build (server work only), the first STL pack 1.5 s and the HTML checklist 0.4 s.

## Task feed read, 2026-11-10

GRE-1290 cut the Rust read path behind `GET /api/v2/tasks`. Both runs below are 10 rounds of `baseline.py` with the default settings on the same host as the baseline, 1-minute load under 9 at every start. "Before" is a release `pp-server` built from `d194ae2a1`; "after" is the GRE-1290 branch at `70da4df64`. Both were built against the same `rust/bundle-manifest.json`, so they serve identical web assets. Only Rust rows changed; Node medians moved by 10% or less between the runs.

| Rust measure | Before median | Before p95 | After median | After p95 |
| --- | ---: | ---: | ---: | ---: |
| Today read `GET /api/v2/tasks`, within the refresh | 996 ms | 1692 ms | 220 ms | 259 ms |
| Today refresh reads | 998 ms | 1693 ms | 222 ms | 262 ms |
| Builds list, warm | 478 ms | 934 ms | 102 ms | 139 ms |
| First Builds list | 868 ms | 936 ms (max) | 141 ms | 154 ms (max) |
| Checkoff burst, write + screen refresh | 168 ms | 293 ms | 40.3 ms | 55.1 ms |
| Plan change: open + edit + Accept | 158 ms | 204 ms | 101 ms | 150 ms |

What changed:

- `read_model::graph::rows` ran `PRAGMA table_info`, an aggregate scan for the text and row budget, and a second scan for the rows, all freshly prepared. It now caches column names per read, keeps prepared statements (256 per connection), and checks each row's text size and budget in one scan. Every read model that uses `rows` gains, which is why the Builds list and checkoff rows also dropped.
- `graph::read` looked up each Required unit and its creation revision one query at a time. It reads the mapped units in one query and reuses each creation revision, charging the budget as before.
- `context::capture` scanned `plan_revision_required_units` once per unit, because `required_unit_token` has no index of its own. It scans once per Build.
- Farm match suggestions interpreted every Part file name again for every unmatched print, and allocated per Levenshtein row and per collation comparison. `pp_domain::gcode_objects::FilenameIndex` keeps those keys once per read.

On a fresh copy of the seed-388 dataset with no workload before it, the task feed answers in about 120 ms. After the harness workload it takes about 220 ms. The workload adds plan revisions, and the remaining time is mostly scans of `plan_revision_required_units`, `required_units`, `plan_revision_parts` and `parts` by columns that no index leads with, plus the per-Build digest and projection work. Going further needs indexes, which are a schema change.

## Thumbnail, Plan change and Checkoff, 2026-11-15

GRE-1668. One default run of `baseline.py` on the seed-388 dataset, Rust-only against the Node runtime, on one machine. The 2026-11-08 table stays as captured. Its export rows stay there too. [Export downloads](#export-downloads-2026-11-15) records the Rust samples.

Captured 2026-11-15 on a Linux host: Intel Xeon Processor, 4 logical CPUs, 15.6 GiB, kernel 6.12.113+. `pp-server` was a release build with `desktop_rebuild` from main at `8623d14ff` (sha256 `0e0ebaff…a41b`). The samples record `checkout_dirty: false`. Toolchain: Rust 1.99.0, Node 24.21.0. Dataset digest `41d97179…44ad1`, the same seed-388 digest as the 2026-11-08 capture. Ten rounds per runtime, default counts (`--settle-secs 5`, `--idle-secs 30`, `--plan-iterations 6`, `--list-repeats 10`, `--export-repeats 2`), `--max-load 4` (the CPU count) and `--quiet-timeout 300`. The 1-minute load at run start stayed between 0.05 and 0.53 (Node 0.05 to 0.29, Rust 0.11 to 0.53). `http_retries` is 0 on both runtimes. Five loopback Moonraker simulators answered status reads; the run sent nothing to a printer. Raw samples and all metadata: [performance-baseline-2026-11-15.json](performance-baseline-2026-11-15.json). That file is the whole harness run, so it also holds the Node export timings the harness records every time. The Rust export rows are in [Export downloads](#export-downloads-2026-11-15).

Requests this run timed:

| Measure | Node request | Rust request |
| --- | --- | --- |
| Thumbnail load | per Part of "Synthetic Build Anchor 100u15p": cold is the placeholder `GET /parts/:id/thumbnail`, `GET /parts/:id/mesh` and `POST /parts/:id/thumbnail` with a 256 px PNG; warm is `GET /parts/:id/thumbnail` | no route on main |
| Plan change | on "Synthetic Build Anchor 47u6p": `POST /api/v2/plans/{id}/drafts/recompute`, `PATCH .../drafts/{draft_id}/parts`, `POST .../drafts/{draft_id}/apply`; also `POST /plans/:id/save` | the same three, plus `POST .../drafts/save` and `GET .../drafts/{draft_id}/accept-preview` |
| Checkoff session | on "Synthetic Build Anchor 100u15p", 15 bursts, 100 units: per unit `PATCH /parts/:id/progress`, then `GET /plans/:id/checkoff` per burst | per burst `POST /api/v2/plans/{id}/checkoff/bursts`, then `GET .../checkoff/units` |

| Measure | Node median | Node p95 | Rust median | Rust p95 | Rust/Node (median) | n (Node/Rust) | Note |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Thumbnails cold, whole Build | 519 ms | 639 ms (max) | - | - | - | 10/0 | GRE-1520 owes the route |
| Thumbnail cold, per Part | 24.2 ms | 37.0 ms | - | - | - | 200/0 | GRE-1520 owes the route |
| Thumbnails warm, whole Build | 148 ms | 162 ms | - | - | - | 30/0 | GRE-1520 owes the route |
| Thumbnail warm, per Part | 7.0 ms | 10.8 ms | - | - | - | 600/0 | GRE-1520 owes the route |
| Plan change: open draft (recompute) | 20.7 ms | 35.8 ms | 10.3 ms | 11.5 ms | 0.50x | 60/60 |  |
| Plan change: edit Part quantity | 14.4 ms | 24.7 ms | 7.3 ms | 8.5 ms | 0.50x | 60/60 |  |
| Plan change: autosave | - | - | 8.7 ms | 9.8 ms | - | 0/60 | Node has no draft-only autosave |
| Plan change: Accept preview | - | - | 3.9 ms | 4.6 ms | - | 0/60 | Node has no Accept preview |
| Plan change: Accept | 18.2 ms | 32.2 ms | 8.8 ms | 11.3 ms | 0.48x | 60/60 |  |
| Plan change: open + edit + Accept | 55.5 ms | 84.7 ms | 26.2 ms | 31.2 ms | 0.47x | 60/60 |  |
| Plan change: whole flow as each UI sends it | 55.7 ms | 85.1 ms | 39.2 ms | 45.2 ms | - | 60/60 | different requests per runtime; see the notes |
| Plan change: Node one-request Save | 88.7 ms | 102 ms | - | - | - | 60/0 | Node only; Rust has no `/plans/:id/save` |
| Checkoff burst, write + screen refresh | 104 ms | 134 ms | 9.0 ms | 11.6 ms | - | 150/150 | different requests per runtime; see the notes |
| Checkoff session, 15 bursts, 100 units | 1558 ms | 1995 ms (max) | 144 ms | 157 ms (max) | - | 10/10 | different requests per runtime; see the notes |

What this run says:

- Thumbnail load stays unclaimed. Node's cold thumbnails for the 20 Parts take 519 ms for the Build and 24.2 ms per Part; the warm reads take 148 ms and 7.0 ms. Rust recorded no thumbnail samples. Main has no `GET /sources/{id}/thumbnail`. [GRE-1520](https://linear.app/greetz/issue/GRE-1520) (part of [GRE-430](https://linear.app/greetz/issue/GRE-430)) still owes that route.
- Plan change: the three steps both serve (open draft, edit, Accept) take 26.2 ms on Rust and 55.5 ms on Node. The whole flow each UI sends takes 39.2 ms on Rust and 55.7 ms on Node. Rust's autosave is 8.7 ms and Accept preview is 3.9 ms. Node's one-request Save takes 88.7 ms.
- Checkoff: the 15 bursts take 144 ms on Rust and 1558 ms on Node. A burst is 9.0 ms on Rust and 104 ms on Node. Node sends one request per unit; Rust sends one request per burst.
- Where both columns have a median, the Rust median is lower than the Node median. That includes the whole Plan-change flow and both Checkoff rows, whose requests differ.

## Export downloads, 2026-11-15

GRE-1535. One default run of `baseline.py` on the seed-388 dataset. The 2026-11-08 table and the thumbnail, Plan and Checkoff capture above stay as captured.

Captured 2026-11-15 on a Linux host: Intel Xeon Processor, 4 logical CPUs, 15.6 GiB, kernel 6.12.113+. `pp-server` was a release build with `desktop_rebuild` from `d2441f8fb` (sha256 `114fffcd…dfcb`). The samples record `checkout_dirty: false`. Toolchain: Rust 1.99.0, Node 24.21.0. Dataset digest `41d97179…44ad1`, the same seed-388 digest as the earlier captures. Ten rounds per runtime, default counts (`--settle-secs 5`, `--idle-secs 30`, `--plan-iterations 6`, `--list-repeats 10`, `--export-repeats 2`), `--max-load 4` (the CPU count) and `--quiet-timeout 300`. The 1-minute load at run start stayed between 0.05 and 0.56 (Node 0.05 to 0.38, Rust 0.09 to 0.56). `http_retries` is 0 on both runtimes. Five loopback Moonraker simulators answered status reads; the run sent nothing to a printer. Raw samples and all metadata: [performance-baseline-2026-11-15-exports.json](performance-baseline-2026-11-15-exports.json).

The four rows are one transfer each. Node starts `POST /jobs/export-stl-pack` or `POST /jobs/export-checklist-html` for "Synthetic Build Anchor 100u15p", polls `GET /jobs/:id` every 20 ms, and downloads the file. Rust reads `GET /api/v2/plans/{id}/exports/stl-pack` and `GET /api/v2/plans/{id}/exports/checklist` from the request to the last byte. The check rows are a later GET of the same path, stopped at the headers with the body unread, which is the request the Production tab aborts before the browser download. The click rows add the download that follows that check. Those rows run after the four GETs, so the files are already in the page cache.

| Measure | Node median | Node p95 | Rust median | Rust p95 | Rust/Node (median) | n (Node/Rust) | Note |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| STL pack export, first in run | 164 ms | 210 ms (max) | 50.0 ms | 56.6 ms (max) | 0.30x | 10/10 |  |
| HTML checklist export, first in run | 143 ms | 209 ms (max) | 7.1 ms | 9.6 ms (max) | 0.05x | 10/10 |  |
| STL pack export, repeated | 181 ms | 227 ms (max) | 48.0 ms | 52.3 ms (max) | 0.27x | 10/10 |  |
| HTML checklist export, repeated | 117 ms | 165 ms (max) | 7.2 ms | 9.1 ms (max) | 0.06x | 10/10 |  |
| STL pack export, first GET until headers | - | - | 9.0 ms | 13.6 ms (max) | - | 0/10 | Rust only; each STL is opened and hashed before the first byte |
| HTML checklist export, first GET until headers | - | - | 7.1 ms | 9.6 ms (max) | - | 0/10 | Rust only; the checklist is built before the first byte |
| STL pack export, repeated GET until headers | - | - | 7.3 ms | 10.3 ms (max) | - | 0/10 | Rust only; each STL is opened and hashed before the first byte |
| HTML checklist export, repeated GET until headers | - | - | 7.2 ms | 9.1 ms (max) | - | 0/10 | Rust only; the checklist is built before the first byte |
| STL pack export, check request | - | - | 7.2 ms | 10.4 ms (max) | - | 0/10 | Rust only; aborted at the headers, body not read |
| HTML checklist export, check request | - | - | 5.7 ms | 9.3 ms (max) | - | 0/10 | Rust only; aborted at the headers, body not read |
| STL pack export, repeated check request | - | - | 6.7 ms | 9.3 ms (max) | - | 0/10 | Rust only; aborted at the headers, body not read |
| HTML checklist export, repeated check request | - | - | 5.5 ms | 6.7 ms (max) | - | 0/10 | Rust only; aborted at the headers, body not read |
| STL pack export, check then download | - | - | 14.4 ms | 19.7 ms (max) | - | 0/10 | Rust only; one click opens and hashes each STL twice |
| HTML checklist export, check then download | - | - | 12.0 ms | 16.3 ms (max) | - | 0/10 | Rust only; one click builds the checklist twice |
| STL pack export, repeated check then download | - | - | 13.6 ms | 17.1 ms (max) | - | 0/10 | Rust only; one click opens and hashes each STL twice |
| HTML checklist export, repeated check then download | - | - | 11.6 ms | 16.3 ms (max) | - | 0/10 | Rust only; one click builds the checklist twice |

What this run says:

- Each of the four export rows has ten Rust samples, and each Rust median is lower than the Node median for that row. The STL pack is 50 ms first and 48 ms repeated, against Node's 164 ms and 181 ms. The HTML checklist is 7.1 ms and 7.2 ms, against 143 ms and 117 ms.
- The STL pack opens and hashes each distinct STL before the first byte: 9.0 ms on the first GET and 7.3 ms on the repeat. The checklist does not open Source files. It is built before the headers, so the time to headers and the time to the last byte are the same 7.1 ms and 7.2 ms.
- Six of the ten first STL reads, and six of the ten repeats, land between 47 ms and 57 ms. The other four finish between 7.5 ms and 13 ms, close to the headers. The response is chunked and sends no Content-Length, and the extra time is after the headers. The checklist, which sends Content-Length, has no such gap.
- A Production-tab click pays for two opens. The check stops at the headers, after the core has opened and verified the files, and the browser download sends the GET again. On this run those samples follow the row GETs, so the files are in the page cache. The STL check is 7.2 ms and the click is 14.4 ms; the repeated click is 13.6 ms. The checklist check is 5.7 ms and the click is 12.0 ms; the repeated click is 11.6 ms. The check is the extra open, about 7 ms for the STL pack and about 6 ms for the checklist, on top of the download in that same click. A click stays under the Node median for the same file.

## Source thumbnails, 2026-11-15

GRE-1695. One default run of `baseline.py` on the seed-388 dataset, after `GET /sources/{id}/thumbnail` landed on main. The 2026-11-08 table, the thumbnail, Plan and Checkoff capture above, and the export rows stay as captured.

Captured 2026-11-15 on a Linux host: Intel Xeon Processor, 4 logical CPUs, 15.6 GiB, kernel 6.12.113+. `pp-server` was a release build with `desktop_rebuild` from `e9ee1d804` (sha256 `ea47463e…932f`). The samples record `checkout_dirty: false`. Toolchain: Rust 1.99.0, Node 24.21.0. Dataset digest `41d97179…44ad1`, the same seed-388 digest as the earlier captures. Ten rounds per runtime, default counts (`--settle-secs 5`, `--idle-secs 30`, `--plan-iterations 6`, `--list-repeats 10`, `--export-repeats 2`), `--max-load 4` (the CPU count) and `--quiet-timeout 300`. The 1-minute load at run start stayed between 0.056 and 0.578 (Node 0.056 to 0.578, Rust 0.097 to 0.426). `http_retries` is 0 on both runtimes. Five loopback Moonraker simulators answered status reads; the run sent nothing to a printer. Raw samples and all metadata: [performance-baseline-2026-11-15-thumbnails.json](performance-baseline-2026-11-15-thumbnails.json).

The four rows time the 20 Parts of "Synthetic Build Anchor 100u15p". Node's cold request is the placeholder `GET /parts/:id/thumbnail`, `GET /parts/:id/mesh` and `POST /parts/:id/thumbnail` with a 256 px PNG. Its warm request is `GET /parts/:id/thumbnail`, three times. Rust's request, cold and warm, is `GET /sources/{id}/thumbnail?path=` for that Part's STL. The Source is the Build layer whose `layer_type:project_name` equals the Part's `source_layer`, and `path` is the Part's `relative_path`. Cold is the first read: the cache is empty, so Rust renders the PNG and stores it under `thumbs/sources`. Warm is the same GET, three times, answered from that cache.

| Measure | Node median | Node p95 | Rust median | Rust p95 | Rust/Node (median) | n (Node/Rust) | Note |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Thumbnails cold, whole Build | 565 ms | 717 ms (max) | 53.9 ms | 63.9 ms (max) | 0.10x | 10/10 |  |
| Thumbnail cold, per Part | 26.8 ms | 40.6 ms | 2.5 ms | 4.2 ms | 0.09x | 200/200 |  |
| Thumbnails warm, whole Build | 164 ms | 250 ms | 4.8 ms | 8.0 ms | 0.03x | 30/30 |  |
| Thumbnail warm, per Part | 7.7 ms | 13.0 ms | 0.2 ms | 0.5 ms | 0.03x | 600/600 |  |

What this run says:

- Each Rust median is lower than the Node median for the same row. The cold Build is 53.9 ms on Rust and 565 ms on Node (0.10x). Per Part that is 2.5 ms and 26.8 ms (0.09x). The warm Build is 4.8 ms and 164 ms (0.03x). Per Part that is 0.2 ms and 7.7 ms (0.03x).
- Node's cold time is the placeholder, the mesh and the upload. Rust's cold GET renders the STL. These STLs are the synthetic closed boxes in the seed-388 dataset.
- Ten rounds and 20 Parts give 10 cold Build samples, 200 cold Part samples, 30 warm Build samples and 600 warm Part samples on each runtime.

## Source thumbnails, 2026-11-24

[GRE-1952](https://linear.app/greetz/issue/GRE-1952/recheck-rust-thumbnail-performance-against-node) rechecks the current runtime against the existing request-sequence median gate (Rust median no greater than Node median in the matching row). GRE-1952 does not define a separate end-to-end, user-facing thumbnail speed gate. The earlier Plan, Checkoff, export, and 2026-11-15 thumbnail evidence above remains unchanged.

### Reproduce this capture

With npm dependencies installed as described in [Run it](#run-it), use Node 24.21.0 for the runtime and Node 22.23.3 as the schema-parity oracle:

```sh
export PATH="$HOME/.nvm/versions/node/v24.21.0/bin:$PATH"
export PP_SCHEMA_PARITY_NODE="$HOME/.nvm/versions/node/v22.23.3/bin/node"
NODE="$HOME/.nvm/versions/node/v24.21.0/bin/node"
VITE_PRINT_PARTNER_DESKTOP=1 npm --prefix web run build
python3 rust/scripts/build-desktop-manifest.py --web web --node "$NODE" \
  --output rust/bundle-manifest.json --commit "$(git rev-parse HEAD)"
cargo build --manifest-path rust/Cargo.toml --locked --release -p pp-server \
  --features desktop_rebuild
OUT="$HOME/gre1952-benchmark/capture-$(date -u +%Y%m%dT%H%M%SZ)"
python3 scripts/perf-baseline/baseline.py --binary rust/target/release/pp-server \
  --node "$NODE" --out "$OUT" --runs 10 --settle-secs 5 --idle-secs 30 \
  --plan-iterations 6 --list-repeats 10 --export-repeats 2 --max-load 8 \
  --quiet-timeout 300
```

`OUT` must be a new or empty directory. The harness seeds dataset 388 and five loopback Moonraker simulators; it runs the same release binary with `PP_DESKTOP_REBUILD` unset for Node and set to `1` for Rust. It interleaves ten fresh starts per runtime. Timings depend on the host and load. The recorded bundle and checkout commit were `064aceaec90bb42e81966c002ef3fc0cb11df143`; the captured release binary SHA-256 is recorded below and in the raw JSON.

Captured 2026-11-24 at 21:40:56 UTC on Linux: Intel Xeon Processor, 8 logical CPUs, 31.3 GiB, kernel 6.8.0-1060-aws. The release `pp-server` (`desktop_rebuild`) and bundle manifest use commit `064aceaec90bb42e81966c002ef3fc0cb11df143`; binary SHA-256 `20f40e9b77bc0d300cd561242f8e145d4e22a100dba4c73fe1779d629fd3879e`. `checkout_dirty` is true because this capture used the thumbnail-response validation changes in the harness; no Rust runtime source changed. Toolchain: Rust 1.99.0, Node 24.21.0, Python 3.12.16 (the Node 22.23.3 schema-parity executable was set for the build). Seed 388, 5 loopback Moonraker simulators, dataset digest `41d971796bceb6e3fc0d290d1e376a2e17fba7d71f9d9ab60f48861cbab44ad1`. Ten interleaved rounds per runtime with default counts and `--max-load 8`; start loads ranged from 0.1 to 5.0. Raw samples and metadata: [performance-baseline-2026-11-24-thumbnails.json](performance-baseline-2026-11-24-thumbnails.json).

Each runtime's sequence covers the same 20 Parts in the seed-388 Build, but the sequences do not render equivalent thumbnail content. Rust cold and warm use `GET /sources/{id}/thumbnail?path=` for the Part's Source and STL path; cold renders, warm reads its cache. Node cold is the 1×1 placeholder `GET /parts/:id/thumbnail`, `GET /parts/:id/mesh`, and `POST /parts/:id/thumbnail` with the harness's synthetic 256×256 PNG; warm is the cached `GET /parts/:id/thumbnail`. The Node warm body must match the uploaded PNG exactly. The harness validates successful status, `image/png`, PNG checksums and decoded data, and the expected dimensions before appending a sample. It accepts only a valid 1×1 placeholder in Node's cold path; a failed request, invalid image, warm placeholder, or mismatched Node cache produces no sample for that pass.

The synthetic Node PNG does not contain equivalent thumbnail content, and generating it does not perform the rendering work Rust does on an STL. The Node browser's actual WebGL render is not timed; the harness uploads a synthetic fixture instead. Although both warm responses are valid 256×256 PNGs, this capture makes no end-to-end thumbnail/rendering parity claim.

| Measure | Node median | Node p95 | Rust median | Rust p95 | Rust/Node (median) | n (Node/Rust) | Note |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Thumbnails cold, whole Build | 551 ms | 861 ms (max) | 72.2 ms | 83.9 ms (max) | 0.13x | 10/10 |  |
| Thumbnail cold, per Part | 25.3 ms | 46.9 ms | 3.2 ms | 6.1 ms | 0.13x | 200/200 |  |
| Thumbnails warm, whole Build | 136 ms | 195 ms | 11.3 ms | 16.8 ms | 0.08x | 30/30 |  |
| Thumbnail warm, per Part | 6.7 ms | 11.6 ms | 0.5 ms | 1.0 ms | 0.07x | 600/600 |  |

What this run says:

- All four Rust medians are below the Node medians, passing the existing request-sequence median gate for this capture. This is not an end-to-end thumbnail-rendering or user-facing speed result.
- Both runtimes measured 20 Parts per Build, with 10 cold Build, 200 cold Part, 30 warm Build, and 600 warm Part samples. `http_retries` was zero on both sides.
