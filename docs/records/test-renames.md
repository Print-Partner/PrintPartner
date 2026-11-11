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
| `rust/crates/pp-api-types/tests/ticket_t_63_contracts.rs` | `rust/crates/pp-api-types/tests/node_boundary_contracts.rs` | T-63 |
| `rust/crates/pp-api-types/tests/ticket_t_9_semantics.rs` | `rust/crates/pp-api-types/tests/schema_semantics.rs` | T-9 |
| `rust/crates/pp-desktop/tests/ticket_t_29_tray.rs` | `rust/crates/pp-desktop/tests/tray_status_badge.rs` | T-29 |
| `rust/crates/pp-desktop/tests/ticket_t_31_autostart.rs` | `rust/crates/pp-desktop/tests/launch_agent.rs` | GRE-369 (T-31) |
| `rust/crates/pp-net/tests/ticket_t_22_stream.rs` | `rust/crates/pp-net/tests/streamed_replies.rs` | GRE-1044 (T-22) |
| `rust/crates/pp-perf/tests/ticket_t_53.rs` | `rust/crates/pp-perf/tests/perf_record_gate.rs` | GRE-388 (T-53) |
| `rust/crates/pp-perf/tests/ticket_t_53_capture.rs` | `rust/crates/pp-perf/tests/perf_capture.rs` | GRE-388 (T-53) |
| `rust/crates/pp-server/tests/slice0_mount.rs` | `rust/crates/pp-server/tests/server_binary_routes.rs` | none in file |
| `rust/crates/pp-services/tests/ticket_t_44_discovery.rs` | `rust/crates/pp-services/tests/native_discovery.rs` | GRE-456 (T-44) |
| `rust/crates/pp-services/tests/ticket_t_58_kit.rs` | `rust/crates/pp-services/tests/kit_bundle_golden.rs` | GRE-425 (T-58) |
| `rust/crates/pp-services/tests/ticket_t_59_sync.rs` | `rust/crates/pp-services/tests/github_sync_golden.rs` | GRE-429 (T-59) |
| `rust/crates/pp-services/tests/ticket_t_60_profile_sync.rs` | `rust/crates/pp-services/tests/profile_sync_golden.rs` | T-60 |
| `rust/crates/pp-services/tests/ticket_t_62_mcp_domain.rs` | `rust/crates/pp-services/tests/mcp_dispatch_parity.rs` | GRE-441 (T-62) |
| `rust/crates/pp-core/tests/slice0_mount.rs` | `rust/crates/pp-core/tests/gateway_mount.rs` | none in name (slice0) |
| `rust/crates/pp-core/tests/ticket_t_30_single_instance.rs` | `rust/crates/pp-core/tests/single_owner.rs` | T-30 |
| `rust/crates/pp-core/tests/ticket_t_3_drain.rs` | `rust/crates/pp-core/tests/process_group_drain.rs` | T-3 |
| `rust/crates/pp-core/tests/ticket_t_53_capture.rs` | `rust/crates/pp-core/tests/app_launch_capture.rs` | T-53 (GRE-388) |
| `rust/crates/pp-core/tests/runtime_m0/slice0_mount.rs` | `rust/crates/pp-core/tests/runtime_m0/launcher_start.rs` | none in name (slice0) |
| `rust/crates/pp-core/tests/runtime_m0/ticket_t_3_drain_compression.rs` | `rust/crates/pp-core/tests/runtime_m0/response_compression.rs` | T-3 |
| `rust/crates/pp-core/tests/runtime_m0/ticket_t_3_drain_repeated_upload.rs` | `rust/crates/pp-core/tests/runtime_m0/repeated_upload.rs` | T-3 (GRE-448) |
| `rust/crates/pp-core/tests/runtime_m0/ticket_t_4_watcher_proof.rs` | `rust/crates/pp-core/tests/runtime_m0/watcher_proof.rs` | T-4 (GRE-449, GRE-335) |
| `rust/crates/pp-core/tests/runtime_m0/ticket_t_68_desktop_account_mapping.rs` | `rust/crates/pp-core/tests/runtime_m0/desktop_account_mapping.rs` | T-68 |
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
| `rust/crates/pp-api/tests/ticket_t_10_fixtures.rs` | `rust/crates/pp-api/tests/node_fixture_replay.rs` | T-10 (GRE-451 parity fixtures) |
| `rust/crates/pp-api/tests/ticket_t_26_send_queue_http.rs` | `rust/crates/pp-api/tests/send_queue_http.rs` | GRE-365, GRE-1215 (T-26) |
| `rust/crates/pp-api/tests/ticket_t_61_read_model_http.rs` | `rust/crates/pp-api/tests/accepted_plan_checkoff_http.rs` | GRE-415 (T-61) |
| `rust/crates/pp-api/tests/ticket_t_62_admin_http.rs` | `rust/crates/pp-api/tests/admin_http.rs` | GRE-438 (T-62) |
