# Packaged-app smoke

The **Desktop packaged smoke** workflow (`.github/workflows/desktop-packaged-smoke.yml`) launches the installed desktop app, not a test build, on a database from an older schema. It checks what a user sees after an upgrade: the app starts, a window opens, and the Builds they had are still there. It is not a required check yet.

The macOS bundle job's `installed_bundle_start_health_shutdown` test checks backend health only. The embedded-driver smoke (`desktop-e2e.yml`) drives a debug build with the WebDriver plugin compiled in, on an empty data directory. This smoke covers the gap between them.

## What a run does

The macOS job runs on `macos-26` arm64, a GitHub-hosted runner. The Linux jobs run on `ubuntu-24.04`, a GitHub-hosted runner, except on a pull request, where they run on `blacksmith-4vcpu-ubuntu-2404`, a paid runner that is not GitHub-hosted, unless `PP_PR_RUNNER=hosted`. No job runs on the pp-local runners. The steps below describe Linux; [macOS](#macos) lists what differs. The `default` job builds the app without features. The `desktop_rebuild` job builds with `--features desktop_rebuild` and launches with `PP_DESKTOP_REBUILD=1`. The flag stays off in every other build. See [Scenarios](#scenarios) for what each job launches.

1. **Fixture.** `web/scripts/storage-fixtures/prepare.mts` (Node 22.23.3) writes its `immutable-corpus` case: a schema 34 database whose rows come from the autosave golden, with the Build "Parity Build", 3 accepted Plan revisions and 2 Plates. `desktop/packaged-smoke/legacy_fixture.py` copies it and applies the transformations `prepare.mts` uses for its schema-31 case: drop `board_posts` and `board_comments`, drop `projects.legacy_manifest_cutover`, and stamp `schema_version` 31. It records the row counts and a digest of every Plan and Plate table in `fixture.json`. Nothing is committed or hand-edited; the fixture is regenerated on every run.
2. **Package.** With Node 24.21.0 and freshly installed web dependencies, the job builds the desktop assets and stages the runtime with `rust/scripts/stage-desktop.py`, then builds the shipped `.deb` with `rust/scripts/desktop-deb.sh` and the Tauri CLI 2.12.1 pinned in `desktop/packaged-smoke/package-lock.json` (see [Linux package](../desktop.md#linux-package)).
3. **Install.** `install_deb.py` unpacks the `.deb` as shipped into a disposable root with `dpkg-deb -x` and fails unless its `desktop-runtime` tree equals the stage, with the inventory from `rust/scripts/assemble-linux-deb.py`: the same files by SHA-256 and the mode Tauri's bundler writes, the same directories, the same symlinks and targets, nothing extra. It changes nothing.
4. **Launch.** `smoke.py`, inside `dbus-run-session`, starts its own Xvfb display and launches `usr/bin/pp-desktop` with a disposable `HOME` whose `.print-partner/print-partner.db` is the fixture. It then requires, in order:
   - a mapped X11 window owned by the app, titled `Print Partner`, at least 900 x 650 (the startup error dialog fails the run at once);
   - the fixture's Build name as a link or table cell inside the page's `document web` node of the app's AT-SPI accessibility tree. If the page shows the first-run "Set up Print Partner" form instead, the smoke types a disposable administrator (`packaged-smoke@example.invalid` with a random password that is never written down), submits it, and follows "Continue to Print Partner";
   - a painted screenshot of the display (at least 64 colors), saved as `window.png`;
   - exit code 0 within 60 seconds of SIGTERM. The app returns 0 only after a complete core shutdown receipt, which the smoke saves. No child process may survive;
   - a database stamped above 31 that has the board tables again and the same rows in every Plan and Plate table.
5. The job repeats each scenario five times, each launch with a fresh `HOME` and display, and uploads `receipt.json`, the screenshot and the app's stdout and stderr for each launch as the `packaged-smoke-linux-<mode>` artifact. The data directory, including its logs, is deleted after each launch.

## Scenarios

| Job | Scenario | Data directory | Expected |
| --- | --- | --- | --- |
| `default` | `migrate` | the schema 31 database | Window, Build visible, clean exit (shutdown receipt with no errors and `storage_released: true`), Node migrates past 31 (to 34 today) and keeps every Plan and Plate row. |
| `desktop_rebuild` | `refuse-unmarked` | the schema 31 database, no marker | The "Print Partner could not start" dialog, exactly `desktop_startup_failed: core_startup` on stderr (any other category, such as `data_directory_permissions`, fails), and every entry under the data directory, recursively, unchanged in content, mode and link target. The `migrate-rust-owned` row runs the same package on the same database, so the marker is the only difference. |
| `desktop_rebuild` | `migrate-rust-owned` | the schema 31 database plus `.pp-rust-owned.json` | First-run administrator form, then the Build, clean exit (same receipt check), Rust migrates past 31 (to 46 today) and keeps every Plan and Plate row. |

The rebuild build opens only a directory that is empty or carries the ownership marker, and refuses an existing unmarked installation without touching it ([Storage](../storage.md#ownership-marker-decision)). The cutover writes the marker on a Rust-owned copy ([Architecture](../architecture.md)). The `migrate-rust-owned` scenario stands in for that copy: `smoke.py --rust-owned` writes the marker in the exact format [Storage](../storage.md#ownership-marker-format) documents, and a unit test compares the bytes with that page. The migrated installation has no account, so the rebuild UI asks for an administrator before it shows the Builds.

The app runs without network sharing, with loopback ports only, and sends nothing to a printer. The fixture's source points at `https://example.test`, so the Builds page shows "Sync failed"; the smoke does not depend on it.

## When it runs

Linux: every push to `main`, nightly at 06:41 UTC, and on manual dispatch. macOS: nightly and on manual dispatch, not on pushes to `main`, because hosted macOS runners are scarce. On a ready pull request a small `changes` job runs, and the smoke jobs run when the PR changes `desktop/packaged-smoke/`, the workflow, `rust/scripts/desktop-deb.sh`, `rust/scripts/desktop-bundle.sh` or `rust/scripts/assemble-linux-deb.py`, or has the `ci:packaged-smoke` label; otherwise they skip. Draft pull requests skip everything.

## Run it locally

On Linux with the WebKitGTK packages from the workflow's install step:

```sh
cd desktop/packaged-smoke && python3 -B -m unittest -v test_packaged_smoke && cd ../..
npm --prefix web ci   # with Node 22.23.3
(cd web && node --conditions=development --import tsx scripts/storage-fixtures/prepare.mts /tmp/pp-prepared)
python3 desktop/packaged-smoke/legacy_fixture.py --prepared /tmp/pp-prepared --output /tmp/pp-fixture
npm --prefix web ci   # again, with Node 24.21.0
VITE_PRINT_PARTNER_DESKTOP=1 npm --prefix web run build
python3 rust/scripts/stage-desktop.py --web web --node "$(command -v node)" --output /tmp/pp-stage --commit "$(git rev-parse HEAD)"
npm --prefix desktop/packaged-smoke ci
PP_DESKTOP_STAGE=/tmp/pp-stage PP_TAURI="$PWD/desktop/packaged-smoke/node_modules/.bin/tauri" rust/scripts/desktop-deb.sh
python3 desktop/packaged-smoke/install_deb.py --deb rust/target/release/bundle/deb/*.deb --stage /tmp/pp-stage --root /tmp/pp-root
dbus-run-session -- python3 desktop/packaged-smoke/smoke.py --app /tmp/pp-root/usr/bin/pp-desktop --fixture /tmp/pp-fixture --output /tmp/pp-evidence
```

For the rebuild mode, pass `--features desktop_rebuild` to `desktop-deb.sh` and run `smoke.py` with `--rebuild --expect-refusal`, then with `--rebuild --rust-owned`. Each output path must be a new directory.

## macOS

The `macos` jobs run the same scenarios on `macos-26` (Apple silicon). What differs from Linux:

- **Package.** `rust/scripts/desktop-bundle.sh` builds the unsigned `Print Partner.app` from the stage with the same pinned Tauri CLI (`PP_TAURI`), passing `--features desktop_rebuild` for the rebuild job. That is the script the macOS bundle workflow ships with.
- **Install.** `ditto` copies the app into a disposable `Applications` folder, and `rust/scripts/check-macos-bundle.py` verifies the installed copy against the stage's `bundle-manifest.json` before any launch.
- **Launch.** `smoke.py` runs `Print Partner.app/Contents/MacOS/pp-desktop` in the runner's logged-in session with a disposable `HOME`. `macos_ui.py` reads windows and the page through the Accessibility API with `ax_probe.swift`, which the job compiles first and checks for Accessibility trust before it builds anything. The window must be an `AXWindow` titled `Print Partner`; the Build must be an `AXLink` or `AXCell` under the page's `AXWebArea`. The first-run form is filled by focusing each field and typing key events. The refusal dialog is the alert that shows "Print Partner could not start". `rfd` displays it with `CFUserNotificationDisplayAlert`, so the `UserNotificationCenter` agent draws it, and the smoke searches that process's windows too. After the app stops, the smoke presses the alert's OK button and requires it to close. Every launch also fails at once if such an alert is already showing. A window counts as visible only when a matching normal-layer window is on screen. The paint check captures the window's own image (`screencapture -o -l`, so other windows can't cover it) and counts colors only inside the `AXWebArea` frame, re-read on each try, so the title bar never counts. A window with no on-screen number or no page area counts 0. The dialog is captured whole. A failed launch saves a full-screen `failure.png`. The receipt records the typing results for the first-run form and, for `--quit tray`, the probe's tray report. A failed launch whose app is still running also saves the accessibility tree as `accessibility.json`.
- The default job also runs `migrate-tray-quit` (`smoke.py --quit tray`): the same checks, but the app is quit with the tray menu's "Quit Print Partner" item instead of SIGTERM. The probe opens the menu with a posted mouse click on the app's menu bar extra, because tray-icon attaches the menu only in its own mouseDown handler and an AXPress on the status button opens nothing. It must exit 0 with a complete shutdown receipt; before GRE-1254 both quit paths aborted with SIGABRT after the receipt. Linux has no tray scenario because its AppIndicator menu is exported over D-Bus, not AT-SPI.
- Process tracking uses `ps` instead of `/proc`. Artifacts are `packaged-smoke-macos-<mode>`.

## Windows

Not covered yet. `pp-desktop` is still excluded from the Windows build (`rust-cross-os.yml`, together with `pp-api`, `pp-gateway` and `pp-server`), and `stage-desktop.py` stages only macOS and Linux, so no Windows package exists to launch. The Windows packaged smoke follows the Windows port of those crates and a Windows stage and installer.
