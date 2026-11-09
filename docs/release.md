# Release

This page will describe how Print Partner builds, signs and publishes each release. It is a stub until the release milestone lands.

## Today

A tag that matches `v*.*.*` starts `.github/workflows/release.yml`. The workflow runs the web quality checks and the manifest validation. Then it builds the Node Docker image, pushes it to `ghcr.io/print-partner/print-partner` and creates the GitHub Release. [Release history](../CHANGELOG.md) lists past releases. [Operations](../OPERATIONS.md) describes upgrades.

### Image location after the organization move

Releases up to 3.3.0 are at `ghcr.io/poitee/print-partner`. Container packages belong to an account and did not move with the repository on 2026-11-07, so the release workflow publishes the next release to `ghcr.io/print-partner/print-partner`, with `org.opencontainers.image.source` set to `https://github.com/Print-Partner/PrintPartner` so GHCR links the new package to this repository. The workflow's `GITHUB_TOKEN` keeps `packages: write`; no new secret is needed. Old tags stay pullable at the old path.

Until that release, the Compose files, `README.md`, `web/DEPLOY.md` and `OPERATIONS.md` still name the old path, because 3.3.0 exists only there. `node scripts/release.mjs prepare` rewrites them to the new path along with the version. `scripts/verify-print-partner/compose.verify.yml` is not a release sink and stays pinned to 3.3.0 at the old path.

The first push creates the organization package, so Chad checks two settings before the next tag:

1. Organization **Settings > Packages**: members may create public container packages, so the release job's `GITHUB_TOKEN` can create `print-partner`.
2. After the release job pushes, the `print-partner` package page: visibility is public, so the Compose files pull without logging in, and **Manage Actions access** lists `PrintPartner` with write access.

## Check health parity fixtures

Before tagging a release, after updating the app version or release metadata:

1. Use Node 22.x and npm 10 or later, then run `npm --prefix web ci` from the
   repository root.
2. Regenerate the version-pinned health snapshot:

   ```sh
   "${PP_TEST_NODE:-node}" --conditions=development fixtures/parity/auth-health/generate.mjs
   ```

3. Review the diff in `fixtures/parity/auth-health/node.json` and commit the
   expected version or release changes with the version bump. Only Node's
   runtime version is normalized. Investigate unrelated response changes before
   accepting a new snapshot.
4. Run the freshness check and the Rust comparison in both feature modes:

   ```sh
   "${PP_TEST_NODE:-node}" --conditions=development fixtures/parity/auth-health/generate.mjs --check
   CARGO_BUILD_JOBS=3 cargo test --manifest-path rust/Cargo.toml --locked -p pp-api --test auth_health_parity
   CARGO_BUILD_JOBS=3 cargo test --manifest-path rust/Cargo.toml --locked -p pp-api --test auth_health_parity --features desktop_rebuild
   ```

The generator must report `auth health: 6 registered-handler cases passed`.
The Rust suite also checks freshness through `node_health_snapshot_is_current`.
See the [Rust test prerequisites](../rust/README.md#run-pp-api-health-parity-tests)
for selecting the same Node runtime used by `npm ci` through `PP_TEST_NODE`,
and the [fixture contract](../fixtures/parity/auth-health/README.md)
for intentional differences. Fixes to production health behavior require a
separate change.

## Target

- macOS first, as signed and notarized DMGs.
- Windows installers and Linux packages before 1.0.
- Signed updates that refuse an unsafe schema change.
- The single-user Rust Docker image.

## To be written

- How to cut a release and who approves it.
- How signing keys are held and rotated.
- How the updater checks schema compatibility and recovers.
- The startup, memory and idle-CPU measurements for each release.
