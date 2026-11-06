# Printer send queue storage (GRE-1208)

The Rust send queue (GRE-365) keeps its items in the `printer_send_queue`
table. Node keeps them as one JSON array in each tenant's `printer.send_queue`
setting. This page covers the storage half: the table, how Rust reads a queue
Node left behind, and when the items move. It is compiled only with the
default-off `desktop_rebuild` feature, and only the Rust-only launcher
(`PP_DESKTOP_REBUILD=1` or `true`) opens the writer that moves them. The send
routes and the upload worker are GRE-365 itself; no Rust code sends to a
printer yet.

## Schema 44

The 43 to 44 step installs the table and its index from
`pp-storage/src/send_queue/schema.sql`, validates them and stamps 44 in one
transaction, after a `pre-schema44.db` copy like every step through
`LAST_COPIED_STEP`. It alters no table Node uses. Preflight refuses the table
in a database below 44, every open checks its SQL against the reference, and
the Node schema-parity comparison omits it, as it omits the other Rust-only
tables.

| Column | Meaning |
| --- | --- |
| `tenant_id`, `id` | Primary key. The tenant and the item id are Node's, unchanged. |
| `position` | Stored order within the tenant, unique per tenant. |
| `filename`, `artifact_path`, `printer_id` | Required, as Node's `parseItem` requires them. `printer_id` is the fleet printer id string Node stored. |
| `profile_id` | The Build, null or a positive integer. |
| `match`, `wait_for_idle`, `start` | Node's send options. |
| `state` | `queued`, `sending`, `done`, `error` or `cancelled`. |
| `checkoff_units` | Null or a JSON array of `{part_id, unit_index, object_name?}`. |
| `upload_job_id`, `error`, `host_name` | Null or non-empty text. |
| `created_at`, `updated_at` | Node's ISO timestamps. |

A partial unique index, `printer_send_queue_one_sending`, allows at most one
`sending` item per tenant and printer, the rule Node's dispatch checks before
it claims an item.

## Reading a queue Node left

While a tenant's `printer.send_queue` setting row exists, every Rust reader
reads the setting exactly as before and the table holds nothing for that
tenant. Once the setting is gone they read the table, which
`send_queue::rows` returns in Node's item JSON shape, so each reader keeps its
own Node-compatible parsing:

- the farm reads (`GET /api/v2/printer-send-queue` and the match suggestion
  evidence), [Farm reads](farm-reads.md)
- Build History's send entries and the farm log's send entries
- the Build context's production counts
- Accept's production check and its Required-unit remap, which writes the
  remapped `checkoff_units` back to the table rows, or to the setting while
  the setting exists

## When items move

`WriterOwner::open` runs `send_queue::recover` after job recovery, in one
immediate transaction. The writer opens only after the ownership marker
decision has given the directory to Rust, and Node refuses a database at
schema 44, so no Node process writes the setting afterwards.

1. For each tenant with the setting, if the setting is a clean queue, every
   item is inserted in stored order and the setting row is deleted. Clean
   means a JSON array whose every item Node's `parseItem` keeps whole: a
   known state; a non-empty id, file name, artifact path and printer; no
   duplicate id; a `profile_id` that is null or a positive integer; and
   `checkoff_units` that are null or an array of whole coordinates. Text is
   trimmed and `error` cut to 500 characters, as `parseItem` does, and a
   missing time is stamped with the open time, as Node stamps it on load.
2. A queue that is not clean stays in the setting and keeps being read from
   there. Moving it would drop or change items that Node or Accept reads, and
   Accept still refuses such a queue as it does today. The only rewrite is the
   one Node's own restart makes: each `sending` item becomes `error` with
   the interrupted text below. A value that is not a JSON array, or holds no
   `sending` item, stays byte for byte.
3. A tenant whose table already holds items keeps both: the setting is read,
   and the rows are never merged with it.
4. Each tenant moves inside its own savepoint. If the table refuses an item
   (for example an id over 128 characters), that tenant's inserts roll back
   and its setting stays, as in item 2; startup and the other tenants are
   unaffected.
5. A `sending` item moves as `error`, and every `sending` row already in
   the table becomes `error`, with Node's
   `INTERRUPTED_PRINTER_SEND_ERROR` text and a new `updated_at`. The process
   that started the upload is gone, so the outcome is unknown; the item is
   never sent again automatically, and only the user may send it again.

A second open finds no setting and no `sending` item, and changes nothing.

## Tests

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild -E 'test(ticket_t_26) | test(remapped_queue_rows_match_literal_node_results)'
```
