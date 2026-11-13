# Global STL naming defaults HTTP

With the default-off `desktop_rebuild` feature compiled, these routes are mounted
at the root and `/api/v1` only when the stored `PP_DESKTOP_REBUILD` value is
exactly `1` or `true`. Otherwise they answer 404 and do not open the catalog.
Sync, ZIP upload, and location stay unregistered; loose-file upload is
`POST /sources/{id}/upload-files` (see [Source uploads](source-uploads.md)). The desktop gateway does not mount
this router.

| Method | Path | Catalog request |
| --- | --- | --- |
| GET | `/settings/stl-naming` | `GetGlobalNaming` |
| PUT | `/settings/stl-naming` | `SaveGlobalNaming` |

`GET` and successful `PUT` responses wrap storage's bare `NamingProfile` as
`{ "profile": <NamingProfile> }` to match Node. Storage remains the authority
for defaults when nothing is stored and for validation on save.

`PUT` takes Node's body shape `{ "profile"?: <NamingProfile> }`. Missing or null
`profile` becomes the default profile. A refused write leaves the previous
global profile and DB bytes unchanged. Source naming overrides keep precedence
over a later global save until the Source returns to `use_defaults`.

| Refusal | Status | Detail |
| --- | --- | --- |
| Non-object body / non-object `profile` | 400 | `Invalid JSON body` |
| Invalid profile (blank label, empty roles, …) | 400 | Exact storage `CatalogFailure::Input` string |

`docs/gre-808-catalog-http.md`, `docs/gre-809-catalog-http.md`, and
`docs/desktop-rebuild.md` are left unchanged.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --test catalog_http
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test catalog_http
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test catalog_http \
  catalog_global_naming_round_trip catalog_global_naming_preserves_source_override
```
