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
(cd web && UPDATE_ADMIN_ORACLE=1 npm run test -w @print-partner/server -- src/routes/settings-routes.oracle.test.ts)
```
