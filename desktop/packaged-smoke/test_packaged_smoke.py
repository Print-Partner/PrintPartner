"""Unit tests for the packaged-smoke helpers. They need no display and no app."""

import json
import pathlib
import sqlite3
import tempfile
import unittest

import install_deb
import legacy_fixture
import macos_ui
import smoke


def corpus(path):
    """A tiny schema 34 database with the tables legacy_fixture.py touches."""
    connection = sqlite3.connect(path)
    statements = [
        "CREATE TABLE app_settings(tenant_id TEXT, key TEXT, value TEXT)",
        "INSERT INTO app_settings VALUES('default','schema_version','34')",
        "CREATE TABLE board_posts(id INTEGER)",
        "CREATE TABLE board_comments(id INTEGER)",
        "CREATE TABLE projects(id INTEGER, legacy_manifest_cutover INTEGER)",
        "CREATE TABLE build_profiles(id INTEGER, name TEXT)",
        "INSERT INTO build_profiles VALUES(1,'Kit')",
    ]
    for table in legacy_fixture.PRESERVED_TABLES:
        if table != "build_profiles":
            statements.append(f'CREATE TABLE "{table}"(id INTEGER, value TEXT)')
            statements.append(f"INSERT INTO \"{table}\" VALUES(1,'{table}')")
    connection.executescript(";".join(statements))
    connection.commit()
    connection.close()


def migrate(database):
    connection = sqlite3.connect(database)
    connection.executescript(
        "CREATE TABLE board_posts(id INTEGER); CREATE TABLE board_comments(id INTEGER);"
        "UPDATE app_settings SET value='34' WHERE key='schema_version';"
    )
    connection.commit()
    connection.close()


class LegacyFixtureTest(unittest.TestCase):
    def setUp(self):
        self.root = pathlib.Path(tempfile.mkdtemp())
        corpus(self.root / "corpus.db")
        self.meta = legacy_fixture.derive(self.root / "corpus.db", self.root / "out")
        self.database = self.root / "out" / "print-partner.db"

    def test_derives_schema_31_without_board_tables(self):
        connection = sqlite3.connect(self.database)
        self.assertEqual(legacy_fixture.schema_version(connection), 31)
        tables = legacy_fixture.table_names(connection)
        self.assertNotIn("board_posts", tables)
        columns = [row[1] for row in connection.execute("PRAGMA table_info(projects)")]
        self.assertNotIn("legacy_manifest_cutover", columns)
        connection.close()
        self.assertEqual(self.meta["builds"], ["Kit"])
        written = json.loads((self.root / "out" / "fixture.json").read_text())
        self.assertEqual(written["preservedDigest"], self.meta["preservedDigest"])

    def test_refuses_a_corpus_that_is_not_schema_34(self):
        connection = sqlite3.connect(self.database)
        connection.execute("UPDATE app_settings SET value='31'")
        connection.commit()
        connection.close()
        with self.assertRaises(ValueError):
            legacy_fixture.derive(self.database, self.root / "again")

    def test_migrated_database_with_unchanged_rows_passes(self):
        migrate(self.database)
        result = smoke.verify_database(self.database, self.meta)
        self.assertEqual(result["schemaVersion"], 34)

    def test_unmigrated_database_fails(self):
        with self.assertRaisesRegex(smoke.SmokeFailure, "not migrated"):
            smoke.verify_database(self.database, self.meta)

    def test_changed_plate_row_fails(self):
        migrate(self.database)
        connection = sqlite3.connect(self.database)
        connection.execute("UPDATE accepted_plates SET value='moved'")
        connection.commit()
        connection.close()
        with self.assertRaisesRegex(smoke.SmokeFailure, "Plan or Plate rows changed"):
            smoke.verify_database(self.database, self.meta)

    def test_dropped_plan_table_fails(self):
        migrate(self.database)
        connection = sqlite3.connect(self.database)
        connection.execute("DROP TABLE plan_revisions")
        connection.commit()
        connection.close()
        with self.assertRaisesRegex(ValueError, "plan_revisions is missing"):
            smoke.verify_database(self.database, self.meta)


class DataDirectoryTest(unittest.TestCase):
    def setUp(self):
        self.root = pathlib.Path(tempfile.mkdtemp())
        (self.root / "fixture").mkdir()
        (self.root / "fixture" / "print-partner.db").write_bytes(b"db")

    def test_marker_matches_the_documented_format(self):
        storage = pathlib.Path(__file__).resolve().parents[2] / "docs" / "storage.md"
        documented = storage.read_text().split("### Ownership marker format")[1]
        example = documented.split("```json\n")[1].split("\n```")[0]
        self.assertEqual(smoke.RUST_OWNED_MARKER, example.encode() + b"\n")

    def test_rust_owned_directory_gets_a_private_marker(self):
        data = self.root / "data"
        smoke.prepare_data(data, self.root / "fixture", rust_owned=True)
        marker = data / ".pp-rust-owned.json"
        self.assertEqual(marker.stat().st_mode & 0o777, 0o600)
        self.assertEqual(data.stat().st_mode & 0o777, 0o700)
        self.assertEqual(
            sorted(smoke.directory_state(data)), [".", ".pp-rust-owned.json", "print-partner.db"]
        )

    def test_directory_state_sees_a_changed_database(self):
        data = self.root / "data"
        smoke.prepare_data(data, self.root / "fixture", rust_owned=False)
        before = smoke.directory_state(data)
        (data / "print-partner.db").write_bytes(b"changed")
        self.assertNotEqual(before, smoke.directory_state(data))

    def test_directory_state_sees_nested_entries_and_modes(self):
        data = self.root / "data"
        smoke.prepare_data(data, self.root / "fixture", rust_owned=False)
        (data / "logs").mkdir()
        before = smoke.directory_state(data)
        (data / "logs" / "a.log").write_text("x")
        nested = smoke.directory_state(data)
        self.assertNotEqual(before, nested)
        (data / "print-partner.db").chmod(0o600)
        self.assertNotEqual(nested, smoke.directory_state(data))


class InstallTreeTest(unittest.TestCase):
    stage = {
        "web": ("dir", None),
        "web/a.js": ("file", "1", False),
        "web/node_modules": ("dir", None),
        "web/node_modules/@pp": ("dir", None),
        "web/node_modules/@pp/core": ("link", "../../packages/core"),
        "bin": ("dir", None),
        "bin/node": ("file", "2", True),
    }

    def packaged(self, **changes):
        return {**self.stage, **changes}

    def test_identical_tree_passes(self):
        found = install_deb.differences(self.stage, self.packaged())
        self.assertFalse(any(found.values()))

    def test_dropped_link_fails(self):
        tree = self.packaged()
        del tree["web/node_modules/@pp/core"]
        self.assertEqual(
            install_deb.differences(self.stage, tree)["missing"], ["web/node_modules/@pp/core"]
        )

    def test_retargeted_link_fails(self):
        tree = self.packaged(**{"web/node_modules/@pp/core": ("link", "../elsewhere")})
        self.assertEqual(
            install_deb.differences(self.stage, tree)["changed"], ["web/node_modules/@pp/core"]
        )

    def test_changed_file_and_lost_executable_bit_fail(self):
        tree = self.packaged(**{"web/a.js": ("file", "x", False), "bin/node": ("file", "2", False)})
        self.assertEqual(
            install_deb.differences(self.stage, tree)["changed"], ["bin/node", "web/a.js"]
        )

    def test_extra_file_fails(self):
        tree = self.packaged(**{"web/b.js": ("file", "3", False)})
        self.assertEqual(install_deb.differences(self.stage, tree)["extra"], ["web/b.js"])


class MacTreeTest(unittest.TestCase):
    """The macOS UI's tree logic on a dump shaped like ax_probe.swift output."""

    dump = {
        "apps": [
            {
                "pid": 7,
                "windows": [
                    {
                        "r": "AXWindow",
                        "t": "Print Partner",
                        "w": 1280,
                        "h": 850,
                        "x": 0,
                        "y": 25,
                        "c": [
                            {
                                "r": "AXGroup",
                                "c": [
                                    {
                                        "r": "AXWebArea",
                                        "x": 0,
                                        "y": 60,
                                        "w": 1280,
                                        "h": 815,
                                        "c": [
                                            {"r": "AXTextField", "l": "Display name"},
                                            {"r": "AXTextField", "t": "Email"},
                                            {
                                                "r": "AXTextField",
                                                "s": "AXSecureTextField",
                                                "d": "Password",
                                            },
                                            {
                                                "r": "AXButton",
                                                "c": [
                                                    {
                                                        "r": "AXStaticText",
                                                        "v": "Create administrator",
                                                    }
                                                ],
                                            },
                                            {
                                                "r": "AXCell",
                                                "c": [
                                                    {
                                                        "r": "AXLink",
                                                        "c": [
                                                            {
                                                                "r": "AXStaticText",
                                                                "v": "Parity Build",
                                                            }
                                                        ],
                                                    }
                                                ],
                                            },
                                        ],
                                    }
                                ],
                            }
                        ],
                    }
                ],
            }
        ]
    }

    onscreen = True

    def ui(self, calls):
        ui = macos_ui.MacUI()

        def call(*args):
            calls.append(args)
            if args[0] == "dump":
                return self.dump
            if args[0] == "cgwindows":
                return [
                    {
                        "id": 41,
                        "pid": 7,
                        "layer": 0,
                        "onscreen": self.onscreen,
                        "x": 0,
                        "y": 25,
                        "w": 1280,
                        "h": 850,
                    }
                ]
            return {"pressed": True}

        ui.call = call
        return ui

    def test_walk_paths_follow_window_and_child_indices(self):
        paths = {path: node.get("r") for _, path, node, _ in macos_ui.walk(self.dump)}
        self.assertEqual(paths["0"], "AXWindow")
        self.assertEqual(paths["0.0.0.4.0"], "AXLink")

    def test_finds_the_build_as_page_content(self):
        found = self.ui([]).find_content({7}, "Parity Build")
        self.assertIn(found["role"], macos_ui.CONTENT_ROLES)
        self.assertIn("AXWebArea", found["path"])

    def test_window_title_alone_is_not_page_content(self):
        self.assertIsNone(self.ui([]).find_content({7}, "Print Partner"))

    def test_window_carries_its_screencapture_number(self):
        window = self.ui([]).windows({7})[0]
        self.assertEqual(
            (window["title"], window["id"], window["visible"]), ("Print Partner", 41, True)
        )

    def test_window_records_its_page_frame(self):
        self.assertEqual(self.ui([]).windows({7})[0]["page"], [0, 60, 1280, 815])

    def test_off_screen_window_is_not_visible_and_not_captured(self):
        self.onscreen = False
        ui = self.ui([])
        window = ui.windows({7})[0]
        self.assertEqual((window["visible"], window["id"]), (False, None))
        self.assertEqual(ui.screenshot(window, "/nonexistent.png"), 0)

    def test_setup_form_types_each_field_then_presses_the_button(self):
        calls = []
        ui = self.ui(calls)
        form = ui.setup_form({7}, smoke.SETUP_FIELDS, smoke.SETUP_BUTTON)
        self.assertIsNotNone(form)
        ui.complete_setup(None, form, {name: "x" for name in smoke.SETUP_FIELDS})
        actions = [call[:3] for call in calls if call[0] in ("type", "press")]
        self.assertEqual(
            actions,
            [
                ("type", 7, "0.0.0.0"),
                ("type", 7, "0.0.0.1"),
                ("type", 7, "0.0.0.2"),
                ("press", 7, "0.0.0.3"),
            ],
        )

    def test_startup_error_is_found_by_its_message_text(self):
        self.dump = {
            "apps": [
                {
                    "pid": 7,
                    "windows": [
                        {
                            "r": "AXWindow",
                            "c": [{"r": "AXStaticText", "v": smoke.STARTUP_ERROR_TITLE}],
                        }
                    ],
                }
            ]
        }
        self.assertIsNotNone(self.ui([]).startup_error({7}, smoke.STARTUP_ERROR_TITLE))


if __name__ == "__main__":
    unittest.main()
