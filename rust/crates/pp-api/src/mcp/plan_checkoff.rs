//! Checkoff reads the Accepted Plan and confirms through the manual burst owner.

use super::{
    checkoff::{CheckoffDomain, KEY_PREFIX},
    positive_id,
};
use pp_services::mcp_domain::{ProposedAction, ToolInvocation};
use pp_storage::{
    checkoff_progress::{
        Basis, Body, Request, Response,
        wire::{ManualBurstRequest, ManualBurstUnit},
    },
    read_model::{AcceptedRead, Snapshot},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};

const READ: &str = "get_plan_checkoff";
const PROGRESS: &str = "propose_set_plan_progress";
const ASSEMBLY: &str = "propose_set_plan_assembly";
pub(super) const TOOLS: [&str; 3] = [READ, PROGRESS, ASSEMBLY];
const UNAVAILABLE: &str = "Plan Checkoff is unavailable";

pub(super) fn is_proposal(name: &str) -> bool {
    matches!(name, PROGRESS | ASSEMBLY)
}

pub(super) fn describe_inputs(catalog: &mut Value) {
    if let Some(tools) = catalog["tools"].as_array_mut() {
        for tool in tools {
            if tool["name"].as_str().is_some_and(is_proposal) {
                tool["inputSchema"]["properties"]["expected"] = json!({
                    "type": "object",
                    "description": "Pass accepted_basis from get_plan_checkoff to refuse coordinates read from an older Accepted Plan."
                });
            }
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Row {
    part_id: i64,
    printed_count: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Assembly {
    part_id: i64,
    unit_index: i64,
    assembled: bool,
}

#[derive(Serialize, Deserialize)]
struct Burst {
    expected: Basis,
    units: Vec<ManualBurstUnit>,
    review: Value,
}

impl CheckoffDomain {
    pub(super) fn plan_checkoff(
        &self,
        name: &str,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let id = positive_id(&Value::Object(args.clone()), "plan_id")
            .ok_or("plan_id must be a positive whole number")?
            .get() as i64;
        let credential = self.caller()?.into_credential();
        let cancel = AtomicBool::new(false);
        let mut batch = self
            .accepted
            .read(credential, &[id], &cancel, self.wait)
            .map_err(|_| UNAVAILABLE)?;
        let accepted = batch.builds.pop().ok_or("Plan not found")?.accepted;
        let snapshot = match &accepted {
            AcceptedRead::Ready { snapshot } => Some(snapshot.as_ref()),
            AcceptedRead::Empty { .. } => None,
            AcceptedRead::Missing => return Err("Plan not found".into()),
            _ => return Err(UNAVAILABLE.into()),
        };
        if name == READ {
            return Ok(ToolInvocation {
                content: checkoff_state(id, snapshot).to_string(),
                proposed_action: None,
            });
        }

        let snapshot = snapshot.ok_or("This Build has no Accepted Plan")?;
        if snapshot
            .profile
            .archived_at
            .as_deref()
            .is_some_and(|at| !at.is_empty())
        {
            return Err("This Build is archived".into());
        }
        let mut fields = args.clone();
        fields.remove("plan_id");
        let action = proposal(name, id, snapshot, fields)?;
        Ok(ToolInvocation {
            content: action.proposed_content(),
            proposed_action: Some(action),
        })
    }

    pub(super) fn apply_plan_checkoff(&self, action: &ProposedAction) -> Result<Value, String> {
        let Burst {
            expected, units, ..
        } = serde_json::from_value(Value::Object(action.params.clone()))
            .map_err(|_| UNAVAILABLE)?;
        let body = serde_json::to_value(ManualBurstRequest {
            idempotency_key: format!("{KEY_PREFIX}{}", action.id),
            expected,
            units,
        })
        .map_err(|_| UNAVAILABLE)?;
        Ok(
            match self.run(Request::ManualBurst {
                profile_id: action.plan_id,
                body,
            })? {
                Response {
                    status: 200,
                    body: Body::Receipt(receipt),
                } => {
                    json!({ "ok": true, "result": receipt })
                }
                Response {
                    status,
                    body: Body::Failure { detail, code },
                } if status < 500 => {
                    json!({ "ok": false, "status": status, "detail": detail, "code": code })
                }
                _ => return Err(UNAVAILABLE.into()),
            },
        )
    }
}

fn checkoff_state(id: i64, snapshot: Option<&Snapshot>) -> Value {
    let Some(snapshot) = snapshot else {
        return json!({"plan_id": id, "state": "empty", "parts": []});
    };
    let (mut total, mut printed) = (0, 0);
    let parts: Vec<_> = snapshot
        .parts
        .iter()
        .filter(|part| part.included)
        .map(|part| {
            let printed_count = part.units.iter().filter(|unit| unit.completed).count();
            total += part.units.len();
            printed += printed_count;
            let units: Vec<_> = part.units.iter().map(|unit| json!({
            "unit_index": unit.unit_index, "token": unit.token, "object_name": unit.object_name,
            "required": unit.required, "completed": unit.completed, "assembled": unit.assembled,
        })).collect();
            json!({
                "part_id": part.projection_part_id, "filename": part.filename,
                "relative_path": part.relative_path, "role": part.effective_role,
                "quantity_effective": part.quantity_effective, "printed_count": printed_count,
                "assembled_count": part.units.iter().filter(|unit| unit.assembled).count(),
                "missing": printed_count < units.len(), "units": units,
            })
        })
        .collect();
    json!({
        "plan_id": id, "plan_name": snapshot.profile.name, "state": "ready",
        "accepted_basis": Basis::from(snapshot), "parts": parts,
        "printed_units": printed, "total_units": total, "remaining_units": total - printed,
    })
}

fn proposal(
    name: &str,
    id: i64,
    snapshot: &Snapshot,
    mut fields: Map<String, Value>,
) -> Result<ProposedAction, String> {
    let expected = Basis::from(snapshot);
    if let Some(observed) = fields.remove("expected")
        && serde_json::from_value::<Basis>(observed).as_ref().ok() != Some(&expected)
    {
        return Err("Accepted Plan changed; read Checkoff again".into());
    }
    let (label, summary, units, review) = if name == PROGRESS {
        let rows = fields.remove("rows").ok_or("rows required")?;
        if !fields.is_empty() {
            return Err("Unexpected printed-count arguments".into());
        }
        let rows: Vec<Row> =
            serde_json::from_value(rows).map_err(|_| "Invalid printed-count rows")?;
        (
            "Update Plan print progress",
            "Set the reviewed printed counts.",
            printed_targets(snapshot, &rows)?,
            json!({ "rows": rows }),
        )
    } else {
        let target =
            serde_json::from_value(Value::Object(fields)).map_err(|_| "Invalid assembly target")?;
        (
            "Update Plan assembly checkoff",
            "Set the reviewed Required unit's assembly state.",
            vec![assembly_target(snapshot, &target)?],
            json!(target),
        )
    };
    let Value::Object(params) = serde_json::to_value(Burst {
        expected,
        units,
        review,
    })
    .map_err(|_| UNAVAILABLE)?
    else {
        return Err(UNAVAILABLE.into());
    };
    Ok(ProposedAction::new(name, id, label.into(), summary, params))
}

fn printed_targets(
    snapshot: &Snapshot,
    rows: &[Row],
) -> Result<Vec<ManualBurstUnit>, &'static str> {
    if rows.is_empty() || rows.len() > 1000 {
        return Err("rows must list 1 to 1000 Parts");
    }
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for &Row {
        part_id,
        printed_count,
    } in rows
    {
        let part = snapshot
            .parts
            .iter()
            .find(|part| part.included && part.projection_part_id == part_id);
        let Some(part) =
            part.filter(|part| !part.units.is_empty() && printed_count <= part.units.len())
        else {
            return Err("A Part is missing or its printed count is out of range");
        };
        if !seen.insert(part_id) {
            return Err("rows repeats a Part");
        }
        // Set both sides of the prefix so the count is exact, even when
        // another Checkoff burst changed progress after this proposal.
        if let Some(index) = printed_count.checked_sub(1) {
            targets.push(printed_target(&part.units[index].token, true));
        }
        if let Some(unit) = part.units.get(printed_count) {
            targets.push(printed_target(&unit.token, false));
        }
    }
    if targets.len() > 1000 {
        return Err("A Checkoff burst may name at most 1000 Required units");
    }
    Ok(targets)
}

fn printed_target(token: &str, printed: bool) -> ManualBurstUnit {
    ManualBurstUnit {
        token: token.to_owned(),
        printed: Some(printed),
        assembled: None,
    }
}

fn assembly_target(
    snapshot: &Snapshot,
    target: &Assembly,
) -> Result<ManualBurstUnit, &'static str> {
    let unit = snapshot
        .parts
        .iter()
        .find(|part| part.included && part.projection_part_id == target.part_id)
        .and_then(|part| {
            part.units
                .iter()
                .find(|unit| unit.unit_index == target.unit_index)
        })
        .ok_or("part_id and unit_index do not name a Required unit on the Accepted Plan")?;
    if target.assembled && !unit.completed {
        return Err("An unprinted unit cannot be assembled");
    }
    Ok(ManualBurstUnit {
        token: unit.token.clone(),
        printed: None,
        assembled: Some(target.assembled),
    })
}
