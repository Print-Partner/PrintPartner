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
/// Tables that a long-lived installation fills and the seeded synthetic
/// installation leaves empty. [`populate`] writes one row in each.
pub const LONG_LIVED_TABLES: [&str; 10] = [
    "app_events",
    "plan_decisions",
    "plan_draft_required_unit_decisions",
    "plan_snapshots",
    "print_job_parts",
    "print_jobs",
    "sessions",
    "source_docs",
    "source_notes",
    "users",
];
const MANIFEST: &str = ".printpartner-source-snapshot.json";

/// Fills `directory`, which already holds a schema 33 or 34
/// `print-partner.db` (`Fixture::NodeSchema34`, or
/// `node_schema::write_schema_33_database`), with Sources, Builds, settings
/// and files, and one row in each table a long-lived installation fills. One
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
        ("printer.unattributed_prints", r#"[{"id":"a","filename":"a.gcode"},{"id":"b","filename":"b.gcode","claimed_at":null},{"id":"c","filename":"c.gcode","claimed_at":"2026-10-02T00:00:00.000Z"},{"id":"d","filename":"d.gcode","dismissed":true}]"#.to_owned()),
        ("feature_toggles", r#"{"farm":true}"#.to_owned()),
        ("feature_toggles:plan:1", r#"{"bambu":false}"#.to_owned()),
        ("production_setup:1", r#"{"printers":[]}"#.to_owned()),
        ("kit_manifest_2", r#"{"modules":[]}"#.to_owned()),
        ("filename_grouping:1", r#"{"mode":"flat"}"#.to_owned()),
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
        // One row in each table that a long-lived installation fills and the
        // rows above leave empty. Foreign keys are off so the rows stay small.
        connection.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO users (id, display_name, created_at) VALUES ('synthetic-owner', 'Synthetic Owner', '2026-10-02');
             INSERT INTO sessions (id, user_id, expires_at) VALUES ('synthetic-session', 'synthetic-owner', '2099-01-01');
             INSERT INTO app_events (at, kind, payload_json) VALUES ('2026-10-02', 'synthetic', '{\"source\":1}');
             INSERT INTO plan_decisions (profile_id, created_at, kind, params_json, result_json) VALUES (1, '2026-10-02', 'synthetic', '{}', '{}');
             INSERT INTO plan_snapshots (profile_id, name, created_at, payload_json) VALUES (1, 'Synthetic snapshot', '2026-10-02', '{\"parts\":[]}');
             INSERT INTO print_jobs (profile_id, at) VALUES (1, '2026-10-02');
             INSERT INTO print_job_parts (job_id, at, profile_id, part_id, result) VALUES (1, '2026-10-02', 1, 'synthetic-part', 'ok');
             INSERT INTO source_docs (project_id, path, kind, updated_at) VALUES (1, 'README.md', 'markdown', '2026-10-02');
             INSERT INTO source_notes (project_id, created_at, updated_at) VALUES (1, '2026-10-02', '2026-10-02');
             INSERT INTO plan_drafts (profile_id, base_plan_version, state, digest_format, snapshot_digest, created_by, idempotency_key, created_at)
               VALUES (1, 0, 'open', 'plan-draft-v2', 'synthetic-digest', 'synthetic', 'synthetic-draft', '2026-10-02');
             INSERT INTO plan_draft_parts (draft_id, part_key) VALUES (1, 'synthetic-part');
             INSERT INTO plan_draft_required_unit_reconciliations (tenant_id, profile_id, draft_id, format, planning_digest, selection_basis_digest, selection_basis_json, decision_digest, result_kind, result_digest, result_json, reconciliation_digest, expected_assignment_count, actor_id, idempotency_key, payload_digest, created_at)
               VALUES ('default', 1, 1, 'required-unit-reconciliation-v1', 'd', 'd', '[]', 'd', 'unresolved', 'd', '{\"kind\":\"unresolved\",\"conflicts\":[]}', 'd', 0, 'synthetic', 'synthetic-reconciliation', 'd', '2026-10-02');
             INSERT INTO plan_draft_required_unit_decisions (tenant_id, reconciliation_id, target_draft_part_id, kind) VALUES ('default', 1, 1, 'replace');",
        )?;
        Ok(())
    };
    run().map_err(io::Error::other)
}
