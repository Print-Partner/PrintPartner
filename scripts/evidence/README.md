# Evidence harness

`scripts/evidence/run.sh` screenshots replacement-UI (`web/apps/ui`) paths from a Rust-only core on throwaway synthetic data, with one command:

```sh
scripts/evidence/run.sh --seed 1138 --module farm --shot /builds --shot /builds/2/history --shot /farm/log
```

One run:

1. Builds `pp-server` with `--features desktop_rebuild`, the web bundle and its `rust/bundle-manifest.json`, and the replacement UI. When the checkout (HEAD plus uncommitted and untracked changes under `web/` and `rust/`) is unchanged since the last harness build, it reuses that build, kept as `rust/target/evidence-harness/pp-server` with hashes of it, the bundle manifest and the UI build, so another `cargo build -p pp-server` cannot swap in a binary without the feature. `--rebuild` forces a build. Run `npm --prefix web ci` first.
2. Generates the [synthetic dataset](../../docs/storage.md#synthetic-installations) for `--seed` (default 1138) into a private temporary directory.
3. Starts the core with `PP_DESKTOP_REBUILD=1` through `scripts/verify-rust-gateway/verify.py launch --binary --seed`, which copies the dataset into its own temporary data directory, adds the Rust ownership marker and binds a loopback port. No Node child runs.
4. Serves the built replacement UI with `vite preview` on a free loopback port, forwarding `/api`, `/ws` and `/health` to the core.
5. Opens the one-use bootstrap link in Playwright's Chromium, registers a throwaway account with a random password, runs each `--step` through the Rust API, and saves one PNG per `--shot`. A shot waits until the screen has an `h1`, no `aria-busy` region and no "Loading" text, and fails on the "Not found" screen.
6. Stops the UI server and the core (SIGTERM, then the launcher's shutdown checks must print `STOP: exit=0`), deletes the seed, the data directory and the credential directory, and checks nothing was left behind. Step 6 runs on failure and on SIGINT, SIGTERM or SIGHUP too.

It prints each PNG path at the end and exits 1 on any failure, including an unclean stop.

## Options

| Option | What it does |
| --- | --- |
| `--seed N` | Synthetic dataset seed. Default 1138. |
| `--shot PATH` | App path to screenshot, such as `/builds/2/history`. Repeat for more. The file is the path with `/` as `-`: `builds-2-history.png`. |
| `--step NAME:BUILD` | Setup step before the shots, run in the order given. See below. |
| `--module NAME` | Turn a Module on (`farm`, `orders`, `bambu` or `spoolman`) before the core starts. Repeat for more. All four start off, so `/farm/...` paths need `--module farm`. |
| `--out DIR` | Where the PNGs go. Default: a new temporary directory. Keep it out of the checkout. Only this run's files are listed and published. |
| `--full-page` | Capture the whole scrolled page, not only the 1440x900 viewport. |
| `--rebuild` | Build even when the checkout is unchanged. |
| `--display [:N]` | Show Chromium on the X display (default `$EVIDENCE_DISPLAY` or `:99`) and capture it with `evidence shot` instead of headless screenshots. |
| `--publish PR --note TEXT` | After a clean run, run `evidence publish` for the PNGs with `EVIDENCE_REPO=Print-Partner/PrintPartner`. |
| `--self-test` | Runs `node --test scripts/evidence/safety.test.mjs`, captures `/builds`, and checks the PNG, the clean stop and the removed data directory. |

## Setup steps

| Step | What it does through the Rust API |
| --- | --- |
| `settle-reconciliation:BUILD` | Resolves every conflict on the Build's open draft with a `replace` decision (`PUT .../drafts/{draft}/reconciliation`). Does nothing without an open draft or conflicts. |
| `snapshot:BUILD` | Accepts the Build's open draft, which stores a pre-apply snapshot and a new revision. Without an open draft it opens one by saving the current header unchanged. Settles reconciliation first. |

`--module` is not an API step: no Rust route writes tenant Module choices yet (Node owns `PATCH /settings/features`). It writes the `default` tenant's `feature_toggles` setting in the generated seed, the row that route and Rust's `/health` read, before the copy is made.

Add a step in `STEPS` in `capture.mjs` and its name in `STEP_NAMES` in `run.py`.

Example: History after a snapshot:

```sh
scripts/evidence/run.sh --step snapshot:2 --shot /builds/2/history
```

## Safety

- Only synthetic data: the seed is generated each run, and `verify.py --seed` refuses a directory without `SYNTHETIC-DATASET.json`. Its printers have no host link (`*.invalid` names), so the core contacts no printer and nothing can send a print.
- Loopback only: the core, the UI server and every request use `127.0.0.1`.
- Secrets: the bootstrap link stays in a `0700` directory, the capture program deletes it after reading it, the account password is random and never printed, and Chromium's profile lives in the work directory that cleanup removes. Playwright's errors carry a call log with every request header, the session cookie included, and navigation errors carry the URL. `safety.mjs` turns every request, navigation and screenshot failure into a fixed message (method, path, status or `transport error`), and prints any other uncaught error as its class name only. `safety.test.mjs` proves a failed request prints no cookie.
- Signals: the core and the UI server are started with SIGINT, SIGTERM and SIGHUP held until the process is recorded, so a signal can never leave one running that cleanup does not know about. The launcher log (server stderr, no bootstrap link) is kept only when the core did not stop cleanly, next to the PNGs as `launcher.log`.
- Open every PNG before you attach it. The repository is public.

## Publishing

`/opt/agent-fleet/bin/evidence` still defaults to `poitee/PrintPartner`, and it pushes to the `origin` of its cache clone, whichever repository that clone came from. `--publish` passes `EVIDENCE_REPO=Print-Partner/PrintPartner` and its own cache, `~/.cache/evidence-repo-Print-Partner-PrintPartner`, and refuses to publish when that clone's `origin` is anything else. To publish by hand:

```sh
EVIDENCE_REPO=Print-Partner/PrintPartner EVIDENCE_CACHE=~/.cache/evidence-repo-Print-Partner-PrintPartner \
  evidence publish <pr> <files...> --note "..."
```
