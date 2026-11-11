//! Database fixtures and assertions for detached Build experiments.
use rusqlite::{Connection, Result};

pub struct ExperimentBaseline {
    parent: (i64, i64),
    original: String,
}

pub fn prepare_parent(db: &Connection) -> Result<ExperimentBaseline> {
    db.execute(
        "UPDATE build_profiles SET order_number='PO-7' WHERE id=1",
        [],
    )?;
    let parent: (i64, i64) = db.query_row(
        "SELECT accepted_plan_revision_id,accepted_plan_version FROM build_profiles WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let original = db.query_row::<String,_,_>("SELECT group_concat(required_unit_token,',') FROM plan_revision_required_units WHERE revision_id=?1",[parent.0],|r|r.get(0))?;
    db.execute("UPDATE parts SET filament_color_id='new-filament',filament_custom_hex='#112233',spoolman_spool_id='spool-42' WHERE profile_id=1",[])?;
    Ok(ExperimentBaseline { parent, original })
}

pub fn assert_detached(
    db: &Connection,
    build: i64,
    draft: i64,
    baseline: &ExperimentBaseline,
) -> Result<()> {
    let header: (Option<String>,Option<i64>,i64,Option<String>) = db.query_row("SELECT order_number,accepted_plan_revision_id,accepted_plan_version,archived_at FROM build_profiles WHERE id=?1", [build], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    assert_eq!(header, (None, None, 0, None));
    let count = |sql: &str, id: i64| db.query_row::<i64, _, _>(sql, [id], |r| r.get(0));
    assert_eq!(
        count("SELECT count(*) FROM parts WHERE profile_id=?1", build)?,
        0
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM plan_revisions WHERE profile_id=?1",
            build
        )?,
        0
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM plan_draft_parts WHERE draft_id=?1 AND base_revision_part_id IS NOT NULL",
            draft
        )?,
        0
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM plan_draft_parts WHERE draft_id=?1",
            draft
        )?,
        count(
            "SELECT count(*) FROM plan_revision_parts WHERE revision_id=?1",
            baseline.parent.0
        )?
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM profile_layers WHERE profile_id=?1",
            build
        )?,
        count("SELECT count(*) FROM profile_layers WHERE profile_id=?1", 1)?
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM build_experiments WHERE parent_build_id=?1",
            1
        )?,
        1
    );
    assert_eq!(db.query_row::<String,_,_>("SELECT group_concat(required_unit_token,',') FROM plan_revision_required_units WHERE revision_id=?1",[baseline.parent.0],|r|r.get(0))?,baseline.original);
    assert_eq!(db.query_row::<i64,_,_>("SELECT count(*) FROM (SELECT part_key,quantity_effective,included,notes,role_override FROM plan_revision_parts WHERE revision_id=?1 EXCEPT SELECT part_key,quantity_effective,included,notes,role_override FROM plan_draft_parts WHERE draft_id=?2)",rusqlite::params![baseline.parent.0,draft],|r|r.get(0))?,0);
    let assignment: (String,String,String) = db.query_row("SELECT filament_color_id,filament_custom_hex,spoolman_spool_id FROM plan_draft_parts WHERE draft_id=?1 LIMIT 1",[draft],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!(
        assignment,
        ("new-filament".into(), "#112233".into(), "spool-42".into())
    );
    assert_eq!(
        db.query_row(
            "SELECT accepted_plan_revision_id,accepted_plan_version FROM build_profiles WHERE id=1",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        )?,
        baseline.parent
    );
    Ok(())
}

pub fn assert_restricted_and_archive(db: &Connection, build: i64) -> Result<()> {
    // The FK also protects ancestry from a caller outside the HTTP adapter.
    db.pragma_update(None, "foreign_keys", "ON")?;
    assert!(
        db.execute("DELETE FROM build_profiles WHERE id=1", [])
            .is_err()
    );
    assert_eq!(
        db.query_row::<i64, _, _>(
            "SELECT parent_build_id FROM build_experiments WHERE build_id=?1",
            [build],
            |r| r.get(0)
        )?,
        1
    );
    // Archived experiments still preserve their parent's ancestry.
    db.execute(
        "UPDATE build_profiles SET archived_at='2026-11-10' WHERE id=?1",
        [build],
    )?;
    Ok(())
}

pub fn prepare_order_claim(db: &Connection) -> Result<()> {
    db.execute_batch("INSERT INTO build_profiles(id,tenant_id,name) VALUES(99,'default','Parent'); INSERT INTO build_experiments(build_id,parent_build_id) VALUES(1,99); INSERT INTO plan_draft_headers(draft_id,tenant_id,name,order_number,header_version,updated_at) VALUES(2,'default','Experiment','PO-9',1,'2026-11-10');")
}
