# Continuous integration

A pull request, a push to `main`, and a merge group run four parts (GRE-1649):

1. `actionlint` and `zizmor` in `.github/workflows/actionlint.yml`, unchanged.
2. `rust (linux)` in `.github/workflows/rust-cross-os.yml`: `cargo fmt`, `cargo clippy -D warnings`, and `cargo nextest run` on the default build. After GRE-1646 that default is the Rust-only build. The commands do not pass `--features desktop_rebuild` or `--no-default-features`. On `ubuntu-latest` (GRE-1664, rebalanced in GRE-1892) nextest runs as six `hash` partitions, and fmt plus clippy run on a separate part. The required check is the aggregate, still named `rust (linux)`.
3. `ui` in `.github/workflows/web-ci.yml`: replacement UI lint, UI and generated Rust client tests, and a build of the replacement UI. The same workflow runs `release-audit` on every ready PR, push and merge group.
4. `rust image` and `rust image (arm64)` in `.github/workflows/rust-image.yml`: build `Dockerfile.rust` and run `scripts/docker-rust-smoke.py`, which includes sign-in, when image inputs change on a pull request (and on every push to `main` and merge group). `rust image` runs on `ubuntu-latest` (amd64) and `rust image (arm64)` on the native `ubuntu-24.04-arm` runner, so each architecture is built and run on its own hardware (GRE-1295).

The general Linux Rust job runs `npm --prefix web run build` and `rust/scripts/build-desktop-manifest.py` to generate the compile input that `pp-compat` embeds. Node parity and the legacy desktop smoke and packaging workflows remain dispatch-only. For desktop pull requests, see [Linux desktop check](#linux-desktop-check).

The nightly schedule (`37 7 * * *`) and a manual dispatch of Rust cross-OS build the workspace on `windows-latest` and `macos-26`. Those legs are named `rust (windows)` and `rust (macos)`. They compile with `cargo build --locked --workspace`. They do not run nextest, and they do not run on pull requests, pushes, or merge groups. A manual dispatch does not also run the Linux nextest job. A nightly `rust (macos)` failure opens the `ci-macos-main` issue. The daily-loop test runs on each push to `main`; see [Daily loop](#daily-loop).

#### Windows tests on pull requests

`rust (windows tests)` in `rust-cross-os.yml` runs named Windows tests on `windows-latest` (GRE-1811). It is not a required check, so a red run does not block a merge. It runs on a manual dispatch of Rust cross-OS on any branch, and on a ready pull request when `scripts/ci-path-filter.py windows-tests` selects it. It does not run on pushes, merge groups or the nightly schedule. The job runs for every pull request except those that change only docs/, root Markdown, non-build governance/sync/evidence scripts, or other workflow files; it is optional and runs in parallel, so it cannot block or delay a merge, at the cost of Windows runner minutes on most PRs.

The filter skips the job only when every changed path is on the skip list:

- `docs/**`.
- Markdown files at the repository root (`*.md` directly in the root, nothing deeper).
- Non-build governance, sync and evidence scripts: `scripts/linear/**`, `scripts/evidence/**`, `scripts/merge_class.py`, `scripts/agent-guard.py`, `scripts/post_pp_score_status.py` and their tests.
- Non-bundled web/UI test files: `web/test/**`, `web/apps/ui/test/**`, `web/apps/ui/src/test/**`, and any `web/apps/ui/**` file whose basename contains `.test.` or `.spec.`.
- Files under `.github/**`, except `.github/workflows/rust-cross-os.yml` and `.github/zizmor.yml`.

Any other path selects it: everything under `rust/`, non-test `web/` paths (including `web/apps/ui/src/*.tsx` and `web/packages/rust-client`), `fixtures/` and other `scripts/`, any other root file, and any new top-level directory. Over-selection is intended, so a new input cannot be forgotten. A change that mixes skipped and other paths runs the job. So do an empty diff, a missing or malformed base or head SHA, a failed `git diff`, and every manual dispatch.

The job installs `web/` without `@print-partner/ui` and `@print-partner/rust-client`, builds the old UI and writes `rust/bundle-manifest.json`. Without `rust-client`, the manifest script does not build the replacement UI, and none of these tests read it. The replacement UI's Windows build is not proven here. Each line below is one `cargo nextest run --locked --no-fail-fast --no-tests=fail` call, and the job summary gets one result row per call:

| Ticket | Tests | `desktop_rebuild` |
| --- | --- | --- |
| GRE-1795 | `ticket_t_2::ticket_t_2_respawn_blocked_during_verification_starts_no_child`, `release::tests::junctions_are_recorded_links_and_cannot_escape` (pp-compat) | on and off |
| GRE-1796 | `slice0_mount::slice0_mount_over_limit_body_gets_the_413_over_a_real_socket` (pp-gateway) | on |
| GRE-1799 | `health::tests::*` in the `pp-server` binary | on |

"On" is the default build of `-p pp-compat -p pp-gateway -p pp-server`. "Off" is `-p pp-compat --no-default-features`. `--no-tests=fail` fails a call that matches no test, so renaming one of the single-test calls fails it. The GRE-1799 call matches `health::tests::` by prefix, so it fails only if all seven tests leave that module. A pass proves only these tests on `windows-latest`. It does not prove the rest of the workspace on Windows, and a pull request that changes only skip-list paths does not run it. Pull requests restore the `rust-windows-tests` cache and never save it; a dispatch on `main` saves it. There is no local equivalent on Linux, and `just ci` does not run it. On a Windows machine with Node 24.21.0, run the job's two steps from Git Bash: the partial `npm --prefix web ci`, the build and `build-desktop-manifest.py`, then, for example, `cargo nextest run --manifest-path rust/Cargo.toml --locked -p pp-compat --no-default-features -E 'test(=ticket_t_2::ticket_t_2_respawn_blocked_during_verification_starts_no_child)'`.

Draft pull requests skip CI jobs before runner allocation, while each job still reports a skipped check. Marking a PR ready starts CI. Converting it back to a draft cancels its older PR run.

On a ready pull request, `rust (linux)` runs when `scripts/ci-path-filter.py rust` finds a change to `rust/**`, `rust-toolchain.toml`, `web/package*.json`, `web/apps/**`, `web/packages/**`, `web/tsconfig*.json`, `web/scripts/atomic-package-build.mjs`, `web/scripts/storage-fixtures/**`, `fixtures/**`, `scripts/setup-ci-linux.sh`, `.github/actions/linux-gui-deps/action.yml`, the workflow itself, or the CI filter scripts and their tests. Other changes skip it while keeping its check report present. The filter is a job `if`, not a workflow `paths:` list, so a skip still reports `skipped`. Markdown is not a Rust input, except the legal texts under `web/apps/server/src/data/` that pp-api and pp-gateway embed, and anything under `fixtures/`. Non-bundled UI/web test files (`web/test/**`, `web/apps/ui/test/**`, `web/apps/ui/src/test/**`, and any `web/apps/ui/**` file whose basename contains `.test.` or `.spec.`) are also skipped for `rust`, `rust-image`, and `windows-tests` on pull requests (`ui` still typechecks and builds `@print-partner/ui`, and merge groups run all jobs). A change under `web/apps/web/` still selects `rust (linux)` because the job builds that tree into the manifest.

When the Linux job runs on a pull request, nextest uses the affected-crate filter from `scripts/ci-affected-nextest-filter.py`: for each changed `rust/crates/<pkg>/` path it unions `rdeps(=<pkg>)`. A crate whose sources read another crate's files by path joins the selection when that crate changes. The nextest step passes `--no-tests=warn`. Shared inputs (workspace `Cargo.toml` / `Cargo.lock`, toolchain, web assets, fixtures, `rust/tests/**`, `rust/scripts/**`, the workflow, or the filter scripts) keep `all()`. Clippy covers the whole workspace. On `ubuntu-latest` that same filter is applied in six parts, `--partition hash:1/6` through `hash:6/6`. A part that matches nothing passes because of `--no-tests=warn`. pp-local runs one part, `hash:1/1`, which is the whole filter.

#### Cross-OS PR label

`ci:cross-os` does not select a pull-request job. Windows and macOS compile on the nightly build and on a manual dispatch of Rust cross-OS. The older `macos`, `ci:windows` and `ci:full` labels do not select a job either. The Linux aggregate is required. Its part jobs report as `rust (linux) check` and `rust (linux) 1/6` through `rust (linux) 6/6` on a hosted runner, and `rust (linux) 1/1` on pp-local.

A push to `main` runs the same four parts, with nextest's filter `all()` (owner decision, 2026-11-12, GRE-1476, kept so a push does not wait on a macOS runner). A merge group runs the same Linux job, including nextest, on `ubuntu-latest` ([Merge queue](#merge-queue)). A new push to a pull request cancels that pull request's older run, and a running `main` run is never cancelled and at most one newer `main` run waits behind it (see [Superseded runs](#superseded-runs)).

Hosted parts restore `Swatinem/rust-cache` with `shared-key: rust-linux`, the job id that saved the cache before the split. The `1/6` part saves that same key on a pull request and on `main`. The other parts only restore. pp-local skips this cache. The Rust image jobs use the GitHub Actions cache with `mode=max`, including on pull requests: scope `dockerfile-rust` for amd64 and `dockerfile-rust-arm64` for arm64. The first image build of a new cache is cold.

This page covers what the jobs run and how to reproduce them. [Change a CI workflow](ci-controls.md) covers review, the actionlint and zizmor checks, action pins and required checks.

## Linux desktop check

Desktop CI on pull requests is the `desktop (linux)` job in `desktop-linux.yml` (GRE-1677). Windows and macOS desktop builds stay in the nightly workspace job and its manual dispatch until M7.

On each ready PR, `scripts/ci-path-filter.py desktop-linux` checks the full PR diff. Changes under `rust/crates/pp-desktop/` or `desktop/`, desktop workflows, bundle staging, assembly and manifest tools, the replacement UI and Rust client, and shared Cargo, Web package and Linux dependency inputs select the job. A draft or an unrelated diff skips it. An unknown diff runs it. The check is advisory under the current ruleset and runs only on pull requests.

The job installs the Linux Tauri libraries, builds the binary with `desktop_rebuild`, and runs the crate's tests with nextest and the `desktop-linux` profile. The profile excludes four origin-change tests that execute JavaScript in Node and the scratch audit that reruns the whole library test binary. The job uses a compile-only manifest that release verification rejects and starts no application Node process, UI, or desktop app. Installed startup and runtime bundle verification require a measured release and separate smoke testing.

To run the build and tests locally with the Linux Tauri libraries, the repository Rust toolchain, and nextest 0.9.148:

```sh
(
  set -e
  temp_dir=$(mktemp -d)
  trap 'rm -r -- "$temp_dir"' EXIT
  export PP_DESKTOP_RELEASE_MANIFEST="$temp_dir/desktop-compile-manifest.json"
  python3 rust/scripts/build-desktop-manifest.py --compile-only --output "$PP_DESKTOP_RELEASE_MANIFEST"
  cargo build --locked --manifest-path rust/Cargo.toml -p pp-desktop --features desktop_rebuild
  cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-desktop --features desktop_rebuild --profile desktop-linux --no-tests=fail
)
```

For the hosted result, check that `changes` logged `desktop-linux inputs changed: true` and that both the build and test steps in `desktop (linux)` passed at the PR head.

## Web job selection

`web-ci.yml` starts on every PR, pushes to `main` and `master`, merge groups,
manual dispatch and reusable-workflow calls. Its jobs are `release-audit` and
`ui`. There is no `web`, `web-build`, `sidecar` or Node `docker` job. The Rust
image is `.github/workflows/rust-image.yml`, not this file.

The `changes` job runs `ci-path-filter.py ui`. The `ui` job needs that output.
The job fails if the selector fails, and runs the full UI checks if the diff is unknown. Draft PRs skip
before runner allocation. The `ui` kind covers `web/apps/ui/`, `web/test/`, shared packages
including `rust-client`, web scripts and configuration, fixtures, manifests,
scripts (other than the non-build governance, sync and evidence scripts) and CI inputs. The job runs on Node 24.21.0:

```sh
npm ci
npm run lint --workspace @print-partner/ui
npm test -w @print-partner/rust-client
npm run test --workspace @print-partner/ui
npm run build --workspace @print-partner/ui
```

`npm run build` for `@print-partner/ui` typechecks and runs Vite. The local
`ui-package` step of `just ci` also runs `test:browser`.

The `web` kind remains in `scripts/ci-path-filter.py` so its tests stay, and no
job calls it. It covers non-Markdown files under `web/`, `scripts/`, `fixtures/`,
`manifests/`, `slicer-sidecar/`, `rust/`, workflows and local actions, plus shared
package and configuration files, Docker and Compose inputs, the Caddyfile, the
Rust toolchain and the root `LICENSE`. The workaround ratchet's extension list
still has to match the filter's list. The ratchet itself runs only in `just ci`.

The `ci-pins` job runs on every non-draft event. It checks action pins, the
desktop-contracts dispatch rule, and that `desktop-contracts.yml` is still an
active workflow. It is the guard the old `web` job ran. It does not lint or
test the old UI.

The independent `release-audit` job runs `node scripts/audit-public-release.mjs`
on every non-draft PR, with no path selection and no dependency on `changes`.
It also runs on non-PR events. It checks public release inputs, repository-wide
forbidden text, broken local links, the root `LICENSE`, `.gitignore` and
1440×900 screenshots. It needs only checkout and pinned Node 22.23.3, with
read-only contents permission and no npm install.

Merge groups select `rust`, `rust-image`, `actions`, `windows`, `macos` and `ui`, but not
`web`. The selector returns true for `ui` on every merge group without reading
the diff. Other non-PR events select every kind. See [required checks](#merge-queue).

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

`just ci` caps duplicated tokens with jscpd 5.4.2. There is no hosted duplication job.
It scans the whole Node server and React UI app directories, shared web packages,
all Rust crates including `pp-server` and `pp-desktop`, and the Python sidecar.
It reads JavaScript, TypeScript, Rust and Python. Tests count as source,
including the UI's `test/` tree.
Generated TypeScript, fixtures, snapshots, dependencies and build outputs do not.

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
`PP_DUPLICATION_BASE`. The local step compares with `origin/main`. No workflow runs this scan.
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

Pull-request CI does not run `test:bundles`. The old `web-build` job is gone.
Run the budgets locally. The check keeps Three.js out of static route imports and enforces raw and gzip
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

`just ci` runs `rust/scripts/check-clippy-limits.py` on Linux. Pull-request CI
does not: the scan is the feature-off build this cut removed. It checks the
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

The local step compares with `origin/main`. `just ci --only clippy-limits` fetches main without making the
local clone shallow, runs the checker's negative controls, and checks the
workspace. Run it on Linux after building the desktop assets and manifest.
The extra Clippy passes reuse the build cache but still visit both feature
modes. These counts cap existing findings; they do not mean the Rust
complexity backlog is fixed.

The Linux job and the nightly matrix use the toolchain pinned in `rust-toolchain.toml`.

| Check name | When it runs | Runner | What it runs |
|------------|--------------|--------|--------------|
| `rust (linux)` | pull request, push to `main`, merge group, when Rust inputs changed | pp-local on a push: one part (`1/1`) runs fmt, clippy and all of nextest. `ubuntu-latest` on a pull request, merge group, or hosted fallback: `check` plus six nextest partitions. The aggregate always runs on `ubuntu-latest` | fmt, clippy, nextest, default features, `--workspace` |
| `rust (windows)` | nightly schedule or manual dispatch | `windows-latest` | `cargo build --locked --workspace` |
| `rust (windows tests)` | pull request that changes its inputs, or manual dispatch | `windows-latest` | The named Windows tests in [Windows tests on pull requests](#windows-tests-on-pull-requests) |
| `rust (macos)` | nightly schedule or manual dispatch | `macos-26` | `cargo build --locked --workspace` |

The Linux parts do not run on the nightly schedule or on a manual dispatch. See [Linux Rust runner](#linux-rust-runner). There is no `desktop_rebuild` rebuild partition and no `CARGO_SCOPE`. Hosted nextest uses `hash` partitions instead.

Build caching and linking:

- Hosted parts restore `Swatinem/rust-cache` with `shared-key: rust-linux`. The `1/6` part saves that key on a pull request and on `main`. The other parts only restore. pp-local parts skip the action and use the container's sccache instead.
- Linux installs `mold` and links with it through `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS=-C link-arg=-fuse-ld=mold`. The nightly Windows and macOS builds keep their default linkers.
- On GitHub-hosted Linux runners only, a part prints `df -h /` and deletes preinstalled toolchains it never uses (`/usr/share/dotnet`, `/usr/local/lib/android`, `/opt/ghc`, `/usr/local/.ghcup` and CodeQL in the tool cache) when fewer than 50G are free. The GRE-1649 warm run spent about two minutes on that delete while 80G were still free, and one default-feature tree fits without it. pp-local skips the step. A hosted part prints `df -h /` again after nextest.
- The workflow sets `CARGO_PROFILE_DEV_DEBUG` and `CARGO_PROFILE_TEST_DEBUG` to `line-tables-only`.

Each Linux part runs these steps. `check` stops after clippy. A `m/n` part skips clippy and runs nextest. `1/1` runs both.

1. Install the toolchain. A part that runs nextest also installs nextest 0.9.148.
2. `cargo fmt --all --check`. A format failure skips the rest of that part.
3. Install the GTK and WebKit packages and `mold`.
4. Install Node 22.23.3 for the storage fixtures and Node 24.21.0, then build the desktop web assets and write `rust/bundle-manifest.json`. `pp-compat` embeds this file at compile time.
5. On `check` and `1/1`: `cargo clippy --workspace --all-targets --keep-going -- -D warnings`, with default features.
6. On `m/n` and `1/1`: `cargo nextest run --workspace --no-fail-fast --no-tests=warn --partition hash:m/n -E "$NEXTEST_FILTER"`, with default features.

The `check` part does not run nextest, so a clippy failure does not cancel the partitions. A partition does not run clippy. On `1/1`, nextest still runs when clippy fails, because its `if` does not require the clippy step. The parts do not run doctests and do not run `rust/scripts/check-clippy-limits.py`. That scan builds with the feature off and on, which is the feature-off pass this cut removes from pull-request CI. `just ci` still runs it on Linux.

`rust (linux)` is the aggregate. It runs on `ubuntu-latest` after the parts, with no checkout. It fails unless the `changes` job and every part succeeded. A failed selector still runs the aggregate, so the required check goes red instead of staying skipped. A docs-only pull request skips the parts and the aggregate. The warm run on GRE-1649 head `6bf5c6379` was 28m40s because nextest alone took 1170s inside one job. Six hosted partitions (rebalanced from four in GRE-1892) bring that wall under 20 minutes. pp-local stays one part: the fleet is six slots and the selector only requires one idle slot, so fanning a push out would queue.

The nightly job installs the toolchain and Node 24.21.0, writes the same manifest, and runs `cargo build --locked --workspace`. It does not mount a case-sensitive volume and does not run tests. The macOS volume note below is for a local test run.

The macOS volume exists because the default macOS `TMPDIR` breaks three kinds of test. Its paths are too long for Unix socket names, it is under the `/var` symbolic link that no-follow opens refuse, and it treats `Straße.stl` and `STRASSE.stl` as one name.

`rust (linux)` is a required check on `main`. `rust (windows)` and `rust (macos)` are nightly builds. `rust (windows tests)` is an optional PR check. [CI controls](ci-controls.md#settings-for-chad) lists the ruleset.

## Run the required checks with one command

A data-loss change, as [Who merges](definition-of-done.md#who-merges) defines it, runs the full set before its first push. Later pushes run the short checks in [Set up and run checks](agent-workflow.md#set-up-and-run-checks).

```sh
PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci
```

`just ci` passes its arguments to `python3 scripts/local-ci.py`, which holds the only list of steps. Without [just](https://github.com/casey/just), run `python3 scripts/local-ci.py` from the repository root with the same arguments. Agents run it, or `just ci --fast` for a normal PR, before they push, as [Gate evidence](../contributing/gate-evidence.md#run-ci-locally-before-you-push) says.

It runs the steps below in the order that `just ci --list` prints and continues after ordinary step failures. An artifact-freshness failure is an exception: it stops the run before nextest, and later steps are not run or listed. The summary prints `PASS`, `FAIL` or `SKIP` for each completed or skipped step and exits 1 if any step failed. To see the step ids, use `just ci --list`. To select steps, use `just ci --only fmt,clippy` or `just ci --skip nextest`.

Runtime nextest needs matching web builds and `rust/bundle-manifest.json`. Selecting
`nextest`, or `--fast --only nextest-affected`, also runs `web-assets`
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

Normal PRs run the fast set instead, and data-loss changes, as [Who merges](definition-of-done.md#who-merges) defines them, run the full `just ci`:

```sh
PP_SCHEMA_PARITY_NODE=/path/to/node-v22.23.3 just ci --fast
```

`--fast` leaves out `nextest` and dual-feature `clippy-limits`, runs `nextest-affected` in place of `nextest`, and skips unaffected heavy steps (`ast-grep`, `boundaries`, `historical-schema`, `fmt`, `rust-deps`, `clippy`, `nextest-affected`, `web-assets`, `ui-package`, `workaround-ratchet`, `duplication`, `node-fixtures`) when the diff from `git merge-base origin/main HEAD` to the working tree plus untracked files does not touch their inputs (GRE-1892). Skipped steps are printed as `SKIP` with `unaffected by branch diff (--fast)` and do not fail `--fail-on-skip`. Explicit `--only` runs the named steps regardless of the diff; an empty diff, missing merge base, or filter error fails closed and runs every fast step. For Rust changes, `nextest-affected` uses the rules in `scripts/ci-affected-nextest-filter.py`; shared inputs such as `rust/Cargo.lock`, `web/`, `fixtures/` and the filter script force `all()`, as in CI. Fetch `origin` first so the merge base is current. `just ci --fast --list` prints the fast step ids, and `--only` and `--skip` take them.

If `sccache` is on `PATH`, or at `/opt/agent-fleet/bin/sccache`, both the full and the fast run set `RUSTC_WRAPPER` to it and `CARGO_INCREMENTAL=0`, unless `RUSTC_WRAPPER` is already in the environment. Set it empty (`RUSTC_WRAPPER= just ci`) to build without a wrapper. The command prints the wrapper it sets. Without sccache, it sets neither.

| Step | What it runs | CI source |
|------|--------------|-----------|
| `local-ci-self-test` | `scripts/test_local_ci.py` | None. This test fails when a copied command no longer matches its workflow step. |
| `selector-tests` | The path-filter and macOS-alert unit tests | `rust-cross-os.yml` and `actionlint.yml` `changes` |
| `perf-baseline-tests` | The unit tests of the Node-versus-Rust baseline harness in `scripts/perf-baseline` | None yet. A local-only line; see [Performance](performance.md). |
| `actionlint` | `actionlint -color` | `actionlint` (required) |
| `agent-guidance` | `test_agent_guidance.py` and `check-agent-guidance.py`: local links and anchors in every tracked Markdown page, and the guidance title and skill rules | `agent-guidance` in `actionlint.yml` |
| `zizmor` | `zizmor --min-severity informational --config .github/zizmor.yml .` | `zizmor` (required) |
| `ast-grep` | `ast-grep test`, `test_ast_grep.py` and `check-ast-grep.py` against `origin/main` | `ast-grep ratchet` |
| `boundaries` | `check-boundaries.py --self-test` and `check-domain-deps.py --self-test` | `actionlint.yml` |
| `historical-schema` | `rust/scripts/fetch-historical-schema-commits.sh`, which fetches the pinned commits that the pp-storage tests archive. It runs before nextest. | `rust (linux)` (required) |
| `fmt` | Step 4 of the OS jobs, with the Linux `--workspace` scope | `rust (linux)` (required) |
| `rust-deps` | Scanner negative controls and `cargo machete rust`, pinned to 0.9.2 | `cargo-machete` in `rust-deps-weekly.yml` |
| `web-assets` | Desktop web build and `rust/bundle-manifest.json` | `rust (linux)` (required) |
| `ui-package` | `lint`, `test`, `build` and `test:browser` for `@print-partner/ui`, on Node 24.21.0. Vitest fails the step when it finds no tests. `test:browser` exits 77 for a missing browser engine, which the step reports as SKIP. | `ui` in `web-ci.yml` for lint, tests and build |
| `workaround-ratchet` | `scripts/workaround-ratchet.mjs` and its tests, which cap `TODO`, `eslint-disable` and Rust `#[allow]` markers, on Node 22.23.3. It runs after `web-assets`, which installs its parser. | None. Local only. |
| `duplication` | jscpd negative controls and the five-area duplicated-token baseline, on Node 22.23.3 | None. Local only; there is no hosted duplication job. |
| `clippy`, `nextest` | Clippy and nextest on the default build, with the Linux `--workspace` scope | `rust (linux)` (required) |
| `clippy-limits` | Clippy complexity negative controls and baseline check, with both feature modes on Linux | None. Local only. The feature-off pass is not a pull-request job. |
| `node-fixtures` | The `--check` mode of each Node parity fixture generator, and the HTTP route inventory diff, on Node 22.23.3 | `desktop-contracts.yml` |

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
writes one version 2 file per crate, removes stale crate files and the old shared
file, then runs the checks. It cannot authorize a statement-count increase.
Identical files keep identical contents, so updates in different crates merge
without editing a shared inventory or manifest.

Each statement cap comes from the same crate's file on `origin/main`. During
the layout migration, when main has no per-crate baseline files yet, the checker
uses that crate's entry in main's shared `desktop-rebuild-baseline.json`.
Once main uses per-crate files, a missing crate file means a zero cap; it never
borrows an allowance from a leftover shared file. A missing or extra local
crate file fails the exact inventory check, including zero-count crates. Every
per-crate file, local or on main, must use version 2.

`cfg!(feature = "desktop_rebuild")` expressions count as statements, including
raw names, raw feature strings and escaped feature values. Lexical `cfg` and
`cfg_attr` metas in macro call arguments also count as statements. The scanner
does not expand macros: it conservatively counts that text even if a particular
macro does not apply the meta as an attribute. Nested conditional metas count
once. For actual `#[...]` attributes, documentation metadata such as
`#[cfg_attr(docsrs, doc(cfg(feature = "desktop_rebuild")))]` does not count.
Opaque input `m!(cfg_attr(docsrs, doc(cfg(feature = "desktop_rebuild"))))`
counts as one statement: the scanner cannot determine what `m!` applies.

The version 2 placement baseline includes the 17 existing `cfg!` expressions:
pp-compat 2, pp-core 5, pp-desktop 1, pp-storage 9. When comparing with an
unversioned shared baseline on `origin/main`, the checker adds only the `cfg!`
inventory in main's source to its statement cap. Version 2 grants no further
allowance.

Except where noted below, each step runs the command lines of its workflow step without changes. The script sets the same environment that CI sets: `NEXTEST_FILTER`, `PP_TEST_NODE`, `VITE_PRINT_PARTNER_DESKTOP`, and `RUNNER_TEMP`. The cargo steps also get `INSTA_UPDATE=no`, so a snapshot mismatch fails the step instead of writing a `.snap.new` file. `RUNNER_TEMP` is under `$CARGO_TARGET_DIR/local-ci`, or under `rust/target/local-ci` if `CARGO_TARGET_DIR` is not set. The local command differs from CI in these ways:

- It does not install tools. If a tool is not on `PATH`, or if its version is not the version that CI pins, the step is reported as `SKIP` with the reason. The pinned versions are Node 24.21.0, ast-grep 0.50.0, actionlint 1.7.12 and zizmor 1.31.1. The `workaround-ratchet` and `node-fixtures` steps are also skipped when `PP_SCHEMA_PARITY_NODE` does not point to Node 22.23.3.
- `ast-grep` fetches `origin/main` without `--depth=1`, because a shallow fetch would make a full clone shallow.
- It does not set the CI-only `CARGO_PROFILE_*_DEBUG` and mold `RUSTFLAGS` values. They change debug info and the linker, not test results, and setting them would invalidate your local build cache.
- `nextest` always runs. Use `--skip nextest` for a shorter run. `--fast` runs `nextest-affected` instead.
- The nextest profile is `default` unless you set `NEXTEST_PROFILE`. CI uses `default`.
- `node-fixtures` runs `npm ci` with Node 22, so `web/node_modules` ends with Node 22 native modules. The next run's `web-assets` step installs them again with Node 24.

The command does not cover these checks:

- `rust (windows)` and `rust (macos)`. They need Windows and macOS runners. The command reports both as `SKIP`. The nightly job builds the workspace on those runners and does not run nextest.
- `rust (windows tests)`. It needs a Windows runner, and the command does not list it. See [Windows tests on pull requests](#windows-tests-on-pull-requests).
- `cargo deny check` (`desktop-deny.yml`). It starts only from a manual dispatch. To run it locally, use `cargo deny --manifest-path rust/Cargo.toml --all-features check`. The workflow also runs `cargo deny --manifest-path rust/fuzz/Cargo.toml --all-features --locked check`.
- The desktop workflows, which start only from `workflow_dispatch`, and the `contracts:verify` and `test:desktop-parity` steps of `desktop-contracts.yml`. The Rust image smoke is `python3 scripts/docker-rust-smoke.py` after `docker build -f Dockerfile.rust`.

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
- PR numbers print one row per PR head: its changed paths, whether the diff touches only `web/apps/ui`, the minutes from the first check run's start until every required check was green, the span of all completed check runs, and any check not green. The required set defaults to `actionlint`, `zizmor` and `rust (linux)`. `--required a,b,c` replaces it, for example once `ui-package` becomes required.

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

The hosted `ui` job runs the package lint, tests and build on Node 24.21.0, and `npm test -w @print-partner/rust-client`. The local `ui-package` step also runs the browser suite. The root `npm test` and `npm run build` scripts still list the old workspaces by name, so they do not run this package. Pull-request CI does not run those root scripts.

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
python3 rust/scripts/build-desktop-manifest.py --web web --node "$(node -p process.execPath)" --output rust/bundle-manifest.json --commit "$(git rev-parse HEAD)"
```

Then run the checks:

```sh
cargo fmt --manifest-path rust/Cargo.toml --all --check
cargo clippy --manifest-path rust/Cargo.toml --locked --workspace --all-targets --keep-going -- -D warnings
cargo nextest run --manifest-path rust/Cargo.toml --locked --workspace --no-fail-fast --no-tests=warn -E "$NEXTEST_FILTER"
```

`just ci` builds the first two items below in its `web-assets` step unless that step is skipped, and sets `PP_TEST_NODE` to the resolved path of `node` on `PATH` unless `PP_TEST_NODE` is already set. When `node` on `PATH` is a shim script or wrapper, that resolved path is not the file `web-assets` measures and the release-artifact check stops before nextest, so export `PP_TEST_NODE` as shown below before running `just ci` too. A direct workspace `cargo nextest` run needs all three first:

- `rust/bundle-manifest.json`. Git ignores it, so a fresh worktree has none, and the `pp-compat` build script stops the build with `Generate the measured desktop manifest before building Rust`. Run the three commands above, or `just ci --only web-assets`. `PP_DESKTOP_RELEASE_MANIFEST` can name a manifest in another place.
- `web/node_modules`, from `npm --prefix web ci`. Tests that start Node with `--import tsx` from the `web` directory, such as `pp-storage`'s `advisory_lock`, fail with `ERR_MODULE_NOT_FOUND` without it.
- `PP_TEST_NODE`, set to the Node that the manifest measured: `export PP_TEST_NODE="$(node -p process.execPath)"`, matching `--node` above and `just ci --only web-assets` (for a custom `PP_DESKTOP_RELEASE_MANIFEST`, point to the Node that manifest measured). Without it, workspace-manifest tests such as `pp-core`'s `runtime_m0` and `incompatible_storage` fall back to `/usr/bin/node`, and release verification fails with `Release artifact missing` if that file cannot be opened or `Node executable content mismatch` if it differs from the measured Node.

A plain workspace command compiles `desktop_rebuild` for the image crates. `--features desktop_rebuild` still turns that feature on for `pp-desktop` and `pp-drivers`. Those two stay off by default. `--no-default-features` builds the old path. Hosted CI runs the plain command only. Doctests are not a CI step. `just ci` still runs the Clippy limits scan, which builds both feature modes, on Linux.

On macOS, set `TMPDIR` to a short directory on a case-sensitive volume before you run the tests. The workflow step `Mount a short case-sensitive TMPDIR` has the commands. Also set `PP_TEST_NODE` to the Node executable, as CI does. Without it the runtime tests fall back to `/usr/bin/node`, which macOS does not have. To skip the same tests as CI, add `-E` with the `NEXTEST_FILTER` value of the macOS job. On Windows, the same `--workspace` commands apply. On Linux, the Tauri shell also needs the GTK and WebKit packages that the workflow installs with `apt-get`.

Clippy and fmt do not read the manifest contents. For a lint-only run, `echo '{}' > rust/bundle-manifest.json` is enough. The tests need the real manifest.

## Packaged-app smoke

`desktop-packaged-smoke.yml` builds the Linux `.deb` and the macOS app, installs each into a disposable location and launches the installed app on a schema 31 database, in the default build and in the `desktop_rebuild` build. It checks the native window, the persisted Build in the accessibility tree, a clean shutdown and the migrated rows. It starts only from `workflow_dispatch`. It is not a required check, and `just ci` does not run it. [Packaged-app smoke](packaged-smoke.md) explains each step and how to run it locally.

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

`rust (windows)` builds the workspace and runs no tests. `rust (windows tests)` runs only the tests named in [Windows tests on pull requests](#windows-tests-on-pull-requests). See [Windows test portability](#windows-test-portability) for what the pp-storage port (GRE-1154) changed. On Windows, `pp-compat` opens log entries through `pp-os` (GRE-1185) and runs its supervision tests against the Job Object and the private named pipe (GRE-1186); each Unix-only process-group, signal or parent-death test names its Windows counterpart in a comment. Its `ticket_t_2::` supervisor tests run on Windows with Node fixtures (GRE-1194). A clean leader exit waits up to 500 ms for the job to empty before the reap (see the pp-compat README). The HealthFixture tests stay Unix-only under GRE-1203. So do the paused-time tests whose pipe stdio or Unix health listener blocks Tokio's auto-advance. The other paused-time tests run on Windows. The respawn-during-verification test is no longer Unix-only (GRE-1204). `rust (windows tests)` runs it with the feature off and on (GRE-1795). `pp-core` reads and publishes its state files through `pp-os` handles (GRE-1188) and runs its runtime tests against the real Node child on Windows. On Windows its tests use `TerminateProcess` for SIGKILL, `NtSuspendProcess` and `NtResumeProcess` for SIGSTOP and SIGCONT, a junction where Unix uses a FIFO, an `icacls` deny where Unix uses mode 000, and a ToolHelp process snapshot where Unix reads `/proc` or `ps`. A runtime test whose restart fails prints the redacted recent logs. Each test that stays Unix-only names its Windows counterpart in a comment. `pp-gateway`'s tests reach a stand-in Node child through `pp_compat::Endpoint::fake`, which is a Unix socket on Unix and the core-owned named pipe on Windows; on Windows they plant symbolic links with `symlink_file`, the receipt test plants no socket or FIFO because Windows directories cannot hold them, and the 0600 mode checks stay Unix-only. `pp-server` stops on Ctrl+C, Ctrl+Break, a console close, logoff or system shutdown on Windows, where Unix uses SIGINT and SIGTERM (GRE-655); `tests/stop_signal.rs` sends Ctrl+Break on Windows and SIGTERM on Unix. The gateway serves each connection through `pp_net::http1::serve`, which reads and discards unread request bytes for up to two seconds before it closes, so a Windows client receives the early 413 for an over-limit body instead of a reset (GRE-1264). `pp-gateway`'s `slice0_mount_over_limit_body_gets_the_413_over_a_real_socket` sends a 32 MiB body to the real gateway, and `pp-api`'s `auth_admission_parity` compiles `rust/crates/pp-net/src/http1.rs` into its harness and serves its router through it. `pp-desktop` builds the Tauri shell on Windows (GRE-656): dialogs come from rfd, links and folders open through `rundll32 url.dll,FileProtocolHandler`, Ctrl+C, Ctrl+Break and a console close stop it, and single-instance focus uses a private named pipe (see the pp-desktop README). The Windows job builds and tests `pp-os`, including
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

The nightly `rust (macos)` job builds the workspace and does not run nextest. A local macOS test run still needs the filter and the volume below. **Native macOS bundle** starts only from `workflow_dispatch` and still contains its lifecycle tests.

GRE-645 removed the earlier group skip. Hosted run 38925272452 mounted `/Volumes/pptmp` with `apfs, local, nodev, nosuid, journaled, nobrowse` and recorded `exec /usr/bin/node` exiting 126: `/bin/sh: line 0: exec: /usr/bin/node: cannot execute: No such file or directory`. Those scripts now `exec "$NODE"` from `PP_TEST_NODE`. Run 38941881212, with `--test-threads 1`, still failed 3 of 74 because one release verification hashed about 42,203 dependency files in 10.7 to 19.7 seconds. Each start verifies once, on a blocking thread, before the 32-second readiness budget (GRE-661). Each restart verifies again after `Starting` is published. The crash-guard status fuse covers that hash plus the 30-second probe. The measured-release parent-death test starts its 30-second acknowledgement when the helper publishes `release-verified`, not when the helper process is spawned. The config test's 3-second deadline starts when the re-executed child enters. The GRE-671 backoff test never ran Node; it runs with the rest of `ticket_t_2::`.

To collect twenty consecutive green hosted `rust (macos)` job links after tip is green, use `scripts/gre645_macos_soak.sh`. Default mode is dry-run. `--execute` is Chad-gated: it dispatches `rust-cross-os.yml` on the chosen ref, waits for each `rust (macos)` job, appends JSONL evidence (`run`, `job`, `sha`), and stops on the first red. It does not change packing or skip lists. Prove with `python3 -B -m unittest discover -s scripts -p test_gre645_macos_soak.py`.

## Other Rust workflows

- **Desktop Rust (hosted)** (`desktop-fleet-rust.yml`) keeps the ast-grep ratchet job and starts only from `workflow_dispatch`. `just ci` still runs the ratchet. It does not build or test Rust; `rust (linux)` does that on a pull request, a push to `main` and a merge group.
- **Native macOS bundle** (`desktop-native-macos.yml`) builds the unsigned macOS app and runs the macOS storage tests. It starts only from `workflow_dispatch`. The job body still selects by event and label when someone dispatches it:
  - On a dispatched run the event is `workflow_dispatch`, so the plan selects every job, including the Intel bundle. Pull requests, pushes and merge groups do not start this workflow.

  Every macOS job also needs a successful `linux-precheck`, so a broken web build never starts a macOS runner. Pull requests, pushes and merge groups do not start this workflow. A manual run keeps its own concurrency group.
  The source fixture registers the named `cleanup_source_fixture` function with `trap cleanup_source_fixture EXIT` after a successful attach. It captures `$?` immediately and exits with that status on every cleanup path. Cleanup targets only this step's owned mount. It tries normal detach of the mount three times, with `sleep 2` between failures. If all three fail, it records holders and then forces detach of the whole-disk device that `hdiutil attach` printed first, again up to three times with `sleep 2` between failures. A forced unmount can succeed while ejecting the backing device returns `Resource busy`. The mount path is then an ordinary directory, so the forced retries target the device. The explicit sleeps total at most eight seconds. The `hdiutil` calls and holder diagnostics add time, so eight seconds is not a wall-clock cleanup bound.

  After normal detach retries fail, `lsof +f -- "$fixture_mount"` records open files on the source fixture's owned APFS filesystem before forced detach. A diagnostic failure, or failure of all three forced attempts, warns without changing the test exit status. Run `python3 scripts/test_native_image_cleanup.py -v` for executable cleanup fixtures. The same suite runs in `macos-storage-gate`, including a macOS-only disposable APFS image held open by a child process. Linux skips that native proof.

  GRE-839's focused targets are `CleanupTest.test_cleanup_function_preserves_all_exit_status_cases` and `CleanupTest.test_cleanup_function_passes_actionlint_shellcheck`. Run them with `python3 scripts/test_native_image_cleanup.py -v CleanupTest.test_cleanup_function_preserves_all_exit_status_cases CleanupTest.test_cleanup_function_passes_actionlint_shellcheck`. Put actionlint 1.7.12 and ShellCheck on `PATH` to run the analysis target. It skips when either tool is absent. The existing Actionlint workflow checks the workflow independently.

  The status target executes the complete extracted workflow step through `bash --noprofile --norc -e -o pipefail`, using disposable files and executable tool doubles. Its 35 cases combine statuses 0, 1, 23, 130 and 255 with normal detach success on each attempt and all diagnostic-success/failure and forced-detach-success/failure combinations. Each case asserts the exact exit status, combined output, ordered command arguments and preserved fixture and unrelated sentinel bytes. The analysis target requires empty successful output from both tools, then injects an unquoted mount expansion into the cleanup body and requires SC2086 from each tool to prove the function is inspected. The cross-OS workflow has no matching EXIT trap to extract.
- **Desktop runtime** (`desktop-runtime.yml`) builds `pp-api` and `pp-server` and runs the proofs that need the built server on Linux: the serial `runtime_m0` lifecycle log, the Python lifecycle, upload and gateway drivers, the source artifact checks and `verify.py`. It starts only from `workflow_dispatch`. Unit tests, clippy and fmt are not repeated here; `rust (linux)` runs them. After the Playwright specs, and only when that step succeeded, it writes an e2e receipt with `scripts/e2e_receipt.py`. The writer also takes the step's outcome and refuses any outcome but `success`, so a failed, cancelled or skipped step gets no receipt and the `e2e-receipt` artifact is not uploaded. It refuses a report whose suites, specs, tests, results, attachments, stats or `errors` do not have the types Playwright's JSON reporter declares (only a suite's child `suites` and an attachment's `path` may be absent), a run ID, attempt or PR below 1, and a head or checkout that is not 40 lowercase hex. Its `timestamp` is the job's start time (`PP_JOB_STARTED`), so the same input gives the same bytes. `receipt.json` holds the repo, the optional `pr` dispatch input, the exact head (`github.event.pull_request.head.sha`, or `github.sha` for a dispatch), the run, the pass/fail/skipped/flaky counts (the writer reads the checkout from `git rev-parse HEAD`, run in its working directory with only `PATH`, `HOME`, `LANG`, `LC_*` and `SYSTEMROOT` from the environment, so no option or `GIT_*` variable overrides it, and refuses a head other than that checkout, and a `pr` whose live `refs/pull/<n>/head` is not that commit), every screenshot with its size and SHA-256, and the URL of the `e2e-screenshots-<run>-<attempt>` artifact. Screenshot copies are numbered in report order, zero-padded to the width of the count, and `collect` refuses an existing `screenshots` directory even when it is empty. The `e2e-receipt-<run>-<attempt>` artifact holds the receipt and the screenshots; CI sets `PP_E2E_SCREENSHOTS=on` so Playwright screenshots every test, not only failures.

When `rust (macos)` fails on the nightly run or on a manual dispatch, an alert job opens an issue labelled `ci-macos-main` for that workflow run, or comments on the existing open issue for the same run. The alert looks for the job name `rust (macos)`. A Windows build failure and a Linux failure do not alert. Only the alert job receives `issues: write`. Native macOS bundle does not run on a push, so it does not open that issue.

Required Rust and lint jobs fail if their input-selection job fails, so a broken filter or failing filter test cannot pass through the required checks. Failure alerts run independently with a separate issue per workflow run, so a shared concurrency queue cannot drop pending alerts. Reruns comment on the existing open issue for that run.

## Ephemeral PR runners

Pull request jobs run on GitHub-hosted runners. pp-local runs only pushes to `main` and manual runs, never a pull request or a merge group. The runner selector has no Blacksmith or pp-iso path. Each hosted job gets a fresh VM. A new push to a pull request cancels the older run (`cancel-in-progress` on `pull_request`), except `linear-pr-sync.yml`, which must not be cancelled. `main` pushes keep the one-running, one-pending rule.

### Linux Rust runner

The Linux parts run on `[self-hosted, linux, x64, pp-local]` for a push to
`main`. A pull request and a merge group run them on `ubuntu-latest`. A merge
group holds PR code that has not landed, and the shared pp-local cache only
sees code from `main`. The nightly schedule and a manual dispatch do not run
these parts. pp-local is six ephemeral JIT slots, two on each of three VMs, and
each job runs in a fresh `pp-slot-N` container. The required check name stays
`rust (linux)` on either runner. That check is the hosted aggregate, not a
part. Hosted runs use six nextest partitions (GRE-1892, up from four in GRE-1664) because one job's nextest was
the whole PR wall (1170s of tests on the GRE-1649 warm run, 28m40s end to end).

PRs stay off pp-local because it ran the PR suite more slowly than hosted. On PR
#1349, the old `rust (linux) build` job took 60m02s on pp-local with a 99.57 %
sccache hit rate, and 87m42s with a cold cache, against 33m40s on a hosted
runner. On `main`, pp-local still moves the Linux job off the hosted pool that
PRs wait for.

The `changes` job picks the runner with `scripts/ci-rust-runner.py` and passes it
on as a job output. It picks `ubuntu-latest` when:

- the event is a pull request, a merge group, or the nightly schedule;
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
`CARGO_INCREMENTAL=0`. The pp-local part ends with `sccache --show-stats`. pp-local parts skip `Swatinem/rust-cache`: the Actions cache
is not needed there, and its post-job cleanup can delete registry sources that
another container is compiling. `scripts/setup-ci-linux.sh` and
`dtolnay/rust-toolchain` install the packages and Rust that hosted images ship.
[Cargo cache trust](cargo-cache-trust.md) covers the shared registry.

### Actions cache

The repository's Actions cache is limited to 10 GB, so older entries are evicted
first when it fills. Hosted parts set `shared-key: rust-linux`, because the
action otherwise includes the job id and a rename misses the saved entry. The
`1/6` part saves that key on `main` and on pull requests. The other parts only
restore. One save avoids six writers racing on the same key. pp-local does not use this cache. The Rust image job
saves its Buildx cache (`scope=dockerfile-rust`, `mode=max`) on pull requests
too. The Tauri CLI archive still saves only from `main`. `actions/setup-node`
and `actions/setup-python` have no save switch; they save only when the lockfile
key is new.

The composite action `.github/actions/linux-gui-deps` caches the `.deb` files that
`apt-get install` downloads for the Linux GTK and WebKit packages (`rust (linux)`, and
`desktop-e2e` or `desktop-webkit` when someone dispatches them), so a slow Ubuntu mirror cannot stall the package download
(GRE-1229); `apt-get update` still contacts the mirror on every run. The cache steps run only on GitHub-hosted runners; self-hosted runners skip
them and install from the mirror as before. The key is the runner OS and architecture, the
runner image version, the ISO week and a hash of the package list. The week limits how long a
cache goes stale: Ubuntu security updates change what `apt-get update` resolves, a restored
cache then still downloads the newer packages, and the first hosted run on `main` of a new week saves a
fresh set. Only a GitHub-hosted run on `main` that is not a pull request saves, and only when the exact key
missed. A push of `rust (linux)` that `scripts/ci-rust-runner.py` sends to a hosted runner is that run. The nightly build does not install these packages. A run on pp-local skips the cache steps and saves nothing.
Pull requests restore only. Each install logs
`apt install took N s (cache hit: ...)`.

### Superseded runs

Pull requests cancel superseded runs. Rust cross-OS and the Rust image also cancel older runs with the same merge-group ref and head SHA. Every CI workflow
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

`desktop-fleet-rust.yml` no longer runs on a push. `just ci` runs the ast-grep ratchet and the Clippy limits scan. Tag pushes, other branches, merge groups, the schedule and manual runs keep their
own `run_id` group, except Rust cross-OS and the Rust image merge groups, which use the queue ref and merge-group head SHA. Queue refs change when entries are superseded, so concurrency does not cancel dropped or superseded entries by itself. A rerun (attempt 2 and
later, which `main-red` starts for failed `main` runs) also keeps its `run_id` group, so it
never replaces the pending run and, because the original has finished, does not wait.

| Event | Group | `cancel-in-progress` |
| --- | --- | --- |
| `pull_request` (except `linear-pr-sync.yml`) | workflow and ref (or PR number) | `true` |
| first attempt of a `push` to `main` | one group per workflow | `false` (one running, one pending) |
| `push` of a tag or another branch, other `merge_group`, `schedule`, `workflow_dispatch` | `run_id` | `false` |
| Rust cross-OS / Rust image `merge_group`, including reruns | workflow, queue ref and merge-group head SHA | `true` |
| rerun of another non-PR run | the original run's `run_id` | `false` |
| rerun of a `pull_request` run | same as the PR group | `true` |

The head SHA isolates different groups if GitHub reuses a queue ref name. A
rerun with the same ref and merge-group head SHA still shares the original group
and cancels an older run of that same pair. This concurrency behavior does not
determine whether a disappeared queue entry merged; the canceller uses PR state.

`pages.yml` keeps `cancel-in-progress: false`, because cancelling a Pages deploy
can leave the site half-published, and `release.yml` runs only on tags.

The `ui` job times out at 20 minutes. Each Linux part times out at 45 minutes, and the `rust (linux)` aggregate times out at 5. `rust image` times out at 45 minutes and `rust image (arm64)` at 60. The nightly build times out at 90 minutes.

## Daily loop

`daily-loop.yml` (workflow `Daily loop`, job `daily loop`) builds the Rust-only image from `Dockerfile.rust` at the commit under test and walks the twelve steps of [the daily loop](../product/daily-loop.md) in a browser. [Score](../product/daily-loop.md#score) explains the states and the markers.

- **Triggers.** Each push to `main`, manual dispatch, and a ready pull request that carries the label `ci:daily-loop`. Adding the label starts a run, and removing it cancels a running one. Each later push, label change or draft change on that PR starts another and cancels the older one. A PR run tests the PR's head commit, not the merge commit. Without the label, or on a draft, the job reports skipped. Every push to `main` gets its own run.
- **Not a required check.** It is not in the ruleset and does not run in merge groups. `main-red.yml` reports a failed push run on `main`, which includes a marked step that passed.
- **Result.** The log prints one line per step and `Daily loop: N of 12`, and the job summary holds the same table. The artifact `daily-loop-score` holds one file, `daily-loop-score.json`: `{commit, score, steps: [{number, title, state, ticket}]}`, where `commit` is the full SHA, `state` is `pass`, `expected_fail`, `fail` or `unexpected_pass`, and `ticket` is a `GRE-n` string or null. The Control Room scorecard reads that file, so keep those names. A run that stops before the tests start (the title check, the image build or the container start) has no score file, artifact or table.
- **Locally.** `npm --prefix web run test:daily-loop` runs the same suite. It needs Docker, Chrome or Chromium, and `npm --prefix web ci`. It builds the working tree and records `HEAD` as the commit, so commit first. It writes `web/test-results/daily-loop/daily-loop-score.json` and removes what it created when the run passes, fails or is stopped: the container, the volume, the network and the image tag, each named `pp-daily-loop-<8 hex digits>`, the simulator container `pp-daily-loop-<8 hex digits>-sim`, the Spoolman stand-in container `pp-daily-loop-<8 hex digits>-spoolman`, the importer container `pp-daily-loop-<8 hex digits>-import`, the import's volume `pp-daily-loop-<8 hex digits>-imported`, Step 12's restore container `pp-daily-loop-<8 hex digits>-restore` and its volume `pp-daily-loop-<8 hex digits>-restored`.

  The run creates two mode-0700 folders. Under the inherited temporary directory, `pp-daily-loop-<8 hex digits>-<6 characters>` holds the synthetic Node installation, the simulator's API key, and temporary files from preflight and fixture generation. Under `/tmp`, `pp-daily-loop-<8 hex digits>-browser-<6 characters>` holds Chromium's profile and Playwright's output and downloads, including Step 12's backup. The short browser path keeps Chromium's Unix socket address within the Linux limit.

  After a stop, preflight has up to 60 s to finish and reap its fixtures. Docker create commands started by the runner have the same deadline to settle. Other running commands receive the stop signal and whatever remains in their group receives SIGKILL after 30 s. The runner waits for the command group and checks for detached fixtures and Chromium before removing the folders. Cleanup removes and lists again, 3 s apart, until three listings in a row are empty, within six passes ([`runner.mjs`](../../web/test/daily-loop/runner.mjs)). This catches containers the daemon creates after a stopped or timed-out `docker run`.
- **Printer simulator.** The run starts the [Moonraker simulator](../../web/test/moonraker-sim/README.md) as a second container on the run's own Docker network, with the network name `pp-sim`, on the Node image that `Dockerfile.rust` pins for its `web` stage. The product container joins the same network and reaches the printer at `http://pp-sim:7125`. Only the simulator's control port (7200) and the product's 8080 are published, each on `127.0.0.1`; nothing binds to the network's gateway. Before the tests, the run checks that the simulator's address is RFC 1918, which the outbound rule accepts, that the product container resolves `pp-sim` to it and gets `klippy_state: ready` from `/server/info` with the key, and that no port is published off loopback. The tests read `PP_DAILY_LOOP_SIMULATOR` (the simulator container), `PP_DAILY_LOOP_PRINTER_URL` and `PP_DAILY_LOOP_PRINTER_KEY_FILE`.
- **Spoolman stand-in.** The run starts `web/test/daily-loop/spoolman.mjs` as a third container on the same network, with the network name `pp-spoolman`, on the same Node image. It answers the Spoolman paths the core calls (`/api/v1/filament`, `/api/v1/spool`, `/api/v1/spool/{id}` and `PUT /api/v1/spool/{id}/use`) from one filament on two spools, plus an archived spool, and takes a use's grams off its spool. The product container reaches it at `http://pp-spoolman:7912`. Only its control port (7913) is published, on `127.0.0.1`. It serves `GET /_stand-in/log` (spools, request counts and every use) and `POST /_stand-in/stock` (sets a spool's grams). Before the tests, the run checks that its address is RFC 1918, that the product container gets `/api/v1/info` from it, and that no port is published off loopback. No real Spoolman server is contacted. Step 11 reads `PP_DAILY_LOOP_SPOOLMAN` (the stand-in container) and `PP_DAILY_LOOP_SPOOLMAN_URL`.
- **Imported data.** Before the image build, the run writes the [synthetic Node installation](../storage.md#synthetic-installations) with `npm run fixture:synthetic -- --seed 1138` into a new temporary folder; when `better-sqlite3` has no native binding (`npm ci --ignore-scripts` installs only the prebuilt ones the package ships), it builds one first with the package's `build-release` script, which needs Python, `make` and a C++ compiler. Step 2 runs `pp-server import` from a one-off container that mounts that folder read-only at its own host path, so the import rebases its Source paths, and writes a second new volume, because the import refuses a folder that holds data. Step 2 then replaces the product container on that volume under the same name and network, so `pp-sim` stays reachable, and creates the owner's account again, because the synthetic installation holds none. Steps 3 to 12 run on the imported installation; Step 6 edits `Synthetic Build 02` and Step 10 checks off a unit of `Synthetic Build Anchor 47u6p`. The tests read `PP_DAILY_LOOP_NODE_INSTALLATION`, `PP_DAILY_LOOP_IMAGE`, `PP_DAILY_LOOP_NETWORK`, `PP_DAILY_LOOP_IMPORTER` and `PP_DAILY_LOOP_IMPORT_VOLUME`; `web/test/daily-loop/installation.mjs` holds the container arguments.

## Merge queue

GitHub's merge queue is available because the repository belongs to the Print-Partner organization. The queue was turned on 2026-11-09 ([CI controls](ci-controls.md#turn-on-the-merge-queue)). A queued PR merges only after the required checks pass on its merge group: a temporary `gh-readonly-queue/main/pr-<n>-<sha>` commit that holds `main` plus every PR ahead of it in the queue.

| Required check | Workflow | On `merge_group` |
| --- | --- | --- |
| `actionlint`, `zizmor` | `actionlint.yml` | Both linters run in full. `ci-path-filter.py` selects the `actions` kind on merge groups, because the group's diff against `main` is not the PR's diff. |
| `rust (linux)` | `rust-cross-os.yml` | The aggregate. On `ubuntu-latest` it waits for fmt, clippy and six nextest hash partitions. The filter is `all()` because a merge group selects the `rust` kind without reading the PR diff. |
| `ui` | `web-ci.yml` | Lints, tests and builds the replacement UI and tests the generated Rust client. |
| `release-audit` | `web-ci.yml` | Audits public release inputs on the combined commit. |
| `rust image` | `rust-image.yml` | Builds the amd64 Rust-only image and smoke-tests sign-in and the replacement UI. |

`rust image (arm64)` also runs in the queue and reports separately as an advisory check. Use the labelled [Daily loop](#daily-loop) run for changes that cross the cutover workflow.

GRE-1466 (2026-11-12) kept merge groups to fmt and clippy because two measured groups took 47 and 49 minutes. GRE-1649 puts nextest back on the merge group. GRE-1664 splits that hosted nextest into four hash partitions so a warm pull request finishes in under 20 minutes. The commands are still one default-feature pass.

On its own head a PR runs clippy and the tests that `scripts/ci-affected-nextest-filter.py` selects: the changed crates and the crates that depend on them or read their sources, or `all()` when a shared input changed. A PR that changes no Rust input skips `rust (linux)`. The merge group runs `all()`.

`scripts/test_merge_group_workflows.py` pins the `merge_group` trigger on the required workflows, Web CI and the Rust image, and the merge-group runner routing.

[CI controls](ci-controls.md#turn-on-the-merge-queue) records the merge_queue rule that turned the queue on.

The Rust client generator writes route modules under `web/packages/rust-client/src/routes/` and named schema modules under `src/schemas/`, with generated indexes preserving package imports. After merging main, rerun the rust-client generator; do not hand-merge generated files. Its `--verify` checks the recursive file inventory and hashes without Cargo; `--check` also compares fresh Rust output and runs contract round trips.
