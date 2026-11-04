# Spoolman inventory fixtures

Disposable inventory for the filament shortfall tests. A test serves these
files from a loopback Spoolman stand-in that it starts and stops itself. No
test contacts a real Spoolman server.

The shapes follow Spoolman's `GET /api/v1/filament` and `GET /api/v1/spool`
responses, trimmed to the fields Print Partner reads.

| File | Served as | Case |
| --- | --- | --- |
| `filaments.json` | `/api/v1/filament` | Filament 2 (Prusament PLA, Galaxy Black) and filament 3 (Generic PETG, Orange) |
| `spools-sufficient.json` | `/api/v1/spool` | Filament 2 has spools 7 (512.4 g) and 8 (240 g), so 752 g. Archived spool 9 does not count |
| `spools-short.json` | `/api/v1/spool` | Filament 2 has only spool 7 (18.9 g), so 18 g. Archived spool 9 does not count |

The unknown case (Spoolman answers 500 or is not listening) and the Module-off
case need no file.

Used by `rust/crates/pp-core/tests/filament_shortfalls_http.rs`.
