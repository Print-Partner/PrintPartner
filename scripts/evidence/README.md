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
| `--publish PR --note TEXT` | After a clean run, publish this run's PNGs as a comment on that `Print-Partner/PrintPartner` PR. See [Publishing](#publishing) for the checks. |
| `--self-test` | Runs `node --test scripts/evidence/safety.test.mjs scripts/evidence/steps.test.mjs`, captures `/builds`, and checks the PNG, the clean stop and the removed data directory. |

## Setup steps

| Step | What it does through the Rust API |
| --- | --- |
| `settle-reconciliation:BUILD` | Resolves every conflict on the Build's open draft with a `replace` decision (`PUT .../drafts/{draft}/reconciliation`). Does nothing without an open draft or conflicts. |
| `snapshot:BUILD` | Accepts the Build's open draft. Without an open draft it opens one by saving the current header unchanged. Settles reconciliation first. Accept stores a pre-apply snapshot only when the Build has an Accepted revision whose Source rows the core can still read: no snapshot without an Accepted revision or its input set, with a missing Source row, or with `legacy:` rows. The step reads `snapshot.saved_before_accept` from the accept preview and fails with `Build N: Accept saves no snapshot (<core note>)` before Accept when it is false. |

`--module` is not an API step: no Rust route writes tenant Module choices yet (Node owns `PATCH /settings/features`). It writes the `default` tenant's `feature_toggles` setting in the generated seed, the row that route and Rust's `/health` read, before the copy is made.

Add a step in `STEPS` in `steps.mjs` and its name in `STEP_NAMES` in `run.py`.

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

`--publish PR` publishes the run's PNGs as a PR comment through the root-owned `/opt/agent-fleet/bin/evidence`, which pushes them to the `evidence/<pr-branch>` branch. To publish PNGs you already have, with the same checks:

```sh
python3 scripts/evidence/publisher.py <pr> <files...> [--note "..."]
```

### Threat model

What it defends: whatever the user's git config or environment says, the harness prints no token, and the PNGs go only to `Print-Partner/PrintPartner`. The user's global and system git config, `includeIf` rules, `insteadOf` and `pushInsteadOf` rewrites, `GIT_TRACE*`, `GIT_CURL_VERBOSE`, `GIT_SSH_COMMAND` and `git -c` settings passed down through `GIT_CONFIG_PARAMETERS` all count as untrusted input.

What it does not defend: a local user who edits the root-owned publisher, this harness, or the cache's hooks. Such a user can already push anything with their own credentials.

How (`scripts/evidence/publisher.py`, tests in `test_publisher.py`):

- **Isolated git.** The clone, every check and the publisher run with no `GIT_*` variable from the caller. Instead they get `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, `credential.helper=!gh auth git-credential` (through `GIT_CONFIG_COUNT`) and `GIT_TERMINAL_PROMPT=0`. No system or global rule, including one that applies only on a branch, can rewrite a URL or turn on tracing. The publisher sets its own commit identity with `git -c`.
- **A clean cache.** The cache is `~/.cache/evidence-repo-Print-Partner-PrintPartner`, used by its real path. Publishing is refused when any of these holds:
  - the cache or its parent directory is a symlink;
  - `.git` is not a real directory;
  - the cache's own config has an `include`, `includeIf` or `url.*` key;
  - any fetch or push URL of `origin` (`git remote get-url [--push] --all origin`) is not `github.com/Print-Partner/PrintPartner`.

  Before comparing, one trailing `/` and one trailing `.git` are dropped, and `git@github.com:` and `ssh://git@github.com/` map to the same form. A URL with any other userinfo is refused, so no URL the harness accepts carries a token.
- **A checked first publish.** A missing cache is cloned first (`git clone --quiet --no-checkout --filter=blob:none`) and checked the same way. A clone that fails or fails the check is removed.
- **No output from git or the publisher.** Refusals name the cache path, never a URL. Any error while reading or parsing a remote counts as a refusal and is dropped, for example Python's `ValueError` for an NFKC-unsafe character in a host, which quotes the whole netloc. The publisher's stdout and stderr are captured. On success the harness prints only the evidence URLs it reports, matched against `https://github.com/Print-Partner/PrintPartner/...`. On failure it prints `publish failed (exit N)` and nothing else.
