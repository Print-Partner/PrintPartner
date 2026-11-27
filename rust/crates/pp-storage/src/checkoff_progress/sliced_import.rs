//! An imported observation creates a link awaiting verification, not progress.

use super::{
    AssignedLink, Assignment, Basis, ProgressClient, archived, assign,
    attribution::{self, Attribution},
    is_token, links, read,
};
use crate::{
    WriterFailure,
    read_model::{AcceptedRead, Credential, Snapshot},
    write::authorize,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, sync::atomic::AtomicBool, time::Duration};

#[derive(Debug, Deserialize, Serialize)]
pub struct ObservedObject {
    pub id: u32,
    pub name: String,
    pub part_number: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Import {
    pub expected: Basis,
    pub objects: Vec<ObservedObject>,
    pub reviewed: Vec<Option<(i64, i64)>>,
    pub filename: String,
    pub integration_id: String,
    pub printer_id: String,
    pub host_name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("Accepted Plan changed; preview the 3MF again")]
    Changed,
    #[error("This Build is archived")]
    Archived,
    #[error("A 3MF object carries a conflicting or stale Required-unit token")]
    StaleToken,
    #[error("3MF objects did not match incomplete Accepted Plan units")]
    NoMatch,
    #[error("That print file is already assigned")]
    Assigned,
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

fn embedded_tokens(value: &str) -> impl Iterator<Item = &str> {
    value.match_indices("ppu_").filter_map(|(start, _)| {
        let end = start + 36;
        let token = value.get(start..end)?;
        (is_token(token)
            && !value
                .as_bytes()
                .get(end)
                .is_some_and(u8::is_ascii_alphanumeric))
        .then_some(token)
    })
}

pub fn preview(snapshot: &Snapshot, objects: &[ObservedObject]) -> Result<Attribution, Refusal> {
    if archived(snapshot) {
        return Err(Refusal::Archived);
    }
    let tokens: HashMap<_, _> = snapshot
        .parts
        .iter()
        .filter(|part| part.included)
        .flat_map(|part| part.units.iter())
        .filter(|unit| unit.required)
        .map(|unit| (unit.token.as_str(), unit.object_name.as_str()))
        .collect();
    let names = objects
        .iter()
        .map(|object| {
            let name = object.name.to_ascii_lowercase();
            let number = object
                .part_number
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let mut candidates = embedded_tokens(&number).chain(embedded_tokens(&name));
            let Some(token) = candidates.next() else {
                return Ok(object.name.clone());
            };
            if candidates.any(|other| other != token) {
                return Err(Refusal::StaleToken);
            }
            tokens
                .get(token)
                .map(|name| (*name).to_owned())
                .ok_or(Refusal::StaleToken)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut attribution = attribution::resolve(snapshot, &names, None, true);
    if attribution.units.is_empty() {
        return Err(Refusal::NoMatch);
    }
    for (outcome, object) in attribution.outcomes.iter_mut().zip(objects) {
        outcome.raw_name.clone_from(&object.name);
    }
    attribution.unmatched_object_names = attribution
        .outcomes
        .iter()
        .filter(|outcome| outcome.unit.is_none())
        .map(|outcome| outcome.raw_name.clone())
        .collect();
    Ok(attribution)
}

pub fn coordinates(attribution: &Attribution) -> Vec<Option<(i64, i64)>> {
    attribution
        .outcomes
        .iter()
        .map(|entry| {
            entry
                .unit
                .as_ref()
                .map(|unit| (unit.part_id, unit.unit_index))
        })
        .collect()
}

pub fn link_id(action_id: &str) -> String {
    format!("mcp-3mf:{action_id}")
}

impl ProgressClient {
    pub fn import_sliced_3mf(
        &self,
        credential: Credential,
        action_id: String,
        import: Import,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Value, Refusal> {
        let (policy, repos) = (self.policy, self.repos.clone());
        self.shared.write(
            "checkoff.import_sliced_3mf",
            cancelled,
            wait,
            move |tx, _| {
                let installation = authorize(tx, &credential, policy)?;
                let tenant = installation.id();
                let id = link_id(&action_id);
                let internal = |error| Refusal::Writer(WriterFailure::from(error));
                if let Some(existing) = links::load(tx, tenant, &links::now())
                    .map_err(internal)?
                    .into_iter()
                    .find(|link| link["id"] == id)
                {
                    return Ok(
                        serde_json::json!({"link": links::public(&existing), "reconciled": true}),
                    );
                }
                let profile_id = import.expected.profile_id;
                let AcceptedRead::Ready { snapshot } =
                    read(tx, tenant, profile_id, &repos).map_err(internal)?
                else {
                    return Err(Refusal::Changed);
                };
                if Basis::from(snapshot.as_ref()) != import.expected {
                    return Err(Refusal::Changed);
                }
                let attribution = preview(&snapshot, &import.objects)?;
                if coordinates(&attribution) != import.reviewed {
                    return Err(Refusal::Changed);
                }
                let assignment = Assignment {
                    profile_id,
                    plan_revision_id: snapshot.revision_id,
                    object_names: import
                        .objects
                        .iter()
                        .map(|object| object.name.clone())
                        .collect(),
                    units: vec![],
                    mappings: None,
                    retain_unmatched: false,
                    completed: true,
                    snapshot_source: None,
                    link: AssignedLink {
                        integration_id: import.integration_id,
                        printer_id: import.printer_id,
                        host_name: import.host_name,
                        filename: import.filename,
                        remote_path: None,
                        remote_identity: None,
                        classification: None,
                    },
                };
                let link = assign::record(tx, tenant, &assignment, &attribution, &id)
                    .map_err(internal)?
                    .ok_or(Refusal::Assigned)?;
                Ok(serde_json::json!({"link": link, "attribution": attribution}))
            },
        )?
    }
}
