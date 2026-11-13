# GRE-796: read one job at both aliases

With the default-off `desktop_rebuild` feature compiled, `GET` and `HEAD`
`/jobs/{id}` and `/api/v1/jobs/{id}` mount only when the stored
`PP_DESKTOP_REBUILD` value is exactly `1` or `true`. Otherwise those routes
answer 404. The bounded list at `/api/v1/jobs` stays as in GRE-795
(`docs/gre-795-jobs-http.md`). Mutation routes stay unregistered. Job
WebSocket admission is GRE-798 (`docs/gre-798-jobs-http.md`). Plan HTTP
remains as in GRE-791–GRE-794.

Admission matches GRE-791 (loopback Host/Origin, session cookie or API key with
the configured tenant) through the shared `loopback` module. Each admitted
request runs `AtomicJobClient::read_public`. Success is `200` with a bare
`JobSnapshot` (status, public `result`, and the other public fields). Missing and
cross-tenant ids answer `404` `"Job not found"`.
Unauthenticated requests answer `401` `"Authentication required"`. HEAD shares
status and auth with GET and returns an empty body. Rows and files stay
unchanged on refusal. Serializer permits for these responses are specified in
GRE-797 (`docs/gre-797-jobs-http.md`).

```sh
umask 022
export CARGO_TARGET_DIR=/persist/targets/cursor-gre-796-job-detail-743c
export CARGO_BUILD_JOBS=2
cargo check -p pp-api --locked --manifest-path rust/Cargo.toml --no-default-features
cargo check -p pp-api --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --tests
cargo test -p pp-api --locked --manifest-path rust/Cargo.toml --test jobs_http job_detail_
cargo test -p pp-api --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --test jobs_http \
  job_detail_
```

Focused counts: feature-off `job_detail_` → 2 passed; feature-on `job_detail_` → 2
passed (`job_detail_aliases_match`, `job_detail_head_has_no_body_and_keeps_status`).
