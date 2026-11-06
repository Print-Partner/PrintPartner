# Continuous integration

The **Rust cross-OS** workflow (`.github/workflows/rust-cross-os.yml`) builds and tests the Rust workspace on Linux, macOS and Windows. It starts on every pull request, every push to `main`, merge groups, a nightly schedule and manual runs.

Draft pull requests skip CI jobs before runner allocation, while each job still
reports a skipped check. Marking a PR ready starts CI with the existing path and
label filters. Converting it back to a draft cancels its older PR run. Main,
merge groups, schedules and manual runs keep their existing behavior.

On ready PRs, the jobs are selected as follows:

- `rust (linux)` and `rust (windows)` run when a `changes` job finds a change to `rust/**`, `rust-toolchain.toml`, `web/package*.json`, `web/apps/**`, `web/packages/**`, `web/tsconfig*.json`, `web/scripts/atomic-package-build.mjs`, `web/scripts/storage-fixtures/**`, `fixtures/**`, `scripts/setup-ci-linux.sh`, the workflow itself, or the CI filter scripts and their tests. Other changes skip both jobs while keeping their check reports present.
- `rust (macos)` uses the same path selection. It runs only when relevant inputs change and the PR has the exact `macos` label and is not a draft. Adding or removing the label, marking the PR ready for review and converting it to a draft each start a new run.
- When those jobs run on a pull request, nextest uses an affected-crate filter from `scripts/ci-affected-nextest-filter.py`: for each changed `rust/crates/<pkg>/` path it unions `rdeps(=<pkg>)` so reverse dependents are covered. A crate whose sources read another crate's files by path (for example pp-gateway's `include_str!("../../pp-storage/...")` drift guards) joins the selection when that crate changes. The filtered nextest steps pass `--no-tests=warn`, so a selection that matches nothing in a job's `CARGO_SCOPE` (Windows excludes every reverse dependent of pp-core, for example) passes instead of exiting 4. Shared inputs (workspace `Cargo.toml` / `Cargo.lock`, toolchain, web assets, fixtures, `rust/tests/**`, `rust/scripts/**`, the workflow, or the filter scripts) keep `all()`. Clippy and doctests still cover the whole `CARGO_SCOPE`.

Pushes to `main`, merge groups, nightly and manual runs execute all three jobs regardless of paths, and nextest keeps `all()`. A new push to a pull request cancels that pull request's older run, and a new push to `main` cancels the older `main` run (see [Superseded runs](#superseded-runs)). Its purpose is to stop code that compiles on one OS only from reaching `main`.

This page covers what the Rust jobs run and how to reproduce them. [Change a CI workflow](ci-controls.md) covers review, the actionlint and zizmor checks, action pins and required checks. Both apply to this workflow.

## Agent guidance check

The `agent-guidance` job in `actionlint.yml` checks the root `AGENTS.md` and the
repo skills in `.factory/skills/`. It checks local file links (not heading
anchors), each skill's `name` and `description`, and that Git does not ignore
these files. It also tests the staged-Rust snapshot in the opt-in pre-commit
hook, including sparse worktrees. It makes no network requests. Run it locally
with `just guidance`.

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
.venv/bin/ruff format --check scripts rust/scripts web/scripts slicer-sidecar .factory/hooks
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
| `rust (macos)` | `macos-26` | The whole workspace | All except [macOS exclusions](#macos-exclusions) |
| `rust (windows)` | `windows-latest` | The workspace without [Windows exclusions](#windows-exclusions) | All in those packages |

`rust (linux) build`, its rebuild partitions and the `rust (linux)` aggregate run on the pp-local fleet or on `ubuntu-latest`; see [Linux Rust runner](#linux-rust-runner).

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

On `push` to `main`, `merge_group`, and `workflow_dispatch`, Linux replaces the in-job step 8 with a nextest archive built once (`--zstd-level 11`), then three `rust (linux) rebuild m/3` jobs that run `cargo nextest run --archive-file … --workspace-remap "$GITHUB_WORKSPACE/rust" --partition slice:m/3` on three `ubuntu-latest` runners. The build job also uploads a zstd-compressed pack of the measured desktop trees (`rust/bundle-manifest.json` and the web `dist` outputs). Artifact uploads set `compression-level: 0` so GitHub does not re-gzip already-compressed archives. Atomic package builds publish `dist/current` as a symlink to `dist/releases/<id>`; the dependency inventory records that link and the files under the release path. The pack step therefore archives each package's full `dist/` tree **without** `tar -h`, asserts the `current` symlink and a release-path file (for example `desktop-resolution.js`) are in the archive, and each partition restores those trees so the symlink topology still matches. Packing only `dist/current` with `-h` flattens the link and fails dependency inventory checks. Each partition runs `npm ci` only — it must not rebuild web assets or regenerate the manifest, because the archived binaries embed those digests at compile time. The required check `rust (linux)` is an aggregate that needs the Linux build and those partitions: on push / merge_group / workflow_dispatch it requires rebuild `success`; on PRs and schedule rebuild may be `skipped`. The schedule keeps the monolithic in-job step 8.

The macOS volume exists because the default macOS `TMPDIR` breaks three kinds of test. Its paths are too long for Unix socket names, it is under the `/var` symbolic link that no-follow opens refuse, and it treats `Straße.stl` and `STRASSE.stl` as one name.

Steps 4 to 10 run even when an earlier one of them fails, so one run reports every failure. Clippy uses `--keep-going`, so it reports errors in every crate whose dependencies compile.

Each job declares its runner in `runs-on`, its package scope in `CARGO_SCOPE`, and its test filter in `NEXTEST_FILTER` (from the `changes` job output, defaulting to `all()`). The three jobs share the same anchored steps. They have no matrix, so GitHub reports their exact required names both when running and when skipped. The Linux rebuild partitions always use `all()` because they only run on non-PR events.

`rust (linux)` and `rust (windows)` are required checks on `main`; `rust (macos)` is advisory. [Settings for Chad](ci-controls.md#settings-for-chad) lists the full ruleset.

## Run the required checks with one command

Run this command from the repository before you push:

```sh
PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci
```

`just ci` passes its arguments to `python3 scripts/local-ci.py`, which holds the only list of steps. Without [just](https://github.com/casey/just), run `python3 scripts/local-ci.py` from the repository root with the same arguments. Agents run this command before they push, as [Gate evidence](../contributing/gate-evidence.md#run-ci-locally-before-you-push) says.

It runs the steps below in the order that `just ci --list` prints, and continues after a failure, so one run reports every failure. At the end it prints one `PASS`, `FAIL` or `SKIP` line for each step and exits 1 if any step failed. To see the step ids, use `just ci --list`. To select steps, use `just ci --only fmt,clippy-off` or `just ci --skip nextest-on`.

| Step | What it runs | CI source |
|------|--------------|-----------|
| `local-ci-self-test` | `scripts/test_local_ci.py` | None. This test fails when a copied command no longer matches its workflow step. |
| `selector-tests` | The path-filter and macOS-alert unit tests | `rust-cross-os.yml` `changes` |
| `actionlint` | `actionlint -color` | `actionlint` (required) |
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

On macOS, set `TMPDIR` to a short directory on a case-sensitive volume before you run the tests. The workflow step `Mount a short case-sensitive TMPDIR` has the commands. Also set `PP_TEST_NODE` to the Node executable, as CI does. Without it the runtime tests fall back to `/usr/bin/node`, which macOS does not have. To skip the same tests as CI, add `-E` with the `NEXTEST_FILTER` value of the macOS job. On Windows, replace `--workspace` with the Windows job's `CARGO_SCOPE` value. On Linux, the Tauri shell also needs the GTK and WebKit packages that the workflow installs with `apt-get`.

Clippy and fmt do not read the manifest contents. For a lint-only run, `echo '{}' > rust/bundle-manifest.json` is enough. The tests need the real manifest.

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

Tests that start real Node children run in the `process-heavy` test group, and at most two of them run at once. The group holds every `pp-core` `runtime_m0` test and the `pp-compat` `ticket_t_2::` supervisor tests. On a busy runner, four of these at once timed out (GRE-956). Other tests keep running on the remaining threads. To see which tests a build puts in the group, run `cargo nextest show-config test-groups --manifest-path rust/Cargo.toml`.

A test that fails at random goes on the flaky list, the override with `retries = 1`. The rules:

1. Add a test to the list only with an open Linear ticket that names it and an owner. Put the ticket id in a comment next to the filter.
2. nextest runs a failed test on the list once more. A pass on the second try passes the job, and the log shows the test as `FLAKY`.
3. If a test needs that retry in two runs within seven days, the owner quarantines it. In one pull request they remove the test from the flaky list and add it to `default-filter` under `[profile.default]`, as `default-filter = 'not (binary(=<bin>) & test(=<name>))'` with the ticket id in a comment. nextest applies `default-filter` on top of the `-E` filter that CI passes, so the test no longer runs anywhere that uses the `default` profile. The comment in `nextest.toml` shows both forms. To count retries, run this from the repository root. It prints, for each test, the number of `Rust cross-OS` runs in the last seven days that logged it as `FLAKY`:

   ```sh
   since=$(date -u -d '7 days ago' +%F)
   gh run list -R poitee/PrintPartner -w "Rust cross-OS" --created ">=$since" -L 500 --json databaseId --jq '.[].databaseId' |
     while read -r id; do
       gh run view "$id" -R poitee/PrintPartner --log 2>/dev/null |
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

## Windows exclusions

These crates do not build on Windows yet. The Windows job excludes them with `--exclude` items in `CARGO_SCOPE`. The table below names each port ticket.

| Crate | Port ticket | Why it is excluded |
|-------|-------------|--------------------|
| `pp-core` | GRE-1188 | Its runtime and its tests are not yet ported to Windows. |
| `pp-api`, `pp-gateway`, `pp-server` | GRE-655 | They depend on an excluded crate. |
| `pp-desktop` | GRE-656 | It is the Tauri shell and depends on an excluded crate. GRE-656 ends with the Windows job building the whole workspace. |

`pp-os`, `pp-storage`, `pp-source` and `pp-compat` are not excluded; see [Windows test portability](#windows-test-portability) for what the pp-storage port (GRE-1154) changed. On Windows, `pp-compat` opens log entries through `pp-os` (GRE-1185) and runs its supervision tests against the Job Object and the private named pipe (GRE-1186); each Unix-only process-group, signal or parent-death test names its Windows counterpart in a comment. Its `ticket_t_2::` supervisor tests still build only on Unix until GRE-1194. The Windows job builds and tests `pp-os`, including
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
- Compare paths in the spelling the product reports. `std::env::temp_dir()` on a runner can hold 8.3 short names (`RUNNER~1`), while `canonicalize` returns the long verbatim form.

`rust (windows)` therefore does not compile the Tauri shell yet. When a port ticket's crates build and pass their tests on Windows, delete its `CARGO_SCOPE` exclusion in the same pull request.

## macOS test filter

On non-PR events the macOS job uses `NEXTEST_FILTER: all()`. On PRs it uses the same affected-crate `rdeps` filter as Linux and Windows. It still runs any selected `pp-core` `runtime_m0`, `pp-compat` `ticket_t_2::`, and `origin_tests::ticket_t_12_missing_config_persists_private_port_and_reuses_it` binaries that match the filter. **Native macOS bundle** still runs its own selected lifecycle tests.

GRE-645 removed the earlier group skip. Hosted run 38925272452 mounted `/Volumes/pptmp` with `apfs, local, nodev, nosuid, journaled, nobrowse` and recorded `exec /usr/bin/node` exiting 126: `/bin/sh: line 0: exec: /usr/bin/node: cannot execute: No such file or directory`. Those scripts now `exec "$NODE"` from `PP_TEST_NODE`. Run 38941881212, with `--test-threads 1`, still failed 3 of 74 because one release verification hashed about 42,203 dependency files in 10.7 to 19.7 seconds. Each start verifies once, on a blocking thread, before the 32-second readiness budget (GRE-661). Each restart verifies again after `Starting` is published. The crash-guard status fuse covers that hash plus the 30-second probe. The measured-release parent-death test starts its 30-second acknowledgement when the helper publishes `release-verified`, not when the helper process is spawned. The config test's 3-second deadline starts when the re-executed child enters. The GRE-671 backoff test never ran Node; it runs with the rest of `ticket_t_2::`.

To collect twenty consecutive green hosted `rust (macos)` job links after tip is green, use `scripts/gre645_macos_soak.sh`. Default mode is dry-run. `--execute` is Chad-gated: it dispatches `rust-cross-os.yml` on the chosen ref, waits for each `rust (macos)` job, appends JSONL evidence (`run`, `job`, `sha`), and stops on the first red. It does not change packing or skip lists. Prove with `python3 -B -m unittest discover -s scripts -p test_gre645_macos_soak.py`.

## Other Rust workflows

- **Desktop Rust (hosted)** (`desktop-fleet-rust.yml`) runs only the ast-grep ratchet, in the `ast-grep ratchet` job. It runs on pull requests and pushes to `main` that change `rust/**`. Every push to `main` gets its own run, so a merge of two individually green pull requests is still checked. Like the duplication check, it does not cancel superseded runs: a cancelled run would skip the ratchet for that push's diff, and the job is short. On a push, the ratchet compares against the `main` commit that push replaced. It does not build or test Rust; `rust (linux)` does that once per push.
- **Native macOS bundle** (`desktop-native-macos.yml`) builds the unsigned macOS app and runs the macOS storage tests. Its `changes` job selects them by event and changed paths:
  - On pull requests, `linux-precheck`, `server-smoke`, the arm64 `bundle` and `macos-storage-gate` run only when the PR has the exact `macos` label, is not a draft, and changes desktop inputs. The selector includes the Rust inputs above, the native workflow, its cleanup tests and the Rust verification helper directory. Fork PRs skip `macos-storage-gate`. The Intel bundle never runs on pull requests.
  - Pushes to `main` or `master`, merge groups, nightly and manual runs select every job, including the Intel bundle.

  Every macOS job also needs a successful `linux-precheck`, so a broken web build never starts a macOS runner. Skipped jobs still report their checks. A new push, label change or draft change on a pull request cancels that pull request's older run. A new push to `main` cancels the older `main` run; reruns, merge groups, nightly and manual runs each keep their own group.
  The source fixture registers the named `cleanup_source_fixture` function with `trap cleanup_source_fixture EXIT` after a successful attach. It captures `$?` immediately and exits with that status on every cleanup path. Cleanup targets only this step's owned mount. It tries normal detach of the mount three times, with `sleep 2` between failures. If all three fail, it records holders and then forces detach of the whole-disk device that `hdiutil attach` printed first, again up to three times with `sleep 2` between failures. A forced unmount can succeed while ejecting the backing device returns `Resource busy`. The mount path is then an ordinary directory, so the forced retries target the device. The explicit sleeps total at most eight seconds. The `hdiutil` calls and holder diagnostics add time, so eight seconds is not a wall-clock cleanup bound.

  After normal detach retries fail, `lsof +f -- "$fixture_mount"` records open files on the source fixture's owned APFS filesystem before forced detach. A diagnostic failure, or failure of all three forced attempts, warns without changing the test exit status. Run `python3 scripts/test_native_image_cleanup.py -v` for executable cleanup fixtures. The same suite runs in `macos-storage-gate`, including a macOS-only disposable APFS image held open by a child process. Linux skips that native proof.

  GRE-839's focused targets are `CleanupTest.test_cleanup_function_preserves_all_exit_status_cases` and `CleanupTest.test_cleanup_function_passes_actionlint_shellcheck`. Run them with `python3 scripts/test_native_image_cleanup.py -v CleanupTest.test_cleanup_function_preserves_all_exit_status_cases CleanupTest.test_cleanup_function_passes_actionlint_shellcheck`. Put actionlint 1.7.12 and ShellCheck on `PATH` to run the analysis target. It skips when either tool is absent. The existing Actionlint workflow checks the workflow independently.

  The status target executes the complete extracted workflow step through `bash --noprofile --norc -e -o pipefail`, using disposable files and executable tool doubles. Its 35 cases combine statuses 0, 1, 23, 130 and 255 with normal detach success on each attempt and all diagnostic-success/failure and forced-detach-success/failure combinations. Each case asserts the exact exit status, combined output, ordered command arguments and preserved fixture and unrelated sentinel bytes. The analysis target requires empty successful output from both tools, then injects an unquoted mount expansion into the cleanup body and requires SC2086 from each tool to prove the function is inspected. The cross-OS workflow has no matching EXIT trap to extract.
- **Desktop runtime** (`desktop-runtime.yml`) builds `pp-api` and `pp-server` and runs the proofs that need the built server on Linux: the serial `runtime_m0` lifecycle log, the Python lifecycle, upload and gateway drivers, the source artifact checks and `verify.py`. Unit tests, clippy and fmt are not repeated here; `rust (linux)` runs them.

When `rust (macos)` or a Native macOS bundle workflow job fails on a push to `main`, an alert job opens an issue labelled `ci-macos-main` for that workflow run, or comments on the existing open issue for the same run. Rust failures confined to Linux or Windows do not alert. Only the alert job receives `issues: write`.

Required Rust and lint jobs fail if their input-selection job fails, so a broken filter or failing filter test cannot pass through the required checks. `rust (macos)` also runs that check when the label and draft state select it. Failure alerts run independently with a separate issue per workflow run, so a shared concurrency queue cannot drop pending alerts. Reruns comment on the existing open issue for that run.

## Ephemeral PR runners

Jobs run on GitHub-hosted runners (`ubuntu-latest`, `ubuntu-24.04`, `macos-26`,
`macos-26-intel`, `macos-latest` or `windows-latest`), except the Linux Rust jobs
below. Each hosted job gets a fresh VM.

### Linux Rust runner

`rust (linux) build`, `rust (linux) rebuild m/3` and the `rust (linux)` aggregate
run on `[self-hosted, linux, x64, pp-local]` for pushes to `main`, merge groups and
manual runs. Every pull request runs them on GitHub-hosted `ubuntu-latest`. pp-local
is six ephemeral JIT slots, two on each of three VMs, and each job runs in a fresh
`pp-slot-N` container. Job names do not change with the runner, so the required
checks stay the same.

PRs stay hosted because pp-local ran the PR suite more slowly than hosted. On PR
#1349, `rust (linux) build` took 60m02s on pp-local with a 99.57 % sccache hit rate,
and 87m42s with a cold cache, against 33m40s hosted. Compiling was as fast as hosted;
the nextest steps were 2.0-2.6x slower. On `main`, pp-local still moves the Linux
jobs off the hosted pool that PRs wait for. The 3 rebuild jobs start within seconds
there, instead of after the 13-17 minute hosted waits seen on main pushes.

The `changes` job picks the runner with `scripts/ci-rust-runner.py` and passes it
on as a job output. It picks `ubuntu-latest` when:

- the event is a pull request (same-repository or fork) or the nightly schedule;
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

### Actions cache

The repository's Actions cache is limited to 10 GB, so older entries are evicted
first when it fills. Caches are saved only from `main`; PRs restore `main`'s
entries. This covers `Swatinem/rust-cache` (`save-if`), the Tauri CLI archive
(`actions/cache/restore` and `actions/cache/save`), and the Docker layer caches in
Web CI (`cache-to` is empty off `main`). `actions/setup-node` and
`actions/setup-python` have no save switch; they save only when the lockfile key
is new.

### Superseded runs

Every CI workflow that runs on pushes to `main` groups those pushes by workflow
and ref and cancels the older run when a newer push arrives, except the ast-grep
ratchet above and the [duplication check](#duplicated-code). Both compare each
push with the commit it replaced. The newest `main` run tests every earlier merge, so a burst of
merges no longer queues one full run per merge. The cost is attribution: a
cancelled merge has no result of its own. To find the merge that broke `main`,
dispatch the workflow on each candidate SHA. Reruns (attempt 2 and later, which
`main-red` starts) keep their own group so they never cancel the newer run. Tag
pushes, merge groups, the schedule and manual runs never cancel each other.
`pages.yml` keeps `cancel-in-progress: false`, because cancelling a Pages deploy
can leave the site half-published, and `release.yml` runs only on tags.

The web validation job keeps its 45-minute limit on same-repository PRs; fork
PRs and non-PR runs keep 20 minutes.

## Review checks

| Hosted check | Workflow | Local command |
| --- | --- | --- |
| `review-gate` (required) | `review-gate.yml` on `pull_request_target`, rerun by `review-gate-rerun.yml` | `python3 -B -m unittest discover -s scripts -p test_review_gate.py -v` |
| `droid-review` | `droid-review.yml` | none; needs the `FACTORY_API_KEY` secret |
| `claude-code-review`, `claude-security-review` | `claude-review.yml` | none; needs the `CLAUDE_CODE_OAUTH_TOKEN` secret, or `ANTHROPIC_API_KEY` as a fallback |

`scripts/test_review_gate.py` runs the gate's Python step unchanged against a fake `gh` and checks each pass and fail case: a Droid success, a Claude code review with its security review, an owner marker for the head, markers for another head or from another account, same-named checks from another workflow, event, or app, a review from a workflow file the PR changes, and a non-owner PR that changes workflows, `.github/zizmor.yml`, or the safety guard. It also checks that the gate runs on `pull_request_target` with no checkout or secrets, that its expected origins match the review workflows, and that the Claude jobs keep their author filter, permissions, and tool lists. The `changes` job in `actionlint.yml` runs it on every PR.

The gate waits up to 30 minutes for the head's `claude-code-review` and `claude-security-review` jobs to finish before it judges the head, so a new push no longer shows a red gate while the review is still running. It reads the final state, so waiting cannot create a pass. A job that never appears within two minutes counts as not running, and a waiting run stops if the PR head or base moves, so a run that started before a retarget can never pass it. A stacked PR is judged against its own base with the same rules: the review runs must belong to this PR into that base, the workflow files must equal `main`'s, and the head needs a passing review or the owner's marker. Nothing passes just because the base is not `main`. A review of a head against another base does not cover the same head against `main`, so the gate also runs on `edited` events: retargeting a PR onto `main` judges it again against `main`. An edit run has its own concurrency group and cancels nothing, and a run for a new push does not cancel it, so an edit can never replace a failing gate with a skipped one. The gate has no job-level `if`, because GitHub counts a skipped required job as a pass. After a review workflow finishes, `review-gate-rerun.yml` waits for the gate run to end and reruns it, as before.

`review-gate.yml` has a `concurrency` group per PR number with `cancel-in-progress: true`, so a push cancels the previous head's gate run instead of leaving it queued. Reruns (`run_attempt` above 1, started by `review-gate-rerun.yml`) get their own group and never cancel the current head's run. Every other `pull_request` workflow except `linear-pr-sync.yml` and `droid.yml` already cancels superseded PR runs. This change adds no cancellation for `main` pushes, merge groups or tags; see [Superseded runs](#superseded-runs) for how existing workflows treat `main`. `linear-pr-sync.yml` keeps `cancel-in-progress: false` so a state sync always finishes, and `droid.yml` has no group because its comment and review events share the workflow with PR events and a PR-keyed group would cancel a live `@droid` run.

[Code review](definition-of-done.md#code-review) describes the three ways to pass the gate, the coordinator's marker, and how the Claude jobs use the owner's subscription.
