# Cargo cache trust for same-repository PRs

The Linux Rust jobs in Rust cross-OS run on pp-local again for pushes to `main`,
merge groups and manual runs (GRE-1042). Pull requests run on Blacksmith runners by
default, or on GitHub-hosted or [pp-iso](pp-iso.md) runners when `PP_PR_RUNNER` selects
them (never pp-local), so PR code does not run on the hosts that share the containers'
host-mounted Cargo registry and sccache. This assessment, written for the earlier
local-runner configuration that ran PRs locally, describes the risk that keeping
PRs off pp-local avoids.

Same-repository PR jobs share a Linux Cargo cache trust domain. A fresh runner
does not make dependencies private when its host mounts persistent writable
cache storage. Treat a later PR's dependency inputs as influenced by earlier
PR code until the runner host enforces cache isolation.

This GRE-887 assessment covers repository configuration at
`963c97526f990f1bb7c8ea5f1193c690da4d6ecc`, after [PR #601](https://github.com/poitee/PrintPartner/pull/601)
merged. It changes no runner, cache, workflow, permissions, or application behavior.

## What the repository establishes

[Rust cross-OS](../../.github/workflows/rust-cross-os.yml) selects pp-local only on
`push` to `main`, `merge_group` and `workflow_dispatch`, never on a `pull_request`. A pull request
uses Blacksmith, hosted or, when the head repository equals `github.repository`, pp-iso, as
`PP_PR_RUNNER` says; the nightly schedule uses `ubuntu-latest`.
There is no actor exception, so a same-repository Dependabot PR satisfies the
same routing predicate as a human-authored PR. The label selects a runner pool,
not a cache namespace.

[Dependabot configuration](../../.github/dependabot.yml) enables weekly Cargo
lockfile updates for `/rust`. Those updates can introduce new dependency code.

[Embedded-driver smoke](../../.github/workflows/desktop-e2e.yml) and
[WebKit smoke](../../.github/workflows/desktop-webkit.yml) run on
`ubuntu-24.04`, hardcoded in their `runs-on` lines. Embedded-driver smoke and WebKit smoke run `Swatinem/rust-cache` on every runner; Rust cross-OS skips it
only when the runner kind is pp-local (`PP_RUNNER_KIND`). Hosted, Blacksmith and pp-iso
runners and the shared Rust steps on macOS and Windows retain that action.

[Desktop runtime and source-artifact jobs](../../.github/workflows/desktop-runtime.yml)
run Cargo on hosted `ubuntu-24.04`. More generally, any local job can modify a cache
if the host exposes that cache writable, even if its normal steps do not use Cargo.

[The existing CI documentation](ci.md#ephemeral-pr-runners) records the runner
routing and the reason pp-local must not run PR-editable jobs. The action's post-job cleanup can delete registry
sources while another container compiles them. Preserve this skip under either
alternative below. Changing an Actions cache key does not isolate a host mount.

[Linux prerequisite setup](../../scripts/setup-ci-linux.sh) installs packages.
It does not assign Cargo cache namespaces or mount storage. The tracked
workflows do not define the host-managed volume names, mount paths, writable
subdirectories, ownership, or cleanup policy. The shared writable-volume finding
in [GRE-887](https://linear.app/greetz/issue/GRE-887/review-shared-cargo-cache-trust-for-same-repo-prs)
is the assessment's operational premise, not a live mount inspection.
Exact exposure of `registry/src`, `registry/cache`, `registry/index`, and `git`
therefore needs confirmation by the runner infrastructure owner.

## The boundary and its limits

PR code executes before approval and can write any cache paths the runner makes
writable. Another job reading the same paths can consume modified dependencies
or fail because files disappear. Dependency build scripts and procedural macros
make dependency integrity relevant to code execution as well as build results.
This is a cross-PR integrity and availability risk even without repository secrets.
This assessment does not demonstrate exploitation or examine real cache contents.

Cargo's [home layout](https://doc.rust-lang.org/cargo/guide/cargo-home.html)
distinguishes downloaded archives, extracted registry sources, and Git checkouts.
It also includes configuration, credentials, and installed executables outside
the download directories. Sharing an entire writable Cargo home would widen the
boundary beyond dependency downloads. The repository does not establish that
the host shares that entire home.

`--locked`, used by these build jobs, constrains dependency resolution. It is
not a filesystem isolation mechanism. Cargo locks coordinate cooperating Cargo
processes. They do not prevent arbitrary PR code from changing writable files.
A private target directory does not isolate a shared dependency source directory.
Ephemeral runner teardown removes private state but leaves a persistent host
cache available to later jobs.

The current routing sends pull requests to Blacksmith, hosted or pp-iso runners and
keeps pp-local for main pushes, merge groups and manual runs. There is no
repository-configured host-volume path from PR jobs to pp-local. It
does not certify separate infrastructure, image-building, or cache promotion
processes that are absent from this repository.

These routing and permission claims describe the checked-in workflows. PRs can
also edit workflow files. Repository YAML alone cannot enforce cache isolation
against a malicious PR. The runner host must enforce the boundary independently.

The assessed local workflows use `contents: read` and checkout with
`persist-credentials: false`. The CI documentation reports no `secrets.`
references in locally routed jobs. These reduce credential exposure but do not
make a writable dependency cache trustworthy. Same-repository status and
Dependabot identity are routing facts, not approval of dependency bytes.

## Alternatives against this configuration

### Host-enforced private writable cache

Prefer a private writable Cargo home for each job as the first infrastructure
change. A host-enforced namespace containing repository identity, PR number,
run ID, run attempt, and job identity prevents reuse across jobs. An owner may choose
per-PR reuse to reduce downloads, but that retains influence across commits,
reruns, and parallel jobs within the same PR. Per-repository, per-runner-slot,
or lockfile-only names still allow different PRs to share writable data.

The host must choose and enforce the mounts before PR code executes. A PR-set
`CARGO_HOME` or workflow cache key alone is insufficient if the original shared
writable path remains reachable. Keep Cargo configuration, credentials,
executables, extracted sources, and Git checkouts private. Private target
directories remain private. Do not promote PR-written cache contents into a
shared trusted seed or a main/release cache.

This fits the existing ephemeral runner model but costs downloads and storage.
Quota enforcement and cleanup after job termination belong to the runner host.
Per-PR reuse also needs retirement when the PR closes. The tracked workflows
cannot enforce host mounts or their lifecycle, so this change requires the
infrastructure owner rather than a repository-only patch.

### Read-only seed with private writable Cargo home

A trusted process could populate a versioned dependency seed and expose it
read-only to PR jobs. Copy approved download data into a private writable Cargo
home, or use a copy-on-write arrangement with a private upper directory.
Keep extracted sources and Git checkouts private. Avoid hard links to a
writable shared seed. Publish seed updates atomically and retain each version
until its readers finish. PR jobs must have neither seed write access nor
access to another writable alias of the seed.

Mounting the active Cargo home or registry directly read-only is not a drop-in
replacement. Cargo needs writable state for cache misses, extraction, index
updates, and locking. Dependabot changes can introduce dependencies absent from
the seed. Private writable storage permits those downloads without publishing
them back. Cargo's [fetch options](https://doc.rust-lang.org/cargo/commands/cargo-fetch.html#manifest-options)
define `--frozen` as locked and offline. Offline operation additionally requires
all needed dependencies to be available. Neither flag supplies write isolation.

This option can preserve warm downloads while separating writers, but it needs
host mount changes, a trusted seed producer, and a tested miss path. It also
needs an inventory of any shared compiler cache or tool directories. This
repository does not define such a seed producer or shared sccache service.
Do not claim a performance improvement until the infrastructure owner measures
cold, warm, and cache-miss builds under the existing build throttle.

## Read-only evidence checks

Run these commands from the repository root. They read tracked configuration
and do not invoke Cargo, inspect credentials, mount storage, or mutate a cache.

```sh
git rev-parse HEAD
git show 963c97526f990f1bb7c8ea5f1193c690da4d6ecc:docs/development/ci.md | sed -n '136,175p'
rg -n 'runs-on:|permissions:|contents:|persist-credentials:|Swatinem/rust-cache|runner.environment.*self-hosted.*runner.os' .github/workflows/rust-cross-os.yml .github/workflows/desktop-e2e.yml .github/workflows/desktop-webkit.yml
rg -n 'CARGO_HOME|CARGO_TARGET_DIR|sccache|volume|mount' scripts/setup-ci-linux.sh .github/workflows
git ls-files '*runner*' '*compose*' '*cargo*'
```

Rust cross-os skips the Linux cache action only on pp-local (`PP_RUNNER_KIND`), and the
other two workflows hardcode `ubuntu-24.04`. The pinned CI text
records the host-managed volume and private target directories. The remaining
searches help locate configuration for owner review. Absence of a match does
not prove live mount isolation. On a later revision, re-read any changed jobs
and the CI documentation before carrying this conclusion forward.

## Infrastructure follow-up

The assessment is complete. Cache isolation remains unimplemented. The runner
infrastructure owner needs to confirm the mount inventory and choose private
per-job storage or a trusted read-only seed with private writable storage.
Keep the existing local Linux `rust-cache` skip in either design.

Acceptance evidence for that separate change needs two synthetic PR jobs. A
marker written through job A's writable dependency path must be absent from
job B's dependency inputs. A cache miss must succeed in B's private storage.
If a seed is used, writes through every job-accessible alias must fail. Cleanup
of A must leave B's in-progress sources intact. Also confirm that pull requests never
route to pp-local and that no PR output is promoted into a trusted
cache. Those checks require approved infrastructure work and are not performed
by this document-only slice.
