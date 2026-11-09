# Keep one desktop product identity

Date: 2026-10-12. Status: accepted for M0. Execution key: T-5 (GRE-337).

## Decision

Retain the native application identifier `com.poitee.printpartner` and product
name `Print Partner`. Reuse the implementation already present on main.
This decision precedes T-12 (GRE-344) and M1 packaging. It has no task
prerequisite and adds no M1 dependency.

Keep the existing desktop store at `~/.print-partner` on macOS, Linux, and
Windows. The identifier is an application namespace, not a command to rename
existing data. Do not select a fresh Tauri application-data directory during
an upgrade. Current Rust entry points use Unix-specific APIs; Windows execution
is not established.

## Consumers and compatibility

- `rust/crates/pp-desktop/tauri.conf.json` supplies the identifier and product
  name to `tauri-build`, `generate_context!`, and target bundle metadata.
  `src/main.rs` uses Print Partner for the window, startup dialog, tray
  tooltip, Show, and Quit labels. The tray's internal ID remains `printpartner`.
- `rust/crates/pp-desktop/src/lib.rs::desktop_data_dir` selects the legacy
  store under the supplied home directory. `src/main.rs` passes that directory
  to CoreRuntime. Core persists `desktop.toml` and owns the existing storage
  lease there. `pp-server` takes an explicit data directory. There is no
  `pp-core/src/paths.rs` in this baseline.
- Tauri's platform directories for auxiliary application state use
  `com.poitee.printpartner`. On macOS these include
  `~/Library/Application Support/com.poitee.printpartner`; Linux uses the XDG
  data and config roots with that identifier; Windows uses the corresponding
  AppData roots. These auxiliary paths do not replace `~/.print-partner`.
  Existing web data volumes and the documented SQLite importer keep their
  own explicit paths.
- `rust/scripts/stage-desktop.py` records the exact source commit, runtime
  version, OS, architecture, and Node identity in `release.json`. These
  build identities are distinct from the stable application identifier.
  `ResourceLayout` reads them, and the measured manifest binds runtime bytes.
  Linux stages use `bin/node` and `web`; macOS uses
  `MacOS/printpartner-node` and `Resources/desktop-runtime/web`.
- `desktop-bundle.sh` consumes the measured stage and builds an unsigned
  `Print Partner.app`. `assemble-macos-resources.py` and
  `check-macos-bundle.py` require `CFBundleIdentifier=com.poitee.printpartner`
  and the expected product bundle. The native macOS workflow uses the same
  app name for original and relocated artifacts. The web release workflow
  publishes tags under `Print-Partner/PrintPartner` with titles `Print Partner <tag>`.
  Executable and archive names are artifact roles, not replacement bundle IDs.

## Data, keychain, signing, and updates

An identity change can select different platform state, break signing
continuity, separate keychain access, or make an updater target another app.
Keep the identifier stable. Any later change needs a separate migration
decision with old-to-new data, credential, and update compatibility evidence.

This baseline has no native OS keychain consumer or keyring namespace.
Compatibility authentication retains its existing file and database storage;
this decision neither moves credentials nor creates keychain entries.
A later keychain implementation must use `com.poitee.printpartner` as its
application namespace and preserve access to prior credentials explicitly.

Native bundling currently uses `--no-sign --no-binary-patching`. No configured
signing identity, native updater plugin, updater public key, or native update
feed is present. Future signing retains the bundle identifier. It must sign
nested code before measurement and compilation, preserve those measured bytes
during assembly, then sign the outer app. Signing, notarization, stapling, and
clean-device launch require separate macOS receipts.

The compatibility update checker reads version tags from the configured
GitHub repository, defaulting to `Print-Partner/PrintPartner`. It reports availability;
it is not a native installer. A future native updater must retain the stable
application identifier and verify its signing key and target artifacts.
Do not infer working native updates from the version checker or Linux stage.

## Verification boundary

The `ticket_t_5` tests exercise Tauri's generated context, legacy-store
selection, and release-layout loading without modifying existing data.
Exact-head Linux app and staged-runtime receipts cover those actual consumers.
They do not establish macOS signing, notarization, native updating, Windows
runtime support, or clean-device installation. Those remain release gates,
not dependencies that postpone this M0 identity decision.
