#!/usr/bin/env python3
"""Derive the packaged-smoke schema 31 database from the storage-fixture generator.

web/scripts/storage-fixtures/prepare.mts writes an "immutable-corpus" case: a
Node schema 34 database whose rows come from the autosave golden, including a
Build with accepted Plan revisions and Plates. This script copies that input
and applies the transformations prepare.mts applies to its schema-31 case
(drop the board tables, drop projects.legacy_manifest_cutover, stamp the
version). Nothing is hand-edited; rerun the generator to change the fixture.
"""

import argparse
import hashlib
import json
import pathlib
import shutil
import sqlite3
import sys

STAMP = 31
TRANSFORMATIONS = (
    "copy-immutable-corpus-input",
    "drop-board-posts-and-board-comments",
    "drop-projects-legacy-manifest-cutover",
    "stamp-schema-version",
)
# Tables whose rows must survive the packaged app's migration unchanged.
PRESERVED_TABLES = (
    "build_profiles",
    "plan_revisions",
    "plan_revision_parts",
    "plan_revision_required_unit_sets",
    "plan_revision_required_units",
    "accepted_plate_heads",
    "accepted_plate_revisions",
    "accepted_plates",
    "accepted_plate_units",
)


def schema_version(connection):
    row = connection.execute(
        "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'"
    ).fetchone()
    return int(row[0]) if row else None


def table_names(connection):
    return {
        row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")
    }


def preserved_rows(connection, columns=None):
    """Return {table: sorted rows}, limited to the given columns per table when supplied."""
    snapshot = {}
    for table in PRESERVED_TABLES:
        present = [row[1] for row in connection.execute(f'PRAGMA table_info("{table}")')]
        if not present:
            raise ValueError(f"{table} is missing")
        wanted = columns[table] if columns else present
        missing = [name for name in wanted if name not in present]
        if missing:
            raise ValueError(f"{table} lost columns {missing}")
        selected = ",".join(f'"{name}"' for name in wanted)
        rows = [
            dict(zip(wanted, row))
            for row in connection.execute(f'SELECT {selected} FROM "{table}"')
        ]
        snapshot[table] = sorted(rows, key=lambda row: json.dumps(row, sort_keys=True))
    return snapshot


def snapshot_columns(connection):
    return {
        table: [row[1] for row in connection.execute(f'PRAGMA table_info("{table}")')]
        for table in PRESERVED_TABLES
    }


def digest(snapshot):
    return hashlib.sha256(json.dumps(snapshot, sort_keys=True).encode()).hexdigest()


def derive(corpus, output):
    output.mkdir(parents=True, exist_ok=False)
    database = output / "print-partner.db"
    shutil.copyfile(corpus, database)
    connection = sqlite3.connect(database)
    try:
        if schema_version(connection) != 34:
            raise ValueError("the immutable-corpus input must be Node schema 34")
        connection.executescript(
            "DROP TABLE board_comments; DROP TABLE board_posts;"
            "ALTER TABLE projects DROP COLUMN legacy_manifest_cutover;"
        )
        connection.execute(
            "UPDATE app_settings SET value=? WHERE tenant_id='default' AND key='schema_version'",
            (str(STAMP),),
        )
        connection.commit()
        if connection.execute("PRAGMA foreign_key_check").fetchall():
            raise ValueError("the derived fixture has broken foreign keys")
        snapshot = preserved_rows(connection)
        columns = snapshot_columns(connection)
    finally:
        connection.close()
    builds = sorted(row["name"] for row in snapshot["build_profiles"])
    if not builds or not snapshot["accepted_plates"] or not snapshot["plan_revisions"]:
        raise ValueError("the corpus has no Build with accepted Plan revisions and Plates")
    metadata = {
        "generator": "web/scripts/storage-fixtures/prepare.mts",
        "case": "immutable-corpus",
        "historicalSnapshot": False,
        "sourceSchemaVersion": 34,
        "startingVersionStamp": STAMP,
        "transformations": list(TRANSFORMATIONS),
        "corpusSha256": hashlib.sha256(corpus.read_bytes()).hexdigest(),
        "derivedSha256": hashlib.sha256(database.read_bytes()).hexdigest(),
        "deriverSha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
        "builds": builds,
        "rowCounts": {table: len(rows) for table, rows in snapshot.items()},
        "preservedColumns": columns,
        "preservedDigest": digest(snapshot),
    }
    (output / "fixture.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")
    return metadata


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--prepared", type=pathlib.Path, required=True, help="output directory of prepare.mts"
    )
    parser.add_argument("--output", type=pathlib.Path, required=True, help="a new directory")
    args = parser.parse_args(argv)
    corpus = args.prepared / "immutable-corpus" / "input" / "print-partner.db"
    print(json.dumps(derive(corpus, args.output), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
