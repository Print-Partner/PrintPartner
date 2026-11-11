# Continuous integration

The **Rust cross-OS** workflow (`.github/workflows/rust-cross-os.yml`) builds and tests the Rust workspace on Linux, macOS and Windows. It starts on every pull request, every push to `main`, merge groups, a nightly schedule and manual runs.

Draft pull requests skip CI jobs before runner allocation, while each job still
reports a skipped check. Marking a PR ready starts CI with the existing path and
label filters. Converting it back to a draft cancels its older PR run. Main,
merge groups, schedules and manual runs keep their existing behavior.

On ready PRs, the jobs are selected as follows:

- `rust (linux)` runs when a `changes` job finds a change to `rust/**`, `rust-toolchain.toml`, `web/package*.json`, `web/apps/**`, `web/packages/**`, `web/tsconfig*.json`, `web/scripts/atomic-package-build.mjs`, `web/scripts/storage-fixtures/**`, `fixtures/**`, `scripts/setup-ci-linux.sh`, `.github/actions/linux-gui-deps/action.yml`, the workflow itself, or the CI filter scripts and their tests. Other changes skip it while keeping its check report present. `rust (windows)` has its own selection, the [Windows tier](#windows-tier) below, and does not depend on this list.
- `rust (macos)` uses the same path selection. It runs only when relevant inputs change and the PR has the exact `macos` or `ci:full` label and is not a draft. Adding or removing the label, marking the PR ready for review and converting it to a draft each start a new run.
- When those jobs run on a pull request, nextest uses an affected-crate filter from `scripts/ci-affected-nextest-filter.py`: for each changed `rust/crates/<pkg>/` path it unions `rdeps(=<pkg>)` so reverse dependents are covered. A crate whose sources read another crate's files by path (for example pp-gateway's `include_str!("../../pp-storage/...")` drift guards) joins the selection when that crate changes. The filtered nextest steps pass `--no-tests=warn`, so a selection that matches nothing in a job's `CARGO_SCOPE` (for example, a change only to a crate that a job's `CARGO_SCOPE` leaves out) passes instead of exiting 4. Shared inputs (workspace `Cargo.toml` / `Cargo.lock`, toolchain, web assets, fixtures, `rust/tests/**`, `rust/scripts/**`, the workflow, or the filter scripts) keep `all()`. Clippy and doctests still cover the whole `CARGO_SCOPE`.

The selection is made at job level by `scripts/ci-path-filter.py`, not with workflow `paths:`, so a skipped job still reports `skipped`: the required `rust (linux)` check and the advisory `rust (windows)` check never stay pending. The rest of this paragraph is about `rust (linux)`; `rust (windows)` follows the [Windows tier](#windows-tier). A PR that changes only docs, other workflows or other non-Rust files skips `rust (linux)`: PR #1548, which changed one workflow and `ci.md`, reported `rust (linux)` as skipped (a workflow change now selects `rust (windows)`), and a PR that touches a path above, such as this filter, runs it. Markdown is not a Rust input and does not select it, except the legal texts under `web/apps/server/src/data/` that pp-api and pp-gateway embed with `include_str!`, and anything under `fixtures/`, where parity READMEs sit inside hashed fixture trees. A UI-only change under `web/apps/web/` still selects `rust (linux)` on purpose, and does not select `rust (windows)`: the Rust jobs run `npm --prefix web run build` and `rust/scripts/build-desktop-manifest.py` on that UI, then test the desktop bundle against the measured manifest. Skipping those PRs would drop that check from the required set. Web CI covers the UI's own lint, tests and build.

#### Windows tier

`rust (windows)` takes up to 120 minutes (the `timeout-minutes` limit), so a PR runs it per push only when it is selected. Main pushes, merge groups, the nightly schedule and manual runs always run it. A PR selects it in two ways, both decided in the `changes` job by `scripts/ci-path-filter.py windows`:

- **Label.** `ci:windows` selects the Windows job. `ci:full` selects it and `rust (macos)`; the existing `macos` label still selects macOS alone. The labels are read from the PR event, and the job re-evaluates when one is added or removed.
- **Path.** The PR changes `.gitattributes` (its `eol=lf` rules only affect Windows checkouts), `rust/crates/pp-os/`, `rust/Cargo.lock`, `rust-toolchain.toml`, `web/package.json`, `web/package-lock.json`, each workspace's `web/apps/*/package.json` and `web/packages/*/package.json` (the Windows job builds every workspace), `web/scripts/atomic-package-build.mjs`, `rust/scripts/build-desktop-manifest.py`, `rust/scripts/desktop_workspaces.py`, `rust/scripts/fetch-historical-schema-commits.sh`, `rust/crates/pp-desktop/tauri.conf.json`, `rust/crates/pp-desktop/icons/` (the Windows build embeds `icon.ico` as a resource), `.github/workflows/`, `.github/actions/`, `.github/actionlint.yaml`, the selector or its test, or any Rust or Cargo file under `rust/`, or TypeScript file under `web/apps/server/src/` or `web/packages/` (the Node child the Windows runtime tests launch, and the shared packages it imports), whose head or base content mentions `windows`, `win32`, `named pipe`, a `\\.\pipe` path, the text `unix` anywhere (as in `os::unix`, `cfg(unix)`, `cfg(any(unix, …))` or `UnixStream`, and also `UNIX_EPOCH`, which selects more than needed), `os::fd`, `target_family`, `target_os` or `libc` (a Windows module or dependency, or Unix-only code that must have a Windows counterpart), `linux`, `process.platform` or `/proc/` (Linux-only code whose fallback runs on Windows), or `sep` (as `path.sep` or destructured from `node:path`), `path.win32`, a backslash string literal (`"\\"` or `'\\'`) or a backslash regex (`/\\/`) (path-separator handling). A deleted file is judged on its base content, and a file neither side can read selects it. These path and label rules apply even when the Rust inputs for Linux did not change, so a workflow-only PR runs Windows. A Windows-port PR therefore selects it without a label.

A PR that changes only other Rust, web or docs files skips the Windows job and keeps Linux. The skipped job still reports `rust (windows)`. The owner removed `rust (windows)` from the required checks on 2026-11-08, so it is advisory and a failure does not block a merge; `rust (linux)` uses the same selector and is still required, and GitHub counts a skipped required job as passing, which is the reason the decision must not be loosened casually. The selector and its test are in CODEOWNERS, so changing what skips a check requests Chad's review (CODEOWNERS does not make it a merge requirement), and the full set (Windows, macOS, packaged smoke, native macOS) runs on `main` pushes and nightly regardless of PR labels. A skipped PR is first tested on Windows on `main` after it merges, or on its merge group when it goes through the [merge queue](#merge-queue); because `rust (windows)` is advisory, a Windows failure on a merge group does not eject the PR, and a Windows-only break on `main` raises no alert. A PR whose Windows job did not run has not been tested on Windows: add `ci:windows` when in doubt.

Pushes to `main`, merge groups, nightly and manual runs execute all three jobs regardless of paths, and nextest keeps `all()`. A new push to a pull request cancels that pull request's older run, and a running `main` run is never cancelled and at most one newer `main` run waits behind it (see [Superseded runs](#superseded-runs)). Its purpose is to stop code that compiles on one OS only from reaching `main`.

This page covers what the Rust jobs run and how to reproduce them. [Change a CI workflow](ci-controls.md) covers review, the actionlint and zizmor checks, action pins and required checks. Both apply to this workflow.

## Agent guidance check

The `agent-guidance` job in `actionlint.yml` runs `scripts/check-agent-guidance.py`. It makes no network requests.

- **Links in every page:** for every tracked Markdown page (`git ls-files '*.md'`), each local link (inline, image or reference definition) must point to a file or directory inside the checkout, and each `#anchor` into a Markdown page must match a heading anchor as GitHub generates it or an explicit HTML `id` or `name` anchor. Links in fenced code blocks and inline code are examples and are skipped. External links are not fetched.
- **Guidance rules:** only for the shared guidance (the root `AGENTS.md`, `docs/development/agent-workflow.md` and the repo skills in `.claude/skills/`): a `# ` title (skills excepted), each skill's `name` and `description`, and that Git does not ignore these files.

The job also tests the agent safety guard and the staged-Rust snapshot in the opt-in pre-commit hook (including sparse worktrees). Run it locally with `just guidance`; the `agent-guidance` step of `just ci` runs the link check and its tests. Renaming a heading or moving a page fails this check until every link to it is updated.

## Large-file and pre-commit checks

The `large-files` job in `actionlint.yml` fails when any file in the Git index
is larger than 1.5 MiB (1,572,864 bytes). The largest tracked file today is a
1.2 MB parity fixture. Generate bigger test data at test time or publish it as
an artifact. Run the check with `python3 scripts/check-large-files.py`.

`just hooks` sets `core.hooksPath` to `.githooks` for your clone. The
pre-commit hook then rejects whitespace errors and oversized files in the
staged changes. When staged Rust files change, it runs `cargo fmt --check` on
a copy of the staged files, so unstaged edits do not change the result.
The hook is opt-in; CI runs the same checks either way.

## TypeScript and Python formatting

GRE-1070 uses pinned Prettier for TypeScript and Ruff for Python. ESLint and
rustfmt keep their existing jobs. Install the web dependencies with `npm ci`
from `web/`. Install Ruff in a development virtual environment, not the host
system Python:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements-dev.txt
npm --prefix web run format:check
.venv/bin/ruff format --check scripts rust/scripts web/scripts slicer-sidecar
```

These are check-only commands. They return nonzero for unformatted source and
never rewrite it. The Prettier command covers application, shared-package, and
maintenance-script TypeScript, including tests. It skips generated
`*.generated.ts` files because their generators are the source of truth. Ruff
covers repository scripts, hooks, and the legacy sidecar. Build outputs,
dependency trees, and ignored scratch files are not source.

Existing source has not been mechanically reformatted in this setup slice.
On main `1a0ea2ea`, Prettier reports 1,137 TypeScript files and Ruff reports 61
Python files that need formatting. Both checks exit 1 until those changes land.
Keep formatting in separate bounded PRs, then enforce the checks in CI and the
opt-in hook. Do not treat configuration alone as a passing formatter readiness
criterion or add broad source exclusions to manufacture a pass.

## Python lint

The `python-lint` job in `actionlint.yml` runs `ruff check` with the pinned Ruff
from `requirements-dev.txt` on repository scripts, hooks and the legacy sidecar.
It runs on every ready PR and main push. It enforces two rules from `ruff.toml`:

- F401 rejects unused imports.
- C901 rejects functions with cyclomatic complexity above 20, the same limit
  ESLint applies to TypeScript.

Six existing functions in `scripts/agent-guard.py`,
`rust/scripts/check-macos-bundle.py` and `rust/scripts/gateway-operations.py`
are over the limit. Each carries a `# noqa: C901` on its `def` line, which
exempts only that function. Split a function to remove its exemption; do not
add new ones. `scripts/test_ruff_lint.py` proves an unused import, a complexity
of 21 and a second function next to an exempt one all fail. The job also runs
`scripts/test_operation_inventory.py`, the fixture tests for the
[operation inventory](operation-inventory.md) script.

```sh
.venv/bin/python -m pip install -r requirements-dev.txt
PATH="$PWD/.venv/bin:$PATH" just ci --only python-lint
```

## Duplicated code

The `duplication` job in `duplication.yml` caps duplicated tokens with jscpd 5.4.2.
It scans the whole Node server and React UI app directories, shared web packages,
all Rust crates including `pp-server` and `pp-desktop`, and the Python sidecar.
It reads JavaScript, TypeScript, Rust and Python. Tests count as source,
including the UI's `test/` tree.
Generated TypeScript, fixtures, snapshots, dependencies and build outputs do not.
It runs on ready PRs and main pushes that change those inputs, on merge groups,
and on manual dispatch. It does not change required-check settings.

Install the locked web dependencies, then run:

```sh
npm --prefix web ci --ignore-scripts
node --test scripts/check-duplication.test.mjs
node scripts/check-duplication.mjs
```

The scanner uses mild mode and at least 50 tokens per clone. The minimum line
count is 0, so line wrapping cannot hide a clone or add one. Before scanning,
the checker formats a temporary copy of the web sources with the pinned Prettier
version and the repository's Prettier config and ignore rules. This keeps
formatting batches from changing web token counts or clone boundaries. Rust and
Python sources are scanned as checked in.
`scripts/duplication-baseline.json` records the scan method and caps each area
separately. A method change requires an explicit baseline update, reviewed with
the changed counts. The scan runs when the Prettier config or ignore file changes.
Any increase within the same method fails and reports clone locations for files
changed since `origin/main`, or the ref in
`PP_DUPLICATION_BASE`. CI uses the PR base or the commit before a main push.
Every main push gets its own run, which a newer push never cancels.
Missing reports, source-free scans and scanner errors also fail.

Extract shared code instead of copying it. After removing duplication, run
`node scripts/check-duplication.mjs --update` and commit the lower counts.
Checking and updating reject increases when the base uses the same scan method.
A new method requires an explicit baseline update because old counts are not
comparable. Review that baseline change with the method change.
The baseline caps existing duplication; it does not mean those clones are fixed.

`just ci --only duplication` runs the same tests and scan on Node 22.23.3.
It expects the web dependencies to be installed, as they are after `web-assets`
in a full local run.

## Web JavaScript byte budgets

Web CI's existing `web-build` job runs `test:bundles` after the production build.
The check keeps Three.js out of static route imports and enforces raw and gzip
byte limits from `web/apps/web/scripts/route-budgets.json`. Run it locally with:

```sh
npm --prefix web run build
npm --prefix web run test:bundles -w @print-partner/web
```

The limits cover the public entry, authenticated shell, and each route's full
static import closure including its shared baseline. Each output file counts
once per closure. The printed route rows still show bytes added to the baseline.
Separate total and single-file limits count every JavaScript file in `dist`,
including lazy previews, workers and public scripts not listed in Vite's manifest.
Gzip totals sum each file's compressed size using Node's default gzip settings.
CSS, images, source maps and native binaries are outside this JavaScript check.

The initial budgets come from main `0c50c15d`, built with locked Vite 8.3.4 and
Node 24.21.0. Each limit allows 5% above that measurement, rounded up to the next
KiB, to absorb small build and compression differences. These limits cap growth;
they do not claim the existing bundles are small. Checks compare exact bytes,
not the rounded table values. Missing budgets, including for new routes, fail.

Remove unnecessary imports or lazy-load code when a limit fails. If a deliberate
change needs a new budget, rebuild first, then generate proposed limits:

```sh
cd web/apps/web
node scripts/route-bundles.mjs --write-budgets
```

Review the JSON diff and explain any increases in the PR. Generation replaces
all limits with the current measurements plus the same allowance. Do not run it
as part of normal checks or CI. It still rejects static Three.js imports.
The fixture tests prove failures for raw and gzip growth, lazy chunks, a
one-byte overage, missing budgets and invalid manifests without changing real
build outputs.

## Unused Rust dependencies

The `cargo-machete` job in `rust-deps-weekly.yml` now fails on unused dependencies
and tool errors. It runs on ready PRs, pushes to `main`, merge groups, manual runs
and the existing weekly schedule. Draft PRs skip the job. This replaces the
weekly report-only behavior without changing required-check settings.

Install the existing pinned version, then run its local CI step:

```sh
cargo install cargo-machete --version 0.9.2 --locked
just ci --only rust-deps
```

The step tests the scanner
against disposable crates, including an unused dependency that must fail, before
running `cargo machete rust`. It scans all Rust crates without compiling them.
The baseline has no findings. The only existing ignore is `tauri-build` in
`pp-desktop`, documented next to its metadata entry for build-script use.
Remove unused dependencies rather than adding ignores. Use an ignore only for a
verified false positive, with a comment explaining the use the scanner missed.

## The OS jobs

### Rust complexity limits

The Linux build also runs `rust/scripts/check-clippy-limits.py`. It checks the
whole workspace and all targets with `desktop_rebuild` off and on. It enables
Clippy's allow-by-default `cognitive_complexity`, `too_many_lines`, and
`match_same_arms` lints with their pinned-toolchain default thresholds.
Warn-by-default lints such as `if_same_then_else` stay with the existing
`-D warnings` passes, which already reject every finding.

`rust/clippy-limits-baseline.json` records each lint's count in each feature
mode. The checker counts a source span once when both library and test builds
report it. Dependency diagnostics do not count. Compilation errors, incomplete
Cargo JSON, and unknown or removed lints fail the check.

Counts must match the baseline. Fix new findings rather than raising it.
After removing findings, regenerate the counts with
`python3 rust/scripts/check-clippy-limits.py --update`.
Both checking and updating reject baseline increases over `origin/main`.
The first landing permits a baseline only when that file does not yet exist
on `origin/main`. A missing base ref is an error.

CI compares a main push with the commit it replaced. Other events compare
with main. `just ci --only clippy-limits` fetches main without making the
local clone shallow, runs the checker's negative controls, and checks the
workspace. Run it on Linux after building the desktop assets and manifest.
The extra Clippy passes reuse the build cache but still visit both feature
modes. These counts cap existing findings; they do not mean the Rust
complexity backlog is fixed.

The workflow has one job per OS. Each job uses the toolchain pinned in `rust-toolchain.toml`.

| Check name | Runner | Packages | Tests |
|------------|--------|----------|-------|
| `rust (linux)` | Aggregate over `rust (linux) build` plus rebuild partitions when selected | The whole workspace | All |
| `rust (macos)` | `macos-26` | The whole workspace | All; see [macOS test filter](#macos-test-filter) |
| `rust (windows)` | `windows-latest` | The whole workspace | All |

`rust (linux) build`, its rebuild partitions and the `rust (linux)` aggregate run on the pp-local fleet or `ubuntu-latest` for non-PR events; on pull requests only the build and the aggregate run (the rebuild partitions do not), on `blacksmith-4vcpu-ubuntu-2404` (default), `ubuntu-latest` (`hosted`) or pp-iso ([the switch](#pull-request-runner-switch)); see [Linux Rust runner](#linux-rust-runner).

Build caching and linking differ by OS:

- Hosted jobs restore `Swatinem/rust-cache`; only `main` saves it. pp-local Linux jobs skip it and use the container's sccache instead.
- Linux installs `mold` and links with it through `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS=-C link-arg=-fuse-ld=mold`. macOS and Windows keep their default linkers.
- On GitHub-hosted Linux runners only, the first step after the input check deletes preinstalled toolchains the job never uses: `/usr/share/dotnet`, `/usr/local/lib/android`, `/opt/ghc`, `/usr/local/.ghcup` and CodeQL in the tool cache. It prints `df -h /` before and after. A PR builds the `desktop_rebuild` off and on test trees in one target directory, and on 2026-11-04 hosted runners ran out of disk three times in `nextest (desktop_rebuild on)`. Free space differs between hosted runners: on PR #1478 the step went from 88 GB to 110 GB free in under two minutes, and 85 GB was still free after `nextest (desktop_rebuild on)`. The same runners print `df -h /` after each nextest step, including after a failed one, so the next disk failure shows how much space was left. pp-local, macOS and Windows skip these steps.
- All jobs set `CARGO_PROFILE_DEV_DEBUG` and `CARGO_PROFILE_TEST_DEBUG` to `line-tables-only`. Panic locations keep their file and line, and the build output is smaller.

Each job runs these steps in order:

1. Install the toolchain, nextest 0.9.148, Node 22.23.3 for the storage fixtures, and Node 24.21.0.
2. Build the desktop web assets and write `rust/bundle-manifest.json`. `pp-compat` embeds this file at compile time.
3. On macOS only, mount a case-sensitive APFS volume at `/Volumes/pptmp` and set `TMPDIR` to `/Volumes/pptmp/tmp`.
4. `cargo fmt --check`.
5. `cargo clippy -D warnings`, with `desktop_rebuild` off.
6. `cargo clippy -D warnings`, with `desktop_rebuild` on.
7. `cargo nextest run`, with `desktop_rebuild` off.
8. `cargo nextest run`, with `desktop_rebuild` on.
9. `cargo test --doc`, with `desktop_rebuild` off. nextest does not run doctests.
10. `cargo test --doc`, with `desktop_rebuild` on.

Linux and Windows PRs run steps 6, 8 and 10; macOS PRs skip them.

On `push` to `main`, `merge_group`, and `workflow_dispatch`, Linux replaces the in-job step 8 with a nextest archive built once (`--zstd-level 11`), then three `rust (linux) rebuild m/3` jobs that run `cargo nextest run --archive-file … --workspace-remap "$GITHUB_WORKSPACE/rust" --partition slice:m/3` on the build job's runner (pp-local for `main` pushes and manual runs unless `PP_RUST_RUNNER=hosted` or the fleet fallback applies, otherwise `ubuntu-latest`; merge groups always use `ubuntu-latest`). The build job also uploads a zstd-compressed pack of the measured desktop trees (`rust/bundle-manifest.json` and the web `dist` outputs). Artifact uploads set `compression-level: 0` so GitHub does not re-gzip already-compressed archives. Atomic package builds publish `dist/current` as a symlink to `dist/releases/<id>`; the dependency inventory records that link and the files under the release path. The pack step therefore archives each package's full `dist/` tree **without** `tar -h`, asserts the `current` symlink and a release-path file (for example `desktop-resolution.js`) are in the archive, and each partition restores those trees so the symlink topology still matches. Packing only `dist/current` with `-h` flattens the link and fails dependency inventory checks. Each partition runs `npm ci` only — it must not rebuild web assets or regenerate the manifest, because the archived binaries embed those digests at compile time. The required check `rust (linux)` is an aggregate that needs the Linux build and those partitions: on push / merge_group / workflow_dispatch it requires rebuild `success`; on PRs and schedule rebuild may be `skipped`. The schedule keeps the monolithic in-job step 8.

The macOS volume exists because the default macOS `TMPDIR` breaks three kinds of test. Its paths are too long for Unix socket names, it is under the `/var` symbolic link that no-follow opens refuse, and it treats `Straße.stl` and `STRASSE.stl` as one name.

Steps 4 to 10 run even when an earlier one of them fails, so one run reports every failure. Clippy uses `--keep-going`, so it reports errors in every crate whose dependencies compile.

Each job declares its runner in `runs-on`, its package scope in `CARGO_SCOPE`, and its test filter in `NEXTEST_FILTER` (from the `changes` job output, defaulting to `all()`). The three jobs share the same anchored steps. They have no matrix, so GitHub reports their exact required names both when running and when skipped. The Linux rebuild partitions always use `all()` because they only run on non-PR events.

`rust (linux)` is a required check on `main`; `rust (windows)` and `rust (macos)` are advisory (the owner removed `rust (windows)` from the required checks on 2026-11-08). A PR that the [Windows tier](#windows-tier) does not select reports `rust (windows)` as skipped. [Settings for Chad](ci-controls.md#settings-for-chad) lists the full ruleset.

## Run the required checks with one command

A sensitive PR, as the [merge gate](definition-of-done.md#merge-gate) defines it, runs the full set before each push:

```sh
PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci
```

`just ci` passes its arguments to `python3 scripts/local-ci.py`, which holds the only list of steps. Without [just](https://github.com/casey/just), run `python3 scripts/local-ci.py` from the repository root with the same arguments. Agents run it, or `just ci --fast` for a normal PR, before they push, as [Gate evidence](../contributing/gate-evidence.md#run-ci-locally-before-you-push) says.

It runs the steps below in the order that `just ci --list` prints and continues after ordinary step failures. An artifact-freshness failure is an exception: it stops the run before nextest, and later steps are not run or listed. The summary prints `PASS`, `FAIL` or `SKIP` for each completed or skipped step and exits 1 if any step failed. To see the step ids, use `just ci --list`. To select steps, use `just ci --only fmt,clippy-off` or `just ci --skip nextest-on`.

Runtime nextest needs matching web builds and `rust/bundle-manifest.json`. Selecting
`nextest-off`, `nextest-on`, or `--fast --only nextest-affected` also runs `web-assets`
first, even with `--only`. This applies to `pp-check iter` too: it calls this script.
Before each nextest step, a local check compares the manifest with a temporary
measurement from the existing release generator. Commit metadata is ignored: a new
Git commit alone does not require a rebuild. Runtime Node identity is checked;
`PP_TEST_NODE` must match the Node used by `web-assets`. A mismatch stops the run before
nextest, with one message naming `just ci --only web-assets` (Node 24.21.0).
An explicit `--skip web-assets` avoids the build but still checks freshness; stale
assets fail rather than launching tests. This matters after `node-fixtures`, which
reinstalls dependencies for Node 22 and can rebuild measured server files. The
freshness check measures built files and dependencies; the automatic build also
covers source edits that have not been compiled. Direct `cargo nextest` commands
do not run this preparation or check.

Normal PRs run the fast set instead, and sensitive PRs, as the [merge gate](definition-of-done.md#merge-gate) defines them, run the full `just ci`:

```sh
PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast
```

`--fast` runs every step except `nextest-off`, `nextest-on`, `doc-off` and `doc-on`. In place of `nextest-on` it runs `nextest-affected`: the same `nextest` command with `desktop_rebuild` on, filtered to the crates this branch changes and their reverse dependents. The filter comes from the rules in `scripts/ci-affected-nextest-filter.py`, applied to the diff from `git merge-base origin/main HEAD` to the working tree plus untracked files, so uncommitted edits count. Shared inputs such as `rust/Cargo.lock`, `web/`, `fixtures/` and the filter script force `all()`, as in CI, and so does a branch that changes no crate. Without `origin/main` or a merge base, or if the filter script fails, the filter is `all()`. Fetch `origin` first so the merge base is current. `just ci --fast --list` prints the fast step ids, and `--only` and `--skip` take them.

If `sccache` is on `PATH`, or at `/opt/agent-fleet/bin/sccache`, both the full and the fast run set `RUSTC_WRAPPER` to it and `CARGO_INCREMENTAL=0`, unless `RUSTC_WRAPPER` is already in the environment. Set it empty (`RUSTC_WRAPPER= just ci`) to build without a wrapper. The command prints the wrapper it sets. Without sccache, it sets neither.

| Step | What it runs | CI source |
|------|--------------|-----------|
| `local-ci-self-test` | `scripts/test_local_ci.py` | None. This test fails when a copied command no longer matches its workflow step. |
| `selector-tests` | The path-filter and macOS-alert unit tests | `rust-cross-os.yml` `changes` |
| `perf-baseline-tests` | The unit tests of the Node-versus-Rust baseline harness in `scripts/perf-baseline` | None yet. A local-only line; see [Performance](performance.md). |
| `actionlint` | `actionlint -color` | `actionlint` (required) |
| `agent-guidance` | `test_agent_guidance.py` and `check-agent-guidance.py`: local links and anchors in every tracked Markdown page, and the guidance title and skill rules | `agent-guidance` in `actionlint.yml` |
| `zizmor` | `zizmor --min-severity informational --config .github/zizmor.yml .` | `zizmor` (required) |
| `ast-grep` | `ast-grep test`, `test_ast_grep.py` and `check-ast-grep.py` against `origin/main` | `ast-grep ratchet` |
| `boundaries` | `check-boundaries.py --self-test` and `check-domain-deps.py --self-test` | `chain-tip-gate.yml` |
| `historical-schema` | `rust/scripts/fetch-historical-schema-commits.sh`, which fetches the pinned commits that the pp-storage tests archive. It runs before nextest. | `rust (linux)` (required) |
| `fmt` | Step 4 of the OS jobs, with the Linux `--workspace` scope | `rust (linux)` (required) |
| `rust-deps` | Scanner negative controls and `cargo machete rust`, pinned to 0.9.2 | `cargo-machete` in `rust-deps-weekly.yml` |
| `web-assets` | Desktop web build and `rust/bundle-manifest.json` | `rust (linux)` (required) |
| `ui-package` | `typecheck`, `lint`, `test`, `build` and `test:browser` for the replacement UI workspace `@print-partner/ui`, on Node 24.21.0. Vitest fails the step when it finds no tests. `test:browser` exits 77 for a missing browser engine, which the step reports as SKIP. | None yet. These are local-only lines; see [Replacement UI package](#replacement-ui-package). |
| `workaround-ratchet` | `scripts/workaround-ratchet.mjs` and its tests, which cap `TODO`, `eslint-disable` and Rust `#[allow]` markers, on Node 22.23.3. It runs after `web-assets`, which installs its parser. | `web` job of `web-ci.yml` |
| `duplication` | jscpd negative controls and the five-area duplicated-token baseline, on Node 22.23.3 | `duplication.yml` |
| `clippy-off`, `clippy-on`, `nextest-off`, `nextest-on`, `doc-off`, `doc-on` | Steps 5 to 10 of the OS jobs, with the Linux `--workspace` scope | `rust (linux)` (required) |
| `clippy-limits` | Clippy complexity negative controls and baseline check, with both feature modes on Linux | `rust (linux)` (required) |
| `node-fixtures` | The `--check` mode of each Node parity fixture generator, and the HTTP route inventory diff, on Node 22.23.3 | `desktop-contracts.yml` and `auth-admission-freshness.yml` |

The `ast-grep` step counts `cfg(..desktop_rebuild..)` and `cfg_attr`
attributes across every crate, including tests. It recursively inspects
conditional attributes applied by `cfg_attr`, so
`#[cfg_attr(all(), cfg(feature = "desktop_rebuild"))]` counts too. Each outer
attachment counts once, even if it contains several matching predicates.
`rust/desktop-rebuild-baseline/<crate>.json` records three classes per crate: `module` (mod/use),
`item` (including variants and fields), and `statement` (statements,
expressions, match arms and ambiguous attachments). Stacked attributes,
comments, raw attribute names such as `r#cfg`, and escaped feature strings do
not change attachment.

A `use` or nested ordinary item inside a function body still counts as module
or item. Item-level macro invocations, extern crate declarations and inner
impl/trait/extern attributes stay in the frozen statement class. These forms
were included in the original statement cap; an ambiguous attachment defaults
to statement. Gates inside macro token trees (`json!`, `vec!`, `select!` and
`macro_rules!`, for example) also count as statements.

An independent text scan and the AST rules check each other: a text gate must
have exactly one AST class (or the macro statement fallback), and an AST hit
without a text gate fails coverage. The text scan ignores comments and literal
contents. It inspects `cfg_attr` predicates and recursively applied `cfg` or
`cfg_attr` attributes; `doc = "desktop_rebuild"` does not count. Inner
crate/module gates count as module gates; inner block gates count as statements.

Counts must match the baseline; update it when gates move or disappear. Only
module and item counts may grow. Statement counts may not rise per crate over
`origin/main`, even with `PP_AST_GREP_ALLOW_BASELINE_RAISE=1`. The first landing
establishes the placement baseline and reports the skipped raise check. This
step runs in both `just ci --fast` and the full `just ci`.

Regenerate the placement files with
`python3 rust/scripts/check-ast-grep.py --write-placement-baseline`. The command
writes one map entry per crate, removes stale crate files and the old shared
file, then runs the checks. It cannot authorize a statement-count increase.
Identical files keep identical contents, so updates in different crates merge
without editing a shared inventory or manifest.

Each statement cap comes from the same crate's file on `origin/main`. During
the layout migration, when main has no per-crate baseline files yet, the checker
uses that crate's entry in main's shared `desktop-rebuild-baseline.json`.
Once main uses per-crate files, a missing crate file means a zero cap; it never
borrows an allowance from a leftover shared file. A missing or extra local
crate file fails the exact inventory check, including zero-count crates.

`cfg!(feature = "desktop_rebuild")` expressions are excluded from these counts
(17 uses at the review baseline). Child [GRE-1397](https://linear.app/greetz/issue/GRE-1397/count-desktop-rebuild-cfg-expressions-in-the-placement-ratchet)
tracks counting them later.

Except where noted below, each step runs the command lines of its workflow step without changes. The script sets the same environment that CI sets: `NEXTEST_FILTER`, `PP_TEST_NODE`, `VITE_PRINT_PARTNER_DESKTOP`, and `RUNNER_TEMP`. The cargo steps also get `INSTA_UPDATE=no`, so a snapshot mismatch fails the step instead of writing a `.snap.new` file. `RUNNER_TEMP` is under `$CARGO_TARGET_DIR/local-ci`, or under `rust/target/local-ci` if `CARGO_TARGET_DIR` is not set. The local command differs from CI in these ways:

- It does not install tools. If a tool is not on `PATH`, or if its version is not the version that CI pins, the step is reported as `SKIP` with the reason. The pinned versions are Node 24.21.0, ast-grep 0.50.0, actionlint 1.7.12 and zizmor 1.31.1. The `workaround-ratchet` and `node-fixtures` steps are also skipped when `PP_SCHEMA_PARITY_NODE` does not point to Node 22.23.3.
- `ast-grep` fetches `origin/main` without `--depth=1`, because a shallow fetch would make a full clone shallow.
- It does not set the CI-only `CARGO_PROFILE_*_DEBUG` and mold `RUSTFLAGS` values. They change debug info and the linker, not test results, and setting them would invalidate your local build cache.
- `nextest-on` always runs. Use `--skip nextest-on` for a shorter run.
- The nextest profile is `default` unless you set `NEXTEST_PROFILE`. CI uses `default`.
- `node-fixtures` runs `npm ci` with Node 22, so `web/node_modules` ends with Node 22 native modules. The next run's `web-assets` step installs them again with Node 24.

The command does not cover these checks:

- `rust (windows)` and `rust (macos)`. They need Windows and macOS runners. The command reports both as `SKIP`.
- `Lint Evidence block`. It reads the pull request body. The command reports it as `SKIP`.
- `cargo deny check` (`desktop-deny.yml`). It runs on changes to `rust/**`. To run it locally, use `cargo deny --manifest-path rust/Cargo.toml --all-features check`. CI also runs `cargo deny --manifest-path rust/fuzz/Cargo.toml --all-features --locked check`.
- The Linux rebuild partitions, the Desktop runtime, Native macOS bundle and Docker workflows, the `web-ci.yml` steps other than the workaround ratchet, and the `contracts:verify` and `test:desktop-parity` steps of `desktop-contracts.yml`.

## Maintainability report

`just report` prints the measures of the maintainability plan (GRE-1306) as a Markdown table: crate and file sizes, the pp-storage writer seam, `desktop_rebuild` gates, ticket-named tests and docs, cross-crate `#[path]` includes, and the other rows of the plan's Evidence and Measures tables. It is a report, not a check. It exits 0 whatever the numbers are, `just ci` does not run it, and no workflow runs it. Quote its output in a PR or a gate comment when a measure moves.

```sh
just report                        # origin/main
just report --rev 044e7706d        # the plan's pinned commit
just report --ci 1626 1622         # add workflow wall times and push-to-green for PRs
```

`just report` runs `python3 -I scripts/maintainability-report.py` with your arguments. It reads every file of the revision through `git cat-file`, never the working tree, so uncommitted edits do not move a number. Each history row (commits on `lib.rs` in 30 days, variant headers added in 7 days, `lib.rs` lines per family-touching commit) uses `git log --since/--until` ending at that revision's commit date, so a pinned revision gives the same numbers on any day. Fetch `origin` first, because the default revision is `origin/main`.

`--base REV` compares two revisions. The report measures `--base` and `--rev` (default `HEAD` when `--base` is given) and prints only the rows whose value differs, as `| Measure | Base | Head | Change |`. When both cells hold numbers in the same text, Change gives each number's signed difference, such as `+201 / +161 / +2`; otherwise it says `values differ`, and the Base and Head columns show both. When no row differs it prints `No measure changed.` instead of a table. The history rows of both revisions end at the head's commit date, so they count only the commits the head adds. Paste the output under Evidence: a PR must not make a measure worse unless its ticket says so. `--base` cannot be combined with `--ci` or PR numbers, which sample live CI rather than a revision.

```sh
git fetch origin
just report --rev HEAD --base origin/main
```

A branch that adds one page under `docs/` prints:

```text
| Measure | Base | Head | Change |
| --- | --- | --- | --- |
| Tracked Markdown pages / docs pages / docs named after tickets outside docs/records | 261 / 150 / 54 | 262 / 151 / 54 | +1 / +1 / +0 |
```

Every row compares the two whole trees. When `main` has moved past the branch's fork point, merge `origin/main` into the branch first, or pass the merge base (`--base "$(git merge-base origin/main HEAD)"`), so main's own changes do not show as the branch's.

Two parts need `gh` and change with every CI run, so each prints a "sample taken" row with the UTC time:

- `--ci` samples the wall times of the successful runs among the last 10 completed `rust-cross-os.yml` and `web-ci.yml` runs, for pull requests and for `main`. A run under 6 minutes is excluded as too short to have run a job's suites.
- PR numbers print one row per PR head: its changed paths, whether the diff touches only `web/apps/ui`, the minutes from the first check run's start until every required check was green, the span of all completed check runs, and any check not green. The required set defaults to `actionlint`, `zizmor`, `rust (linux)`, `Lint Evidence block` and `review-gate`. `--required a,b,c` replaces it, for example once `ui-package` becomes required.

## Replacement UI package

`web/apps/ui` (`@print-partner/ui`) is checked by the local `ui-package` step:

```sh
npm --prefix web run typecheck --workspace @print-partner/ui
npm --prefix web run lint --workspace @print-partner/ui
npm --prefix web run test --workspace @print-partner/ui
npm --prefix web run build --workspace @print-partner/ui
npm --prefix web run test:browser --workspace @print-partner/ui
```

`test:browser` serves the build on a loopback port and opens `/type` in a fresh
profile in Chromium (through `playwright-core`) and in the system WebKitGTK
(through PyGObject, under `xvfb-run` when there is no display). It fails on any
request to another origin, on a bundled face that did not load, and when the
figure cells are not Plex Mono with tabular figures. `frame.browser.mjs` then
runs in WebKitGTK: axe at WCAG 2.2 AA on every frame page, screen state and
dialog in all three themes, and a sideways-scroll check at 200% text and 400%
zoom. It serves the Builds routes from a three-Build fake core
(`test/browser/coreFixture.mjs`), so the Builds list and a Build's Overview are
audited with data. Set `PP_UI_EVIDENCE_DIR` to save its screenshots.
`builds.browser.mjs` serves 10,000 Builds and, in WebKitGTK and Chromium, records
the time to the first and last row and from each Down or End key to the next
frame. It fails when a row is missing, next actions never load, or focus lands
on the wrong row; the
timings are recorded, not judged. Under `xvfb-run` the harness reads the
script's stderr on its own descriptor, because `xvfb-run` merges stderr into
stdout. A missing engine prints `SKIP <engine>`. `test/browser/all.mjs` runs
every check even when one skips or fails, and exits with the worst status: a
failure, then 77, then 0. The step declares 77 as its
`skip_status`, so the summary shows `SKIP ui-package` and `--fail-on-skip`
fails.

Hosted Web CI covers part of it already. Its `npm run lint` runs `eslint .` over
all of `web/`, which includes the package and its import-boundary rule, and
`npm run lint:dead` runs knip over every workspace. The root `typecheck`, `test`
and `build` scripts list workspaces by name, so they do not run this package.
Running its typecheck, tests and build in hosted CI needs a `web-ci.yml` change
by the workflow owner.

The desktop does not ship `apps/ui`, so `rust/bundle-manifest.json` does not
measure it. You can run these commands after building the manifest; Vitest's
cache in `apps/ui/node_modules/.vite` and `apps/ui/dist` leave the Rust launch
tests passing.

`THIRD_PARTY_NOTICES.md` lists the direct runtime dependencies of every
workspace in `web/package.json`, including this one. After changing a
workspace's `dependencies`, run `npm --prefix web run docs:licenses`. Web CI runs
the same script with `--check`. `fixtures/admin/node-oracle.json` records the
notice's length and SHA-256 for `/legal/third-party`, so a changed table also
needs the oracle update in [fixtures/admin/README.md](../../fixtures/admin/README.md).

## Run a job locally

To run the same checks on your machine, build the assets and manifest first. From the repository root:

```sh
npm --prefix web ci
VITE_PRINT_PARTNER_DESKTOP=1 npm --prefix web run build
python3 rust/scripts/build-desktop-manifest.py --web web --node "$(command -v node)" --output rust/bundle-manifest.json --commit "$(git rev-parse HEAD)"
```

Then run the checks:

```sh
cargo fmt --manifest-path rust/Cargo.toml --all --check
cargo clippy --manifest-path rust/Cargo.toml --locked --workspace --all-targets --keep-going -- -D warnings
cargo clippy --manifest-path rust/Cargo.toml --locked --workspace --all-targets --keep-going --features desktop_rebuild -- -D warnings
cargo nextest run --manifest-path rust/Cargo.toml --locked --workspace --no-fail-fast
cargo nextest run --manifest-path rust/Cargo.toml --locked --workspace --no-fail-fast --features desktop_rebuild
cargo test --manifest-path rust/Cargo.toml --locked --workspace --no-fail-fast --doc
cargo test --manifest-path rust/Cargo.toml --locked --workspace --no-fail-fast --doc --features desktop_rebuild
```

On macOS, set `TMPDIR` to a short directory on a case-sensitive volume before you run the tests. The workflow step `Mount a short case-sensitive TMPDIR` has the commands. Also set `PP_TEST_NODE` to the Node executable, as CI does. Without it the runtime tests fall back to `/usr/bin/node`, which macOS does not have. To skip the same tests as CI, add `-E` with the `NEXTEST_FILTER` value of the macOS job. On Windows, the same `--workspace` commands apply. On Linux, the Tauri shell also needs the GTK and WebKit packages that the workflow installs with `apt-get`.

Clippy and fmt do not read the manifest contents. For a lint-only run, `echo '{}' > rust/bundle-manifest.json` is enough. The tests need the real manifest.

## Packaged-app smoke

`desktop-packaged-smoke.yml` builds the Linux `.deb` and the macOS app, installs each into a disposable location and launches the installed app on a schema 31 database, in the default build and in the `desktop_rebuild` build. It checks the native window, the persisted Build in the accessibility tree, a clean shutdown and the migrated rows. It is not a required check, and `just ci` does not run it. [Packaged-app smoke](packaged-smoke.md) explains each step and how to run it locally.

## Read a failure

1. Open the failed check on the pull request, or run `gh pr checks <number>`.
2. Find the first failed step. The step name says which tool and which feature setting failed.
3. Run `gh run view <run-id> --log-failed` to print only the failed steps.

These failures are common:

- `error[E0433]: cannot find unix in os`, or `cannot find function ... in crate libc`, on Windows only. Code uses a Unix API outside a platform module. Move it into one, as [Keep platform code in cfg modules](#keep-platform-code-in-cfg-modules) describes.
- `unused variable` or `function is never used` on one OS only. A value is used only inside a `#[cfg]` block. Compute the value inside that block, or gate the function with the same `#[cfg]`.
- A test that fails on one OS only. Run that test locally on the same OS with `cargo nextest run -E 'test(<name>)'`.

## Test groups and flaky tests

`rust/.config/nextest.toml` sets both rules below in the `default` profile, which CI and `just ci` use.

Tests that start real Node children run in the `process-heavy` test group, and at most two of them run at once. The group holds every `pp-core` `runtime_m0` test, the `gateway_mount` binary in `pp-core` and the `server_binary_routes` binary in `pp-server` (both formerly `slice0_mount`), `pp-server`'s `stop_signal` test and the `pp-compat` `ticket_t_2::` supervisor tests. On a busy runner, four of these at once timed out (GRE-956). Other tests keep running on the remaining threads. To see which tests a build puts in the group, run `cargo nextest show-config test-groups --manifest-path rust/Cargo.toml`.

A test that fails at random goes on the flaky list, the override with `retries = 1`. The rules:

1. Add a test to the list only with an open Linear ticket that names it and an owner. Put the ticket id in a comment next to the filter.
2. nextest runs a failed test on the list once more. A pass on the second try passes the job, and the log shows the test as `FLAKY`.
3. If a test needs that retry in two runs within seven days, the owner quarantines it. In one pull request they remove the test from the flaky list and add it to `default-filter` under `[profile.default]`, as `default-filter = 'not (binary(=<bin>) & test(=<name>))'` with the ticket id in a comment. nextest applies `default-filter` on top of the `-E` filter that CI passes, so the test no longer runs anywhere that uses the `default` profile. The comment in `nextest.toml` shows both forms. To count retries, run this from the repository root. It prints, for each test, the number of `Rust cross-OS` runs in the last seven days that logged it as `FLAKY`:

   ```sh
   since=$(date -u -d '7 days ago' +%F)
   gh run list -R Print-Partner/PrintPartner -w "Rust cross-OS" --created ">=$since" -L 500 --json databaseId --jq '.[].databaseId' |
     while read -r id; do
       gh run view "$id" -R Print-Partner/PrintPartner --log 2>/dev/null |
         sed -E 's/(\x1b|\^\[)\[[0-9;]*m//g' | grep -oE 'FLAKY [0-9]+/[0-9]+ \[ *[0-9.]+s\] .*' |
         sed -E 's/^FLAKY [^]]*\] +//' | sort -u
     done | sort | uniq -c | sort -rn
   ```

   A count of 2 or more means quarantine, unless rule 4 applies.
4. Never quarantine a test of a core guarantee: no orphan processes and no data loss. It stays required. Fix it or make its wait longer.
5. When the ticket is fixed, remove the test from the list in the same pull request.

The list is empty while no flaky ticket is open.

### Taking storage back after an HTTP test

A test that serves a router built from a shared `Arc<Mutex<WriterOwner>>` must not call `Arc::into_inner` or `Arc::try_unwrap` on the owner after `axum::serve`'s graceful shutdown alone (GRE-1195). In axum 0.8.9, `serve` returns once each connection task drops its shutdown receiver, but the task still holds its clone of the router, and the router holds the owner, until the task ends. The result is a `routers dropped` panic in about 1 of 10 hosted runs. Serve with `pp_api::serving::Serving` (or `pp_api::fixtures::Served`) and call `stop`: it returns only after every connection has dropped its router clone, so taking the owner back needs no retry or sleep.

## Keep platform code in cfg modules

Put code that differs by OS in one module for each platform, and select the module once:

```rust
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
```

Shared code calls `platform::lock_file(...)` and never names an OS. Every platform module defines the same functions. When one module lacks a function, the job for that OS fails to compile.

Do not scatter `#[cfg(...)]` on statements or `if cfg!(...)` checks through shared functions. Tests are the exception: a test, or one block in a test that checks an OS-only fact, may carry its own `#[cfg]`.

## Windows coverage

`rust (windows)` lints and tests the whole workspace (`CARGO_SCOPE: --workspace`) with `desktop_rebuild` off and on. See [Windows test portability](#windows-test-portability) for what the pp-storage port (GRE-1154) changed. On Windows, `pp-compat` opens log entries through `pp-os` (GRE-1185) and runs its supervision tests against the Job Object and the private named pipe (GRE-1186); each Unix-only process-group, signal or parent-death test names its Windows counterpart in a comment. Its `ticket_t_2::` supervisor tests run on Windows with Node fixtures (GRE-1194). A clean leader exit waits up to 500 ms for the job to empty before the reap (see the pp-compat README). The HealthFixture tests stay Unix-only under GRE-1203. So do the paused-time tests whose pipe stdio or Unix health listener blocks Tokio's auto-advance. The other paused-time tests run on Windows. The junction-gated respawn test stays Unix-only under GRE-1204. `pp-core` reads and publishes its state files through `pp-os` handles (GRE-1188) and runs its runtime tests against the real Node child on Windows. On Windows its tests use `TerminateProcess` for SIGKILL, `NtSuspendProcess` and `NtResumeProcess` for SIGSTOP and SIGCONT, a junction where Unix uses a FIFO, an `icacls` deny where Unix uses mode 000, and a ToolHelp process snapshot where Unix reads `/proc` or `ps`. A runtime test whose restart fails prints the redacted recent logs. Each test that stays Unix-only names its Windows counterpart in a comment. `pp-gateway`'s tests reach a stand-in Node child through `pp_compat::Endpoint::fake`, which is a Unix socket on Unix and the core-owned named pipe on Windows; on Windows they plant symbolic links with `symlink_file`, the receipt test plants no socket or FIFO because Windows directories cannot hold them, and the 0600 mode checks stay Unix-only. `pp-server` stops on Ctrl+C, Ctrl+Break, a console close, logoff or system shutdown on Windows, where Unix uses SIGINT and SIGTERM (GRE-655); `tests/stop_signal.rs` sends Ctrl+Break on Windows and SIGTERM on Unix. `pp-api`'s `auth_admission_parity` serves its router through a harness loop that reads and discards unread request bytes for up to two seconds before it closes a connection, so a Windows client still receives the early 413 for an over-limit body instead of a reset. The shipping gateway closes without that linger; GRE-1264 tracks the product fix. `pp-desktop` builds the Tauri shell on Windows (GRE-656): dialogs come from rfd, links and folders open through `rundll32 url.dll,FileProtocolHandler`, Ctrl+C, Ctrl+Break and a console close stop it, and single-instance focus uses a private named pipe (see the pp-desktop README). The Windows job builds and tests `pp-os`, including
renaming and removing a directory while a `Dir` handle on it is open, entry
operations through a handle after its directory was renamed, and Windows-only
tests for symbolic links, junctions, directory locks, write-through renames,
device-name refusal and the FAT fallbacks.

### Windows test portability

These rules came out of the pp-storage Windows port (GRE-1154). New tests follow them too:

- Locks from `pp_os::try_lock` and `Dir::lock` cover one byte at offset `i64::MAX` on Windows, so other handles can read and copy a held lease file. A whole-file lock (`std::fs::File::lock`, `fs4`) still conflicts with them.
- `.gitattributes` checks out `rust/**`, `fixtures/**` and `web/**` with LF endings on every OS. Schema SQL, JSON fixtures, generator output and the web source hashes that parity fixtures record are compared byte for byte, and a CRLF checkout made them differ.
- Build SQLite `file:` URIs with `pp_storage::sqlite_uri::immutable_uri`. SQLite on Windows cannot open a URI that holds a verbatim `\\?\` prefix.
- Hand Node a path from `pp_testkit::node_path::node_path`, and an `--import` or `import()` target as a `file:` URL (`pp_testkit::node_path::file_url`, or `pathToFileURL` in scripts). Node cannot resolve verbatim paths, and its ESM loader refuses a bare `C:\` path.
- Close every connection and file under a directory before removing it. Windows refuses to delete an open file.
- Store, return or compare a path in its display spelling, `pp_os::display_path`, which turns the verbatim `\\?\C:\…` and `\\?\UNC\…` that `canonicalize` returns into `C:\…` and `\\server\share\…` and leaves every other path unchanged. pp-storage writes a Source's `local_path` and reports Source roots (accepted-plan snapshot roots and draft-preparation roots) that way, and pp-storage and pp-core's review observations compare a stored path with the repos root that way, as Node does. It still opens files through the canonical path.
- Join a relative path that holds `/` onto a canonical root through its components (`relative.components().collect::<PathBuf>()`). A verbatim `\\?\` path reads `/` as part of a name, not as a separator.
- Compare paths in the spelling the product reports. `std::env::temp_dir()` on a runner can hold 8.3 short names (`RUNNER~1`), while `canonicalize` returns the long verbatim form.

## macOS test filter

On non-PR events the macOS job uses `NEXTEST_FILTER: all()`. On PRs it uses the same affected-crate `rdeps` filter as Linux and Windows. It still runs any selected `pp-core` `runtime_m0`, `pp-compat` `ticket_t_2::`, and `origin_tests::ticket_t_12_missing_config_persists_private_port_and_reuses_it` binaries that match the filter. **Native macOS bundle** still runs its own selected lifecycle tests.

GRE-645 removed the earlier group skip. Hosted run 38925272452 mounted `/Volumes/pptmp` with `apfs, local, nodev, nosuid, journaled, nobrowse` and recorded `exec /usr/bin/node` exiting 126: `/bin/sh: line 0: exec: /usr/bin/node: cannot execute: No such file or directory`. Those scripts now `exec "$NODE"` from `PP_TEST_NODE`. Run 38941881212, with `--test-threads 1`, still failed 3 of 74 because one release verification hashed about 42,203 dependency files in 10.7 to 19.7 seconds. Each start verifies once, on a blocking thread, before the 32-second readiness budget (GRE-661). Each restart verifies again after `Starting` is published. The crash-guard status fuse covers that hash plus the 30-second probe. The measured-release parent-death test starts its 30-second acknowledgement when the helper publishes `release-verified`, not when the helper process is spawned. The config test's 3-second deadline starts when the re-executed child enters. The GRE-671 backoff test never ran Node; it runs with the rest of `ticket_t_2::`.

To collect twenty consecutive green hosted `rust (macos)` job links after tip is green, use `scripts/gre645_macos_soak.sh`. Default mode is dry-run. `--execute` is Chad-gated: it dispatches `rust-cross-os.yml` on the chosen ref, waits for each `rust (macos)` job, appends JSONL evidence (`run`, `job`, `sha`), and stops on the first red. It does not change packing or skip lists. Prove with `python3 -B -m unittest discover -s scripts -p test_gre645_macos_soak.py`.

## Other Rust workflows

- **Desktop Rust (hosted)** (`desktop-fleet-rust.yml`) runs only the ast-grep ratchet, in the `ast-grep ratchet` job. It runs on pull requests and pushes to `main` that change `rust/**`. Every push to `main` gets its own run, so a merge of two individually green pull requests is still checked. Like the duplication check, it does not cancel superseded runs: a cancelled run would skip the ratchet for that push's diff, and the job is short. On a push, the ratchet compares against the `main` commit that push replaced. It does not build or test Rust; `rust (linux)` does that once per push.
- **Native macOS bundle** (`desktop-native-macos.yml`) builds the unsigned macOS app and runs the macOS storage tests. Its `changes` job selects them by event and changed paths:
  - On pull requests, `linux-precheck`, `server-smoke`, the arm64 `bundle` and `macos-storage-gate` run only when the PR has the exact `macos` label, is not a draft, and changes desktop inputs. The selector includes the Rust inputs above, the native workflow, its cleanup tests and the Rust verification helper directory. Fork PRs skip `macos-storage-gate`. The Intel bundle never runs on pull requests.
  - Pushes to `main` or `master`, merge groups, nightly and manual runs select every job, including the Intel bundle.

  Every macOS job also needs a successful `linux-precheck`, so a broken web build never starts a macOS runner. Skipped jobs still report their checks. A new push, label change or draft change on a pull request cancels that pull request's older run. A push to `main` never cancels a running `main` run: pushes to `main` share one group (one running, one pending, a newer push replaces only a pending run), while merge groups, nightly and manual runs each have their own group, and a rerun keeps its run's group: a rerun of a push, merge group, schedule or manual run shares that run's group without cancelling anything, and a rerun of a pull request run stays in the PR group.
  The source fixture registers the named `cleanup_source_fixture` function with `trap cleanup_source_fixture EXIT` after a successful attach. It captures `$?` immediately and exits with that status on every cleanup path. Cleanup targets only this step's owned mount. It tries normal detach of the mount three times, with `sleep 2` between failures. If all three fail, it records holders and then forces detach of the whole-disk device that `hdiutil attach` printed first, again up to three times with `sleep 2` between failures. A forced unmount can succeed while ejecting the backing device returns `Resource busy`. The mount path is then an ordinary directory, so the forced retries target the device. The explicit sleeps total at most eight seconds. The `hdiutil` calls and holder diagnostics add time, so eight seconds is not a wall-clock cleanup bound.

  After normal detach retries fail, `lsof +f -- "$fixture_mount"` records open files on the source fixture's owned APFS filesystem before forced detach. A diagnostic failure, or failure of all three forced attempts, warns without changing the test exit status. Run `python3 scripts/test_native_image_cleanup.py -v` for executable cleanup fixtures. The same suite runs in `macos-storage-gate`, including a macOS-only disposable APFS image held open by a child process. Linux skips that native proof.

  GRE-839's focused targets are `CleanupTest.test_cleanup_function_preserves_all_exit_status_cases` and `CleanupTest.test_cleanup_function_passes_actionlint_shellcheck`. Run them with `python3 scripts/test_native_image_cleanup.py -v CleanupTest.test_cleanup_function_preserves_all_exit_status_cases CleanupTest.test_cleanup_function_passes_actionlint_shellcheck`. Put actionlint 1.7.12 and ShellCheck on `PATH` to run the analysis target. It skips when either tool is absent. The existing Actionlint workflow checks the workflow independently.

  The status target executes the complete extracted workflow step through `bash --noprofile --norc -e -o pipefail`, using disposable files and executable tool doubles. Its 35 cases combine statuses 0, 1, 23, 130 and 255 with normal detach success on each attempt and all diagnostic-success/failure and forced-detach-success/failure combinations. Each case asserts the exact exit status, combined output, ordered command arguments and preserved fixture and unrelated sentinel bytes. The analysis target requires empty successful output from both tools, then injects an unquoted mount expansion into the cleanup body and requires SC2086 from each tool to prove the function is inspected. The cross-OS workflow has no matching EXIT trap to extract.
- **Desktop runtime** (`desktop-runtime.yml`) builds `pp-api` and `pp-server` and runs the proofs that need the built server on Linux: the serial `runtime_m0` lifecycle log, the Python lifecycle, upload and gateway drivers, the source artifact checks and `verify.py`. Unit tests, clippy and fmt are not repeated here; `rust (linux)` runs them.

When `rust (macos)` or a Native macOS bundle workflow job fails on a push to `main`, an alert job opens an issue labelled `ci-macos-main` for that workflow run, or comments on the existing open issue for the same run. Rust failures confined to Linux or Windows do not alert. Only the alert job receives `issues: write`.

Required Rust and lint jobs fail if their input-selection job fails, so a broken filter or failing filter test cannot pass through the required checks. `rust (macos)` also runs that check when the label and draft state select it. Failure alerts run independently with a separate issue per workflow run, so a shared concurrency queue cannot drop pending alerts. Reruns comment on the existing open issue for that run.

## Ephemeral PR runners

Pull request jobs run on GitHub-hosted runners, which cost nothing for this public repository,
and on pp-iso for the Linux Rust jobs when `PP_PR_RUNNER=pp-iso`; pp-local runs only pushes to `main` and manual runs, never a pull request or a merge group. Two workflows' jobs run on Blacksmith
by default because their speed pays for the per-minute price: `rust (linux) build` and its
aggregate, and the Linux packaged smoke job (a two-job matrix, `default` and `desktop_rebuild`). Nothing else runs on
Blacksmith: lint, actionlint, zizmor, evidence, duplication, Web CI, sidecar, docs, `rust (windows)`, macOS, release,
Pages, schedules, merge groups and `main` pushes stay on GitHub-hosted runners (`main` pushes may use pp-local), and the
workflows use no Blacksmith Docker layer cache, sticky disk or cross-PR cache. Each hosted or Blacksmith job
gets a fresh VM. The workflows that a push to a pull request re-triggers cancel a superseded run (`cancel-in-progress` on
`pull_request`), so a new push stops the jobs still paying for the old head (`linear-pr-sync.yml`, which must not be cancelled, runs on a free runner and has no cancelling group); `main` pushes keep
the one-running, one-pending rule instead.

Three jobs were tried on Blacksmith and are not kept there. `release-signing-check.yml` failed because
`docker buildx imagetools inspect --format '{{.Manifest.Digest}}'` printed the whole manifest instead of
the digest and `cosign verify` got a malformed reference. The `rust-verify` job of
`desktop-runtime.yml` failed because its LAN guard rejects new connections to `192.168.0.0/16` and counted
2630 blocked packets on Blacksmith (the runner's own services appear to use that range) before the artifact
upload failed with `ECONNREFUSED`. `rust (windows)` ran on `blacksmith-4vcpu-windows-2025` and failed two tests that pass on
`windows-latest` (a Node loopback `ECONNRESET` in `node_auth_admission_snapshot_is_current` and a timestamp-window assertion at
`import_worker_policy.rs:269`, run 42437330789), so it stays on GitHub's free Windows runner until GRE-1271 resolves them.

### Linux Rust runner

`rust (linux) build`, `rust (linux) rebuild m/3` and the `rust (linux)` aggregate
run on `[self-hosted, linux, x64, pp-local]` for pushes to `main` and
manual runs. Merge groups run them on `ubuntu-latest`: a merge group holds PR code that has not landed, and the shared pp-local cache only sees code from `main`. A pull request runs the build and the aggregate (there are no rebuild partitions on pull requests) on `blacksmith-4vcpu-ubuntu-2404` unless `PP_PR_RUNNER` says otherwise ([the switch](#pull-request-runner-switch) below). pp-local
is six ephemeral JIT slots, two on each of three VMs, and each job runs in a fresh
`pp-slot-N` container. Job names do not change with the runner, so the required
checks stay the same.

PRs stay off pp-local because it ran the PR suite more slowly than hosted. On PR
#1349, `rust (linux) build` took 60m02s on pp-local with a 99.57 % sccache hit rate,
and 87m42s with a cold cache, against 33m40s on a hosted runner. Compiling was as fast as hosted;
the nextest steps were 2.0-2.6x slower. On `main`, pp-local still moves the Linux
jobs off the hosted pool that PRs wait for. The 3 rebuild jobs start within seconds
there, instead of after the 13-17 minute hosted waits seen on main pushes.

The `changes` job picks the runner with `scripts/ci-rust-runner.py` and passes it
on as a job output. It picks `ubuntu-latest` when:

- the event is the nightly schedule (pull requests use the [runner switch](#pull-request-runner-switch));
- the `PP_RUST_RUNNER` repository variable is `hosted` (unset or `local` means pp-local;
  any other value means hosted);
- the optional `PP_RUNNER_READ_TOKEN` secret can list runners and no online pp-local
  runner is idle;
- without that secret, or when it cannot list runners: another `rust (linux)` job has
  waited 15 minutes or more for a pp-local runner.

`GITHUB_TOKEN` cannot list self-hosted runners, so without the secret the choice
reads only this workflow's queued jobs (`actions: read`). If that read fails too,
the job stays on pp-local. One dead VM leaves four slots, so jobs keep running.
If the whole fleet is down, the first job queues and each later run goes hosted
after 15 minutes; rerun a stuck run to move it. `PP_RUST_RUNNER=hosted` moves every
new run at once. To enable the idle check, store a fine-grained token with the
repository's Administration read permission as `PP_RUNNER_READ_TOKEN`.

The containers provide `SCCACHE_DIR`, `SCCACHE_CACHE_SIZE` and a host-mounted Cargo
registry. `scripts/ci-pp-local-sccache.sh` sets `RUSTC_WRAPPER` only when it is
unset. It looks on `PATH`, then at `/opt/pp-sccache-bin`, `/opt/sccache-bin`, and any
`sccache` file under `/opt`, `/usr/local` or `$CARGO_HOME/bin`, and takes the first one
whose `--version` reports sccache. It never downloads sccache or creates cache
directories; with none found it warns and lists `/opt`. It also sets
`CARGO_INCREMENTAL=0`. The build and partition jobs end with `sccache --show-stats`. pp-local jobs skip `Swatinem/rust-cache`: the Actions cache
is not needed there, and its post-job cleanup can delete registry sources that
another container is compiling. `scripts/setup-ci-linux.sh` and
`dtolnay/rust-toolchain` install the packages and Rust that hosted images ship.
[Cargo cache trust](cargo-cache-trust.md) covers the shared registry.
[pp-iso](pp-iso.md) is the separate pool of isolated, one-job runners for jobs that
run PR-editable code.

### Pull request runner switch

The repository variable `PP_PR_RUNNER` chooses where pull request jobs run. It is read only on `pull_request` events, so `main` pushes, merge groups, manual runs and the nightly schedule keep the routing above and `PP_RUST_RUNNER`. Unset, empty or any other value means `blacksmith`. Set it under Settings, Secrets and variables, Actions, Variables; the next run, or a label change on the PR, uses the new value, and `hosted` puts both paid jobs back on GitHub's free runners at once. `rust (windows)` always runs on `windows-latest`. `main` pushes, merge groups, manual runs and the nightly schedule never use Blacksmith.

| `PP_PR_RUNNER` | `rust (linux)` build and aggregate (pull requests) | `packaged smoke (linux, …)` |
| --- | --- | --- |
| `blacksmith` (default) | `blacksmith-4vcpu-ubuntu-2404` | `blacksmith-4vcpu-ubuntu-2404` |
| `hosted` | `ubuntu-latest` | `ubuntu-24.04` |
| `pp-iso` | `[self-hosted, linux, x64, pp-iso]` ([pp-iso](pp-iso.md)) | `blacksmith-4vcpu-ubuntu-2404` |

- Every job keeps its name, so the required checks do not change, and a PR that skips a job still reports it. `scripts/ci-rust-runner.py` makes the choice in the `changes` job of `rust-cross-os.yml` and `desktop-packaged-smoke.yml` and has tests for every value and event. A PR runs its own copy of that script, so the script and its test are in CODEOWNERS: a change to the routing requests Chad's review, like a workflow change. Under `pp-iso`, the script sends a pull request from a fork to Blacksmith, but a pull request runs its own copy of the workflow and script, so what actually keeps fork code off the self-hosted pool is the pp-iso runner group's fork-run settings, not the script. The packaged smoke job uses Blacksmith only on `pull_request` events when the variable is not `hosted`; every other event uses `ubuntu-24.04`. The selector jobs, lint, review gates and Web CI always use GitHub's free runners.
- Sizing: the hosted 4 vCPU medians over the last PR runs are `rust (linux) build` 36.6 min (8 runs) and packaged smoke 7.6 min (4 runs), against job timeouts of 90 minutes each. Both paid jobs get 4 vCPU, the smallest size that keeps the slower job, the Linux build, well under its timeout: Blacksmith claims about twice the speed of a hosted runner of the same size, and one measured 4 vCPU run (head bcc519d on #1600) took 20.6 min for `rust (linux) build` and 4.5 and 5.5 min for the two smoke jobs. 2 vCPU is not used because it has no measurement behind it.
- Cost: Blacksmith's pricing page lists $0.004 per minute for Ubuntu x64 at 2 vCPU and 3,000 free minutes per month; this document assumes the price scales linearly with vCPU, which the page does not state, so 4 vCPU is $0.008 per minute. The measured run above cost about $0.16 for `rust (linux) build` and $0.08 for the smoke workflow (its `default` and `desktop_rebuild` matrix jobs run in parallel and are billed separately), plus a few seconds of the `rust (linux)` aggregate, which Blacksmith may bill as a minute: about $0.24 per PR run, against $0 on GitHub's runners. If Blacksmith were no faster than the hosted medians, the bound is about $0.29 and $0.12. The one-day measurement after merge replaces these figures with actual minutes per job and wall time per PR.
- Jobs get the same permissions and secrets as on hosted runners; the switch adds none. The existing cache steps are unchanged: `Swatinem/rust-cache` saves only on `main`, and the apt package cache of `linux-gui-deps` runs only on GitHub-hosted runners, so Blacksmith jobs run a plain `apt-get install` and the "Free disk" step is skipped. Only the packaged smoke job on Blacksmith uses `actions/setup-node` with `cache: npm`, which saves on any ref, so a PR run writes an npm cache entry under its own ref. Blacksmith's "Branch Protected Caches" setting scopes each cache entry to its branch; it must stay on, because turning it off shares entries across branches and a PR-written npm entry could be restored by another PR. This repository does not verify the setting; Chad checked it is on in the Blacksmith dashboard on 2026-11-07.
- The setup, Rust toolchain, sccache and `Swatinem/rust-cache` steps adapt to the runner kind (`PP_RUNNER_KIND`), not to `runner.environment`; the disk-space steps and `linux-gui-deps` still check `runner.environment`. pp-local runs the self-hosted setup, Rust toolchain and sccache steps; pp-iso runs the self-hosted setup and Rust toolchain steps and then `Swatinem/rust-cache` from `actions/cache`, with no shared registry or sccache; Blacksmith and hosted runners use their images and `Swatinem/rust-cache`.
- pp-iso covers only the Linux Rust jobs: it has no GUI session or system package control for the packaged smoke, and no Windows runner. If no pp-iso runner is online, the jobs queue until one is, so set `hosted` first when a host is down.

### Reading Blacksmith usage

Blacksmith bills by the minute and GitHub's runners are free here, so check the spend after any change to the paid jobs and every week while the switch is new.

- Blacksmith dashboard: the usage view shows minutes and cost per runner type, repository and workflow. Compare the `blacksmith-4vcpu-ubuntu-2404` total with the 3,000 free minutes per month after converting: the allowance is counted in 2 vCPU x64 minutes, so each 4 vCPU minute uses two of them (the Linux build and smoke jobs of the measured PR run total about 31 job-minutes and use about 61).
- Per run, from GitHub: this prints each Blacksmith job of a run with its size and minutes (replace RUN_ID):

  `gh api repos/Print-Partner/PrintPartner/actions/runs/RUN_ID/jobs --jq '.jobs[]|select(.completed_at != null and .conclusion != "skipped" and any(.labels[]; startswith("blacksmith-")))|[.name,.labels[0],(((.completed_at|fromdate)-(.started_at|fromdate))/60|.*10|round/10)]|@tsv'`

  Multiply minutes by the per-minute price above. A job that appears with a `blacksmith-` label outside `rust (linux)` and `packaged smoke` is a regression: move it back to a GitHub-hosted runner.
- Cancelled runs still bill the minutes they ran. A new push cancels the superseded run, but cancelling is not instant and the last run bills in full, so a burst of pushes still costs the minutes each run reached before it was cancelled, across the parallel paid jobs.

### Actions cache

The repository's Actions cache is limited to 10 GB, so older entries are evicted
first when it fills. Caches are saved only from `main`; PRs restore `main`'s
entries. This covers `Swatinem/rust-cache` (`save-if`), the Tauri CLI archive
(`actions/cache/restore` and `actions/cache/save`), and the Docker layer caches in
Web CI (`cache-to` is empty off `main`). `actions/setup-node` and
`actions/setup-python` have no save switch; they save only when the lockfile key
is new.

The composite action `.github/actions/linux-gui-deps` caches the `.deb` files that
`apt-get install` downloads for the Linux GTK and WebKit packages (`rust-cross-os` build and
rebuild, `desktop-e2e`, `desktop-webkit`), so a slow Ubuntu mirror cannot stall the package download
(GRE-1229); `apt-get update` still contacts the mirror on every run. The cache steps run only on GitHub-hosted runners; self-hosted runners skip
them and install from the mirror as before. The key is the runner OS and architecture, the
runner image version, the ISO week and a hash of the package list. The week limits how long a
cache goes stale: Ubuntu security updates change what `apt-get update` resolves, a restored
cache then still downloads the newer packages, and the first hosted run on `main` of a new week saves a
fresh set. Only a GitHub-hosted run on `main` that is not a pull request saves, and only when the exact key
missed: pushes and manual runs of `desktop-e2e` and `desktop-webkit` (always hosted), the nightly
`rust-cross-os` schedule (the build job package set), and a `rust (linux)` push or manual run on
`main` that `scripts/ci-rust-runner.py` sends to a hosted runner (the schedule does not run the rebuild
job, so only these runs save its package set). A run on pp-local skips the cache steps and saves nothing.
Pull requests restore only. Each install logs
`apt install took N s (cache hit: ...)`. `chain-tip-gate` keeps its inline install.

### Superseded runs

Only pull request runs cancel superseded runs. Every CI workflow
groups a pull request by workflow and ref (or PR number) with
`cancel-in-progress: true`, so a new push replaces the older run, except `linear-pr-sync.yml`, which must not be cancelled. A run that is already running on `main` is never
cancelled (GRE-1228): on 2026-11-06 a sample of 60 `main` push runs had 25
cancelled, 23 succeeded and 12 in progress, because a newer merge cancelled each
older run and left that merge with no result.

Giving every `main` push its own group removed the cancelling but left the load
unbounded: on 2026-11-07 four `rust-cross-os` runs ran at once, each with a
one-hour macOS job, and pull request jobs queued behind them for hours
(GRE-1248). Pushes to `main` now share one group per workflow (`main` in the
group name, `cancel-in-progress: false`). GitHub runs one and keeps one pending
run; a newer push replaces the older **pending** run, which never started. So the
load is one running plus one pending run per workflow, and the newest run tests
every merge that was replaced, but a replaced merge gets no run of its own.

Two kinds of check still run once per push, because each compares a push with the
commit it replaced: the duplication check and the ast-grep ratchet in
`desktop-fleet-rust.yml`. The clippy-limits ratchet inside `rust-cross-os` also
compares a push with `github.event.before`; it is bounded like the rest, and the
merge-group run of the same commit and the PR runs compare against current `main`.
Tag pushes, other branches, merge groups, the schedule and manual runs keep their
own `run_id` group, so a merge-group run is never replaced. A rerun (attempt 2 and
later, which `main-red` starts for failed `main` runs) also keeps its `run_id` group, so it
never replaces the pending run and, because the original has finished, does not wait.

| Event | Group | `cancel-in-progress` |
| --- | --- | --- |
| `pull_request` (except `linear-pr-sync.yml`) | workflow and ref (or PR number) | `true` |
| first attempt of a `push` to `main` | one group per workflow | `false` (one running, one pending) |
| `push` to `main` of the duplication check and the ast-grep ratchet | `run_id` | `false` |
| `push` of a tag or another branch, `merge_group`, `schedule`, `workflow_dispatch` | `run_id` | `false` |
| rerun of a non-PR run | the original run's `run_id` | `false` |
| rerun of a `pull_request` run | same as the PR group | `true` |

`pages.yml` keeps `cancel-in-progress: false`, because cancelling a Pages deploy
can leave the site half-published, and `release.yml` runs only on tags.

The web validation job keeps its 45-minute limit on same-repository PRs; fork
PRs and non-PR runs keep 20 minutes.

## Review checks

| Hosted check | Workflow | Local command |
| --- | --- | --- |
| `review-gate` (required) | `review-gate.yml` on `pull_request_target` and `merge_group`, rerun by `review-gate-rerun.yml` | `python3 -B -m unittest discover -s scripts -p test_review_gate.py -v` |
| `claude-code-review`, `claude-security-review` | `claude-review.yml` | none; needs the `CLAUDE_CODE_OAUTH_TOKEN` secret, or `ANTHROPIC_API_KEY` as a fallback |

`scripts/test_review_gate.py` runs the gate's Python step unchanged against a fake `gh` and checks each pass and fail case: a Claude code review with its security review, an owner marker for the head, markers for another head or from another account, same-named checks from another workflow, event, or app, a review from a workflow file the PR changes, and a non-owner PR that changes workflows, `.github/zizmor.yml`, or the safety guard. It also checks that the gate runs on `pull_request_target` with no checkout or secrets, that its expected origins match the review workflows, and that the Claude jobs keep their author filter, permissions, and tool lists. The `changes` job in `actionlint.yml` runs it on every PR.

The gate waits up to 30 minutes for the head's `claude-code-review` and `claude-security-review` jobs to finish before it judges the head, so a new push no longer shows a red gate while the review is still running. It reads the final state, so waiting cannot create a pass. A job that never appears within two minutes counts as not running, and a run that is still waiting stops if the PR head or base moves. The run also re-reads the head and base right before it can pass and fails if either moved, which closes the gap between the last cancellation point and the pass. The `pull_request_target` trigger carries `branches: [main]`. A PR into another base, such as a stacked PR, runs that base's copy of `review-gate.yml`, which could always pass, and gets no run of `main`'s copy. `main`'s copy fails a PR whose base is not `main` (a rerun after a retarget), and fails when any `review-gate` check run on the head lists a PR into another base. GitHub lists the PRs open from a run's head branch now, so that check sees a PR into another base only while it is open; see the remaining risk under [Code review](definition-of-done.md#code-review). Only a PR into `main` can pass on a Claude run, because a review of a head against another base does not cover the same head against `main`. The gate also runs on `edited` events. Retargeting a PR to `main` runs the gate against `main`. Retargeting it away from `main` starts no run of `main`'s copy, so an earlier green `review-gate` check stays on that head. That is harmless: the PR no longer targets `main` and cannot merge into it, and retargeting it back to `main` runs the gate again. While waiting it follows only the latest run of each Claude job, so an older run from another PR that shares the head does not delay it. An edit run is a full run in the same group as a push run, so a newer edit or push run cancels an older one and the newest run decides. The gate has no job-level `if`, so an edit can never replace a failing gate with a skipped one, because GitHub counts a skipped required job as a pass. After a review workflow finishes, `review-gate-rerun.yml` waits up to about 40 minutes, longer than the gate's 30-minute wait, for the gate run to end and reruns it. It reruns only the newest gate run that lists this PR and lists only PRs into the default branch, never a run that also lists a PR into another base on the same head. It reads the default branch from the event, not a hardcoded name. A fork PR's runs list no PR, so for a fork PR it reruns nothing; rerun `review-gate` from the PR's Checks tab after the marker or review.

`review-gate.yml` has one `concurrency` group per PR number with `cancel-in-progress: true` for every PR run, reruns included, so a push cancels the previous head's gate run and at most one run per PR is queued behind it. Reruns used to get a group per run id, which nothing ever superseded, so they piled up in the queue (GRE-1242). `review-gate-rerun.yml` therefore puts its concurrency group on the job, after the job `if`, so an ordinary PR comment that the `if` rejects never joins the group or cancels a useful rerun. The group is the PR number for owner comments and the head SHA for review runs, with `cancel-in-progress: true`, so a newer event for the same target replaces an older one that would do the same work. A review run for a head that is no longer the PR's head exits at once without rerunning anything; a review run that ended `cancelled` on the current head still refreshes the gate, while one that ended `skipped` produces no evidence and is filtered out by the job `if`, which needs no runner. The rerun job re-reads the PR's head on every poll and right before it reruns, and exits if the head moved, so it does not rerun an old head's gate run into the PR-wide group; a push in the few seconds between that last read and the rerun request can still lose the new head's gate run to a cancel, and the next event or a manual rerun recovers it. A manual rerun of an old head's gate run shares the group and cancels the current run; rerun the current head afterwards. Every other `pull_request` workflow except `linear-pr-sync.yml` already cancels superseded PR runs. This change adds no cancellation for `main` pushes, merge groups or tags; see [Superseded runs](#superseded-runs) for how existing workflows treat `main`. `linear-pr-sync.yml` keeps `cancel-in-progress: false` so a state sync always finishes.

[Code review](definition-of-done.md#code-review) describes the two ways to pass the gate, a Claude review or the coordinator's marker, and how the Claude jobs use the owner's subscription.

## Merge queue

GitHub's merge queue is available because the repository belongs to the Print-Partner organization. The queue was turned on 2026-11-09 ([CI controls](ci-controls.md#turn-on-the-merge-queue)). A queued PR merges only after the required checks pass on its merge group: a temporary `gh-readonly-queue/main/pr-<n>-<sha>` commit that holds `main` plus every PR ahead of it in the queue.

| Required check | Workflow | On `merge_group` |
| --- | --- | --- |
| `actionlint`, `zizmor` | `actionlint.yml` | Both linters run in full. `ci-path-filter.py` treats every event other than `pull_request` as relevant, because the group's diff against `main` is not the PR's diff. |
| `rust (linux)` | `rust-cross-os.yml` | The archive build plus three rebuild partitions, as on `main`, on `ubuntu-latest`. Merge groups never use pp-local or Blacksmith. |
| `Lint Evidence block` | `pr-evidence.yml` | Passes without a PR body; the body was linted on the PR. |
| `review-gate` | `review-gate.yml` | Passes only when every PR in the group passed `review-gate` on its current head. |

`rust (windows)` is advisory, not required. It also runs on merge groups.

On a merge group, `review-gate` does not judge a review again. Its second step reads the queue through GraphQL (`repository.mergeQueue(branch: "main").entries`), finds the one entry whose `headCommit` is the group's head SHA, and checks that the entry's PR number matches the `pr-<n>` in the group ref. Every entry at or ahead of that position is in the group. For each of those PRs it requires base `main` and checks the `review-gate` check runs on the PR's current head. Every one of them must come from `.github/workflows/review-gate.yml` on `pull_request_target`; a same-named check from any other workflow or event fails the group. Of those, only runs whose Actions run lists this PR, with every listed PR into `main`, count, and the latest of them must have concluded `success`. A `pull_request_target` run executes the base branch's copy of the file, so a run for another PR with the same head into another branch, whose copy of the gate might always pass, does not count. A fork PR's run lists no PR, so a fork PR fails the merge group; queue fork changes from a same-repository branch. These also fail the group: no matching entry within two minutes, a group ref or base other than `main`, more than 100 queue entries, or a failed API call. An owner marker alone does not pass a merge group, because the PR's own `review-gate` already decides when a marker is enough. The step checks out nothing, reads the API only, and gets no secrets.

A merge group runs the workflow files from the group commit, so a PR that changes `review-gate.yml` runs its own copy on its merge group. The same holds for any other workflow file: the ruleset pins `review-gate` to GitHub Actions, not to one file, so a PR that adds a job named `review-gate` elsewhere also reports that context on its merge group. Such a PR already passed `review-gate` on its head, where the base branch's copy decided and, for an author other than the owner, required the owner's marker because it changes `.github/workflows/`. The merge-group check repeats that decision; the PR-head check is the review.

The job has no job-level `if`. GitHub counts a skipped required job as a pass, so each step carries the event condition instead and the job always reports. Merge-group runs get their own `concurrency` group (`github.ref` is unique per group) and never cancel each other; only `pull_request_target` runs cancel older runs for the same PR.

`scripts/test_merge_group_workflows.py` pins the `merge_group` trigger on all four required workflows and the merge-group runner routing. `scripts/test_review_gate.py` runs the merge-group step against a fake `gh` for each pass and fail case.

[CI controls](ci-controls.md#turn-on-the-merge-queue) records the merge_queue rule that turned the queue on.

The Rust client generator writes route modules under `web/packages/rust-client/src/routes/` and named schema modules under `src/schemas/`, with generated indexes preserving package imports. After merging main, rerun the rust-client generator; do not hand-merge generated files. Its `--verify` checks the recursive file inventory and hashes without Cargo; `--check` also compares fresh Rust output and runs contract round trips.
