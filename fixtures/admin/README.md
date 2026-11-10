# Admin route fixtures (T-62-admin, GRE-438)

- `settings-cases.json` lists the seeded `integrations` setting and 48
  requests to the date format, build tracking and default Spoolman routes.
  `settings-oracle.json` records Node's reply and stored rows after each one.
  `web/apps/server/src/routes/settings-routes.oracle.test.ts` checks Node
  against it, and `rust/crates/pp-api/tests/settings_routes_http.rs` replays
  it through Rust. `UPDATE_ADMIN_ORACLE=1` rewrites it as below.

- `custom-filaments-cases.json` lists `custom_filaments.json` contents by case
  name: `text` (written as UTF-8), `base64` (raw bytes), `absent` (no file) or
  `directory` (a directory at the store path).
- `node-oracle.json` is what the Node app answers for every case on
  `GET /filaments/custom` and `GET /api/v1/filaments/custom`, plus `HEAD`, and
  for each `GET`/`HEAD` `/legal/*` route at `/` and `/api/v1`. It records the
  status, `content-type`, `cache-control`, `content-length` and body (the
  SHA-256 for legal documents).

`web/apps/server/src/routes/admin-routes.oracle.test.ts` checks that Node still
produces `node-oracle.json`. Web CI runs it when `web/` or `fixtures/` change.
`rust/crates/pp-api/tests/ticket_t_62_admin_http.rs` replays the same cases
through the Rust routes. To rewrite the oracle after a deliberate Node change:

```sh
(cd web && UPDATE_ADMIN_ORACLE=1 npm run test -w @print-partner/server -- src/routes/admin-routes.oracle.test.ts)
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-storage \
  --features pp-api/desktop_rebuild,pp-storage/desktop_rebuild --no-tests=fail -E 'test(ticket_t_62_admin)'
```
