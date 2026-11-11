# GRE-438: admin routes over HTTP

With the default-off `desktop_rebuild` feature compiled, `pp_api::admin`
mounts these `GET` and `HEAD` routes at `/` and `/api/v1`, where Node
registers its core routes, only when stored `PP_DESKTOP_REBUILD` is exactly `1`
or `true`. `pp_core::rebuild_api::read_router` mounts them beside the
Accepted Plan reads, from the same `WriterOwner`.

| Route | Body |
| --- | --- |
| `/legal/summary` | `LICENSE-SUMMARY.md` |
| `/legal/license` | `LICENSE` |
| `/legal/attribution` | `ATTRIBUTION.md` |
| `/legal/third-party` | `THIRD_PARTY_NOTICES.md` |
| `/filaments/custom` | `{filaments}` read from `<data>/custom_filaments.json` |

Legal documents are compiled in from `web/apps/server/src/data/legal`. The
Node build copies that directory to `dist/data/legal` and requires every file,
and `legal.ts` reads that copy first, so Node never reaches its other
candidates or its 404.

Each request first proves its credential with an empty `ReadClient` batch, so
a missing or wrong credential answers `401` `Authentication required` and a
full queue answers `503`, as on the Accepted Plan routes.

`/filaments/custom` reads the store as `services/custom-filaments.ts::loadStore`
does and serializes it as `JSON.stringify` would:

| Store file | Response |
| --- | --- |
| Missing, a directory, unreadable, or text `JSON.parse` rejects (including a byte order mark) | `200` `{"filaments":[]}` |
| A JSON object with `filaments` | `200` `{"filaments":<value>}`, re-serialized with JavaScript number, string and key-order rules |
| Any other JSON value, or an object without `filaments` | `200` `{}` |
| The literal `null` | `500` with Fastify's `TypeError` body |

Rust only reads the file. Node keeps writing it (`POST` and `DELETE`
`/filaments/custom` stay on Node), so the `custom-filaments` live format still
declares `rust_writes: "none"`.

One known difference: Rust's JSON parser stops at 128 levels of nesting and
then treats the store as unparsable (`{"filaments":[]}`), while Node serves
the nested value. Node writes flat entries, so only a hand-edited store nests
that deep. `ticket_t_62_admin_http` pins this case.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-storage -p pp-core \
  --features pp-api/desktop_rebuild,pp-storage/desktop_rebuild,pp-core/desktop_rebuild \
  --no-tests=fail -E 'test(ticket_t_62_admin) | binary(rebuild_read_http)'
```

## Date format, build tracking and default Spoolman settings (GRE-1357)

`pp_api::settings` mounts `GET` (with `HEAD`) and `PUT` for these routes at
`/` and `/api/v1` under the same flag and credential check, and
`read_router` mounts them beside the admin reads. Each request reads or writes
one `app_settings` row on the writer thread, in the tenant its credential
resolves to in the same transaction. The `pp_storage::app_settings` client
names only these three keys.

| Route | Row | `GET` body | `PUT` |
| --- | --- | --- | --- |
| `/settings/date-format` | `date_format` | `{format}`, `mdy_12h` when empty | `400` unless `format` is a `DATE_FORMAT_PRESETS` id |
| `/settings/build-tracking` | `build_tracking_assembly` | `{assembly_tracking}`, true unless empty or `"0"` | stores `"1"` for any truthy value; answers `true` only for boolean `true` |
| `/settings/spoolman-default` | `default_spoolman_integration_id` | `{integration_id}`, `null` when empty | trims the id; a non-string or empty id stores `""`; `400` unless the first stored integration with that id has `type` `spoolman` |

A cookie-authenticated `PUT` needs the listener's own `Origin`, as on the
other rebuild routes; without it the request answers `403` before any write.
A full writer queue, a stopped writer, or a write lock past its admission
deadline answers `503`.

The Spoolman check reads only the matched integration's `type`, never its
`config`. Rust writes these rows only in the Rust-only runtime; Node keeps its
own routes and there is no dual write or fallback.

`fixtures/admin/settings-cases.json` lists 52 requests;
`fixtures/admin/settings-oracle.json` records Node's reply and the three stored
rows after each one. `settings_routes_http` replays them and matches status,
cache header and stored rows on all 52, and the body on every `200` and on
each `400` with a `detail`. These differ by design:

- Errors are `application/problem+json`; the `detail` matches Node's.
- An empty or unparsable JSON body answers `400` `Request is invalid` instead
  of Fastify's `FST_ERR_CTP_*` body; an unsupported content type answers `415`.
- Rust also answers `400` for JSON that Fastify accepts but `serde_json`
  refuses: a leading byte order mark, invalid UTF-8, a number out of `f64`
  range such as `1e400`, or nesting past 128 levels.
- A chunked empty body with no content type answers `500`; Fastify answers
  `415`.
- A `null` body, or a `PUT` with no body and no content type, answers `500`
  `Internal Server Error`, where Node answers its `TypeError`.
- A stored `integrations` value with an unpaired UTF-16 surrogate escape holds
  no integrations for Rust, so the Spoolman `PUT` answers `400` where Node
  would accept it.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-storage -p pp-core \
  --features pp-api/desktop_rebuild,pp-storage/desktop_rebuild,pp-core/desktop_rebuild \
  --no-tests=fail -E 'binary(settings_routes_http) | binary(rebuild_read_http) | test(spoolman_lookup) | test(/^settings::/)'
(cd web && UPDATE_ADMIN_ORACLE=1 npm run test -w @print-partner/server -- src/routes/admin-routes.oracle.test.ts)
```

## Module write route (GRE-1341)

`PUT /api/v2/modules/{module}` turns one Module on or off for the replacement
Settings screen. `{module}` is one of the five `GET /health` lists (`farm`,
`spoolman`, `orders`, `bambu`, `assembly_tracking`) and the body is exactly
`{"on": true}` or `{"on": false}`. It is `setModule` in the generated
`@print-partner/rust-client`.

- It mounts only under `/api/v2` and only when `PP_DESKTOP_REBUILD` is `1` or
  `true`, so only the Rust-only runtime serves it. Node keeps
  `PATCH /settings/features` at `/` and `/api/v1` in mixed mode, as
  `operations.json` records, so the two never write the same setting.
- The accepted gate admits loopback requests with an exact Host and Origin and
  a credential; storage proves the credential inside the write's transaction
  (401, or 403 when the account needs an owner mapping). 30 requests a minute
  that reach the write, whatever storage answers, then 429.
- `FeatureClient::set_module` writes and reads back in one transaction.
  `farm`, `spoolman`, `orders` and `bambu` write the user's `feature_toggles`
  layer. Plan overrides (`feature_toggles:plan:<id>`) and every other stored key
  stay as they were. `assembly_tracking` writes `build_tracking_assembly` as
  `"1"` or `"0"`, as Node's `PUT /settings/build-tracking` does.
- `200` answers `{"modules": [...]}`: every Module's state read from the same
  rows `GET /health` and send admission read.
- `404` `Module not found` for any other name. `400` for any other body.
- When the stored `feature_toggles` doesn't parse (as `/health` reads it), a
  feature Module answers
  `500` with code `FEATURE_TOGGLES_INVALID` (Node's text) and nothing is
  written. `/health` keeps reporting those Modules `setting_unreadable`.
  `assembly_tracking` is its own row and still writes.
- A writer that stays busy past its admission deadline answers `503`
  `transaction_unavailable`; a full or stopping writer answers `503` as the
  other routes do. Nothing was written in either case.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core -p pp-storage -p pp-api \
  --features pp-core/desktop_rebuild,pp-storage/desktop_rebuild,pp-api/desktop_rebuild \
  --no-tests=fail -E 'test(module_route) | test(client_routes_match_registrations) | test(health_modules) | test(unreadable_stored_layer)'
```
