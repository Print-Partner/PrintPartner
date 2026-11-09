# D19 publication client and writer admission, GRE-612 PR 3

With `desktop_rebuild`, `WriterOwner::publication_with_policy` validates the
deployment's `AuthPolicy` before constructing a `PublicationClient`.
`SingleAccountDefault` resolves sessions to tenant `default`; `AccountTenant`
resolves sessions to the account's tenant. Both retain the account as the receipt
actor. Invalid policy fails before queue admission or writes. The policy-less
`publication()` helper is available only in unit tests. `PublicationCommand`
trims keys with ECMAScript whitespace rules and admits 1 to 160 UTF-16 units.
`normalized_http` keeps the original admitted request separate from a request
normalized by a caller. The writer resolves the authenticated tenant and actor
inside the publication transaction. Request data cannot select either identity.

`PublicationClient::apply` uses the bounded writer queue. Before admission,
cancellation returns `required_units::AdmissionFailure::Cancelled`, a full queue
at its deadline returns `QueueFull`, and closed or poisoned admission returns
`Stopped`. After admission, the caller waits for the result even if cancelled;
a lost writer reply returns `OutcomeUnknown`. The writer uses its dequeue
transaction deadline and the shared bounded SQLite lock admission path. It
commits applied or replayed receipts and rolls back unsuccessful publication.
Its token allocator uses operating-system randomness.

The feature installs and validates the admission table and immutable triggers
at schema 37. Main never uses version 36: the reference's version 36 import
tables arrive with D14's version 38 step. Reopening checks schema text and
orphan admissions. Without the feature, storage remains at schema 35 and refuses
a newer database.

D13 save, GRE-608, can call `AuthenticatedPublicationContext::resolve` and
`publish_in_transaction` on its own transaction, using `PublicationRequests::new`
for admitted and execution requests. That caller owns commit or rollback. It
must not enqueue `PublicationClient::apply` while holding its transaction.
D13 integration remains GRE-608's caller work.

Eight literal fixtures run the actual Node Apply route and workspace service
against fresh SQLite databases. They cover missing, whitespace-only and
UTF-16-sized keys, embedded BOM preservation, identical replay receipts,
session actor attribution, tenant isolation and unexpected-error text. The
fixture supplies authenticated request context; authentication middleware
itself is covered by the auth tests. Rust compares these literals through its
client, queue and transaction executor with deterministic fixture tokens. A
separate owner test proves schema installation, random token generation,
writer dispatch, shutdown and replay after reopen.

GRE-665 item 1 applies because this public client can return errors. Invalid
keys now use Node's `Request is invalid`; unexpected publication errors use
Node's `Plan draft update failed`. The Node error fixture injects a SQLite
trigger failure through the real route. Internal transaction callers retain
the original diagnostic error. Authentication failures retain the auth module's
existing errors. Queue admission has no Node counterpart; direct Rust tests
check the supplied reference behavior, including cancellation after admission.
