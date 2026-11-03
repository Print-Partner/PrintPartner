# Retire the rebuild flag together with Node

Status: accepted on 2026-11-02.

Turning `desktop_rebuild` on is a data migration, not a configuration change, so the flag is removed in the same change that removes Node rather than being flipped and left in place.

The reason is in the schema version. `pp-storage/src/schema.rs` sets `CURRENT_VERSION` to 41 when the feature is on and 35 when it is off, and records the version in `app_settings`. A build refuses a database that is newer than the version it can read. So enabling the flag performs an irreversible upgrade from 35 to 41: once a directory has been opened by a flagged build, the unflagged build will not open it.

There are therefore three schema states in play, not the two the migration plan assumed: the Node schema, the Rust schema at 35 with the flag off, and the Rust schema at 41 with the flag on. That has three consequences.

First, the flag flip needs the same protections as cutover: a verified pre-upgrade copy taken before the migration runs, a refusal path in the older build instead of a repair attempt, and a stated rollback window. It is not something a developer turns on locally and forgets.

Second, AGENTS.md currently forbids turning the flag on by default, and that rule stays in force until the flag does not exist. Flipping the default would create a period in which two runtimes both claim the same data directory, which is the dual-writer situation the whole design works to prevent.

Third, the flag cannot be used as the long-lived switch between "old product" and "new product". Shipping the new UI behind a permanently off by default flag means shipping nothing. The sequence is: the flag stays off while slices land and are exercised by tests, the flag is removed and Node is deleted in one change after cutover passes, and there is never a release in which both runtimes are live options for the same directory.

The rejected alternative was to flip the default at Docker cutover and keep the flag for a soak period. That is cheaper to write and much worse to explain: during the soak, the same data directory is upgradeable to 41 and unreadable by the build a user might roll back to, and the only way back is restoring the pre-upgrade copy that the flag path does not require anyone to take.
