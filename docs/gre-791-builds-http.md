# GRE-791: gated Plan list and detail reads

With the default-off `desktop_rebuild` feature compiled, `GET` and `HEAD`
`/plans` and `/plans/{id}` mount at the root, `/api/v1`, and `/api/v2` only when
the stored `PP_DESKTOP_REBUILD` value is exactly `1` or `true`. Otherwise those
routes answer 404 and do not open the build graph. Create and delete are
documented in `docs/gre-792-builds-http.md`. Layer list and touch are
`docs/gre-793-builds-http.md`. `GET`/`HEAD` kit-manifest ReadKit is GRE-801
(`docs/gre-801-manifest-http.md`); SaveKit `PUT` is GRE-802
(`docs/gre-802-manifest-http.md`); plan-manifest-builder ReadBuilder is GRE-803
(`docs/gre-803-manifest-http.md`). Layer mutate is `docs/gre-794-builds-http.md`.
Job list HTTP is `docs/gre-795-jobs-http.md`; read-one aliases are
`docs/gre-796-jobs-http.md`. The desktop gateway does not mount this router in
this slice.

Admission (loopback Host/Origin, principal, forwarded-header refusal) is shared
with drafts through the crate-private `loopback` module.

List responses are `{ "profiles": [ <ProfileSummary>, ... ] }`. Detail
responses are a bare `ProfileSummary` (`id`, `name`, `order_number`,
`special_request`, `part_count`, `accepted_progress`, `build_stale`,
`freshness`, `archived_at`, `last_used_at`, and under `desktop_rebuild` the
header's `customer` and `due_date` from GRE-1100). Missing or cross-tenant ids answer
404 with `"Profile not found"`. Unauthenticated requests answer 401 with
`"Authentication required"`. HEAD shares status and auth with GET and returns
an empty body.

`freshness.status: "unavailable"` with a `code` (for example `accepted_inputs`)
is an integrity failure while evaluating freshness. It is not rewritten as
`untracked` / `no_accepted_inputs`.

The session cookie or one API key selects the tenant. API keys use the
configured tenant, not a request header.

```sh
umask 022
export CARGO_TARGET_DIR=/persist/targets/cursor-gre-791-build-reads-2106
export CARGO_BUILD_JOBS=2

cargo check -p pp-api --locked --manifest-path rust/Cargo.toml --no-default-features
cargo check -p pp-api --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --tests
cargo test -p pp-api --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --test builds_http \
  build_reads_flag_off_are_404 build_reads_are_authenticated_and_tenant_scoped
```
