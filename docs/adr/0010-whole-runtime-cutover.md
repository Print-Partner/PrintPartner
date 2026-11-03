# Cut over the whole runtime with explicit rollback limits

Status: accepted on 2026-11-02 by Chad in a later answer than
[screen replacement](0008-replace-the-old-screens.md). Production execution still
requires separate owner approval.

Choose a whole-runtime switch over the existing tested per-route strangler.
The replacement UI uses only Rust, so partial live route ownership has no target
consumer. This removes mixed-runtime coordination at the cost of losing per-route
rollback. Node and Rust never share a live data directory.

Stop/drain Node, verify a consistent pre-cutover database and matching managed
files, prove restoration on a separate copy, then migrate a separate Rust-owned
copy. Rollback restores the pre-cutover copy and loses post-cutover changes.
Prefer fix-forward after new writes; a data-losing restore needs informed owner
approval. Restoring data cannot undo physical printer or integration effects,
and unknown printer outcomes require reconciliation, never automatic resend.

The [architecture](../architecture.md) defines rehearsal and admission gates.
Chad must set the acceptance/rollback window and retention period before
production approval. Unsupported-schema refusal remains mandatory. The
`desktop_rebuild` gates stay default-off until separately approved retirement.
Strangler removal is scheduled scope after replacement evidence, not repair of
broken machinery.
