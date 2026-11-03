# Operation inventory

`scripts/operation-inventory.py` writes [the operation inventory report](../parity/operation-inventory.md).
The report counts what still runs in Node and what has a Rust owner. It is the
measure of how much has been converted.

```sh
python3 scripts/operation-inventory.py           # rewrite docs/parity/operation-inventory.md
python3 scripts/operation-inventory.py --check   # exit 1 when the committed report is stale
```

Rerun the script and commit the report whenever an input changes.

## Inputs

The script reads only these files and never writes them:

| Input | What it supplies |
| --- | --- |
| `rust/crates/pp-gateway/operations.json` | 781 compatibility routes in 310 route groups (the `operation` field), 57 denied routes, and 1 gateway-owned route (`POST /auth/logout`) |
| `rust/crates/pp-gateway/ownership-manifest.json` | 8 Node-owned operations, 82 reviewed and 347 unresolved resource sets, and 10 `live_formats` |
| The "Operation families and write ownership" table in [Architecture](../architecture.md) | The family names a roster entry may name as its Rust owner |
| `docs/plan/inventory-roster.json` | Entry → family and ticket, and exceptions |

The two JSON files under `pp-gateway` are generated. Never edit them by hand; run
their generators.

## Entries

Each entry has one of these kinds:

- `route`: a route group from `operations.json`. Its routes share one
  `operation` value, for example `GET /health current` covers `GET /health`
  and `HEAD /health`.
- `denied`: a denied route.
- `gateway`: a gateway-owned route.
- `operation`: a Node-owned operation from the ownership manifest.
- `participant`: a non-HTTP resource set such as an MCP tool, assistant tool,
  startup hook or migration.
- `format`: a live format.

An entry ID is `<kind>:<name>`, for example `route:GET /health current` or
`participant:tool.get_build_workflow`.

## Classes

Each entry gets exactly one class:

| Class | Source |
| --- | --- |
| Rust owner + ticket | A roster `owners` entry with an architecture family and a ticket |
| Delete with Node / stays denied | Every denied route, and roster exceptions with class `delete_with_node` |
| Gateway-owned, kept | `POST /auth/logout`; its Rust owner is `pp-gateway` |
| Deferred, owner OK | Roster exceptions with class `deferred` and an `owner_ok` record |
| Uncategorized | Every other entry |

Uncategorized is an expected state while the roster grows. It is not a failure.
Denied and gateway-owned entries have fixed classes; the roster cannot change them.

## Migrated routes

A route counts as migrated only when the generated inputs name Rust as its owner,
either in its `operations.json` `owner` field or through a manifest operation
whose owner is `rust`. A roster ticket does not make a route migrated. A route that
the Rust-only runtime does not serve counts as unmigrated.

## Roster

`docs/plan/inventory-roster.json` holds the route-group → ticket column:

```json
{
  "schema": "pp-inventory-roster-v1",
  "active_phase": "P1",
  "owners": {
    "route:GET /health current": {
      "family": "Admin, legal, logs, diagnostics, health",
      "ticket": "ABC-101"
    }
  },
  "exceptions": [
    {
      "entry": "format:kit-catalog",
      "class": "deferred",
      "ticket": "ABC-102",
      "expires": "2027-01-31",
      "owner_ok": "Owner approval, with its date and where it was given",
      "reason": "Why it waits"
    }
  ]
}
```

- `active_phase` names the current plan phase. The report prints it.
- `family` must match a row of the architecture family table exactly.
- `ticket` is a Linear issue ID such as `ABC-123`.
- Each exception carries a ticket ID, an `expires` date (`YYYY-MM-DD`) and a
  reason. A `deferred` exception also needs `owner_ok`. It may name a `family`.
- An entry ID may appear once, in `owners` or in `exceptions`.

The script refuses an invalid roster with exit code 2 and the reason. A roster
entry whose ID matches no inventory entry, for example after a route group is
removed, is listed in the report instead. An expired exception keeps its class
and prints a warning on stderr. The warning does not change the report or the
exit code.

Add a mapping only when its ticket names the route or operation. The first seed
maps five entries from the P1 tickets: `GET /health`,
`GET /plans/:id/build-planning`, `GET /plans/:id/decisions`,
`GET /plans/:id/workflow`, and the `get_build_workflow` MCP tool.

## Report

The rollup gives:

- uncategorized entries;
- kept entries without a Rust owner: entries classed Rust owner, gateway-owned
  or deferred that have no family;
- routes migrated to Rust out of the 781 compatibility routes;
- journey route misses: not measured yet; a later ticket fills this field;
- roster entries that match no inventory entry.

The report also gives entry counts by kind and class, each live format's
`node_reads` and `rust_writes` versions, exceptions by expiry, and one table per
entry kind. Route groups and participants show the Node operation they belong to
and whether their resource set is reviewed or unresolved.

The output is deterministic. It ends with the SHA-256 of each input file and of
the parsed family table, so a change to any input makes `--check` fail.

## Report mode

No CI step runs `--check`, and the report never blocks a merge.
`scripts/test_operation_inventory.py` tests the script over small fixtures. It runs
in the `python-lint` job and in `just ci --only python-lint`.
