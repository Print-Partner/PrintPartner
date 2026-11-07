//! The create path of Node's `materializeAcceptedPrinterLink`
//! (`db/repository.ts`) for `POST /printer-checkoff/file-assignments`
//! (`routes/printer-checkoff.ts`), behind `desktop_rebuild` (GRE-1217).
//!
//! The route reads the file, decides what it is and parses the request; this
//! records it. Against the Build's Accepted Plan at the revision the operator
//! saw, the confirmed Required units must all be incomplete Required units, the
//! optional object mappings must be valid, and no open link may already watch
//! that file on that integration. The link is created watching, or awaiting
//! verify when the print already happened (`completed`), in one writer
//! transaction. Node patches the completed link in a second transaction; here
//! it is one write, so a completed print can never be left watching.

use super::{
    Body, Outcome, Response,
    attribution::{self, Attribution, Mapping, Unit},
    links,
};
use anyhow::anyhow;
use rusqlite::Transaction;
use serde_json::{Value, json};
use std::path::Path;

/// The printer link a print-file assignment creates.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignedLink {
    pub integration_id: String,
    pub printer_id: String,
    pub host_name: String,
    pub filename: String,
    pub remote_path: Option<String>,
    /// The provider identity of `remote_path`, with the SHA-256 of the bytes.
    pub remote_identity: Option<Value>,
    /// What the bytes classified as, never what a client claimed.
    pub classification: Option<Value>,
}

/// A parsed `POST /printer-checkoff/file-assignments`.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub profile_id: i64,
    pub plan_revision_id: i64,
    pub object_names: Vec<String>,
    /// The Required units the operator confirmed the file produces.
    pub units: Vec<Unit>,
    pub mappings: Option<Vec<Mapping>>,
    pub retain_unmatched: bool,
    /// The print already happened: the link awaits verify instead of watching.
    pub completed: bool,
    pub link: AssignedLink,
}

fn refuse(detail: &str) -> Outcome {
    Ok(Ok(Response::failure(409, detail, None)))
}

const NO_MATCH: &str = "That print file does not map to an incomplete Required unit in this Build";

/// The extras an assignment that keeps unmatched objects records: every
/// object that bound no unit and is not a repeat, or the file itself when
/// there were no objects and no units.
fn retained(attribution: &Attribution, assignment: &Assignment) -> Value {
    let mut extras: Vec<Value> = attribution
        .outcomes
        .iter()
        .filter(|outcome| outcome.unit.is_none() && outcome.kind != "duplicate_observation")
        .map(|outcome| json!({ "name": outcome.raw_name, "kind": "object", "checkoff": { "result": "pending" } }))
        .collect();
    if assignment.object_names.is_empty() && attribution.units.is_empty() {
        extras.push(json!({ "name": assignment.link.filename, "kind": "file", "checkoff": { "result": "pending" } }));
    }
    json!({ "extras": extras })
}

pub(super) fn assign(
    tx: &Transaction<'_>,
    tenant: &str,
    assignment: Assignment,
    repos: &Path,
) -> Outcome {
    let snapshot = match attribution::accepted(tx, tenant, assignment.profile_id, repos)? {
        Ok(snapshot) => snapshot,
        Err(refusal) => return Ok(refusal),
    };
    if snapshot.revision_id != assignment.plan_revision_id {
        return refuse("The Accepted Plan moved on; reload the Build and choose the file again");
    }
    let suggestion = attribution::resolve(
        &snapshot,
        &assignment.object_names,
        None,
        assignment.mappings.is_some(),
    );
    let Some(confirmed) = attribution::confirm(&snapshot, &assignment.units) else {
        return refuse(NO_MATCH);
    };
    let attribution = match &assignment.mappings {
        Some(mappings) => match attribution::apply_mappings(&suggestion, &confirmed, mappings) {
            Some(mapped) => mapped,
            None => return refuse(NO_MATCH),
        },
        None => Attribution {
            units: confirmed,
            fallback: "unused",
            ..suggestion
        },
    };

    let now = links::now();
    let link = &assignment.link;
    let filename = links::normalized_filename(&link.filename);
    let mut all = links::load(tx, tenant, &now)?;
    if all.iter().any(|open| {
        matches!(open["state"].as_str(), Some("watching" | "awaiting_verify"))
            && open["integration_id"] == link.integration_id.as_str()
            && links::normalized_filename(open["filename"].as_str().unwrap_or_default()) == filename
    }) {
        return refuse("That print file is already assigned");
    }
    let mut raw = json!({
        "id": links::uuid(),
        "profile_id": assignment.profile_id,
        "integration_id": link.integration_id,
        "printer_id": link.printer_id,
        "host_name": link.host_name,
        "filename": link.filename,
        "remote_path": link.remote_path,
        "plan_revision_id": snapshot.revision_id,
        "remote_identity": link.remote_identity,
        "classification": link.classification,
        "units": attribution.units,
        "unlabeled_names": attribution.unmatched_object_names,
        "state": "watching",
        "host_outcome": "unknown",
        "saw_active": false,
        "started": false,
        "created_at": now,
    });
    if assignment.retain_unmatched {
        raw["imported_inventory"] = retained(&attribution, &assignment);
    }
    if assignment.completed {
        for (key, value) in [
            ("state", json!("awaiting_verify")),
            ("host_outcome", json!("success")),
            ("saw_active", json!(true)),
            ("last_progress", json!(100)),
            ("completed_at", json!(now)),
        ] {
            raw[key] = value;
        }
    }
    let created =
        links::parse(&raw, &now).ok_or_else(|| anyhow!("Accepted printer link creation failed"))?;
    let public = links::public(&created);
    all.push(created);
    links::save_all(tx, tenant, all)?;
    Ok(Ok(Response::ok(Body::Assigned {
        link: public,
        attribution: Box::new(serde_json::to_value(&attribution)?),
    })))
}
