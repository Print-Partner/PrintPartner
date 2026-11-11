//! Detached experiments. The writer owns the whole copy transaction.
use super::*;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ExperimentBuild {
    pub build_id: i64,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ExperimentCopy {
    pub build_id: i64,
    pub parent_build_id: i64,
    pub draft_id: Option<i64>,
}

pub(crate) fn children(tx: &Transaction<'_>, build: i64) -> Result<Vec<ExperimentBuild>> {
    Ok(tx.prepare(
        "SELECT b.id,b.name FROM build_experiments e JOIN build_profiles b ON b.id=e.build_id WHERE e.parent_build_id=?1 ORDER BY b.id",
    )?.query_map([build], |r| Ok(ExperimentBuild { build_id:r.get(0)?, name:r.get(1)? }))?
      .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn duplicate(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
    name: &BuildName,
) -> Result<BuildOutcome> {
    let parent = build.get() as i64;
    if !build_exists(tx, tenant, parent)? {
        return Ok(BuildOutcome::MissingBuild);
    }
    if header::name_taken(tx, tenant, name.as_str(), -1)? {
        return Ok(BuildOutcome::RenameTaken {
            name: name.as_str().to_owned(),
        });
    }
    // No accepted pointers, order number, Checkoff, Plate or send state is copied.
    tx.execute(
        "INSERT INTO build_profiles(tenant_id,name,special_request,config_modified_at,last_used_at) SELECT tenant_id,?1,special_request,?2,?2 FROM build_profiles WHERE tenant_id=?3 AND id=?4",
        params![name.as_str(), auth::catalog_timestamp(), tenant, parent])?;
    let build = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO build_experiments(build_id,parent_build_id) VALUES(?1,?2)",
        params![build, parent],
    )?;
    tx.execute(
        "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) SELECT tenant_id,?1,layer_order,layer_type,project_id FROM profile_layers WHERE tenant_id=?2 AND profile_id=?3 ORDER BY layer_order,id",
        params![build,tenant,parent])?;
    tx.execute(
        "INSERT INTO build_profile_details(profile_id,tenant_id,customer,due_date,updated_at) SELECT ?1,tenant_id,customer,due_date,?2 FROM build_profile_details WHERE tenant_id=?3 AND profile_id=?4",
        params![build,auth::catalog_timestamp(),tenant,parent])?;
    for prefix in ["kit_manifest_", "feature_toggles:plan:", "role_filaments_"] {
        tx.execute(
            "INSERT INTO app_settings(tenant_id,key,value) SELECT tenant_id,?1,value FROM app_settings WHERE tenant_id=?2 AND key=?3",
            params![format!("{prefix}{build}"),tenant,format!("{prefix}{parent}")])?;
    }
    let draft_id = crate::working_drafts::experiment::copy(tx, tenant, parent, build)?;
    Ok(BuildOutcome::Duplicated(ExperimentCopy {
        build_id: build,
        parent_build_id: parent,
        draft_id,
    }))
}
