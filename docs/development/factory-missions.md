# Factory Missions setup

Missions use this repository's AGENTS.md, skills, and lifecycle hooks. They do
not need a custom droid for each application. Workers inherit these instructions.
Keep scrutiny and user-testing validation enabled.

## Local compute

Use the [development container](devcontainer.md) for the pinned Rust and Node
toolchains and Linux native libraries. Run its post-create and runtime smoke
commands before starting a Mission. A missing tool or skipped check is a blocker,
not successful validation. Native Windows/macOS acceptance still needs those
platforms.

## Factory cloud Environment

After this setup script is on the selected repository branch:

1. Open Factory Settings → Environments → New environment → Create your own.
2. Name it `print-partner-missions`. Choose personal authorization unless you
   intend to share it with the organization. Do not change organization defaults.
3. Use Factory managed / Factory default. Grant both the personal source-control
   connection and the Factory organization GitHub app access to PrintPartner.
4. Add `poitee/PrintPartner`. In its **repository setup script**, enter
   `bash scripts/factory-environment/setup.sh`. Leave global setup empty.
5. Create the Environment, inspect the build log, and wait for Ready.
6. Select that Environment when creating a Mission. Validate a fresh instance
   before trusting it for migration work.

The script targets Debian/Ubuntu Linux with root or passwordless sudo inside
the cloud build. It installs GTK/WebKit build libraries, checksum-verified Node
24.21.0 and 22.23.3, rustup, the repository Rust toolchain, sidecar dependencies,
and npm dependencies. It runs `predev`, not a product service or full CI.
Node versions and native packages come from the development container; sidecar Python pins
come from its existing Dockerfile. The setup also links the runtime Node to
`/usr/bin/node` for existing absolute-path runtime proofs.
It writes `~/.local/print-partner/env.sh` and sources it from shell profiles.
That file sets `PP_SIDECAR_PYTHON`, which `.devcontainer/smoke.sh` uses for the
sidecar tests.
For a non-interactive shell, source that file explicitly.

Build steps do not receive Factory runtime secrets. This setup needs none.
Do not bake API keys, pairing credentials, production databases, or customer
files into an Environment. Do not run this system-package installer on the
host just to prepare a local session.

The development container and the cloud setup install the same Chromium package.
The setup sets `PLAYWRIGHT_CHROMIUM_EXECUTABLE=/usr/bin/chromium`, so a Mission can
take the screenshot that web and desktop PRs need. Chromium runs headless, which
needs no display. The cloud build still has no X or Wayland session and no native
window, so desktop window and tray capture stays blocked there. A successful
Debian simulation does not establish a Ready Factory build; confirm the browser
works in the Environment before relying on it for evidence.

## Plan and validate a migration slice

Start Mission planning from the current Linear slice and linked Notion plan.
Use [pp-readiness](../../.factory/skills/pp-readiness/SKILL.md) for applicable
checks and [pp-mission-qa](../../.factory/skills/pp-mission-qa/SKILL.md) for
service launch, logs, driving, assertions, and shutdown.

In the approved Mission plan, name the operation owner, default-off flag modes,
SQLite/ID/recovery invariants, and the evidence each milestone must produce.
Keep PRs near 600 changed lines for GRE-1067. Finish one PR's Droid review and CI
before opening the next. Only the maintainer merges.

The project guard hook refuses common merge, force-push, direct-main push,
release publish or edit, hook-bypass, secret-file edit, and generated-manifest edit commands.
`git push` needs an explicit branch refspec and common options spelled in full.
Write push destinations literally. The guard refuses a destination built from a
shell expansion, the `:` matching refspec, and `HEAD` when the same command
also switches branches. When `push.followTags` is on, or a `remote.<name>.push`
mapping exists, add `--no-follow-tags` or write `source:destination` refspecs.
The guard treats both settings as unknown after a `git config` write or a
`git remote` change in the same command. It refuses commits and pushes that run
with `GIT_CONFIG_*` environment variables. Git and gh subcommands and option
names must be literal. Run a `gh api` write that reads a request file
(`-F key=@file` or `--input`) as its own command, after writing the file.
`cd -P` and directory changes inside conditionals or groups leave the directory
unknown.
After a `cd` in a subshell or pipeline, or under `env -C`, the guard treats the
directory as unknown and refuses relative `gh api` input files.
Through `gh api`, agents may read, write PR or issue comments, submit
comment-only or pending PR reviews, and reply to or resolve review threads. Other API writes need the maintainer.
It refuses a git or gh command it cannot classify. It is a mistake guard, not
a shell sandbox or a complete security boundary.
Repository rules and required GitHub checks remain authoritative. Review the
project entry in `/hooks` after checkout; Droid snapshots hooks at startup.
Do not disable the hook to finish a task.

References: [Missions](https://docs.factory.com/missions/reference.md),
[Environment creation](https://docs.factory.com/environments/create.md), and
[hooks](https://docs.factory.com/harness/hooks.md).
