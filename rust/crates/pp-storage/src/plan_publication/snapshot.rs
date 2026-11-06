//! The snapshot Accept saves of the Accepted Plan it is about to replace, so
//! History can bring back that Plan's Source layers. The payload has the shape
//! of Node's `capturePlanSnapshotPayload` (`services/plan-snapshots.ts`), which
//! Node's restore reads. Its layers come from the replaced revision's recorded
//! inputs, not from `profile_layers`: Accept publishes the working layers, so
//! they already equal the Plan being accepted. Accepted revisions record no kit
//! manifest and no Source tag or branch, so those are `null` and a restore
//! keeps the current ones.

use super::*;

/// Inserts one `pre_apply` `plan_snapshots` row for Build `profile` inside
/// Accept's transaction, before the publication writes. The first Accept, and
/// a replaced revision with no recorded inputs, have nothing to restore and
/// save none.
pub(super) fn save_before_accept(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft_id: i64,
    accepted: (Option<i64>, i64),
    at: &str,
) -> Result<()> {
    let (Some(revision), version) = accepted else {
        return Ok(());
    };
    let layers = tx
        .prepare(
            "SELECT input.source_layer,input.source_id,source.name,input.layer_order FROM plan_revisions revision JOIN plan_revision_inputs input ON input.tenant_id=revision.tenant_id AND input.input_set_id=revision.input_set_id JOIN projects source ON source.tenant_id=input.tenant_id AND source.id=input.source_id WHERE revision.tenant_id=?1 AND revision.profile_id=?2 AND revision.id=?3 ORDER BY input.layer_order,input.source_id",
        )?
        .query_map(params![tenant, profile, revision], |row| {
            let layer: String = row.get(0)?;
            let layer_type = layer.split_once(':').map_or(layer.as_str(), |(kind, _)| kind);
            Ok(json!({
                "layer_type": layer_type,
                "project_id": row.get::<_, i64>(1)?,
                "source_name": row.get::<_, String>(2)?,
                "tag": null,
                "branch": null,
                "order": row.get::<_, i64>(3)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if layers.is_empty() {
        return Ok(());
    }
    let payload = json!({
        "layers": layers,
        "kit_manifest": null,
        "recipe_summary": null,
        "captured_at": at,
        "accepted_plan_revision_id": revision,
        "accepted_plan_version": version,
    });
    tx.execute(
        "INSERT INTO plan_snapshots(tenant_id,profile_id,name,created_at,source,payload_json) VALUES(?1,?2,?3,?4,'pre_apply',?5)",
        params![
            tenant,
            profile,
            format!("Before accepting draft {draft_id}"),
            at,
            payload.to_string()
        ],
    )?;
    Ok(())
}
