# Checkoff transaction-start errors

With `desktop_rebuild`, Checkoff progress writes classify transaction-start
failures by the SQLite error code. `SQLITE_BUSY`, including admission deadline
expiry, returns 503. Every other transaction-start failure returns 500.

Unit completion and assembly return `Accepted Plan update is unavailable` for
503 and `Internal Server Error` for 500 in the JSON `detail` field.

Printed-count imports return `Plan draft update is unavailable` with code
`transaction_unavailable` for 503. For 500, they return
`Accepted Progress import failed` with code `internal_error`.

Run the response regression tests with:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --lib checkoff_progress::tests
```
