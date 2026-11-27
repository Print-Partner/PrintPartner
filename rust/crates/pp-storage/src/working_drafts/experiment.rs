//! Copy Accepted choices into a detached draft with no predecessor links.
use super::*;

pub(crate) enum Choices {
    NoAcceptedPlan,
    InputsChanged,
    Accepted { revision: i64, inputs: Vec<Value> },
}

pub(crate) fn choices(tx: &Transaction<'_>, tenant: &str, source: i64) -> Result<Choices> {
    let revision: Option<i64> = tx.query_row(
        "SELECT accepted_plan_revision_id FROM build_profiles WHERE tenant_id=?1 AND id=?2",
        params![tenant, source],
        |row| row.get(0),
    )?;
    let Some(revision) = revision else {
        return Ok(Choices::NoAcceptedPlan);
    };
    Ok(
        match super::restore::matching_revision_inputs(tx, tenant, source, revision)? {
            Some(inputs) => Choices::Accepted { revision, inputs },
            None => Choices::InputsChanged,
        },
    )
}

pub(crate) fn copy(
    tx: &Transaction<'_>,
    tenant: &str,
    source: i64,
    target: i64,
    revision: i64,
    inputs: &[Value],
) -> Result<i64> {
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
    let digest = model::planning(&base, inputs, &parts)?;
    insert_snapshot(
        tx,
        tenant,
        "system:experiment",
        target,
        "experiment-copy",
        DraftRows {
            base: &base,
            inputs,
            parts: &parts,
            digest: &digest,
        },
        Origin::New,
    )
}
