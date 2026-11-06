//! The snapshot Accept saves of the Accepted Plan it is about to replace, so
//! History can bring back that Plan's Source layers. The payload has the shape
//! of Node's `capturePlanSnapshotPayload` (`services/plan-snapshots.ts`), which
//! Node's restore reads. Its layers come from the replaced revision's recorded
//! inputs, not from `profile_layers`: Accept publishes the working layers, so
//! they already equal the Plan being accepted. Accepted revisions record no kit
//! manifest and no Source tag or branch, so those are `null` and a restore
//! keeps the current ones.

use super::*;
use rusqlite::OptionalExtension;

/// The Source layers of the Accepted Plan revision `previous` in the payload's
/// `layers` shape, or `None` when there is nothing Node's restore can bring
/// back as recorded: no Accepted Plan, a revision with no input set, an input
/// whose Source row is gone (restore resolves layers by Source name), or an
/// input set migrated by schema v18 whose `legacy:<id>` rows name no layer
/// type (restore would treat each as an addon and drop the base). An input set
/// with no rows is a Plan with no Source layers, and restoring `[]` brings
/// that back. Accept saves no snapshot for `None`, and the preview reports
/// `saved_before_accept` from the same answer.
pub(super) fn replaced_layers(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    previous: Option<i64>,
) -> Result<Option<Vec<Value>>> {
    let Some(revision) = previous else {
        return Ok(None);
    };
    let input_set: Option<i64> = tx
        .query_row(
            "SELECT input_set_id FROM plan_revisions WHERE tenant_id=?1 AND profile_id=?2 AND id=?3",
            params![tenant, profile, revision],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(input_set) = input_set else {
        return Ok(None);
    };
    let layers = tx
        .prepare(
            "SELECT input.source_layer,input.source_id,source.name,input.layer_order FROM plan_revision_inputs input LEFT JOIN projects source ON source.tenant_id=input.tenant_id AND source.id=input.source_id WHERE input.tenant_id=?1 AND input.input_set_id=?2 ORDER BY input.layer_order,input.source_id",
        )?
        .query_map(params![tenant, input_set], |row| {
            let layer: String = row.get(0)?;
            let layer_type = layer.split_once(':').map_or(layer.as_str(), |(kind, _)| kind);
            let project_id: i64 = row.get(1)?;
            let source_name: Option<String> = row.get(2)?;
            let order: i64 = row.get(3)?;
            Ok(source_name
                .filter(|_| matches!(layer_type, "base" | "addon"))
                .map(|name| {
                    json!({
                        "layer_type": layer_type,
                        "project_id": project_id,
                        "source_name": name,
                        "tag": null,
                        "branch": null,
                        "order": order,
                    })
                }))
        })?
        .collect::<rusqlite::Result<Option<Vec<_>>>>()?;
    Ok(layers)
}

/// Inserts one `pre_apply` `plan_snapshots` row for Build `profile` inside
/// Accept's transaction, before the publication writes, when
/// [`replaced_layers`] finds a recorded Plan to restore.
pub(super) fn save_before_accept(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft_id: i64,
    accepted: (Option<i64>, i64),
    at: &str,
) -> Result<()> {
    let (previous, version) = accepted;
    let Some(layers) = replaced_layers(tx, tenant, profile, previous)? else {
        return Ok(());
    };
    let payload = json!({
        "layers": layers,
        "kit_manifest": null,
        "recipe_summary": null,
        "captured_at": at,
        "accepted_plan_revision_id": previous,
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
