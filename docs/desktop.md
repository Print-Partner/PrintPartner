# Desktop app

`pp-desktop` is the Print Partner desktop app. It is a Tauri window and tray around the Rust core. This page will cover install, the data location, logs and network sharing. Most of it is a stub until the desktop milestone lands.

## Platforms

macOS ships first. Windows and Linux ship before 1.0. CI will build and test the app on all three (GRE-632).

## Behavior today

- Closing the window hides it to the tray. The core keeps running.
- The tray has Show, the service status, Restart service and Quit.
- Quit waits for the core to shut down before the process exits.
- The default data directory is `~/.print-partner`. See [Storage](storage.md).
- The web view gets no native commands. Navigation stays on the core's loopback origin, and popups are denied.

[pp-desktop/README.md](../rust/crates/pp-desktop/README.md) describes how to run the development app. [ADR 0005](adr/0005-desktop-product-identity.md) records the product identity.

## Linux package

`rust/scripts/desktop-deb.sh` builds the Linux `.deb` from a completed stage, the way `desktop-bundle.sh` builds the macOS app. Both take `PP_TAURI` for the Tauri CLI command and pass extra arguments such as `--features desktop_rebuild` to `tauri build`:

```sh
python3 rust/scripts/stage-desktop.py --web web --node /path/to/node-v24.21.0 --output /tmp/pp-stage --commit "$(git rev-parse HEAD)"
PP_DESKTOP_STAGE=/tmp/pp-stage rust/scripts/desktop-deb.sh   # add --features desktop_rebuild for the rebuild build
```

`PP_TAURI` names the Tauri CLI command and defaults to `cargo tauri`; use tauri-cli 2.12.1. The package lands in `${CARGO_TARGET_DIR:-rust/target}/release/bundle/deb`; the helper refuses `--target`, `--debug` and `--profile`, which would move it. The helper runs `tauri build --bundles deb`, then `assemble-linux-deb.py`. Tauri's deb bundler skips symlinks in `bundle.resources`, so the raw package lacks the four workspace links the bundle manifest lists (`web/node_modules/@print-partner/{contracts,domain,server,web}`), and the installed app then fails resource verification with `desktop_startup_failed: core_startup`. The assembler unpacks the `.deb`, refuses it unless its `desktop-runtime` tree equals the stage except for exactly those links (the top-level entries come from the stage's `bundle-config.json` mapping; files are compared by SHA-256 and mode, directories by mode, links by target; stage modes are first normalized the way Tauri's bundler writes them: directories 0755, files 0755 when owner-executable, else 0644), adds them with 0755 parent directories, rebuilds the `.deb` in place with root ownership, and checks the rebuilt tree equals the stage. The package installs `usr/bin/pp-desktop` and the runtime under `usr/lib/Print Partner/desktop-runtime`. Its tests run with `python3 rust/scripts/assemble-linux-deb.py --self-test`.

## Share on network

The "Share on network" setting is off by default. When it is on, phones, tablets and other computers can use the web UI after they sign in or pair. See [Network sharing](network-sharing.md).

## To be written

- Install and update steps for each OS.
- Data location for each OS, and how to move it.
- Where logs live and how to read them.
- Settings, autostart and the first-run wizard.
- Crash reporting, which is opt-in and off by default.

## Evidence for desktop changes

A PR that changes the desktop app attaches captures from the real app built at the PR head. [Gate evidence](contributing/gate-evidence.md#desktop-and-tray-capture) describes the steps.
