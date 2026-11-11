//! Copy Accepted choices into a detached draft with no predecessor links.
use super::*;

pub(crate) fn copy(
    tx: &Transaction<'_>,
    tenant: &str,
    source: i64,
    target: i64,
) -> Result<Option<i64>> {
    let revision: Option<i64> = tx.query_row(
        "SELECT accepted_plan_revision_id FROM build_profiles WHERE tenant_id=?1 AND id=?2",
        params![tenant, source],
        |row| row.get(0),
    )?;
    let Some(revision) = revision else {
        return Ok(None);
    };
    let inputs = rows(
        tx,
        "SELECT i.* FROM plan_revisions r JOIN plan_revision_inputs i ON i.input_set_id=r.input_set_id AND i.tenant_id=r.tenant_id WHERE r.tenant_id=? AND r.profile_id=? AND r.id=? ORDER BY i.layer_order,i.source_id",
        &[&tenant, &source, &revision],
    )?;
    let parts = crate::plan_publication::validate_projection(tx, tenant, source, revision)?;
    let parts: Vec<_> = parts
        .into_iter()
        .map(|stored| {
            let mut part = stored.projected(model::PART_FIELDS);
            part["baseRevisionPartId"] = Value::Null;
            part
        })
        .collect();
    let base = json!({"baseRevisionId":null,"basePlanVersion":0});
    let digest = model::planning(&base, &inputs, &parts)?;
    insert_snapshot(
        tx,
        tenant,
        "system:experiment",
        target,
        "experiment-copy",
        DraftRows {
            base: &base,
            inputs: &inputs,
            parts: &parts,
            digest: &digest,
        },
        Origin::New,
    )
    .map(Some)
}
