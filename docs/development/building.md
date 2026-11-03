# Building

The repository has two build trees:

- `web/` holds the React web UI and the Node server that the Rust core replaces.
- `rust/` holds the Rust workspace. [Architecture](../architecture.md#crate-map) lists its crates.

## Web

Use Node 22 and npm 10 or later. From the repository root:

```sh
cd web
npm ci
npm run quality
```

Run `npm run quality` before you open a PR that changes `web/`. The root [README](../../README.md#local-development) describes the dev server and the package layout.

## Rust

Use the toolchain pinned in `rust-toolchain.toml`. These commands check formatting and lints for the whole workspace and build the headless launcher:

```sh
cargo fmt --manifest-path rust/Cargo.toml --all --check
cargo clippy --manifest-path rust/Cargo.toml --locked --workspace --all-targets -- -D warnings
cargo build --manifest-path rust/Cargo.toml --locked -p pp-server
```

To compile the flagged rebuild code, select a crate and add `--features desktop_rebuild`. For example:

```sh
cargo check --manifest-path rust/Cargo.toml --locked -p pp-storage --features desktop_rebuild
```

The workspace also has these checks:

- `python3 rust/scripts/check-boundaries.py --self-test` checks that each crate depends only on the crates it is allowed to use.
- `python3 rust/scripts/check-ast-grep.py` checks the ast-grep lint counts against `rust/ast-grep-baseline.json`. Its `no-writer-family-literal` rule flags family path components from GRE-402 no-writer declarations in Rust string literals, including inline `#[cfg(test)]` code. Under `rust/`, it scans `crates/**/*.rs` and exempts only `crates/*/tests/**`, `crates/*/src/**/tests.rs`, and `crates/*/src/**/tests/**` by path. This rule starts at a zero baseline. Legitimate format names, empty-directory setup, and inline test literals need per-line `ast-grep-ignore: no-writer-family-literal` comments with reasons.

[Continuous integration](ci.md) lists the checks that every PR runs on Linux, macOS and Windows. [rust/README.md](../../rust/README.md) has the full command list. Some tests need the built web assets and the desktop bundle manifest that it describes.

## Desktop app

[pp-desktop/README.md](../../rust/crates/pp-desktop/README.md) describes how to run the native development app.

`rust/scripts/stage-desktop.py` builds the unsigned desktop stage that [pp-desktop/README.md](../../rust/crates/pp-desktop/README.md#create-an-unsigned-target-native-stage) describes. It copies the server, web, contracts and domain builds, then every production package in `web/package-lock.json`. An npm workspace link, such as `node_modules/@print-partner/ui -> apps/ui`, is copied only when its target workspace is part of the stage; otherwise the stager skips it and lists it under `skipped_workspace_links`. `rust/scripts/check-macos-bundle.py` applies the same rule: it accepts a missing workspace link whose target is not in the bundle and rejects a link that points at one. A new workspace that the desktop runtime needs must be added to the stager's artifact and metadata lists. `python3 rust/scripts/stage-desktop.py --self-test` runs the stager tests. [Desktop app](../desktop.md) describes the app itself.
