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

/// Writes one printable file at `relative` under Source `source`.
pub fn write_stl(data_dir: &Path, source: i64, relative: &str) -> std::io::Result<()> {
    let path = data_dir.join(format!("repos/{source}")).join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("solid {relative}\nendsolid {relative}\n"))
}

/// An evidence item linked to Source `source` with role `role`.
pub fn evidence(id: &str, source: i64, role: &str) -> Value {
    json!({"id":id,"url":format!("https://example.test/{id}"),"normalized_url":format!("https://example.test/{id}"),
        "kind":"canonical_design","title":id,"source_id":source,"source_role":role})
}

/// Build 3's planning brief with `evidence` and `resolutions`.
pub fn brief(evidence: Value, resolutions: Value) -> Value {
    planning_brief(evidence, json!([]), json!([]), resolutions)
}

/// Build 3's planning brief, including contributions and differences.
pub fn planning_brief(
    evidence: Value,
    contributions: Value,
    differences: Value,
    resolutions: Value,
) -> Value {
    json!({"version":1,"build_id":3,"special_request":"Fixture","requirements":[],"evidence":evidence,
        "contributions":contributions,"compatibility_findings":[],"differences":differences,
        "resolutions":resolutions,"role_filaments":[],"checklist_items":[],"draft_review_blockers":[],
        "created_at":"2026-11-01T00:00:00.000Z","updated_at":"2026-11-01T00:00:00.000Z"})
}

/// A confirmed printable contribution on `evidence_id`, scoped to `scopes`.
pub fn contribution(evidence_id: &str, scopes: &[&str]) -> Value {
    json!({"id":format!("contribution-{evidence_id}"),"evidence_id":evidence_id,"slot":"parts",
        "responsibility":"printable_parts","path_scopes":scopes,"confidence":"high",
        "evidence_text":"Scoped printable parts","status":"confirmed"})
}

/// One changed-file difference. `path_a` belongs to `source_a`.
pub fn difference(
    group: &str,
    source_a: &str,
    path_a: &str,
    source_b: &str,
    path_b: &str,
) -> Value {
    json!({"id":group,"group_id":group,"family":"parts","kind":"changed","source_a":source_a,
        "source_b":source_b,"path_a":path_a,"path_b":path_b,"detail":"Content differs"})
}

/// A stored resolution of `kind`.
pub fn resolution(kind: &str) -> Value {
    json!({"resolution":kind,"rationale":"Fixture","custom_resolution":null,
        "resolved_at":"2026-11-01T00:00:00.000Z"})
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

/// Source 5 scoped to `mods/a/**`, and a resolution that drops Source 4's
/// `superseded.stl` in favour of Source 5.
pub fn scoped_choices() -> Value {
    planning_brief(
        roles(),
        json!([contribution("addon", &["mods/a/**"])]),
        json!([difference(
            "group-1",
            "Base",
            "superseded.stl",
            "Addon",
            "mods/a/plate.stl"
        )]),
        json!({"group-1": resolution("choose_source_b")}),
    )
}

/// A resolution that names a Source the evidence does not.
pub fn unresolved_choices() -> Value {
    planning_brief(
        roles(),
        json!([]),
        json!([difference(
            "group-1", "Gone", "gone.stl", "Addon", "clip.stl"
        )]),
        json!({"group-1": resolution("choose_source_b")}),
    )
}
