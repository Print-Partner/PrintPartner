//! Set the plan-specific role of a Build's evidence (GRE-1300 PR 1), the
//! operation behind MCP `propose_set_build_source_roles`.
//!
//! [`BuildGraphClient::set_source_roles`] runs in one write transaction on
//! the write seam: prove the credential, read the Build's planning brief,
//! set each named evidence item's `source_role`, refuse a second structural
//! base, clear the selected draft and store the brief. A Source's Library
//! role and the Build's layers are not touched.
//! [`BuildGraphClient::check_source_roles`] proves the credential and finds
//! the brief on the read seam, and writes nothing.
//!
//! Errors are [`SourceRolesError`]. This module holds no status codes and no
//! response text; `pp-api` maps the variants.

use super::{
    BuildGraphClient,
    create_with_brief::{BriefError, store_changed_brief, stored_brief},
};
use crate::{
    WriterFailure,
    read_model::{
        Credential,
        planning::{BuildPlanningBrief, SourceRole},
    },
    write::authorize,
};
use pp_domain::{PositiveId, build_identity::trim_js};
use std::{sync::atomic::AtomicBool, time::Duration};

/// One requested role. A later change for the same evidence replaces an
/// earlier one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleChange {
    evidence_id: String,
    role: SourceRole,
}

impl RoleChange {
    /// Trims `evidence_id`, as Node does.
    pub fn new(evidence_id: &str, role: SourceRole) -> Self {
        Self {
            evidence_id: trim_js(evidence_id).to_owned(),
            role,
        }
    }
}

/// An evidence item whose role a change set, as stored after the write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleSet {
    pub evidence_id: String,
    pub source_id: Option<i64>,
    pub role: SourceRole,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceRolesError {
    #[error("the Build has no planning brief")]
    NoBrief,
    /// The stored brief does not parse or names another Build.
    #[error("the stored planning brief is invalid")]
    BriefInvalid,
    /// No evidence item in the brief has this id.
    #[error("unknown evidence id")]
    EvidenceNotFound(String),
    #[error("a Build can have only one structural base")]
    MultipleStructuralBases,
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

impl From<rusqlite::Error> for SourceRolesError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Writer(error.into())
    }
}

impl BriefError for SourceRolesError {
    fn no_brief() -> Self {
        Self::NoBrief
    }
    fn invalid() -> Self {
        Self::BriefInvalid
    }
}

impl BuildGraphClient {
    /// Proves the credential and finds `build`'s brief in a read
    /// transaction. `Ok` is no promise: confirm reads the brief again.
    pub fn check_source_roles(
        &self,
        credential: Credential,
        build: PositiveId,
        wait: Duration,
    ) -> Result<(), SourceRolesError> {
        let policy = self.policy;
        let name = "build_graph.check_source_roles";
        self.settings
            .read(name, &AtomicBool::new(false), wait, move |tx| {
                let installation = authorize(tx, &credential, policy)?;
                stored_brief::<SourceRolesError>(tx, installation.id(), build).map(drop)
            })?
    }

    /// Sets the roles in `build`'s brief and clears its selected draft, in
    /// one transaction. Answers the changed evidence items in brief order.
    pub fn set_source_roles(
        &self,
        credential: Credential,
        build: PositiveId,
        changes: Vec<RoleChange>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Vec<RoleSet>, SourceRolesError> {
        let policy = self.policy;
        let name = "build_graph.set_source_roles";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let tenant = installation.id();
            let mut brief = stored_brief::<SourceRolesError>(tx, tenant, build)?;
            let set = apply(&mut brief, &changes)?;
            store_changed_brief(tx, tenant, build, brief)?;
            Ok(set)
        })?
    }
}

/// Sets each change's role on `brief`'s evidence. Every change must name
/// evidence in the brief, and at most one item may end as the structural
/// base. On `Err` the caller drops `brief`, so nothing is stored.
fn apply(
    brief: &mut BuildPlanningBrief,
    changes: &[RoleChange],
) -> Result<Vec<RoleSet>, SourceRolesError> {
    if let Some(unknown) = changes.iter().find(|change| {
        !brief
            .evidence
            .iter()
            .any(|item| item.id == change.evidence_id)
    }) {
        return Err(SourceRolesError::EvidenceNotFound(
            unknown.evidence_id.clone(),
        ));
    }
    let mut set = Vec::new();
    for item in &mut brief.evidence {
        if let Some(change) = changes
            .iter()
            .rev()
            .find(|change| change.evidence_id == item.id)
        {
            item.source_role = Some(change.role);
            set.push(RoleSet {
                evidence_id: item.id.clone(),
                source_id: item.source_id,
                role: change.role,
            });
        }
    }
    let bases = brief
        .evidence
        .iter()
        .filter(|item| item.source_role == Some(SourceRole::StructuralBase))
        .count();
    if bases > 1 {
        return Err(SourceRolesError::MultipleStructuralBases);
    }
    Ok(set)
}

/// Shared with `import_inputs` tests, which start from the same brief.
#[cfg(test)]
pub(super) mod tests;
