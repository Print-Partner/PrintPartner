# Test file renames

Ticket-named Rust test files, renamed to the behaviour they prove (GRE-1323, plan W1.3).
Each file keeps its ticket id in its first doc comment. Test function names do not change,
so `-E 'test(...)'` filters still match. The file stem is the test binary name, so
`--test <name>` and `binary(...)` filters must use the new name. Linear acceptance items
quote old commands; they are not edited.

| Old file | New file | Ticket |
| --- | --- | --- |
| `rust/crates/pp-services/tests/ticket_t_44_discovery.rs` | `rust/crates/pp-services/tests/native_discovery.rs` | GRE-456 (T-44) |
| `rust/crates/pp-services/tests/ticket_t_58_kit.rs` | `rust/crates/pp-services/tests/kit_bundle_golden.rs` | GRE-425 (T-58) |
| `rust/crates/pp-services/tests/ticket_t_59_sync.rs` | `rust/crates/pp-services/tests/github_sync_golden.rs` | GRE-429 (T-59) |
| `rust/crates/pp-services/tests/ticket_t_60_profile_sync.rs` | `rust/crates/pp-services/tests/profile_sync_golden.rs` | T-60 |
| `rust/crates/pp-services/tests/ticket_t_62_mcp_domain.rs` | `rust/crates/pp-services/tests/mcp_dispatch_parity.rs` | GRE-441 (T-62) |
| `rust/crates/pp-api/tests/ticket_t_10_fixtures.rs` | `rust/crates/pp-api/tests/node_fixture_replay.rs` | T-10 (GRE-451 parity fixtures) |
| `rust/crates/pp-api/tests/ticket_t_26_send_queue_http.rs` | `rust/crates/pp-api/tests/send_queue_http.rs` | GRE-365, GRE-1215 (T-26) |
| `rust/crates/pp-api/tests/ticket_t_61_read_model_http.rs` | `rust/crates/pp-api/tests/accepted_plan_checkoff_http.rs` | GRE-415 (T-61) |
| `rust/crates/pp-api/tests/ticket_t_62_admin_http.rs` | `rust/crates/pp-api/tests/admin_http.rs` | GRE-438 (T-62) |
