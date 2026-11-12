//! A small synthetic Node installation: rows and files in the shapes a Node
//! server stores, written over a schema 34 database. Every name and value is
//! invented here; nothing comes from a real data directory.

use rusqlite::Connection;
use std::{fs, io, path::Path};

pub const BUILDS: u64 = 3;
pub const SOURCES: u64 = 2;
pub const PRINTERS: u64 = 2;
/// Stored unmatched prints with neither a claim time nor a dismissal. Two
/// more are stored, one claimed and one dismissed.
pub const UNMATCHED_PRINTS: u64 = 2;
/// A value stored in the integration's config. No report may contain it.
pub const INTEGRATION_SECRET: &str = "synthetic-not-a-real-key";
const MANIFEST: &str = ".printpartner-source-snapshot.json";

/// Fills `directory`, which already holds a schema 34 `print-partner.db`
/// (`Fixture::NodeSchema34`), with Sources, Builds, settings and files. One
/// printer has a numeric id and a bed size stored as text, as old Node
/// installations do. Source paths are absolute, under `directory`.
pub fn populate(directory: &Path) -> io::Result<()> {
    let revision = directory.join("repos/1/revisions/synthetic-a");
    fs::create_dir_all(&revision)?;
    fs::write(revision.join(MANIFEST), "{}\n")?;
    fs::write(
        revision.join("synthetic-part.stl"),
        "solid synthetic\nendsolid synthetic\n",
    )?;
    fs::write(
        // ast-grep-ignore: no-writer-family-literal -- A synthetic copy of the Node file for tests; pp-testkit is never in a release build.
        directory.join("custom_filaments.json"),
        r#"[{"name":"Synthetic PLA"},{"name":"Synthetic PETG"}]"#,
    )?;
    let settings = [
        ("printer.fleet", r#"[{"id":1,"name":"Synthetic Printer 1","bed_width_mm":"250"},{"id":"synthetic-printer-2","name":"Synthetic Printer 2","bed_width_mm":250}]"#.to_owned()),
        ("integrations", format!(r#"[{{"id":"synthetic-integration","kind":"moonraker","config":{{"host":"printer.invalid","port":"7125","api_key":"{INTEGRATION_SECRET}"}}}}]"#)),
        ("printer.unattributed_prints", r#"[{"id":"a"},{"id":"b","claimed_at":null},{"id":"c","claimed_at":"2026-10-02T00:00:00.000Z"},{"id":"d","dismissed":true}]"#.to_owned()),
        ("feature_toggles", r#"{"farm":true}"#.to_owned()),
        ("feature_toggles:plan:1", r#"{"bambu":false}"#.to_owned()),
        ("production_setup:1", r#"{"printers":[]}"#.to_owned()),
        ("kit_manifest_2", r#"{"modules":[]}"#.to_owned()),
        ("date_format", "iso".to_owned()),
        ("build_planning.create.synthetic-key-7", "1".to_owned()),
    ];
    let run = || -> rusqlite::Result<()> {
        let connection = Connection::open(directory.join(crate::DATABASE_FILE))?;
        for (name, imported_paths, metadata) in [
            (
                "Synthetic Source 01",
                Some(" parts/ "),
                Some(r#"{"category":"synthetic"}"#),
            ),
            ("Synthetic Source 02", None, None),
        ] {
            connection.execute(
                "INSERT INTO projects (name, url, local_path, imported_paths, metadata_json)
                 VALUES (?1, 'https://synthetic.invalid/source', ?2, ?3, ?4)",
                (name, revision.to_string_lossy(), imported_paths, metadata),
            )?;
        }
        for number in 1..=BUILDS {
            connection.execute(
                "INSERT INTO build_profiles (name) VALUES (?1)",
                [format!("Synthetic Build {number:02}")],
            )?;
        }
        for (key, value) in &settings {
            connection.execute(
                "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', ?1, ?2)",
                (key, value),
            )?;
        }
        Ok(())
    };
    run().map_err(io::Error::other)
}
