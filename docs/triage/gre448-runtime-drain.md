# GRE-448 core shutdown evidence

`CoreRuntime` closes tracked-work admission and gateway admission, disarms compat
restarts, and gives both drains the same five-second budget. Shutdown receipts
report `tracked_idle`, `gateway_idle`, and `compat_exit` alongside cleanup results.
`complete()` requires both drains and cleanup to succeed. A forced compat exit can
complete process cleanup, but cannot authorize a cutover.

If tracked Rust work outlives that budget, shutdown reports
`tracked_drain_timed_out` and `storage_ownership_retained`. Compat still terminates
under its existing ten-second SIGTERM grace. The storage lease stays held until
the owning process exits because the tracked task may still write. A same-process
restart refuses even if that task subsequently releases its guard. After process
exit, the next owner can recover the stale lease through the existing startup path.

A gateway drain timeout reports `gateway_drain_timed_out`. Gateway and compat
cleanup still run. Storage release requires proved compat cleanup and an idle
tracked-work gate. No timeout commits an ownership change.

The headless server emits its shutdown receipt before checking completeness.
The streaming smoke test holds a download open through the drain deadline and
requires exit 1 with only `gateway_drain_timed_out`, plus child cleanup, removal
of the runtime directory and owner marker, and reacquisition of the storage lock.
Its ordinary logout case still requires exit 0.
The portable server smoke likewise holds a WebSocket open across SIGTERM and
requires the exact timeout receipt, exit 1, and complete resource cleanup.

The tests in `rust/crates/pp-core/tests/runtime_m0/drain.rs` launch the measured
Node bundle through the public core API. Each case runs in a separate process so
runtime flags and retained leases cannot contaminate another case. They cover:

- Cancellation, rejection of new tracked work and HTTP mutations, release after
  work finishes, and repeated startup without overlapping compat children.
- A five-second tracked-work timeout, exact failure receipt, retained lock,
  refused same-process restart, and successful restart after owner process exit.
- An admitted HTTP request stalled before sending its body, a five-second gateway
  drain timeout, and an incomplete receipt despite successful process cleanup.
- Flag-off cutover refusal while the original runtime remains usable.
- With `desktop_rebuild` and `PP_DESKTOP_REBUILD=1`, a graceful cutover of all four
  catalog GET/HEAD aliases, two recoveries, and unchanged authority bytes.
- Cutover refusal after a tracked timeout or a SIGSTOP-injected compat stall.
  The latter requires the real ten-second grace and records forced termination.

The cutover tests supply a synthetic one-fixture parity receipt to exercise the
lifecycle gate. They do not prove Node/Rust catalog parity. Rust-owned requests
still return `rust_owner_unavailable`; unrelated Node routes remain available.
The timeout safety fix applies without flags. Cutover activation keeps both
existing opt-ins.

Build the current Node/web bundle and measured manifest before running:

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail -E 'test(ticket_t_3_drain)'
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core --test runtime_m0 --features desktop_rebuild --no-tests=fail -E 'test(ticket_t_3_drain)' --success-output immediate
```

The second command runs in the desktop rebuild CI gate and emits exact shutdown,
cutover, and restart evidence. GRE-448 remains In Progress. Worker fencing, Rust
handlers, operator entry, real parity generation, rollback rules, interrupted
upload coverage beyond the already merged fixtures, and Mac logout evidence
remain outside this slice. This is headless unsigned evidence.

The pp-compat simulator acceptance test
`ticket_t_3_drain_repeated_eof_interrupts_mutation_without_resend` runs three
supervisor starts against a real Node Unix-socket server. Each POST records its
payload before withholding the response. The server ignores SIGTERM so its
recorded EOF and exit prove that shutdown closed the parent pipe. Each cycle
requires graceful shutdown within five seconds, an unknown dispatch outcome,
zero tracked connection tasks, a closed stale endpoint, exactly one accepted
payload, and an absent old process group before the next start. It emits the
exact accumulated event log and failure strings. This proves the compat transport
under a simulator; the merged printer-upload fixtures cover persisted job retry
policy. It does not prove parent death during desktop startup or Mac logout.

The pp-compat simulator test
`ticket_t_3_drain_force_killed_owner_during_startup_kills_group` covers abrupt
owner death during the pending startup setup write. In each of two isolated
supervisor cycles, a real Node process acknowledges execution without reading
stdin. A 1 MiB setup keeps startup pending. The owner confirms `Starting`, no
endpoint, zero health probes, and one child before the outer test stops Node
and kills the Rust owner with SIGKILL.

A live peer in Node's process group prevents POSIX orphan-group SIGHUP from
making the test pass without the parent-death watcher. Node cannot handle stdin
EOF while stopped. Both Node and the peer must terminate within one five-second
wall-clock budget, and the peer must report SIGKILL. The test checks the first
cycle's deaths before starting the second and emits a receipt for each cycle.
Failure cleanup kills the owner and Node groups and reaps direct children.

This proves the supervisor's shell simulator retains the parent-death guard
across the setup await. It does not exercise the measured release spawn path,
desktop storage recovery, or actual Mac logout. Full worker cutover and those
release-level gates remain open.

The startup fixture's negative control temporarily calls
`parent_death.release()` in `Process::spawn_shell`, after child PID recording
and before the setup write. The new acceptance test fails with
`startup group outlived force-killed owner`. Restoring the original guard makes
both cycles pass. No control hook or production change ships. This is a
coverage gap, not a newly fixed production defect.

The final local pp-compat drain selection passed 19 tests with 50 skipped.
Its environment-gated owner helper is outside the drain selection. Run it from
the integration worktree root with a generated measured manifest available:

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail -p pp-compat -E 'test(ticket_t_3_drain)' --success-output immediate
cargo clippy --locked --manifest-path rust/Cargo.toml -p pp-compat --all-targets -- -D warnings
cargo fmt --all --manifest-path rust/Cargo.toml --check
node --check rust/crates/pp-compat/tests/fixtures/startup-no-stdin.cjs
```

The local run supplied an existing manifest through
`PP_DESKTOP_RELEASE_MANIFEST` for compile-time embedding. The startup fixture
uses its checked-in simulator and does not load the manifest's release assets.

The pp-compat simulator test
`ticket_t_3_drain_repeated_eof_joins_unread_response_body` covers shutdown after
response headers arrive while the caller keeps the response body unread. A Node
Unix-socket fixture writes a 1 MiB chunk without ending the response and ignores
SIGTERM. The test retains the body until shutdown has returned and all cleanup
assertions have passed. Each of two supervisor cycles requires a successful
response header, an active connection task, graceful shutdown within five
seconds, zero connection tasks, an unpublished and rejecting stale endpoint,
and an absent child process group before the next start. The exact event log
contains one `GET /stream` and one `EOF` per cycle.

The negative control temporarily removes `self.cancelled.cancel()` from
`Connections::close`. The test fails with
`unread response connection was not joined: Compatibility connection join timed out`.
Restoring the cancellation makes both cycles pass. This proves the connection
join needs cancellation even after pipe EOF and child exit. No production change
or control hook ships. The fixture does not exercise the measured release bundle,
HTTP gateway draining, storage recovery, worker cutover, or actual Mac logout.

Run with a generated manifest available for the build-time embedding prerequisite:

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-compat --no-tests=fail -E 'test(ticket_t_3_drain_repeated_eof_joins_unread_response_body)' --success-output immediate
node --check rust/crates/pp-compat/src/fixtures/drain-stream.cjs
```

Local verification supplied `/tmp/gre333-stage/bundle-manifest.json` through
`PP_DESKTOP_RELEASE_MANIFEST` only for compile-time embedding. The negative
control failed 0/1 in 2.321s. The restored drain selection passed 20/20, with
50 skipped, in 30.324s. The new case recorded shutdown at 12ms and 8ms, zero
connection tasks, and the exact event log `GET /stream\nEOF\n` twice. These are
simulator acceptance results, not release performance measurements.

## Measured release startup parent death

`ticket_t_3_drain_measured_release_startup_parent_death` adds the measured
`Process::spawn` path to the startup proof. It is an ordinary pp-compat unit
test, with no ignore or environment skip. The desktop runtime and fleet Rust
CI jobs already build the web assets and embed their measured manifest. The
test uses that embedded identity, the repository web assets, and native
`/usr/bin/node`. Missing assets or a mismatched manifest fail the test.

The `record_child` callback records the PID, stops Node, and returns a test
ownership marker. A 1 MiB lease then backpressures the real setup write. On the
owner's current-thread Tokio runtime, the owner task resumes only after spawn
has yielded. Before acknowledging startup, the owner requires stopped Node,
`Starting`, no published endpoint, zero completed health probes, and one child.
This tests startup before setup delivery, without requiring backend readiness
or opening desktop storage.

The existing RAII fixture now runs both the shell and measured cases. The
measured case allows 30 seconds to reach startup while verifying full CI assets.
The shell case keeps its eight-second startup bound. Each measured case has
two sequential cycles. A live peer joins Node's group before
the Rust owner receives SIGKILL. It excludes orphan-group SIGHUP as the reason
for cleanup. Stopped Node cannot process stdin EOF. Owner exit, Node death, and
peer death share one five-second deadline. The direct peer must report SIGKILL.
Each cycle proves both processes dead before the next cycle starts. Failure
cleanup kills both groups and reaps the direct owner and peer.

This worktree has no built web assets. The local rerun script builds an explicit
measured simulator package with a copy of native Node v24.21.0, a preload, and
an entry that leaves stdin unread. It invokes the unchanged release manifest
builder, embeds that manifest through `PP_DESKTOP_RELEASE_MANIFEST`, and uses
`PP_COMPAT_MEASURED_STARTUP_STAGE` only to select those assets in the test.
`verify_bundle` and the real spawn's `verify_backend` both run. The simulator
entry records its PID and preload marker before the callback stops it. The
fixture asserts both markers and unread stdin. The package includes minimal
measured metadata, frontend, contract, domain, and dependency files. These are
simulator assets, not a production backend bundle or a substitute for release
verification. No generated manifest or verification bypass is checked in.

Run the bounded fail-before/pass-after proof from the repository root:

```sh
python3 rust/scripts/prove-measured-startup-death.py --negative-control --evidence /tmp/gre448-proof
```

The script first requires one passing acceptance test with two cycle receipts.
It temporarily calls `parent_death.release()` only in measured `Process::spawn`,
then requires exit 101 and `startup group outlived force-killed owner`.
A `finally` block restores `lib.rs` byte-for-byte before another passing run.
The script records its restored source hash and directly checks that negative
failure cleanup left no live owner, Node, or peer. There is no production code
change or control hook in the committed diff.

The exact Cargo command for all three runs is:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-compat --lib ticket_t_2::ticket_t_3_drain_measured_release_startup_parent_death -- --exact --nocapture
```

The two environment variables and generated asset paths are recorded in
`/tmp/gre448-proof/environment.json`. `commands.log` records the manifest and
Cargo commands. `baseline.log`, `negative.log`, and `restored.log` retain the
full results. `negative-cleanup.json` records the post-failure process states;
`restored-lib-sha256.txt` records the restored production source hash.

Local results prove measured simulator startup on Linux. CI execution against
the full built web bundle remains to be reviewed by the parent. Mac logout,
storage recovery, worker fencing, and full worker cutover remain outside this
slice.

The final local proof passed one baseline test, failed one negative test with
exit 101 at the intended five-second invariant, and passed one restored test.
Both passing runs completed two cycles. All three recorded negative-control
processes were absent after failure cleanup. The final pp-compat drain selection
passed 21 tests with 60 skipped. Clippy with warnings denied, workspace format
checking, both Node syntax checks, and Python syntax checking passed. A supplied
shell dispatcher was rejected without execution or stage creation.

For local follow-up checks, export both values from `environment.json` before
running these commands. Full logs are `drain.log`, `clippy.log`, `fmt.log`,
`syntax.log`, and `native-rejection.log` in the same evidence directory.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail -p pp-compat -E 'test(ticket_t_3_drain)' --success-output immediate
cargo clippy --locked --manifest-path rust/Cargo.toml -p pp-compat --all-targets -- -D warnings
cargo fmt --all --manifest-path rust/Cargo.toml --check
node --check rust/crates/pp-compat/tests/fixtures/measured-startup-entry.cjs
node --check rust/crates/pp-compat/tests/fixtures/measured-startup-preload.cjs
```

The review follow-up validates Node v24.21.0, ABI 137, host OS, and host
architecture before creating evidence or simulator directories. Native binary
validation still precedes execution. `node-identity.json` records the queried
identity. Negative cleanup checks every owner, Node, and peer tuple in the log.

The rerun with `--negative-control --evidence /tmp/gre448-proof-review1` passed
baseline and restored tests with two cycles each. The negative test exited 101
at `startup group outlived force-killed owner`; all three recorded PIDs were
absent afterward. An independent byte comparison confirmed unchanged `lib.rs`.
The focused script tests passed four cases, including a synthetic two-cycle log
and real `ps` rejection of a live second-cycle PID in each role. Mocked identity
queries against a native executable proved wrong version, ABI, OS, and
architecture fail before evidence or stage creation. Dispatcher rejection,
Python compilation, and all seven existing stager self-tests also passed.
Focused results are in `/tmp/gre448-proof-review1/script-tests.log`.

```sh
python3 -m unittest discover -s rust/scripts -p test_prove_measured_startup_death.py -v
```

The second review follow-up uses resolved `sys.executable` for mocked identity
tests and accepts the stager's complete nine-header ELF, Mach-O, and FAT set.
All five proof-script tests passed, including nine header subcases that reach
identity validation, canonical header-set equality, and dispatcher rejection
before execution. All seven stager self-tests passed. Logs are
`/tmp/gre448-proof-review2/script-tests.log` and `stager-tests.log`.

The bounded rerun with `--negative-control --evidence /tmp/gre448-proof-review2`
passed one baseline and one restored test with two cycles each. The negative
test failed once with exit 101 at `startup group outlived force-killed owner`.
All three recorded PIDs were absent afterward. An independent byte comparison
confirmed unchanged `lib.rs`, recorded in `source-restoration.log`. Full driver
results are in `driver.log`, `baseline.log`, `negative.log`, and `restored.log`
in that evidence directory. No Rust or stager changes were retained.

## Measured release repeated start, stop, and EOF cleanup

`ticket_t_3_drain_measured_release_repeated_eof_cleanup` runs three sequential
supervisor cycles through measured `Process::spawn`. Each cycle owns its scratch
directory and recorded process group. The completed cycle records retain their
literal event logs. Before the next spawn and while its successor is ready, the
test checks that every prior group is absent, its spawn count remains one, and
its event log has not changed.

Each cycle holds an unread 1 MiB response body and sends one POST. The fixture
records the accepted payload but withholds its response. Shutdown must finish
within five seconds with `Graceful`, zero connection tasks, an unpublished
endpoint, `Stopped`, and an absent child group. The retained endpoint must reject
a new POST with `Compatibility generation is stopping`. The interrupted POST
must return `Compatibility dispatch outcome unknown`. The held response body
stays alive until all cleanup assertions pass. The exact per-cycle log is:

```text
SPAWN <pid>
GET /stream <pid>
POST /upload payload-<cycle> <pid>
EOF <pid>
EXIT <pid> 0
```

The lifecycle entry needs a server and a flowing setup pipe. Extending the
startup entry would defeat its unread-stdin invariant, so this slice adds one
separate entry and reuses the existing native Node validation, measured package
builder, preload, and proof driver. The entry ignores SIGTERM. A graceful exit
therefore requires observed stdin EOF. The real release verifier measures the
Node executable, preload, entry, frontend, metadata, contracts, domain, and
simulator dependency inventory. No production guard, verification bypass, or
control hook is added.

Run the bounded proof from the repository root:

```sh
python3 rust/scripts/prove-measured-startup-death.py --scenario lifecycle --negative-control --evidence /tmp/gre448-lifecycle-proof2
```

The driver requires one executed acceptance test and all three cycle receipts.
Its negative control temporarily removes `self.cancelled.cancel()` from
`Connections::close`. The unread body must then fail owned connection cleanup
with `Compatibility connection join timed out`. A `finally` block restores
`lib.rs` byte-for-byte before a passing rerun. The driver records the restored
source hash and checks that every PID reported by the failing test is dead.
This establishes a coverage gap. Production cleanup already works.

The test is explicitly ignored in ordinary unit selections because its embedded
manifest must identify the lifecycle simulator, rather than the startup simulator
or production backend. The driver selects this exact test with `--ignored` and
rejects zero tests or missing cycle receipts. The desktop runtime workflow runs
the driver as a required step after its built-backend unit and lint checks.
It generates and validates the simulator assets in the retained evidence
directory before running Cargo. This CI step exercises measured simulator
assets, not the production backend's HTTP or persistence behavior.

`environment.json` records both manifest and stage selectors. `commands.log`
records the exact manifest and Cargo invocations. `asset-identity.json` records
the manifest, native Node, entry, and preload hashes and the source commit.
`baseline.log`, `negative.log`, `restored.log`, `negative-cleanup.json`, and
`restored-lib-sha256.txt` retain the proof evidence. Startup remains the driver's
default scenario.

This is Linux headless simulator acceptance evidence for the measured spawn,
transport, and cleanup path. It does not prove storage recovery, persisted job
retry policy, production backend shutdown, worker fencing, full worker cutover,
or Mac logout. GRE-448 remains open. The next release-level slice is to prove
these lifecycle observations against the full built backend with its real
storage ownership and interrupted request behavior.

Initial local verification used native Node v24.21.0, ABI 137, Linux x64. The measured
simulator manifest identifies source baseline
`c65dee177c5b1e0c4754d474d5c6b7b9917997b7`. Its SHA-256 is
`498537e8eec435a4f9bc4258ef3e43ab32edc307b72a490e6ca3cbed729096c7`.
The entry SHA-256 is
`be7e5e3f94825bff4fb6a46646ff2b5749ae442fb450ca10cc4e5cf86105f338`.
The complete native Node and preload identities are in `asset-identity.json`.
The final bounded proof in `/tmp/gre448-lifecycle-proof2` passed one baseline
test with three cycles in 6.27s, failed the intentional negative test with
exit 101 at the owned join timeout in 4.77s, and passed one restored test with
three cycles in 5.93s. The negative PID was absent afterward. The independent
source hash matches `restored-lib-sha256.txt`, and `lib.rs` has no final diff.
These durations describe acceptance runs, not release performance.

The unchanged default startup scenario also passed its two-cycle proof in
`/tmp/gre448-lifecycle-startup-regression`. Five driver unit tests, Python
compilation, the new Node fixture's syntax check, workspace format checking,
and pp-compat Clippy with warnings denied passed. Supporting logs are
`script-tests.log`, `fmt.log`, and `clippy.log` in the lifecycle evidence directory.

The nonzero drain selection passed 21/21 with 72 skipped in 30.329s, using the
startup simulator manifest for the existing measured startup case. Export both values from
`/tmp/gre448-lifecycle-startup-regression/environment.json` before the Cargo
commands below. The output is in `drain.log`. The new lifecycle case runs
separately under its own measured manifest through the driver above.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail -p pp-compat -E 'test(ticket_t_3_drain)' --success-output immediate
cargo clippy --locked --manifest-path rust/Cargo.toml -p pp-compat --all-targets -- -D warnings
cargo fmt --all --manifest-path rust/Cargo.toml --check
python3 -m unittest discover -s rust/scripts -p test_prove_measured_startup_death.py -v
node --check rust/crates/pp-compat/tests/fixtures/measured-lifecycle-entry.cjs
```

## Built backend repeated EOF cleanup

`rust/tests/built_backend_lifecycle.py` runs the built
`web/apps/server/dist/current/desktop.js` with its real dependency tree and native
Node. It supplies the desktop ownership marker, inherited lock descriptor 198,
and parent setup protocol. Signed Unix-socket `/health` requests require an
available SQLite database and an authenticated desktop principal before each
shutdown.

Three sequential cycles reuse one data directory. Each cycle closes the setup
pipe while the Python owner stays alive. A test-only preload records stdin EOF,
received signals, and the application's process-group kill without replacing the
server entry, routes, database, or shutdown implementation. The harness requires
EOF before the application's kill, no SIGTERM, child exit, an absent process
group, and a removed socket before the next start.
Receipts identify the resolved Node version and ABI, built entry digest, server
package version, health response, and prior child identities.

Run after `npm --prefix web ci` and `npm --prefix web run build`:

```sh
python3 rust/tests/built_backend_lifecycle.py --node "$(command -v node)" --negative-control --evidence /tmp/gre448-built-backend
```

Each invocation prints and creates a unique `run-*` directory under the supplied
evidence root. Repeating the command preserves all previous receipts.

The negative control suppresses stdin EOF delivery in the test observer. It must
fail the same bounded shutdown assertion after real health readiness. Failure
cleanup kills and reaps the owned child. The driver then restores normal EOF
delivery and requires three further cycles to pass. Production source and built
assets stay unchanged during this control. Desktop runtime CI runs this command
against its freshly built backend and uploads the receipts and logs.

This is Linux headless evidence for the built backend's EOF path. It does not
exercise the Rust supervisor's measured spawn path, interrupted printer uploads,
full worker cutover, startup parent-death, or actual macOS logout. Those areas
retain their separate evidence and remaining ticket gates.

The owner independently ran the full command on 2026-10-15 with native Node
v24.21.0, ABI 137, Linux x64. Three baseline and three restored cycles passed.
The suppressed-EOF cycle failed with `backend did not stop after stdin EOF`;
its guard recorded external cleanup and no remaining PID or group. All six
positive guards recorded no external signal. All seven child PIDs were absent
when checked after completion. The built entry digest stayed
`0356744362f0fa28e5ad3ed516de7fc71aca167af7fa6428a4e6b87650541bc7`.
Local receipts are in `/tmp/gre448-built-proof/owner-proof`. This negative control
proves the test detects missing EOF delivery; it does not identify a new
production defect. Python compile, Node syntax, native dispatcher rejection,
comment review, and diff checks passed. The implementation child independently
ran the same complete proof before the owner's rerun.

Review found that reusing the documented evidence root raised `FileExistsError`
after overwriting its artifact identity. The owner reproduced that failure, then
ran the corrected command twice against the existing root. Both isolated runs
passed three baseline cycles, the expected negative control, and three restored
cycles. All 14 child PIDs were absent, and all 60 original evidence files stayed
byte-for-byte unchanged. The isolated receipts are under
`/tmp/gre448-built-proof/owner-proof/run-9dy9pxzk` and `run-7pjt7tgu`.

## Full built backend startup parent-death cleanup

`rust/scripts/prove-built-startup-death.py` drives an ignored, exact Rust test
against an isolated copy of the full desktop build. The owner launches the real
`desktop.js` through production `Process::spawn`, including its production
parent-death watcher and release verification. The driver compares the original
and instrumented release manifests. Only `desktop-resolution.js` changes,
including its physical path in the workspace dependency inventory. The desktop
entry, frontend, other workspace files, Node binary, and other dependencies must
have identical hashes.

The staged resolver preserves the original resolver bytes and appends
`built-startup-observer.mjs`. This test observer delegates the actual stdin
`data` listener registration, records the real `setupFromParent` call stack and
entry SHA-256, then sends itself SIGSTOP before returning to the entry. Static
entry dependencies have loaded at this milestone. Setup has not arrived, and
storage and server readiness have not been reached.

The Rust `record_child` callback waits for that stopped milestone. It then
returns to the production setup write with a one-megabyte fixture lease. The
stopped backend cannot consume that write. After a further 100 ms, the owner
requires `Starting`, zero completed health probes, no published endpoint, no
socket, and exactly one recorded spawn. The oversized setup is intentionally
invalid for the desktop's normal setup size limit. It never reaches the setup
parser in this proof.

Each of two cycles SIGKILLs only the owner PID. An unrelated `/bin/sleep` child
of the test runner joins the backend group. The owner and backend groups must
differ. The backend remains stopped, so it cannot handle stdin EOF. Within five
seconds, the backend group must disappear and the peer must die by SIGKILL.
An isolated reexecution of the exact ignored Rust test acts as a Linux
subreaper and reaps the adopted backend and watcher. The calling test runner
never changes its subreaper state. The child uses `--include-ignored`, and the
GRE-826 validator requires exactly one named test to pass. On failure, the
wrapper forwards captured stdout and stderr, preserving the proof driver
assertion `built startup group outlived force-killed owner`. Receipts require
absent process identities and an absent group, rather than treating zombies as
reaped. The spawn ledger must remain at one entry.
The fixture guard runs only after these positive assertions.

The asset-free regression
`built_startup_reexec_keeps_parent_subreaper_unchanged` runs parent states 0 and
1 in separate test processes. Each calls the built-test wrapper across a real
executable boundary. A probe after the child's subreaper call records its PID
and enabled state before any assets are read. The regression requires a
different PID, state 1 in the child, and unchanged state in each parent. It also
rejects a launch that omits ignored-test selection.

Build and run with a host-native Linux Node 24.21.0 ABI 137 binary:

```sh
npm --prefix web ci
VITE_PRINT_PARTNER_DESKTOP=1 npm --prefix web run build
python3 rust/scripts/prove-built-startup-death.py --node /absolute/path/to/native/node --negative-control --evidence /tmp/gre448-built-startup
```

Keep the pinned native Node directory first in `PATH` for the npm commands.
The driver rejects shell dispatchers before executing them. Each invocation
creates a separate `run-*` evidence directory and records its exact commands.

The negative control copies the Rust workspace into the isolated stage and
disarms the production parent-death guard only in that private source copy:
Unix `adopt` in `pp-compat/src/unix.rs` calls `release()` right after it drops
the watcher pipe's read end.
Its Cargo target directory is separate from the calling checkout. The same exact
test must fail with
`built startup group outlived force-killed owner`. A pre-cleanup observation
requires the owner to be reaped, the backend still stopped, and the peer alive.
The fixture then kills and reaps the leaked group. A `finally` block restores
the private `unix.rs` byte-for-byte before checking the negative result. The
calling checkout's source stays unchanged, including if termination bypasses
`finally`. The driver requires two more cycles with the original guard and
retains source isolation and restoration hashes.

Evidence includes original and instrumented manifests, original and instrumented
preload hashes, the instrumented preload, built entry and Node hashes, startup
stacks, PID start identities, group membership, owner logs, armed state, cycle
receipts, failure observations, cleanup receipts, and source restoration.
Desktop runtime CI explicitly runs the full command. CI uploads receipts and
manifests but excludes the staged package copy. Local runs retain that copy.

This is Linux headless evidence with an instrumented, measured preload and an
unchanged full built entry and dependency tree. It proves group cleanup after
owner death during setup, without relying on JavaScript EOF handling. It does
not prove packaged desktop launch, platform signing, storage readiness, macOS
logout, or a natural startup stall. Built ready-time EOF and SQLite health remain
covered by the separate lifecycle proof above.

The implementation run on 2026-10-15 passed two baseline and two restored
cycles. Disarming the production guard failed the intended group-survival
assertion, and its guard removed the leaked group. All 19 recorded owner,
backend, peer, and watcher PIDs were absent after completion. `lib.rs` matched
its original bytes, and the built entry retained SHA-256
`0356744362f0fa28e5ad3ed516de7fc71aca167af7fa6428a4e6b87650541bc7`.
Local receipts are in
`/tmp/gre448-parent-death-implementation/proof/run-8z69qpl1`.
The artifact identifies 42,167 measured dependency files. Focused Clippy with
warnings denied, workspace formatting, Python compilation, observer syntax,
workflow YAML parsing, native-dispatcher rejection, and the AST ratchet also
passed. The test helper lives under `src/tests/`, with unchanged lint baselines.
This is the implementation run; independent parent verification remains separate.

The owner independently ran the full negative-control command on 2026-10-15.
Both baseline cycles passed, the disarmed guard failed the intended invariant,
and both restored cycles passed. Direct `/proc` and process-group checks found
every recorded owner, backend, watcher, and peer absent. Positive cleanup used
no fallback signals. The restoration hash matched production source, and the
built entry hash stayed unchanged. Independent receipts and `owner-audit.json`
are in `/tmp/gre448-parent-death-owner/proof/run-o8j_4qrm`. Crate boundaries,
the unchanged AST ratchet, Python compilation, observer syntax, comment review,
and diff checks also passed.

Review identified that termination could bypass restoration of the original
source-mutating control. The control now mutates only a private Rust workspace
copy and compiles it into a separate Cargo target. The owner reran the full
proof in `/tmp/gre448-parent-death-owner/isolated-proof/run-s92c0dby`: four
positive cycles passed, the isolated negative failed the intended invariant,
and all 19 recorded PIDs were absent. Tracked `lib.rs` stayed unchanged.
The cancellation harness `/tmp/gre448-parent-death-owner/prove-interruption.py`
then ran two real baseline cycles per signal and blocked Cargo immediately
after the private fault was written. SIGTERM and SIGKILL each bypassed
`finally` and left the private copy mutated, while tracked source remained
byte-for-byte unchanged. Receipts are in
`/tmp/gre448-parent-death-owner/interruption/receipts.json`.

## Full built backend interrupted upload recovery

`rust/tests/built_backend_upload.py` runs the full built desktop entry with native
Node 24.21.0, real SQLite, and signed Unix HTTP. It reuses the lifecycle proof's
inherited lock, owner identity, setup protocol, health check, observer, and process
guard. The proof creates its integration, fleet printer, and plan through public
HTTP. The public multipart queue route stages a 64 MiB G-code file. SQLite is
opened read-only for independent persistence and integrity assertions. No fixture
state is seeded into the database, and no production module is imported by the
Python proof.

A local Moonraker HTTP peer reports an idle printer. The real queue drain creates
a running `printer-upload` job and sends a multipart POST to
`/server/files/upload` with `start=true` requested by the queued item. The peer
sets `SO_RCVBUF` to 4096, reads exactly 65,536 bytes, and then stops reading and
withholds its response. The received prefix must include the G-code filename and
fixture content. The measured receive buffer must be at most 65,536 bytes, while
the received `Content-Length` must exceed 67,108,864 bytes. These checks establish
an unfinished body transfer, rather than a completed upload waiting for a reply.

Two cases reuse each case's own data directory across a restart:

- SIGKILL terminates the backend process group while SQLite still records
  `sending`. The abrupt crash can leave the Unix socket pathname. The fixture
  removes that temporary runtime directory after proving the old process and
  group are absent. It does not claim graceful crash cleanup.
- Closing stdin reaches the production EOF shutdown path. Observer events must
  record stdin EOF before the application's own group SIGKILL. Production
  shutdown records the unknown-outcome error and removes its Unix socket.

Before any queue HTTP read after restart, the SQLite row must contain the exact
unknown-outcome error from `interruptSendingItem`. This directly checks startup
reconciliation. A real queue GET and missing-job GET then exercise HTTP recovery.
Four real HTTP drains must return empty results. The peer must still have one
upload attempt, the queue and upload job IDs must match their original values,
and the restarted job list must stay empty. The staged file must retain its
original length and SHA-256. The proof never requests an explicit retry.

Run after `npm --prefix web ci` and `npm --prefix web run build`:

```sh
python3 rust/tests/built_backend_upload.py --node /absolute/path/to/native/node --negative-control --evidence /absolute/path/to/evidence
```

The optional negative control copies the complete web tree, including built
artifacts and dependencies, under its evidence directory. It rejects copied
symlinks that escape that private tree. It changes only the private built
`selectPrinterSendQueueDrainItems` predicate to admit `error` rows as well as
`queued` rows. The ordinary recovery error remains intact. The next drain must
produce a second real multipart POST and a different job ID, then fail
`interrupted upload was resent automatically`. A state or message assertion
cannot satisfy this control. Two further pristine cases must pass afterward.
Shared production source, built files, dependency files, and symlink targets are
measured before and after the run. The control never edits the shared tree.

Each run creates a fresh `run-*` evidence directory. It retains artifact identity,
per-file hashes, signed HTTP response logs, SQLite snapshots, network attempt
records, child stdout and stderr, stop events, and cleanup receipts. Local runs
also retain the staged G-code and private faulty bundle. Use a filesystem with
space for the copied web tree and five staged files. Desktop runtime CI runs the
full negative-control command and excludes the copied tree and G-code bodies
from its uploaded evidence.

Process guards kill and reap remaining backend groups on failures. Peer cleanup
stops admission, releases held uploads, closes accepted sockets, and joins its
listener and request threads. Socket and HTTP waits have deadlines. SIGTERM and
KeyboardInterrupt unwind these guards. Successful cases require no fallback
process kill. The intended negative assertion uses the fallback guard to remove
its deliberately active second upload.

This is unsigned Linux headless evidence for the full built backend's persisted
no-resend policy after interrupted transfers. It does not prove a physical
printer's final outcome, an explicit retry, the Rust supervisor's upload path,
packaged launch, platform signing, macOS logout, or release manifest verification.

The owner independently ran the full command on 2026-10-15. Evidence is in
`/persist/home/poitee/.local/state/gre448-upload-owner/run-7gv8lip2`.
All four positive cases passed. The private negative control recorded exactly
two uploads and a different job ID. Its output included:

```text
EXPECTED FAILURE: interrupted upload was resent automatically; actual network uploads=2
PASS: built backend interrupted uploads retain unknown outcome without automatic resend
```

All ten backend PIDs and groups were absent afterward. All five peer cleanup
receipts passed. All 39,276 measured source, artifact, dependency, and link entries
remained unchanged. The final run emitted no transport traceback.

## Legal summary representation inventory

`drain::ticket_t_3_drain_legal_summary_representation_parity` extends the CORS
parity gate for GET and HEAD on `/legal/summary` and `/api/v1/legal/summary`.
Its 768 cases cross both aliases, both methods, allowed or absent Origin,
200 or 429, twelve Accept-Encoding values and four Accept values. The encoding
inventory includes absent and empty headers, identity, identity excluded,
Brotli, gzip, both configured encodings, weighted preferences, wildcard,
unsupported deflate, and zero-quality exclusions. Accept includes absent,
text/plain, application/json, and an excluded wildcard.

The current 868-byte bundled summary and the rate-limit JSON are below Node's
1024-byte compression threshold. Node returns the original representation for
these requests, including those excluding every encoding or media type. Accept
does not select another media type or produce 406. Both implementations must
omit Content-Encoding and Vary: Accept-Encoding. Vary contains only Origin.
This records existing compatibility behavior, including its permissive handling
of exclusions. It does not establish general HTTP negotiation compliance.

Every GET must match the complete expected bytes. HEAD must have an empty body
and the full corresponding representation length. Successful responses retain
text/plain; charset=utf-8. A 429 retains application/json; charset=utf-8 and the
exact statusCode, error and message fields in their serialized order. Its retry
message and Content-Length are checked against that response's own reset value,
using the wording appropriate to its observed timer. Retry-After must equal
X-RateLimit-Reset within 1 through 60 seconds. Limit and remaining headers,
no-store, and CORS metadata are also checked. Time-dependent headers and body
lengths are validated locally before stable metadata is compared across runs.

The gate captures Node fixtures, refuses a receipt with one failed fixture,
checks an isolated Rust candidate, and commits the passing receipt only after
comparison. Two restarts repeat the matrix with Node stopped and verify that
the ownership record stays unchanged. Existing fleet CI selects this test through
`test(ticket_t_3_drain)` with `desktop_rebuild` enabled.

These fixtures prove identity representations for the current small documents.
They do not prove Brotli or gzip output above the threshold, static asset
negotiation, or a common desktop HTTP compression policy. A larger summary or a
changed compression policy must pass this gate before cutover. Common HTTP
policy and the GRE-343 asset negotiation work remain follow-ups. Default-off
cutover flags remain unchanged. GRE-448 stays In Progress; remaining operation
and worker acceptance and actual Mac logout remain open.

Author: Codex gpt-6.1-sol through the Codex harness on fleet VM 133. The requested
Claude Opus 5.5 design review through claudeAgent returned exactly
`Claude API rate limit reached. Try again later.` The authorized gpt-6-astra
fallback reviewed the design. Exact-head scoring and CI evidence belong to the
PR's merge gate.

Local verification on 2026-10-15 built this worktree's server and desktop frontend,
generated its measured bundle manifest with native Node v24.21.0, then ran:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --test runtime_m0 --features desktop_rebuild ticket_t_3_drain_legal_summary -- --nocapture
```

Result: 3 passed, 0 failed. The representation test reported 768 passed and zero
failed for Node, the isolated Rust candidate, restart 1, and restart 2. Both
restarts ran with Node stopped and left authority unchanged. The alias and
flag-off tests also passed. Local logs are under
`/persist/home/.local/state/pstack-t3/gre448-negotiation/` on fleet VM 133.

The companion `rate_limit::tests::ticket_t_3_drain_rate_limit_representation_boundaries`
uses a paused Tokio clock to force reset values 60, 59, and 1. It checks exact
429 GET bytes, HEAD emptiness, representation lengths, retry/reset headers and
quota metadata at each boundary. This deterministically covers Rust's
`1 minute`, `59 seconds`, and `1 second` serialization. The live Node/Rust matrix
validates observed timers; it does not force Node's clock to these boundaries.

Both rate-limit unit tests passed locally after this addition. A temporary
`1 seconds` mutation failed the new test's representation-length assertion
with actual 98 and expected 97 bytes. Restoring the source returned both tests
to green. The mutation was not committed. Logs are `boundary-negative.log` and
`boundary-restored.log` in the evidence directory above.

## Shared coordinator drain deadline

`drain::tests::ticket_t_3_drain_coordinator_shares_one_deadline` uses a paused
Tokio clock, a real `ShutdownGate`, and retained tracked work. The gateway
callback starts its pending-operation timeout only when polled. The test polls
the coordinator to establish its timers, requires Pending one millisecond before
`MUTATION_DRAIN`, and requires `(false, false)` within that bound plus 10ms.
Elapsed simulated time must fall within the same interval. The tracked work
stays alive through every timing and outcome assertion. The existing two drain
unit tests keep their admission-order and restart-disarming scope.

Local `pp-core --lib` verification passed the focused test and all three drain
unit tests with default features and with `desktop_rebuild`. Private copies
replaced `join!` with sequential waits in each order. Both controls failed once
at `coordinator drains exceeded one shared deadline` with exit 101. Restoring
the private source passed once. Exact commands, raw logs, and source hashes are
in `/tmp/gre448-shared-deadline-evidence/`. The shared production bytes stayed
unchanged throughout both controls.

This is coordinator simulation evidence. It does not prove a live Gateway or
`CoreRuntime` shutdown, actual Mac logout, or full GRE-448 completion.

## Live Gateway and CoreRuntime deadline

`drain::ticket_t_3_drain_runtime_shares_one_deadline` starts a real `CoreRuntime`
through the `runtime_m0` drain fixture. It holds tracked shutdown work and an
admitted `POST /plans` that has received `100 Continue` but not its body.
`CoreRuntime::shutdown` must report `tracked_drain_timed_out` and
`gateway_drain_timed_out`, retain storage, and finish in at least one
`MUTATION_DRAIN` and less than that budget plus four seconds.

On 2026-10-22 the committed test passed on the restored shared join. The receipt
had both timeout errors, graceful compat cleanup, and `elapsed_ms` 5130. A private
sequential wait in `stop_resources` failed at 10.103s with exit 101. That duration
also exceeds the committed four-second cleanup allowance. Production
`reject_and_wait` was restored and is not in the diff. Logs are under
`/persist/scratch/GRE-448/` on fleet VM 133.

Atomic route and worker cutover, and actual Mac logout, stay outside this slice.
GRE-448 stays open.

## Worker operations switch with their route aliases

Before this slice, `cutover::plan` refused any operation with a non-HTTP
participant (`node_cannot_fence`). Node never read the cutover record, so a
Rust-owned worker would still start in Node and both runtimes would write.

`web/apps/server/src/data/node-fenced-participants.json` lists the participants
Node can fence. pp-gateway embeds the same file. An operation can switch only
when every participant is an HTTP route the gateway fences or is on that list.
At startup Node reads `operation-cutover.json` from its data directory through a
non-blocking, no-follow, 256 KiB bounded read. If Rust owns
`worker.startManagedProfileSync`, Node starts neither that worker nor the
`/ws/profile-sync` WebSocket route. A record that assigns Node a participant it
cannot fence, or a malformed, oversized, wrong-schema or symlinked record, stops
Node startup before the worker runs.

The bundled ownership manifest now groups `profile.sync` as
`http.GET /ws/profile-sync`, its HEAD twin and `worker.startManagedProfileSync`.
Before, the worker was its own operation, so allowing workers to switch would
have let the worker move without its route. The unresolved resource partition
drops from 387 to 386 entries because two entries became one.

Tests:

- `ticket_t_3_drain_cutover_switches_fenced_worker_with_aliases` commits all
  three participants, recovers them as Rust-owned, and refuses a planted
  record that leaves out the worker (`participant_set_mismatch`).
- `ticket_t_3_drain_cutover_gateway_routes_worker_operation` starts the gateway
  with that record. GET and HEAD `/ws/profile-sync` return
  `rust_owner_unavailable`, Node sees no forwarded request, and an unrelated
  route still forwards.
- `ticket_t_3_drain_cutover_refuses_participant_node_cannot_fence` keeps
  `source.sync` refused for `assistant.propose_import_source_files`, both at
  plan time and at recovery.
- `cutover::tests::ticket_t_3_drain_node_fence_list_names_bundled_workers`
  pins the shared list to the one worker Node fences.
- `src/ownership/operation-cutover.test.ts` builds the real Fastify app. Without
  a record, the worker starts and the route exists. With the `profile.sync`
  record, neither does, and unrelated routes still answer. Records for
  unrelated HTTP operations leave the worker running. The startup refusals
  above each stop `buildApp` before the worker starts.

Rust has no profile-sync handler, so after this cutover nothing writes profile
sync state. That is the intended no-dual-write, no-failover result: the route
returns `rust_owner_unavailable` and nothing falls back to Node. Fences for
other workers, jobs, startup hooks and assistant dispatch, and a live
`CoreRuntime` cutover of `profile.sync`, are outside this slice (GRE-593 owns
the complete inventory). Actual Mac logout stays a parent gate. GRE-448 stays
open.

## Source Sync switches with its jobs, watcher, hooks and assistant actions

`source.sync` groups 30 HTTP routes and aliases (GET routes bring their HEAD
twins) with nine non-HTTP participants:
the four source jobs (`job.sync`, `job.import-scan`, `job.extract-source-docs`,
`job.check-source-updates`), the startup and scheduled source watcher, the legacy
manifest migration, MCP `start_sync` and the assistant action
`propose_import_source_files`. Until this slice none of the nine was on the Node
fence list, so `cutover::plan` refused the operation with `node_cannot_fence`.

Every one of those participants already passes through
`requireSourceSyncParticipant` before it reads or publishes. Node now reads the
cutover record before Source Sync admission. When the record gives Rust all nine
non-HTTP participants, admission writes no Node receipt and every later
`requireSourceSyncParticipant` call fails with `OwnershipAdmissionError`
`rust_owner` (HTTP 503). Node skips the source watcher and the legacy migration
at startup, after a backup restore, and in the MCP stdio server. A record that
gives Rust only some of the nine stops Node startup. Profile sync and unrelated
routes keep running in Node. The fence list now holds exactly the non-HTTP
participants of `profile.sync` and `source.sync`, and a pp-gateway unit test
holds it to that.

Tests:

- `ticket_t_3_drain_cutover_switches_source_sync_as_one_operation` commits all
  39 participants, recovers them as Rust-owned, and refuses each of nine planted
  records that leaves one non-HTTP participant with Node.
- `ticket_t_3_drain_node_fence_list_names_bundled_operations` holds the list to
  the two operations' non-HTTP participants.
- `ticket_t_3_drain_cutover_refuses_participant_node_cannot_fence` now uses
  `job.printer-upload`, which Node still cannot fence.
- Node `src/ownership/operation-cutover.test.ts`: with a `source.sync` record
  the watcher and migration never start, no admission receipt is written,
  `POST /jobs/sync` and `/api/v1/jobs/check-source-updates` return 503, and all
  nine non-HTTP participants refuse with `rust_owner`. A partial record stops
  startup.

Still open: Rust handlers for Source Sync (Rust-owned requests return
`rust_owner_unavailable`), fences for the export and printer-upload jobs, the
other assistant actions and startup hooks (they are not grouped into operations
yet; GRE-593 owns that inventory), a live `CoreRuntime` cutover, and Mac logout.

## Export jobs switch with their routes

Each of the five export jobs is now its own operation. The job switches with
its `/jobs/export-*` route and the `/api/v1` alias:

| Operation | Participants |
|---|---|
| `export.stl-pack` | `job.export-stl-pack`, `POST /jobs/export-stl-pack` and alias |
| `export.checklist-html` | `job.export-checklist-html`, `POST /jobs/export-checklist-html` and alias |
| `export.kit-bundle` | `job.export-kit-bundle`, `POST /jobs/export-kit-bundle` and alias |
| `export.direct-3mf` | `job.export-direct-3mf`, `POST /jobs/export-direct-3mf` and alias |
| `export.accepted-plate-3mf` | `job.export-accepted-plate-3mf`, `POST /jobs/export-accepted-plate-3mf`, `POST /slicer-instances/:id/open-accepted-plates`, each with its alias |

The slicer handoff route belongs to `export.accepted-plate-3mf` because it
records a finished job of that kind (`recordAcceptedPlateExport`). Moving the
job without that route would leave two writers of the same export record.

The five jobs are on the shared Node fence list. `InProcessJobRunner.start`
reads the cutover record from the runner's own data directory the first time
it is called. It refuses any `job.<kind>` that Rust owns with
`OwnershipAdmissionError` `rust_owner` (503). The runner does not depend on an
entry point admitting the record first, which answers the #1236 review note
about module-level Source Sync state for jobs: with a `source.sync` record,
`start("sync")` is refused even when nothing called `beginSourceSyncAdmission`.
The watcher, migration, MCP and assistant entry points still rely on that call
(polish list GRE-974).

The unresolved resource partition drops from 381 to 375 entries because eleven
route and job entries became five operations.

Tests:

- `ticket_t_3_drain_cutover_switches_export_jobs_with_their_routes` checks
  each operation's exact participant set, commits it, recovers every
  participant as Rust-owned, and refuses a planted record that leaves the job
  with Node (`participant_set_mismatch`).
- `ticket_t_3_drain_cutover_gateway_routes_export_operation` starts the gateway
  with `export.stl-pack` and `export.accepted-plate-3mf` switched. Both export
  routes, the slicer handoff route and their aliases return
  `rust_owner_unavailable`, and Node sees no forwarded request.
- `ticket_t_3_drain_node_fence_list_names_bundled_operations` holds the list to
  the non-HTTP participants of the seven fenced operations (15 entries).
- Node `src/ownership/operation-cutover.test.ts`: with an
  `export.checklist-html` record both routes return 503 `rust_owner` and no job
  is listed, while `export-kit-bundle` still starts. With a `source.sync`
  record, a fresh runner refuses `sync` without any admission call.

Still open after this slice: see "Printer send queue switches with every
writer" below.

## Printer send queue switches with every writer

`job.printer-upload` and every Node writer of the `printer.send_queue` setting
are now one operation, `printer.send-queue`:

| Participant | Writes |
|---|---|
| `GET /printer-send-queue` and alias (HEAD twins inherit) | interrupts `sending` rows whose job Node no longer has |
| `POST /printer-send-queue` and alias | enqueues a row |
| `POST /printer-send-queue/:id/dispatch`, `POST /printer-send-queue/drain`, each with its alias | claims rows and starts upload jobs |
| `DELETE /printer-send-queue/:id` and alias | cancels a row |
| `POST /jobs/printer-upload` and alias | starts an upload job |
| `job.printer-upload` | marks the row done or failed |
| `reconcile.reconcilePrinterSendsAfterProcessStart` | interrupts `sending` rows when the routes register |
| `reconcile.reconcileMissingPrinterUploadJob` | interrupts a `sending` row from `GET /jobs/:id` when Node does not know the job |
| `shutdown.interruptPrinterUploadsOnShutdown` | interrupts `sending` rows when Node stops |
| `migration.migrateLegacySelfHostExports` | moves legacy exports under `tenant-default` and rewrites queue artifact paths |

`shutdown` and `reconcile` are new participant kinds. They name a write that
Node makes while it stops, or inside a route that stays with Node
(`GET /jobs/:id` serves every job kind). The ownership census reads them from
the literal ids at the Node checks in `routes/jobs.ts`,
`routes/printer-send-queue.ts` and `services/printer-send-queue.ts`, and
requires each id to name a call in that file.

Decision on the legacy export migration: the whole migration moves with the
queue. Moving the files without rewriting the queue paths would leave rows
pointing at files that are gone, and rewriting rows Rust owns would split the
operation. Node has run the migration on every start before cutover, and
pp-gateway commits a cutover only after Node drained, so a data directory with
a record has already been migrated.

`GET /printer-send-queue/suggestions` only reads the queue and stays with Node.

Node fences, all read from the cutover record in the data directory:

- `InProcessJobRunner.start("printer-upload")` refuses with `rust_owner` (503),
  as for the export jobs. The new `InProcessJobRunner.rustOwns(participant)`
  holds the one cached read of the record.
- The route registration skips the startup reconcile, and `GET /jobs/:id`
  skips the missing-job reconcile, through `rustOwns`.
- `interruptPrinterUploadsOnShutdown(dataDir, ports, tenantId?)` still halts
  local uploads but leaves the rows alone. `index.ts` passes
  `config.dataDir`; `desktop.ts` passes the desktop setup's `data_dir`.
- `app.ts` skips `migrateLegacySelfHostExports`.

The unresolved resource partition drops from 375 to 368 entries: six route
operations, the job and the migration became one operation.

Tests:

- `ticket_t_3_drain_cutover_switches_printer_send_queue_with_every_writer`
  checks the exact 19-participant set, commits it, recovers every participant
  as Rust-owned while suggestions and `GET /jobs/:id` stay with Node, and
  refuses a planted record without the shutdown hook
  (`participant_set_mismatch`).
- `ticket_t_3_drain_cutover_gateway_routes_printer_send_queue_operation`
  starts the gateway with the operation switched. Every queue route, the
  upload route and their aliases return `rust_owner_unavailable` and Node sees
  no forwarded request; suggestions still reach Node.
- `ticket_t_3_drain_node_fence_list_names_bundled_operations` now holds the
  list to 20 entries.
- `ticket_t_7_ownership_manifest_claims_every_source_participant` counts the
  three `shutdown`/`reconcile` participants from Node source.
- Node `src/ownership/operation-cutover.test.ts`: with no record, a legacy
  upload is moved and its stale `sending` row becomes `error` at startup. With
  a `printer.send-queue` record the row stays `sending` with its legacy path
  through startup, `GET /jobs/job-gone` and the shutdown hook, and the runner
  refuses `printer-upload`.

Still open:

- Plan Apply (`applyPlanChanges`, `applyCheckoffRemap` in `db/repository.ts`)
  remaps `checkoff_units` in queue rows in the same transaction as the
  checkoff links. It belongs with the Plan Apply and checkoff operations, which
  have not switched, so it is not fenced here.
- Fences for the other assistant actions and startup hooks (GRE-593), a live
  `CoreRuntime` cutover of the other operations (`profile.sync` is below), and
  Mac logout (parent gate).

## Live CoreRuntime cutover of the profile-sync worker

`drain::ticket_t_3_drain_runtime_cutover_fences_profile_sync_worker` (pp-core
`runtime_m0`, `desktop_rebuild`) commits `profile.sync` through a real
`CoreRuntime` and the built Node backend, rather than only through the gateway.
Node's profile-sync worker seeds the stock slicer instances while `buildApp`
starts it, before Node reports ready, so the slicer instance list shows
whether the worker ran.

- First boot (control): the worker seeds the stock instances (three today).
  The test deletes them, then `shutdown_with_cutover("profile.sync", parity)`
  drains cleanly and commits the operation's three participants: GET and HEAD
  `/ws/profile-sync` and `worker.startManagedProfileSync`.
- Two restarts: the slicer instance list stays empty (Node did not start the
  worker), GET and HEAD `/ws/profile-sync` return 503
  `rust_owner_unavailable`, a second cutover is refused as
  `already_rust_owned`, the record bytes are unchanged, and each boot event
  names `profile.sync`.

The parity receipt is synthetic (`fixtures: 1, passed: 1`), as in the other
runtime cutover tests. Rust has no profile-sync handler, so no real parity
fixture exists yet. The receipt checks themselves are in pp-gateway
`ticket_t_3_drain_cutover_parity_refusals`. This test covers the
`profile.sync` operation only. The other operations stay with GRE-593.

Red control: removing the `rustOwned` check before `startManagedProfileSync`
in a private `app.ts` build failed the test at `drain.rs:583`, "Node started
the Rust-owned profile-sync worker". With the check restored, the ticket
command with the feature,
`cargo nextest run --locked --manifest-path rust/Cargo.toml --workspace
--features pp-core/desktop_rebuild --no-tests=fail -E 'test(ticket_t_3_drain)'`, ran 83 tests and all passed.
Of those, 16 are `runtime_m0` `drain::` tests: 13 in `drain.rs`, 2 in its
`compression` module and 1 in `ticket_t_3_drain_repeated_upload.rs`.

Still open: the remaining operation inventory and cutover (GRE-593, blocked by
GRE-401), Plan Apply's queue remap, and actual Mac logout (parent gate).
