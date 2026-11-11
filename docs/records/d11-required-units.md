# D11 required units (GRE-606)

`required_units` compiles only with `desktop_rebuild`. It has a model, readers,
and one write operation.

The model provides planning and selection digests, canonical decisions, exact
predecessor matching, required-unit assignments, conflicts, surplus tokens, and
selection basis rows. It keeps `DraftPart` manifest metadata separate from scalar
JSON. Planning hashes preserve UTF-16 ordering and lone surrogates in manifest
fields. Predecessor units are sorted by prior index before validation, as in Node.
Target, predecessor, token, and decision errors use Node's messages and order.

The readers verify stored data on every read, as Node's repository does:

- `storage::draft` loads a Plan draft and checks its snapshot digest. A selected
  reconciliation must exist for the same draft and be finalized, or the read
  fails with "Plan draft Required-unit selection is corrupt".
- `storage::saved` checks a finalized reconciliation's canonical JSON, digests,
  decision rows, and assignment rows.
- `storage::base` reads the accepted revision's Required-unit set and checks its
  coordinates, Object names, progress, orphan units, and mapping digest.
- `view::workspace` builds the draft workspace and its diff against the base
  revision. Diff rows sort by their serialized ID, as in Node.

Corrupt rows return Node's messages for the same checks, with two known gaps.
Selection basis rows share one message, "Required-unit selection basis is
invalid". Some malformed stored result JSON returns a different message than
Node, although both refuse the row. GRE-648 tracks both gaps.

The operation ports Node's `savePlanDraftRequiredUnitReconciliation` and the
`reconcile` path of `PlanDraftWorkspaceService`. Get a client with
`WriterOwner::required_units_with_policy`. `ReconciliationClient::reconcile`
queues one command on the single writer, as `build_graph` does. The writer runs
it in one immediate transaction. It checks cancellation before the transaction
starts and again before it commits, so a cancelled request writes nothing.
The expansion and insert loops also check cancellation once per assignment and
once per inserted decision and assignment row, so a cancelled selection stops
inserting and rolls back promptly. There is no timing target. Unexpected
transaction failures surface Node's
`Plan draft update failed` on the client, with the source retained through
`.context`, matching the publication client and Node's reconcile route.
Authentication failures and admission outcomes (`Cancelled`, `QueueFull`,
`Stopped`, `OutcomeUnknown`) stay typed and unwrapped.

Selection refuses a draft before it expands assignments when the sum of part
quantities is above 230. The error is "Reconciliation row limit exceeded".
230 is ten times the largest assignment list in the checked-in fixtures:
`edit-selected` in `working-draft-writes-node.jsonl` stores 23 assignments.
The constant is `storage::ASSIGNMENT_CAP`. Chad checks it against plans 14–23
before this feature is turned on. Raise that one constant if a real plan would
be refused. Node still expands a draft of this size. A quantity above 10,000
on one Part is still "Required-unit target quantity is invalid", which is
checked before the cap.

- `ReconciliationRequest` has the contract's snake-case fields. It rejects
  unknown fields, a snapshot digest that is not 64 lowercase hex characters,
  more than 10,000 decisions, and two decisions for the same draft Part.
- `ReconciliationCommand::new` trims the idempotency key as JavaScript `trim`
  does and requires 1 to 160 UTF-16 units, as the Node route does.
- A session records the user ID as the actor. An API key records `tenant:<id>`
  and updates the key's last use time.
- `Outcome` serializes with Node's `kind` values and workspace shape.
- A retry with the same actor, key, and payload returns the saved selection,
  including rows Node wrote. If the draft is still at the expected v1 digest
  with no selection, the retry selects the saved reconciliation again. A retry
  with a different payload returns `idempotency_conflict`.

GRE-416 mounts the operation as `PUT /plans/{id}/drafts/{draft_id}/reconciliation`
on the draft router (see [D24 drafts HTTP](d19-publication-client.md#d24-drafts-http-gre-619)). The local
`Decision` and request types exist because `pp-storage` has no dependency on
`pp-contracts`.

Tests open Node-written databases in
`rust/crates/pp-storage/tests/fixtures/required-units/`. Each operation test
compares the outcome with the Node result in the matching JSON file, or with
literal digests from Node's service, and checks the stored rows. The
`required-unit-fixture` binary runs one request twice, with a restart between
runs, and prints both outcomes and the database before and after:

```sh
PP_RECONCILIATION_SESSION=required-unit-fixture-secret \
  cargo run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild,test-unmarked-writer \
  --bin required-unit-fixture -- DIRECTORY PROFILE_ID DRAFT_ID IDEMPOTENCY_KEY REQUEST_JSON
```

Run the tests with:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib required_units -- --test-threads=2
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test required_unit_operations -- --test-threads=2
```

The T-61 acceptance target (GRE-416) replays the 24 seeded Node histories in
`fixtures/parity/required-unit-histories/node.json` (inclusion and quantity
edits, valid and invalid decisions in shuffled order, stale digests, key replays,
changed payloads and a restart) through the public clients, checks every refused
request left the draft, decision and assignment rows unchanged, checks surviving
units keep their predecessor's token, and checks a second tenant cannot read,
replay or change the first tenant's selection. Active production and running
operations refuse publication with every membership row unchanged in the
publication parity cases `production-active` and `running-export`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
  --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -E 'test(ticket_t_61_required_units) | test(reconciliation_)'
```

