# Import worker policy

`WriterOwner::job_worker_with_policy(policy, admission)` creates a worker that can
advance supplied Source imports. The worker keeps the supplied `AuthPolicy` for
credential revalidation. Creating an import client with
`WriterOwner::imports(policy, quota)` does not configure a worker's policy.

`WriterOwner::job_worker(admission)` creates a worker without an authentication
policy. It can claim an import admitted with the same owner's generated
`Credential::PhysicalOwner`. A successful claim does not establish that the
worker can run an import phase. Even for PhysicalOwner imports,
`ServerWorkerClient::import_phase` requires a policy-configured worker.

For a lease belonging to that worker, a policy-less phase call returns an
`anyhow::Error` containing typed `AuthorityFailure::PolicyRequired`. Callers can
inspect it with `downcast_ref::<AuthorityFailure>()`. The phase policy guard runs
before the phase command reaches the storage writer. It leaves the persisted
import journal, quota, job history, and Source rows unchanged. This refusal is
distinct from a configured worker's later credential revalidation failure, which
can record an authority refusal.

## Claim and Source lease proof

`claim_import(&operation.job_id)` targets the admitted import and returns a
`ClaimedAttempt`. Its `lease` identifies the worker's fenced attempt. Its
`source_work` contains the live `SourceWorkLease` for the import's tenant and
Source. Keep the claim alive and pass both leases from that claim to
`import_phase(&claim.lease, claim.source_work.as_ref().unwrap(), phase)`.

An attempt lease alone does not prove ownership of the Source reservation. The
configured phase path checks that the Source lease still represents a live
reservation for the matching tenant and Source. Dropping or releasing the Source
lease gives up that reservation. Adding a policy does not replace this proof or
transfer a claim to a different worker.

For an admitted import, `Phase::Owned` persists the captured input, moves the
journal to `owned_input_ready`, and adds an `owned` Source observation. The
observation records cursor 1 and the claim's generation and fence. This phase
leaves the quota reserved and does not activate a revision or finish the job.

## Evidence and gates

[`import_worker_policy.rs`](../../rust/crates/pp-storage/tests/import_worker_policy.rs)
uses two disposable databases with the same admission and claim setup. It creates
physical credentials through each `WriterOwner`, without account secrets or
external services. Each random temporary directory is removed after the writer
drops, including during panic unwinding.

The refusal test takes a transactionally consistent snapshot of every SQLite
table after claim and immediately before the phase call. It compares every
column and typed cell after the typed refusal. Whole-database comparison also
covers changes outside a curated journal, quota, and Source table list.

The success test advances the same `Phase::Owned` with a policy-configured worker.
It asserts the exact returned operation and expected whole-database contents.
Only the journal state and document and one fenced Source observation change.
The observation timestamp must fall within the phase call's measured clock
bounds. All other cells, including quota and job state, must match the snapshot.

The integration target has `#![cfg(feature = "desktop_rebuild")]`.
`pp-storage` compiles that feature by default. `--no-default-features` executes
zero tests for this target and does not establish the worker-policy behavior.

```sh
cargo test --manifest-path rust/Cargo.toml --locked -p pp-storage --test import_worker_policy
```

These are library tests. They do not enable product behavior or change feature
settings. User-visible rebuild behavior also requires the stored
`CoreRuntime::desktop_rebuild()` setting derived from `PP_DESKTOP_REBUILD` at
startup. Only the exact environment values `1` and `true` enable that setting
when the core was compiled with `desktop_rebuild`. Storage does not read the
environment flag. See [Desktop rebuild flag](../desktop-rebuild.md).
