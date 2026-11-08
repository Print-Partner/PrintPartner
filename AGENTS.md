# Working on Print Partner

Print Partner plans and tracks layered STL kit builds for one user. The backend is moving from Node to one Rust core, served to a Tauri 2 desktop window and a headless Docker image. The old React UI in `web/apps/web` is frozen to correctness and data-safety fixes. The replacement UI in `web/apps/ui` talks only to Rust ([ADR 0008](docs/adr/0008-replace-the-old-screens.md)). Read [Architecture](docs/architecture.md) for the target, the current state, the crate map, and the 2026-11-08 re-scope.

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
just ci            # canonical local CI; same as python3 scripts/local-ci.py
just ci --list     # step ids for --only / --skip
just guidance      # checks this file, the repo skills, and the safety guard
```

`just ci` exits 0 with SKIP lines when a tool is missing. A SKIP is not a pass: list it under Evidence. The [CI guide](docs/development/ci.md) maps each hosted check to a local command.

## Rules

- Desktop rebuild work stays behind the default-off Cargo feature `desktop_rebuild` and the runtime switch `PP_DESKTOP_REBUILD=1` or `true`. See [Desktop rebuild flag](docs/desktop-rebuild.md). Never turn it on by default.
- One runtime owns each whole operation: routes, workers, tables, and files. No dual writes, shadow printer sends, or fallback from Rust to Node for mutations.
- Keep SQLite formats, history, immutable Plan/Plate/Required-unit IDs, autosave, and restart recovery intact.
- Printer sends need an explicit user action. Never send a real print to test something. Reconcile uncertain results instead of retrying.
- Sharing stays off by default, and MCP always requires authentication, even on loopback. See [Network sharing](docs/network-sharing.md).
- No product analytics, default-on telemetry, multi-user, SaaS, or Postgres work. Sentry stays opt-in.
- Use disposable data and loopback ports for testing. Keep tokens, cookies, pairing secrets, and customer files out of commits, logs, screenshots, and PRs.
- Don't hand-edit generated manifests or schema fixtures; run their generators.

## Pull requests

Follow the [definition of done](docs/development/definition-of-done.md):

- One job per PR, under about 1,500 changed lines, or about 600 for a Mission PR. Every PR says `Fixes GRE-xxx`. Unmet acceptance items go to a child ticket that quotes them verbatim; don't use `Refs`. `linear-pr-sync.yml` sets the ticket states that follow a PR.
- Put what you ran and saw under `## Evidence`. For changes to the web UI or desktop app, attach a screenshot or video. The rules are in [Gate evidence](docs/contributing/gate-evidence.md).
- The required `review-gate` check passes on a PR head when Factory Droid (`droid-review`) or the Claude code and security reviews (`claude-review.yml`) succeeded on that head, or when the coordinator recorded its own review of it. Droid review is suspended until a provider-neutral gate replaces it or the Factory quota returns, so the Claude reviews or the coordinator marker carry the gate. Fix each finding, or reply with why it doesn't apply, then resolve the thread. Merging needs green required checks and no open threads. See [Code review](docs/development/definition-of-done.md#code-review).
- The coordinator merges from its own worktree under the [merge gate](docs/development/definition-of-done.md#merge-gate). Lanes and other agents report readiness and never merge. A posted score is a record; the coordinator's own scorer run decides.
- When you stop with work in flight or hand it over, post a Handoff comment on the GRE ticket and push your work. To take work over, start from the newest Handoff. See [Handoffs](docs/development/agent-workflow.md#handoffs).
- Write docs, comments, and PR text plainly: concrete facts, commands, and numbers, active voice, no filler. Before large or risky work, write the plan, list how it could fail, and get open decisions answered. In Droid, the `unslop` and `grilling` skills do this.

[Agent and contributor workflow](docs/development/agent-workflow.md) is the shared procedure for any agent or person: task briefs exported from Notion, checks, the safety guard, running the app, evidence, and handoffs. The Factory skills in `.factory/skills/` link to it: [pp-verify](.factory/skills/pp-verify/SKILL.md) for screenshots and video, [pp-mission-qa](.factory/skills/pp-mission-qa/SKILL.md) for Mission user testing, and [pp-readiness](.factory/skills/pp-readiness/SKILL.md) for agent-readiness work.

`scripts/agent-guard.py` refuses common maintainer-only actions. Droid's project hook runs it automatically. Claude Code runs it through `.claude/hooks/guard.py` once you add the [hook settings](docs/development/agent-workflow.md#claude-code). Other runners must call it themselves. It does not replace review. For local and cloud Missions, follow [Factory setup](docs/development/factory-missions.md) and keep scrutiny and user testing enabled.
