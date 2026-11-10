# Test file renames

Ticket-named Rust test files, renamed to the behaviour they prove (GRE-1323, plan W1.3).
Each file keeps its ticket id in its first doc comment, or in its table row when the old
name carried no id. Test function names do not change, so `-E 'test(...)'` filters still
match. The file stem is the test binary name for top-level tests, so `--test <name>` and
`binary(...)` filters must use the new name. A file that a test binary includes as a module
gets the new module name, so the full test path gains the new prefix. Linear acceptance items
quote old commands; they are not edited.

| Old file | New file | Ticket |
| --- | --- | --- |
| `rust/crates/pp-core/tests/slice0_mount.rs` | `rust/crates/pp-core/tests/gateway_mount.rs` | none in name (slice0) |
| `rust/crates/pp-core/tests/ticket_t_30_single_instance.rs` | `rust/crates/pp-core/tests/single_owner.rs` | T-30 |
| `rust/crates/pp-core/tests/ticket_t_3_drain.rs` | `rust/crates/pp-core/tests/process_group_drain.rs` | T-3 |
| `rust/crates/pp-core/tests/ticket_t_53_capture.rs` | `rust/crates/pp-core/tests/app_launch_capture.rs` | T-53 (GRE-388) |
| `rust/crates/pp-core/tests/runtime_m0/slice0_mount.rs` | `rust/crates/pp-core/tests/runtime_m0/launcher_start.rs` | none in name (slice0) |
| `rust/crates/pp-core/tests/runtime_m0/ticket_t_3_drain_compression.rs` | `rust/crates/pp-core/tests/runtime_m0/response_compression.rs` | T-3 |
| `rust/crates/pp-core/tests/runtime_m0/ticket_t_3_drain_repeated_upload.rs` | `rust/crates/pp-core/tests/runtime_m0/repeated_upload.rs` | T-3 (GRE-448) |
| `rust/crates/pp-core/tests/runtime_m0/ticket_t_4_watcher_proof.rs` | `rust/crates/pp-core/tests/runtime_m0/watcher_proof.rs` | T-4 (GRE-449, GRE-335) |
| `rust/crates/pp-core/tests/runtime_m0/ticket_t_68_desktop_account_mapping.rs` | `rust/crates/pp-core/tests/runtime_m0/desktop_account_mapping.rs` | T-68 |
