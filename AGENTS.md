# Working on Print Partner

Print Partner plans and tracks layered STL kit builds for one user. The backend is moving from Node to one Rust core, served to a Tauri 2 desktop window and a headless Docker image. The old React UI in `web/apps/web` is frozen to correctness and data-safety fixes. The replacement UI in `web/apps/ui` talks only to Rust ([ADR 0008](docs/adr/0008-replace-the-old-screens.md)). Read [Architecture](docs/architecture.md) for the target, the current state, the crate map, and the 2026-11-08 re-scope. The active plan is [the daily loop](docs/product/daily-loop.md): until the owner has walked it on Rust, take only tickets labelled `daily-loop`.

## Repository map

| Path | What it is |
| --- | --- |
| `rust/crates/pp-*` | Rust workspace. `pp-server` is the gateway/headless binary, `pp-desktop` the Tauri app; the rest are libraries. |
| `web/apps/ui` | Replacement React UI (Vite). Built only against the Rust API through `web/packages/rust-client`. New UI work goes here. |
| `web/apps/web` | Old React UI (Vite). Frozen: correctness and data-safety fixes only. Desktop, Docker, and browsers load it until cutover. |
| `web/apps/server` | Node compatibility server. Rust is replacing it; maintain it, don't grow it. |
| `web/packages` | Shared TypeScript contracts and domain code, and the generated Rust client `rust-client`. Run its generator; don't hand-edit it. |
| `slicer-sidecar` | Legacy Python slicer integration. Change only to maintain it. |
| `docs/` | Product, architecture, and development docs. Update them in the same PR as the change. |

## Setup and checks

Toolchains: Rust from `rust-toolchain.toml` (1.99), Node 24.21 for native assets and runtime, and Node 22.23.3 only for the schema-parity oracle (`PP_SCHEMA_PARITY_NODE`). Keep both Node versions; reinstall npm dependencies after switching.

```sh
npm --prefix web ci
npm --prefix web run lint
npm --prefix web run typecheck
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
just ci --fast     # local CI for a normal PR: affected nextest, no full test or doc passes
just ci            # full local CI for a sensitive PR; same as python3 scripts/local-ci.py
just ci --list     # step ids for --only / --skip
just guidance      # checks links in every .md, this file, the repo skills, and the safety guard
```

`just ci` exits 0 with SKIP lines when a tool is missing. A SKIP is not a pass: list it in the PR description. The [CI guide](docs/development/ci.md) maps each hosted check to a local command.

## Rules

- GRE-1646 turns the Cargo feature `desktop_rebuild` on by default for the crates the Rust-only image builds. `pp-desktop` and `pp-drivers` stay off unless the feature is named. A `--workspace` build still compiles `pp-core` with the feature, because Cargo unifies it from those image crates. `pp-desktop`'s own feature is what selects its Node path. The runtime switch `PP_DESKTOP_REBUILD` still defaults off. Only the Rust-only image sets it to `1`. `--no-default-features` builds the old path. See [Desktop rebuild flag](docs/desktop-rebuild.md).
- One runtime owns each whole operation: routes, workers, tables, and files. No dual writes, shadow printer sends, or fallback from Rust to Node for mutations.
- Keep history, immutable Plan/Plate/Required-unit IDs, autosave, and restart recovery intact. Rust owns its stored formats, and the import is the only reader of Node's ([ADR 0016](docs/adr/0016-rust-owns-its-data-formats.md)).
- Build to the product contract in `docs/`. Match Node on the data a real library holds; its error strings, coercions and ordering are not requirements ([Parity scope](docs/development/definition-of-done.md#parity-scope)).
- Printer sends need an explicit user action. Never send a real print to test something. Reconcile uncertain results instead of retrying.
- Sharing stays off by default, and MCP always requires authentication, even on loopback. See [Network sharing](docs/network-sharing.md).
- No product analytics, default-on telemetry, multi-user, SaaS, or Postgres work. Sentry stays opt-in.
- Use disposable data and loopback ports for testing. Keep tokens, cookies, pairing secrets, and customer files out of commits, logs, screenshots, and PRs.
- Don't hand-edit generated manifests or schema fixtures; run their generators.
- Give times to the maintainer in US Eastern time (EST or EDT as in effect), not UTC, in replies and reports. UTC stays in commands, file contents, and commit or PR text.

## Pull requests

Follow the [definition of done](docs/development/definition-of-done.md):

- One job per PR. Aim for 300 to 600 changed lines; about 1,500 is the hard ceiling, or about 600 for a cross-cutting PR. Split larger work into sequential PRs, each started from `main` after the previous one merges; never stack PRs. Every PR says `Fixes GRE-xxx`. Unmet acceptance items go to a child ticket that quotes them verbatim; don't use `Refs`. `linear-pr-sync.yml` sets the ticket states that follow a PR.
- Work comes from the Rust cutover project through `/implement`, one ticket per fresh session.
- Rust test rename PRs record old and new paths and the ticket in their crate's page under [Test file renames](docs/records/test-renames.md). Add rows only to that crate's file; the index stays navigation.
- Every PR needs an independent GitHub approval. For import, schema, backup and restore, Checkoff, send, or Accept and publication, record what the reviewer checked for data loss. Chad merges. See [Who merges](docs/development/definition-of-done.md#who-merges).
- For a change to the web UI or desktop app, attach a screenshot or video. The capture steps are in [Gate evidence](docs/contributing/gate-evidence.md).
- When you stop with work in flight, push the branch and write what is true at that head in the PR description.
- Write docs, comments, and PR text plainly: concrete facts, commands, and numbers, active voice, no filler. Before large or risky work, write the plan, list how it could fail, and get open decisions answered.

[Agent and contributor workflow](docs/development/agent-workflow.md) is the shared procedure: task briefs, checks, the safety guard, and running the app. The repo skills are [pp-verify](.claude/skills/pp-verify/SKILL.md) for screenshots and video, [pp-mission-qa](.claude/skills/pp-mission-qa/SKILL.md) for user testing of a migration slice, and [pp-send-once](.claude/skills/pp-send-once/SKILL.md) for the unknown-outcome printer send check.

`scripts/agent-guard.py` refuses common maintainer-only actions. Claude Code runs it through `.claude/hooks/guard.py` once you add the [hook settings](docs/development/agent-workflow.md#claude-code). Other runners must call it themselves. It does not replace review.

## Agent skills

### Issue tracker

Linear, team GRE, through the Linear MCP. See `docs/agents/issue-tracker.md`.

### Triage labels

Defaults, except `ready-for-human` is `needs-chad-action` and `wontfix` is `pp:later`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the root. See `docs/agents/domain.md`.
