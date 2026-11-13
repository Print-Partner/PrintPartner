# Source create and patch HTTP

With the default-off `desktop_rebuild` feature compiled, `POST /sources` and `PATCH /sources/{id}` are mounted at the root and `/api/v1` only when the stored `PP_DESKTOP_REBUILD` value is exactly `1` or `true`. Otherwise those routes answer 404 and do not open the catalog. Sync and upload stay unregistered. Delete is described in `docs/gre-806-catalog-http.md`. The desktop gateway does not mount this router.

Create reads a typed `CreateSourceRequest` (GRE-1490): a required string `name`, and optional strings `url`, `branch`, `tag`, `source_kind` and `role` and an object `metadata`. A null optional field is the same as leaving it out. Unknown fields, `source_type` among them, are ignored. A field of the wrong type, including a `metadata` array (which Node dropped) or a non-string `name`, answers 400 `Invalid Source: …`. A non-object body answers 400 `Invalid JSON body`. A missing or null `name` reaches storage, which answers 400 `Source name is required`, and a name in use answers 400 `Source already exists: <name>`. None of these writes. The generated client exports it as `createSource`, answering `SourceSummary`, and its schema rejects unknown fields, so the replacement UI sends none. Patch keeps Node's `String()` coercion. Missing and null fields are omitted, except `tag`, where JSON null clears the stored tag. Metadata is kept only when it is a JSON object; a JSON array is omitted. On both routes a non-null `local_path` is refused with 400 `Raw local_path requires native Source selection` and does not write. Source ids use the same canonical integers as catalog reads.

The session cookie or one API key selects the tenant. API keys use the configured tenant, not a request header. Patching another tenant's id returns 404 `Source not found`. The refusal leaves stored rows and files unchanged.

`docs/desktop-rebuild.md` still says create and patch stay unregistered. That file is left unchanged while another open PR owns it.

`fixtures/parity/catalog-source-http/generate.mjs` records Node's route coercion (Rust now refuses its create body and stores Node's result from the same values sent as strings), GitHub location normalization, and category-path normalization. Regenerate it from the repository root with `node fixtures/parity/catalog-source-http/generate.mjs`, and check it with `--check`.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --test catalog_http
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test catalog_http
```
