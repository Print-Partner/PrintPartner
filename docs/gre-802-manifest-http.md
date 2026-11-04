# GRE-802: gated kit-manifest SaveKit HTTP

With the default-off `desktop_rebuild` feature compiled, `PUT`
`/plans/{id}/kit-manifest` mounts at the root, `/api/v1`, and `/api/v2` next to
the GRE-801 ReadKit `GET`/`HEAD` routes, only when the stored
`PP_DESKTOP_REBUILD` value is exactly `1` or `true`. Otherwise those routes
answer 404 and do not open the build graph. Plan-manifest-builder ReadBuilder is
GRE-803 (`docs/gre-803-manifest-http.md`). The desktop gateway does not mount
this router in this slice.

Admission (loopback Host/Origin, principal, forwarded-header refusal) is shared
with Plan list/detail through the Build router (`docs/gre-791-builds-http.md`).
The body limit for kit-manifest PUT is 8 MiB (same as Node bulk JSON).

Successful responses are storage `ManifestOptionsOutcome::Saved` JSON via
`into_success_body()`: `{ "profile_id": <id>, "kit": <KitManifest> }`. Request
bodies use storage `KitManifest::from_request` (`{ "kit": { ... } }`). Missing
or cross-tenant ids answer 404 with `"Profile not found"`. Invalid kit input
answers 400 with storage `ManifestInputDetail` JSON. A changed capture basis
answers 409 with
a [problem response](problem-responses.md) with
`"detail":"Build inputs changed during manifest observation","code":"stale_observation"`
and does not write the kit setting. Source observation failures answer 503 with
`source_observation_failed`. Unauthenticated requests answer 401 with
`"Authentication required"`.

SaveKit reuses storage capture, double-observe, `basis_matches`, and
known-selection validation from GRE-644 / #520; this slice only registers the
HTTP route.

```sh
umask 022
export CARGO_TARGET_DIR=/persist/targets/gre-802-kit-manifest-save-f602
export CARGO_BUILD_JOBS=2

cargo check -p pp-api --locked --manifest-path rust/Cargo.toml --no-default-features
cargo check -p pp-api --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --tests
cargo test -p pp-api --locked --manifest-path rust/Cargo.toml --test manifest_options_http
cargo test -p pp-api --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --test manifest_options_http \
  kit_manifest_save_rejects_changed_basis kit_manifest_save_rejects_unknown_selection
```
