# Rust owns its data formats; the import is the only reader of Node's

Status: accepted on 2026-11-12 by Chad. Refines step 4 of
[whole-runtime cutover](0010-whole-runtime-cutover.md) and replaces the "keep
SQLite formats" rule that predates it.

Convert a Node installation once, in the import that creates the Rust-owned
copy. The import is the only Rust code that reads Node's stored shapes. After
it, every row and stored JSON document has one typed Rust shape, and Rust
changes its schema through its own migrations.

The import keeps the data, not its encoding. Plan, Plate and Required-unit
IDs, accepted history, drafts, Checkoff and farm history, printers, and
Sources with their revision bytes arrive intact. The import proves that on a
copy and leaves the original untouched, as ADR 0010 requires.

The alternative was the rule in force until now: keep Node's SQLite formats,
so that both runtimes could read one data directory. ADR 0010 already ended
directory sharing. Node and Rust never share a live directory, and rollback
restores the pre-cutover copy for the pinned Node runtime. Format compatibility
therefore protects nothing, and it costs runtime code that reads loosely typed
JSON the way JavaScript does. On 2026-11-12, 25 Rust source files defined
`js_*` coercion helpers (`js_trim` nine times), and Checkoff links travelled
as `Map<String, Value>`.

The same reasoning covers behaviour. Rust follows the product contract in
`docs/`. Node's error strings, type coercions and ordering are not
requirements. [Parity scope](../development/definition-of-done.md#parity-scope)
holds the working rule.

## Consequences

- The import carries the conversion and its tests. It is sensitive work: a
  mistake there loses data.
- Existing `js_*` helpers and loosely typed readers stay until the import
  replaces their inputs. New code adds none.
  [Simplification after cutover](../architecture.md#simplification-after-cutover)
  schedules their removal.
- A Rust-owned directory written before the import lands is disposable test
  data, as today.
- Rollback is unchanged: stop Rust and restore the pre-cutover copy.
