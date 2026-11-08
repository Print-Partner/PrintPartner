# Performance baseline

`scripts/perf-baseline/baseline.py` measures the Node runtime and the Rust-only runtime on the same synthetic data, so the owner's slow operations have a number before budgets are set (GRE-388). It records raw samples; it sets no budget and gates nothing. [`pp-perf`](../../rust/crates/pp-perf/README.md) keeps the Measurement record and the 10% regression comparison for later captures.

## Run it

Build the web assets, the bundle manifest and a release `pp-server` with the rebuild feature, then run the harness:

```sh
npm --prefix web ci
VITE_PRINT_PARTNER_DESKTOP=1 npm --prefix web run build
python3 rust/scripts/build-desktop-manifest.py --web web --node "$(command -v node)" \
  --output rust/bundle-manifest.json --commit "$(git rev-parse HEAD)"
cargo build --manifest-path rust/Cargo.toml --locked --release -p pp-server --features desktop_rebuild
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
| Thumbnails | per Part of the 100-unit Build: cold is the placeholder `GET /parts/:id/thumbnail`, `GET /parts/:id/mesh` and `POST /parts/:id/thumbnail` with a 256 px PNG; warm is `GET /parts/:id/thumbnail` | none |
| Plan change | `POST .../drafts/recompute`, `PATCH .../parts`, `POST .../apply` under `/api/v2/plans/{id}`; also `POST /plans/:id/save` | the same three, plus `POST .../drafts/save` and `GET .../accept-preview` |
| Checkoff session | per unit `PATCH /parts/:id/progress`, then `GET /plans/:id/checkoff` per burst | per burst `POST /api/v2/plans/{id}/checkoff/bursts`, then `GET .../checkoff/units` |
| Exports | `POST /jobs/export-stl-pack` or `/jobs/export-checklist-html`, `GET /jobs/:id` every 20 ms until done, then the download | none |
| Printer event | a simulator turns `printing` into `complete`; poll `GET /api/v1/integrations/{id}/status` every 50 ms | same |
| Today refresh | All Production's reads at once: `/printer-checkoff`, `/printer-checkoff/unattributed` | Today's reads at once: `/api/v2/tasks`, `/api/v2/printer-checkoff`, `/api/v2/printers` |

What the numbers leave out:

- No browser. Rendering, the WebGL thumbnail render and React work are not timed. A cold thumbnail is the server's share only.
- Neither the old Builds list nor the replacement Builds list requests thumbnails, so the thumbnail rows time a Build's Part thumbnails, which the old app shows on the Build's Sources and Review steps.
- The printer event row times the gateway's Rust Moonraker watcher in both modes; it polls every 2 s, so the row shows where the event fell in that cycle. It is not a Node-versus-Rust comparison.
- Rows marked "different requests" have no ratio. Checkoff on Node is one request per unit and on Rust one per burst; Today and All Production are different screens.
- When a server closes an idle keep-alive connection, the request is sent again once and only that attempt is timed. `http_retries` in the samples counts these: 0 on Node and 3 on Rust in the baseline below.
- p95 is nearest-rank. Below 20 samples it is the maximum, and the table says "(max)". Per-request rows pool samples from all rounds.

## Baseline, 2026-11-08

Captured 2026-11-08 on a shared Linux host: Intel Xeon E5-2650 v3, 12 logical CPUs, 31 GiB, kernel 7.0.0-42. `pp-server` was a release build with `desktop_rebuild` of main at `a71591e4c` (sha256 `87bd1e32…5bc6`), Rust 1.99.0, Node 24.21.0. Dataset digest `41d97179…44ad1`, seed 388. Ten rounds per runtime, `--max-load 16 --quiet-timeout 180`. Other agents' builds kept the 1-minute load between 9.5 and 22 for the whole run, so both runtimes ran on a busy machine and the spread is wide; compare medians. Raw samples and all metadata: [performance-baseline-2026-11-08.json](performance-baseline-2026-11-08.json).

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
