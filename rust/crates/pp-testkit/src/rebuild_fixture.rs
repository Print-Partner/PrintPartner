//! A Build ready to rebuild from Source roles, shared by the storage and MCP
//! tests of the rebuild. Build 3 has no layers, no draft and no Accepted Plan;
//! Sources 4 (`Base`) and 5 (`Addon`) are synced local Sources under the data
//! directory's `repos/`.
use serde_json::{Value, json};
use std::path::Path;

/// Rows for Build 3 and Sources 4 and 5. Replace `<data-dir>` before running.
pub const SEED: &str = "INSERT INTO build_profiles(id,tenant_id,name) VALUES(3,'default','Rebuild Fixture');
INSERT INTO projects(id,tenant_id,name,url,source_type,branch,local_path,last_synced_at,last_commit_sha) VALUES(4,'default','Base','','local','main','<data-dir>/repos/4','2026-11-01T00:00:00.000Z','rev4');
INSERT INTO projects(id,tenant_id,name,url,source_type,branch,local_path,last_synced_at,last_commit_sha) VALUES(5,'default','Addon','','local','main','<data-dir>/repos/5','2026-11-01T00:00:00.000Z','rev5');";

/// Creates both Sources' folders under `data_dir`, with printable files
/// when `files`.
pub fn write_sources(data_dir: &Path, files: bool) -> std::io::Result<()> {
    for (source, names) in [(4, &["bracket", "gear"][..]), (5, &["clip"][..])] {
        let root = data_dir.join(format!("repos/{source}"));
        std::fs::create_dir_all(&root)?;
        for name in names.iter().filter(|_| files) {
            std::fs::write(
                root.join(format!("{name}.stl")),
                format!("solid {name}\nendsolid {name}\n"),
            )?;
        }
    }
    Ok(())
}

/// An evidence item linked to Source `source` with role `role`.
pub fn evidence(id: &str, source: i64, role: &str) -> Value {
    json!({"id":id,"url":format!("https://example.test/{id}"),"normalized_url":format!("https://example.test/{id}"),
        "kind":"canonical_design","title":id,"source_id":source,"source_role":role})
}

/// Build 3's planning brief with `evidence` and `resolutions`.
pub fn brief(evidence: Value, resolutions: Value) -> Value {
    json!({"version":1,"build_id":3,"special_request":"Fixture","requirements":[],"evidence":evidence,
        "contributions":[],"compatibility_findings":[],"differences":[],"resolutions":resolutions,
        "role_filaments":[],"checklist_items":[],"draft_review_blockers":[],
        "created_at":"2026-11-01T00:00:00.000Z","updated_at":"2026-11-01T00:00:00.000Z"})
}

/// Source 4 as the structural base and Source 5 as an addon.
pub fn roles() -> Value {
    json!([
        evidence("base", 4, "structural_base"),
        evidence("addon", 5, "addon")
    ])
}

/// Both Sources as structural bases.
pub fn two_bases() -> Value {
    json!([
        evidence("base", 4, "structural_base"),
        evidence("addon", 5, "structural_base")
    ])
}

/// One resolved difference group: a path choice the first rebuild refuses.
pub fn resolved() -> Value {
    json!({"group-1":{"resolution":"choose_source_a","rationale":"Fixture","custom_resolution":null,"resolved_at":"2026-11-01T00:00:00.000Z"}})
}
