# Test file renames

Ticket-named Rust test files, renamed to the behaviour they prove (GRE-1323, plan W1.3).
Each file keeps its ticket id in its first doc comment. Test function names do not change,
so `-E 'test(...)'` filters still match. The file stem is the test binary name for top-level
tests, so `--test <name>` and `binary(...)` filters must use the new name. A file that a test
binary includes as a module gets the new module name, so the full test path gains the new
prefix. Linear acceptance items quote old commands; they are not edited.

| Old file | New file | Ticket |
| --- | --- | --- |
| `rust/crates/pp-storage/tests/gre_636_advisory_lock.rs` | `rust/crates/pp-storage/tests/advisory_lock.rs` | GRE-636 |
| `rust/crates/pp-storage/tests/ticket_t_12_directory.rs` | `rust/crates/pp-storage/tests/directory_permissions.rs` | T-12 |
| `rust/crates/pp-storage/tests/ticket_t_28_streams.rs` | `rust/crates/pp-storage/tests/job_status_streams.rs` | GRE-410 (T-28-streams) |
| `rust/crates/pp-storage/tests/ticket_t_61_checkoff.rs` | `rust/crates/pp-storage/tests/checkoff_verification.rs` | T-61-checkoff |
| `rust/crates/pp-storage/tests/ticket_t_61_plates.rs` | `rust/crates/pp-storage/tests/plate_revisions.rs` | GRE-1189, GRE-1184 (T-61-plates) |
| `rust/crates/pp-storage/tests/ticket_t_61_read_model.rs` | `rust/crates/pp-storage/tests/read_model_batches.rs` | T-61 |
| `rust/crates/pp-storage/tests/ticket_t_61_required_units.rs` | `rust/crates/pp-storage/tests/required_unit_histories.rs` | T-61 (required units) |
| `rust/crates/pp-storage/tests/ticket_t_7_writer.rs` | `rust/crates/pp-storage/tests/writer_owner.rs` | T-7 |
| `rust/crates/pp-storage/tests/ticket_t_8_archive.rs` | `rust/crates/pp-storage/tests/archive_backups.rs` | GRE-403 (T-8-archive) |
| `rust/crates/pp-storage/tests/ticket_t_8_archive_retention.rs` | `rust/crates/pp-storage/tests/archive_retention.rs` | GRE-403 (T-8-archive) |
| `rust/crates/pp-storage/src/working_drafts/tests/ticket_t_10_draftreads.rs` | `rust/crates/pp-storage/src/working_drafts/tests/draft_reads.rs` | GRE-1052 (T-10-draftreads) |
