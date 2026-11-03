# Print Partner documentation

Use this index to find the page for your task. The in-app Help page covers the product workflow.

Print Partner is moving its backend from Node to one Rust core. The core runs as a desktop app and as a single-user Docker image. [Architecture](architecture.md) describes the target and the current state.

Pages are grouped by type: tutorials teach by example, how-to guides complete one task, reference pages state facts to look up, and explanations give the design and its reasons. Records at the end are history, not authority.

## Authority by topic

When two pages disagree, the authority below wins. Goals and decisions in flight live in Notion, and Linear tracks execution state; [Contributing](development/contributing.md#sources-of-truth) names both.

| Topic | Authority |
|-------|-----------|
| Product words | [CONTEXT.md](../CONTEXT.md) |
| Replacement UI: Today, Build tabs, History/Accept, Checkoff | [UI and UX target](product/ui-ux-target.md) |
| Target runtime, crates, allowed edges, operation owners, cutover | [Architecture](architecture.md) |
| Accepted decisions and what replaced them | [ADR index](adr/README.md) |
| Which operations run in Node and which have a Rust owner | [Operation inventory](development/operation-inventory.md) and [the generated report](parity/operation-inventory.md) |
| `desktop_rebuild` feature and `PP_DESKTOP_REBUILD` switch | [Desktop rebuild flag and slice plan](desktop-rebuild.md) |
| Data directory, single-writer lock, migrations | [Storage](storage.md) |
| Visual tokens and components | [Design system](design-system.md) |
| Manifest format | [Manifest format](../manifests/README.md) |
| Agent and contributor rules | [AGENTS.md](../AGENTS.md), [Agent and contributor workflow](development/agent-workflow.md) |
| Merge bar and review policy | [Definition of done](development/definition-of-done.md) |
| Hosted checks and their local commands | [Continuous integration](development/ci.md) |
| Security reports | [Security](../SECURITY.md) |

## Tutorials

| Page | Teaches |
|------|---------|
| [Voron 2.4 with Stealthburner and Tap](examples/golden-ldo-voron-2.4-sb-tap.md) | Creating a Build from a real stack. |

## How-to guides

### Use Print Partner

| Guide | Use it when |
|-------|-------------|
| [Install](INSTALL.md) | You are setting up the Docker container or opening Print Partner on a LAN. |
| [Operations](../OPERATIONS.md) | You need backups, recovery, API keys, metrics, logs, or update steps. |
| [MCP setup](assistant-mcp.md) | You want to connect Cursor, Claude, or another MCP client. |
| [Connect a printer](integrations/PRINTER_SETUP.md) | You are adding printer geometry or a network connection. |
| [Optional printer connections](printer-connections.md) | You want to know what works without a printer connection. |
| [Connect Spoolman](integrations/SPOOLMAN.md) | You want filament inventory in Builds. |
| [Author a stack manifest](playbooks/author-manifest-on-stack.md) | You are writing a repository manifest. |
| [Build from a stack preset](playbooks/kit-studio-build.md) | You are creating a Build from a base Source and add-ons. |
| [Change one part's color](part-colors.md) | You need a different color for one part. |
| [Export STLs by color and filename group](export-by-filename.md) | You want parts grouped for your slicer. |
| [STL downloads and browser warnings](stl-downloads.md) | A large export warns or fails in the browser. |
| [Capture screenshots](screenshots/README.md) | You are refreshing the README and site images. |

### Develop Print Partner

| Guide | Use it when |
|-------|-------------|
| [Contributing](development/contributing.md) | You want to pick up a ticket and open a pull request. |
| [Agent and contributor workflow](development/agent-workflow.md) | You need task briefs, checks, the safety guard, app verification, or evidence. |
| [Building](development/building.md) | You are setting up the local toolchain. |
| [Rust and React development container](development/devcontainer.md) | You want the pinned toolchains in a container. |
| [Change a CI workflow](development/ci-controls.md) | You are editing a workflow or required check. |
| [Run the stress-runner self-tests](development/stress-runner-self-tests.md) | You changed the stress runner. |
| [Factory Missions setup](development/factory-missions.md) | You are running a Factory Mission. |
| [Golden kit export](examples/golden-ldo-voron-2.4-export.md) | You need the regression and release smoke-test kit bundle. |
| [Gate evidence](contributing/gate-evidence.md) | You are filling in a PR's Evidence section. |

## Reference

### Product workflow

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

### Rust core

| Page | Covers |
|------|--------|
| [Storage](storage.md) | Data directory, single-writer lock, and migrations. |
| [HTTP API](api.md) | The versioned integration API. |
| [MCP](mcp.md) | The MCP server in the Rust core. |
| [Network sharing](network-sharing.md) | "Share on network" in the desktop app. |
| [Desktop app](desktop.md) | The Tauri app, its tray, and its data. |
| [Docker image](docker.md) | The single-user headless image. |
| [Release](release.md) | How releases are built, signed, and published. |
| [Domain](domain/README.md) | The core model and one page per product area. |
| [Desktop rebuild flag and slice plan](desktop-rebuild.md) | The `desktop_rebuild` feature and its slices. |
| [Checkoff transaction-start errors](development/checkoff-progress-errors.md) | How Checkoff writes classify SQLite start failures. |
| [Import worker policy](development/import-worker-policy.md) | How the import worker applies its policy. |

### Formats, contracts and parity

- [Manifest format](../manifests/README.md)
- [Cross-source Voron stack](examples/cross-source-voron/ldo-2.4-golden-stack.md): the golden stack manifests by repository
- [Plan autosave](plan-autosave.md)
- [Accepted Plate 3MF validation](3MF_EXPORT_VALIDATION.md)
- [Design system](design-system.md)
- [Operation inventory](development/operation-inventory.md): what still runs in Node, what has a Rust owner, and [the generated report](parity/operation-inventory.md)
- [API-key HTTP parity](parity/auth-key.md) and [password-reset request parity](parity/auth-reset.md)
- [Plan publication client parity cases](parity/plan-publication-clients.md)
- [TypeScript outbound calls and offline behavior](parity/outbound-calls.md)

### Project

- [Definition of done](development/definition-of-done.md)
- [Continuous integration](development/ci.md)
- [Agent instructions](../AGENTS.md)
- [Security](../SECURITY.md)
- [Release history](../CHANGELOG.md)

## Explanation

- [Architecture](architecture.md): target design, current state, crates, and cutover.
- [Architecture decision records](adr/README.md): every ADR, its status, and what replaced it.
- [UI and UX target](product/ui-ux-target.md): the replacement UI and why it replaces the old screens.
- [References-only sharing](reference-sharing.md): why Builds and Library collections share recipes and references, never model files, and the v1 format.
- [Cargo cache trust for same-repository PRs](development/cargo-cache-trust.md)
- [PR Evidence workflow trust](development/pr-evidence-trust.md)
- [Subreaper proof contract](development/subreaper-proof-contract.md)

## Node server

These pages describe the Node server that the Rust core replaces. Its multi-user, hosted planning, and Postgres modes do not carry over to the Rust core.

- [Deployment reference](../web/DEPLOY.md)
- [Invite-beta hosted planning](../web/DEPLOY.md#invite-beta-hosted-planning)
- [Hosted tenant storage](hosted-storage.md)
- [Single-process guarantees](architecture/background-work-guarantees.md): what one Node server process guarantees for the send queue and backup restore.

## Records

These pages record an investigation, audit, or ticket's evidence at one point in time. They are not authority; check the pages above for current rules.

- Audits in [`audits/`](audits/) and triage notes in [`triage/`](triage/)
- Ticket evidence pages named `gre-*.md` in this folder and in [`development/`](development/)
- [Desktop parity checklist](parity/checklist.md): the parity-first list of UI pages and MCP tools. ADR 0008 replaced old-screen parity with new screens
- [D9 coverage audit](desktop-rebuild-d9-audit.md)
- [Linear PR sync concurrency investigation](architecture/linear-pr-sync-concurrency.md)
- [Accepted STL export timeout diagnosis](development/accepted-stl-export-diagnosis.md)

## Project media

- [Project site](https://poitee.github.io/PrintPartner/)
- [Screenshot capture guide](screenshots/README.md)
