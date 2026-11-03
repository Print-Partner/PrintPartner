# Print Partner documentation

Use this index to find the guide for your task. The in-app Help page covers the product workflow.

Print Partner is moving its backend from Node to one Rust core. The core runs as a desktop app and as a single-user Docker image. [Architecture](architecture.md) describes the target and the current state.

## Start here

| Guide | Use it when |
|-------|-------------|
| [Install](INSTALL.md) | You are setting up the Docker container or opening Print Partner on a LAN. |
| [Operations](../OPERATIONS.md) | You need backups, recovery, API keys, metrics, logs, or update steps. |
| [MCP setup](assistant-mcp.md) | You want to connect Cursor, Claude, or another MCP client. |
| [Contributing](development/contributing.md) | You want to pick up a ticket and open a pull request. |

## Product workflow

The [accepted replacement UI](product/ui-ux-target.md) defines Today, Build-local
tabs, History/Accept and burst Checkoff. [CONTEXT.md](../CONTEXT.md) defines the
product words. The paths below describe the current Node screens, not the new
UI contract. The current app saves Plan changes automatically. The replacement
will autosave into a draft and require an explicit Accept.

| Area | Browser path | Task |
|------|--------------|------|
| Library | `/library` | Register and sync shared source repositories. |
| Builds | `/builds` | Create, open, copy, archive, or restore a build. |
| Sources | `/sources?profile=<id>` | Attach sources and select STL files for the active build. |
| Plan | `/plan?profile=<id>` | Choose parts, quantities, and colors. Changes save automatically. |
| Checkoff | `/progress?profile=<id>` | Track required units through printing and assembly. |
| Production | `/export?profile=<id>` | Arrange plates, export files, and send sliced jobs. |
| Production overview | `/production` | Review remaining work across builds. |

## Rust core

| Page | Covers |
|------|--------|
| [Architecture](architecture.md) | Target design, current state, and the crate map. |
| [Storage](storage.md) | Data directory, single-writer lock, and migrations. |
| [HTTP API](api.md) | The versioned integration API. |
| [MCP](mcp.md) | The MCP server in the Rust core. |
| [Network sharing](network-sharing.md) | "Share on network" in the desktop app. |
| [Desktop app](desktop.md) | The Tauri app, its tray, and its data. |
| [Docker image](docker.md) | The single-user headless image. |
| [Release](release.md) | How releases are built, signed, and published. |
| [Domain](domain/README.md) | The core model and one page per product area. |
| [Desktop rebuild flag and slice plan](desktop-rebuild.md) | The `desktop_rebuild` feature and the GRE-595 slices. |

## Development

- [Building](development/building.md)
- [Rust and React development container](development/devcontainer.md)
- [Contributing](development/contributing.md)
- [Continuous integration](development/ci.md)
- [Change a CI workflow](development/ci-controls.md)
- [Run the stress-runner self-tests](development/stress-runner-self-tests.md)
- [Definition of done](development/definition-of-done.md)
- [Agent instructions](../AGENTS.md)
- [Gate evidence](contributing/gate-evidence.md)

## Setup and integrations

- [Printer connections](integrations/PRINTER_SETUP.md)
- [Spoolman](integrations/SPOOLMAN.md)
- [3MF export validation](3MF_EXPORT_VALIDATION.md)
- [Export STLs by color and filename group](export-by-filename.md)
- [STL download capacity and browser warnings](stl-downloads.md)
- [Manifest authoring](playbooks/author-manifest-on-stack.md)
- [Stack presets and variants](playbooks/kit-studio-build.md)
- [Change one part's color](part-colors.md)
- [Optional printer connections](printer-connections.md)

## Reference

- [Manifest format](../manifests/README.md)
- [Security](../SECURITY.md)
- [Release history](../CHANGELOG.md)
- [Plan autosave](plan-autosave.md)

## Decisions

- [ADR 0001: Model the Build workflow as preparation and making](adr/0001-model-build-workflow-as-preparation-and-making.md)
- [ADR 0002: Keep Sources and Plan state contextual](adr/0002-keep-sources-and-plan-state-contextual.md)
- [ADR 0004: Share references without redistributing models](adr/0004-share-references-without-redistributing-models.md)
- [ADR 0005: Keep one desktop product identity](adr/0005-desktop-product-identity.md)
- [ADR 0006: Autosave text contains Unicode scalar values](adr/0006-autosave-text-contract.md)
- [ADR 0007: Record the Rust crate layout and dependency edges](adr/0007-rust-crate-layout.md)
- [ADR 0008: Replace the old screens](adr/0008-replace-the-old-screens.md)
- [ADR 0009: Keep current crates and deepen modules](adr/0009-keep-crates-and-deepen-modules.md)
- [ADR 0010: Whole-runtime cutover with explicit rollback limits](adr/0010-whole-runtime-cutover.md)

## Node server

These pages describe the Node server that the Rust core replaces. Its multi-user, hosted planning, and Postgres modes do not carry over to the Rust core.

- [Deployment reference](../web/DEPLOY.md)
- [Invite-beta hosted planning](../web/DEPLOY.md#invite-beta-hosted-planning)
- [Hosted tenant storage](hosted-storage.md)
- [Single-process guarantees](architecture/background-work-guarantees.md): what one Node server process guarantees for the send queue and backup restore.

## Examples

- [References-only Build and Library sharing](reference-sharing.md)
- [Golden LDO Voron 2.4 with SB Tap](examples/golden-ldo-voron-2.4-sb-tap.md)
- [Golden kit export](examples/golden-ldo-voron-2.4-export.md)
- [Cross-source Voron stack](examples/cross-source-voron/ldo-2.4-golden-stack.md)

## Project media

- [Project site](https://poitee.github.io/PrintPartner/)
- [Screenshot capture guide](screenshots/README.md)
