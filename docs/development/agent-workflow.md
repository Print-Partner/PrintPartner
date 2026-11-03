# Agent and contributor workflow

This is the shared procedure for any coding agent or person working on Print Partner. It needs a shell, Git, Python 3, and the toolchains in [AGENTS.md](../../AGENTS.md). Factory Droid is supported but not required. Its skills in `.factory/skills/` link to the sections below, and its project hook runs the safety guard for you.

The rules in AGENTS.md apply everywhere in this page.

## Start from a task brief

Notion holds goals and decisions and stays authoritative. Linear tracks execution. [Contributing](contributing.md#sources-of-truth) names both. You don't need Notion or Linear credentials when the task owner gives you an exported brief: a reviewed copy of the ticket and its linked plan, pasted into the prompt, the PR description, or a file outside the checkout.

A brief needs these fields:

| Field | Contents |
| --- | --- |
| Source | Notion page URL, Linear ID if any, and the export date |
| Base | The commit SHA the brief was written against |
| Goal | What the change must achieve, in one or two sentences |
| Acceptance criteria | Checkable conditions for done |
| Invariants | Operation owner, flag modes, and the SQLite, ID, and recovery rules that apply |
| Tests and evidence | Commands to run and screenshots or recordings to attach |
| Status | What is done and the next action |
| Open decisions | Unresolved questions and who decides each one |

The owner copies only what the task needs. Tokens, cookies, pairing secrets, customer files, personal data, and unrelated private pages stay out of the brief. Exports are made by hand and reviewed; nothing syncs Notion into the repository.

A brief is a snapshot. Stop and ask the owner before you continue when:

- a field is missing or says "TBD";
- files the task touches changed on `main` after the base SHA;
- the brief disagrees with the code, [Architecture](../architecture.md), an ADR, or AGENTS.md;
- you can read the Notion page and it differs from the export.

Report the conflict and the facts on each side. The owner decides which wins and updates the source.

When you hand work off, put the current status, open decisions, and next action in the PR description. Session notes and `/tmp` files are gone for the next reader.

## Set up and run checks

AGENTS.md lists the setup and check commands. `just` is a convenience; `python3 scripts/local-ci.py` takes the same arguments as `just ci`. On Linux, the [development container](devcontainer.md) provides the pinned toolchains and native libraries. Don't install system packages on your own machine just to run a check.

A full local CI run exits 0 even when steps print SKIP, because a tool or platform is missing. A SKIP is not a pass. List each one under Evidence. For steps that must run on your machine, add `--fail-on-skip`:

```sh
python3 scripts/local-ci.py --list
python3 scripts/local-ci.py --only python-lint,local-ci-self-test --fail-on-skip
```

`just guidance` checks links, heading anchors, skill metadata, and Git trackability in AGENTS.md, this page, and the repo skills, and runs the safety guard tests. The [CI guide](ci.md) maps each hosted check to a local command.

## Safety guard

`scripts/agent-guard.py` checks one shell command, file edit, or patch before it runs. It refuses actions reserved for the maintainer. Exit 0 allows the operation. Exit 2 refuses it, or rejects input it does not understand, with the reason on stderr.

Droid runs it automatically through `.factory/hooks.json` and the adapter `.factory/hooks/guard.py`. No other runner does. Configure your runner's pre-tool hook to call the guard, or run it yourself before any `git` or `gh` command that pushes, merges, publishes, or changes settings, and before editing environment, Git, or generated files.

```sh
python3 -B scripts/agent-guard.py command --cwd "$PWD" 'git push -u origin HEAD:chore/topic'
python3 -B scripts/agent-guard.py command - < command.sh
python3 -B scripts/agent-guard.py file web/.env.local docs/guide.md
python3 -B scripts/agent-guard.py patch change.patch
```

A hook adapter can send one JSON object on stdin to `python3 -B scripts/agent-guard.py json`. Each operation needs exactly these fields, all strings:

```json
{"operation": "command", "command": "git push origin HEAD:topic", "cwd": "/abs/checkout"}
{"operation": "file", "path": "/abs/checkout/docs/guide.md"}
{"operation": "patch", "patch": "*** Update File: /abs/checkout/a.py\n..."}
```

The guard rejects other operations, missing or extra fields, a relative or missing `cwd`, an empty path, and a patch with no `*** Add File:`, `*** Update File:`, `*** Delete File:`, or `*** Move to:` line. A unified diff has none of those lines, so the guard rejects it rather than leave its paths unchecked. Map your runner's shell tool to `command`, its file writes to `file`, and its patch tool to `patch`; never pass a tool through unchecked.

The guard refuses merges, force pushes, pushes to `main`, release publishing or editing, tag pushes, and hook bypasses. It also refuses edits to real `.env` files, Git metadata, and generated manifests.

- `git push` needs an explicit branch refspec and common options spelled in full. Write push destinations literally. The guard refuses a destination built from a shell expansion, the `:` matching refspec, and `HEAD` when the same command also switches branches.
- When `push.followTags` is on, or a `remote.<name>.push` mapping exists, add `--no-follow-tags` or write `source:destination` refspecs. The guard treats both settings as unknown after a `git config` write or a `git remote` change in the same command.
- It refuses commits and pushes that run with `GIT_CONFIG_*` environment variables. Git and gh subcommands and option names must be literal.
- Run a `gh api` write that reads a request file (`-F key=@file` or `--input`) as its own command, after writing the file. `cd -P`, directory changes inside conditionals or groups, and a `cd` in a subshell, a pipeline, or under `env -C` leave the directory unknown, so relative `gh api` input files are refused.
- Through `gh api`, agents may read, write PR or issue comments, submit comment-only or pending PR reviews, and reply to or resolve review threads. Other API writes need the maintainer.
- It refuses a git or gh command it cannot classify.

The guard catches mistakes. It is not a shell sandbox or a security boundary, and an agent can run commands without it. Repository rules and required GitHub checks stay authoritative. Changes to the guard need the same review as any other security control.

## Writing and plans

Write docs, comments, and PR text plainly. State what the code or command does, with the number, path, or command where there is one. Use active voice. Cut filler, puffery, and abstractions the change doesn't need. Droid users can run the `unslop` skill; the result is what counts.

Before large or risky work, write the plan down and challenge it. Risky work includes moving operation ownership, persistence formats, printer sends, CI or security controls, and changes over about 1,500 lines. List how the plan could fail, check each risk against the code, and get open decisions answered by the owner before you build. The `grilling` skill does this in Droid.

## Review and merge

Every ready PR currently needs the required `droid-review` check, which runs Factory Droid review. That stays required until a separate change adds a provider-neutral review gate and the maintainer adopts it. If you work in another tool, the check still runs on your PR; ask the maintainer when it cannot. Fix each finding, or reply with why it doesn't apply, then resolve the thread. Merging needs green required checks and no open threads. Only the maintainer merges.

## Verify in a running app

Run the real app at the PR head and look at the change. Mocked tests passing does not show that a screen works.

### Choose the runtime

- `web/apps/server` owns Node compatibility behavior.
- `rust/crates/pp-server` owns only the operations already migrated to Rust.
- `web/apps/web` is shared. Pick the server that owns the operation under test.
- `rust/crates/pp-desktop` needs native execution for window, tray, protocol, and OS behavior. Browser evidence is not native acceptance.
- `slicer-sidecar` uses a synthetic slicer in tests. Never send a real print.

Before launching, write down the exact HEAD, operation owner, fixture, expected result, feature modes, time budget, and cleanup. Use fresh data and free loopback ports. Keep `desktop_rebuild` off unless the task tests its on mode.

### Web UI

The helper drives the Node server and the shared React UI. Give each run its own state, evidence, and data folders and two free loopback ports:

```sh
(
  set -eu
  : "${API_PORT:?pick a free loopback port}" "${UI_PORT:?pick a different free port}"
  ROOT="$(mktemp -d)"
  export PP_VERIFY_STATE_DIR="$ROOT/state" PP_VERIFY_EVIDENCE_DIR="$ROOT/evidence"
  trap 'node scripts/verify-print-partner/control-print-partner.mjs cleanup' 0
  node scripts/verify-print-partner/control-print-partner.mjs launch \
    --mode npm --data-dir "$ROOT/data" --api-port "$API_PORT" --ui-port "$UI_PORT"
  node scripts/verify-print-partner/control-print-partner.mjs doctor
  node scripts/verify-print-partner/control-print-partner.mjs screenshot \
    --path /builds --out "$ROOT/evidence/builds.png" --wait 1500
)
```

Screenshot each screen the change affects, in both themes (`--theme dark`) if styling changed. The launcher waits for readiness. Its state is `$PP_VERIFY_STATE_DIR/state.json`, and the service log is `$PP_VERIFY_STATE_DIR/<project>/npm-dev.log`, where `project` comes from that state. Keep those variables unchanged through cleanup, and read only the log lines you need.

### Dialogs and other client-only state

The helper opens a fresh browser for each command, so chaining its `click` and `screenshot` commands loses an open dialog, menu, or dropdown. Interact and capture in one Playwright program instead. Save it outside the checkout, then run it inside the launch block above, after the `doctor` line, as `ROOT="$ROOT" UI_PORT="$UI_PORT" node /path/to/capture.mjs`. It loads Playwright from `web/node_modules`, so run it from the repository root after `npm --prefix web ci`:

```js
import { createRequire } from "node:module";
import { resolve } from "node:path";

const { chromium } = createRequire(resolve("web/package.json"))("playwright-core");
const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE,
});
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(`http://127.0.0.1:${process.env.UI_PORT}/library`);
  // Replace these two steps with the interaction your change needs.
  await page.getByRole("button", { name: "Add source" }).click();
  await page.getByRole("menu").waitFor();
  await page.screenshot({ path: `${process.env.ROOT}/evidence/add-source-menu.png` });
} finally {
  await browser.close();
}
```

`PLAYWRIGHT_CHROMIUM_EXECUTABLE` names a Chrome or Chromium binary; the development container and the Factory cloud setup set it. Don't reload between the interaction and the capture. In Droid, the `agent-browser` skill works too if one session covers the interaction and the capture.

### Rust gateway

The Rust gateway requires a desktop session. A bare request returns `401 Desktop session required`. Use the existing launcher and its one-use bootstrap credential:

```sh
(
  set -eu
  : "${CAPTURE:?path to capture-rust.mjs, saved outside the checkout}"
  PRIVATE_DIR="$(mktemp -d)"
  EVIDENCE_DIR="$(mktemp -d)"
  LAUNCHER=""
  trap '[ -z "$LAUNCHER" ] || { kill -TERM "$LAUNCHER" 2>/dev/null || true; wait "$LAUNCHER" || true; }; rm -rf "$PRIVATE_DIR"' 0
  python3 scripts/verify-rust-gateway/verify.py launch \
    --credential-file "$PRIVATE_DIR/credential.json" >"$PRIVATE_DIR/launcher.log" 2>&1 &
  LAUNCHER=$!
  until grep -q '^PP_SERVER_URL=' "$PRIVATE_DIR/launcher.log"; do
    kill -0 "$LAUNCHER" 2>/dev/null || { echo "launcher exited before readiness" >&2; exit 1; }
    sleep 2
  done
  CREDENTIAL_FILE="$PRIVATE_DIR/credential.json" EVIDENCE_DIR="$EVIDENCE_DIR" node "$CAPTURE"
  echo "evidence: $EVIDENCE_DIR"
)
```

`mktemp -d` creates a directory only you can read. The credential path must not exist before launch. The first run builds `pp-server`, so readiness can take minutes. The log holds server stderr; read only the lines you need and never paste it whole.

The capture program, `capture-rust.mjs`, reads the bootstrap URL from the file, opens it so the redirect sets the session cookie, and then works in that same browser context:

```js
import { readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";

const { chromium } = createRequire(resolve("web/package.json"))("playwright-core");
const { bootstrap_url: bootstrapUrl } = JSON.parse(readFileSync(process.env.CREDENTIAL_FILE, "utf8"));
rmSync(process.env.CREDENTIAL_FILE);
const origin = new URL(bootstrapUrl).origin;
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE });
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(bootstrapUrl);
  if (new URL(page.url()).pathname === "/__desktop/bootstrap") throw new Error("bootstrap did not redirect");
  await page.goto(`${origin}/builds`);
  // Interact here, then capture without reloading.
  await page.screenshot({ path: `${process.env.EVIDENCE_DIR}/rust-builds.png` });
} finally {
  await browser.close();
}
```

Keep the credential and its URL out of command lines, logs, traces, screenshots, and PR text. Capture only after the redirect has left the bootstrap page. The gateway rejects a request whose `Origin` header differs from its own origin and requires `Origin` on writes. The browser sets it for you; a script that calls the gateway with another HTTP client must send `Origin` equal to `PP_SERVER_URL` and the session cookie. A bare `401` does not prove an operation works.

Stopping the launcher with SIGTERM shuts `pp-server` down and checks cleanup. The log should end with `STOP: exit=0`. [The launch test](../../scripts/verify-rust-gateway/test_launch.py) holds the bootstrap and shutdown contract.

### Desktop app

Desktop evidence comes from the native app built at the PR head, on a real display. Follow [Desktop and tray capture](../contributing/gate-evidence.md#desktop-and-tray-capture) for the lifecycle checks: close to tray, restore the same window, and quit with cleanup. Any native recorder works, such as macOS Screenshot (Shift-Command-5) or `screencapture`, Windows Snipping Tool, OBS, or the Homelab `evidence` CLI on Linux. The recorder changes nothing else: on Linux, disclose D-Bus tray activations as that section says.

A browser screenshot is never native evidence. With no native display, report desktop acceptance as BLOCKED; a missing display is not an exemption.

### Check and attach evidence

1. Open every file. Confirm it shows the change itself, not just a page loading, and only the app: no terminals, other windows, tokens, or customer data. The repository is public.
2. Drag the files into the PR description on GitHub, or use `evidence publish <pr-number> <files...>` where that tool exists.
3. Under `## Evidence`, give the head SHA and one line per file saying what it shows. When a UI path changed with no visible effect, such as a type-only refactor, give the reason on the `Exemption:` line. [Gate evidence](../contributing/gate-evidence.md) has the lint rules.

### Report and clean up

Record the journeys you ran and what you asserted, the runtime owner, flag mode, exact HEAD, duration, log location, evidence files, and cleanup outcome. For persistence changes, include restart, recovery, and stable IDs. For ownership changes, show that the non-owner refused and that no second mutation happened. Report a missing browser, display, or native tool as BLOCKED, never as a pass.

Stop only the processes you started, and remove the temporary folders you created. Keep non-secret logs and evidence outside the disposable data.
