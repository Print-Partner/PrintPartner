# Test file renames

Ticket-named Rust test files, renamed to the behaviour they prove (GRE-1323, plan W1.3).
Each file keeps its ticket id in its first doc comment, or in its table row when the old
name carried no id. Test function names do not change, so `-E 'test(...)'` filters still
match. The file stem is the test binary name for top-level tests, so `--test <name>` and
`binary(...)` filters must use the new name. A file that a test binary includes as a module
gets the new module name, so the full test path gains the new prefix. Linear acceptance items
quote old commands; they are not edited.

Rename PRs add rows only to their crate's file. Leave other crate pages unchanged;
update this index only when a crate gets its first rename record. Keep each existing
row verbatim when moving records.

| Crate | Rename records |
| --- | --- |
| `pp-api` | [pp-api](test-renames/pp-api.md) |
| `pp-api-types` | [pp-api-types](test-renames/pp-api-types.md) |
| `pp-core` | [pp-core](test-renames/pp-core.md) |
| `pp-desktop` | [pp-desktop](test-renames/pp-desktop.md) |
| `pp-net` | [pp-net](test-renames/pp-net.md) |
| `pp-perf` | [pp-perf](test-renames/pp-perf.md) |
| `pp-server` | [pp-server](test-renames/pp-server.md) |
| `pp-services` | [pp-services](test-renames/pp-services.md) |
| `pp-storage` | [pp-storage](test-renames/pp-storage.md) |
