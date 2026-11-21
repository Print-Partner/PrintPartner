# GRE-792: Plan create and delete

With the default-off `desktop_rebuild` feature compiled, `POST /plans` and
`DELETE /plans/{id}` mount at the root, `/api/v1`, and `/api/v2` only when the
stored `PP_DESKTOP_REBUILD` value is exactly `1` or `true`. Otherwise those
routes answer 404. List/detail reads remain as in GRE-791. Layer list and touch
land in GRE-793. Kit-manifest ReadKit is GRE-801
(`docs/gre-801-manifest-http.md`). Layer mutate is
`docs/gre-794-builds-http.md`. Job list is `docs/gre-795-jobs-http.md`; read-one
aliases are `docs/gre-796-jobs-http.md`. SaveKit
`PUT` is GRE-802 (`docs/gre-802-manifest-http.md`); plan-manifest-builder
ReadBuilder is GRE-803 (`docs/gre-803-manifest-http.md`).

Admission matches GRE-791 (loopback Host/Origin, session cookie or API key with
the configured tenant).

## Create (`POST /plans`)

Request body is the validated `CreateBuildRequest` DTO (`name`, optional
`base_project_id`). The core trims JavaScript whitespace from `name`
(`BuildName::parse`), then requires 1 to 4096 Unicode scalars.

Success is `200` with a Node-compatible `ProfileSummary` plus a `layers` array
(empty when no base source). Refusals write nothing:

| Condition | Status | Detail |
| --- | --- | --- |
| Duplicate name | 400 | `Profile already exists: {name}` |
| 64 Builds already exist | 409 | `Build limit reached: an installation holds at most 64 Builds` |
| Unknown base source | 400 | `Project not found` |
| Invalid / empty name body | 400 | `Profile name is required` / `Profile name is too long` / `Request is invalid` |
| Unauthenticated | 401 | `Authentication required` |

The duplicate-name check runs first, so a retried create at the limit still
answers `Profile already exists`. The limit is `MAX_BUILDS` in
`pp-storage/src/build_graph.rs`. The list reads at most that many Builds, so a
create past it would make `GET /plans` fail.

### Client contract (`createBuild`, GRE-1876)

`web/packages/rust-client` exports the `/api/v2` route as `createBuild`:
`CreateBuildRequest` in, `ProfileSummary` out. The client drops `layers`.
`buildNameSchema` checks the same bound as the core. The Rust `BuildName`
schema carries `x-trim-js`, so the generator counts `value.trim()`.
`String.prototype.trim` strips the same characters as `trim_js`. The
`CreateBuildRequest` fixtures in `pp-api/src/client_contract.rs` hold both sides
to that, character by character.

A repeat or retry never makes a second Build. Names are unique per
installation (`uq_profiles_tenant_name`), and create checks the name in its
write transaction. A repeat of the same trimmed name answers `400 Profile
already exists: {name}`. Names compare exactly, so `rack` and `Rack` are two
Builds. When a client cannot tell whether its create landed, it lists Builds
and looks for the name, instead of sending the create again.

## Delete (`DELETE /plans/{id}`)

Success is `204` with an empty body. Missing or cross-tenant ids answer `404`
`Profile not found` and leave rows and files unchanged. Unauthenticated and
admission refusals likewise leave Plan state unchanged.

```sh
umask 022
export CARGO_TARGET_DIR=/persist/targets/cursor/gre-792-create-delete-builds
export CARGO_BUILD_JOBS=2

cargo check -p pp-api --locked --manifest-path rust/Cargo.toml --no-default-features
cargo check -p pp-api --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --tests
cargo test -p pp-api --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --test builds_http \
  build_create_matches_node_contract build_delete_refusal_preserves_plan
```
