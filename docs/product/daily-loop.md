# The daily loop

Owner: Chad. Approved 2026-11-12. This page defines what the Rust core and the
replacement UI must do before anything else gets built. Linear ticket GRE-1428
maps each step to its tickets and owns execution state; this page owns what
"done" means.

The plan is finished when Chad has walked every step below on the Rust-only
Docker image with the replacement UI, on a copy of his own data. Node stays in
production until then.

## Steps

Each step is done when Chad can do it himself and see the result. A mounted
route or a merged PR is not a finished step.

| # | The owner does | Done when |
| --- | --- | --- |
| 1 | Starts a fresh Rust-only image and opens the `pp-server bootstrap-url` link | He sets a password and lands on Today in the replacement UI. After signing out, the sign-in screen takes him back to the page he asked for. |
| 2 | Imports a copy of his Node installation | His Builds, accepted Plans, Plates, Checkoff history, printers and Sources open in Rust. Plan, Plate and Required-unit IDs match the source. The original is unchanged ([ADR 0016](../adr/0016-rust-owns-its-data-formats.md)). |
| 3 | Adds a Moonraker or PrusaLink printer in Settings | The probe passes, the printer is saved, and it shows live status on Farm after a restart. |
| 4 | Adds a GitHub Source, or uploads a ZIP | Its files and thumbnails appear in Library. Sync shows progress and resumes after a restart. |
| 5 | Creates a Build, through the assistant over MCP or in the UI | The Build exists with its Sources and a first draft Plan. |
| 6 | Edits the Plan and passes the Accept gate | A new accepted revision exists, and Required units keep their IDs. |
| 7 | Downloads the STL pack and the HTML checklist | Both match the accepted Plan. |
| 8 | Sends a sliced file to a printer from Production | The printer receives it once. An unknown outcome asks him to reconcile and is never resent. |
| 9 | Watches Farm and Today while it prints | Status follows the printer without a page reload. |
| 10 | Checks off the finished print | The units are done in Checkoff, and Undo reverses the burst. |
| 11 | Reads remaining work and filament stock | Today and the Build show what is left. Spoolman shortfalls are current, and a deduction is recorded once. |
| 12 | Makes a backup and restores it on a copy | The restored copy opens with the same Builds and history. |

## Score

The daily-loop test counts the steps that work today. It is the measure of progress
between Chad's walks; the plan still finishes only on his own walk on his data.

`web/test/daily-loop/daily-loop.spec.ts` holds twelve tests, one per row above, titled
with the row's "The owner does" text. They run in that order, in a browser, against one
installation of the Rust-only image built from the commit under test. A separate test
fails when the titles and the table differ.

```sh
npm --prefix web run test:daily-loop
```

The run prints one line per step and ends with `Daily loop: N of 12`. N counts `pass`
only. No step is skipped.

| State | Meaning | The run |
| --- | --- | --- |
| `pass` | The step passed and has no marker. | passes |
| `expected_fail` | The step failed and has a marker: the ticket that will make it pass. | passes |
| `fail` | The step failed or did not finish, and has no marker. | fails |
| `unexpected_pass` | The step passed and still has a marker. | fails |

A marker is the ticket in `step(3, "GRE-1431", …)`. The test of a marked step goes as far
as the screens allow and then stops at `unwritten(…)`, which names what is still to check.
The ticket that makes a step work finishes that test and sets the marker to `null` in the
same PR; the label `ci:daily-loop` runs the test on that PR. The `Daily loop` workflow runs
it on each push to `main` and publishes the score ([CI](../development/ci.md#daily-loop)).

## Speed

The four operations Chad named as slow must be no slower than the Node
baseline in [Performance](../development/performance.md): thumbnail load, a
Plan change, a Checkoff session, and an STL pack or HTML checklist export.

## What waits

Windows, sharing on the network, native slicer handoff, Bambu beyond status,
webhooks and Home Assistant, 3MF and kit-bundle exports, and every Node feature
not in the steps above. Each returns when Chad misses it.

## Rules while this plan runs

[Definition of done](../development/definition-of-done.md#work-in-progress)
holds the lane rule, [Parity scope](../development/definition-of-done.md#parity-scope)
the parity rule, and the [merge gate](../development/definition-of-done.md#merge-gate)
the review depth. [Rust core](../../rust/AGENTS.md#new-code) holds the rules
for new Rust code.
