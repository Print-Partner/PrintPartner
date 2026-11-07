//! Node's PATCH /plans/{id} and POST /plans/{id}/archive (GRE-421 part 2):
//! a direct edit of the Build header, outside any draft.
//!
//! Like Node, each step writes as it goes: a rename saved before an archive
//! refusal stays saved.

use super::{AcceptedProgress, AcceptedProgressUnavailable, BuildOutcome, ProfileSummary};
use crate::production::model::trim_ecmascript;
use crate::{auth, read_model};
use anyhow::{Result, anyhow};
use pp_domain::{PositiveId, features::Feature};
use rusqlite::{Transaction, params};

/// The fields Node's PATCH reads. `special_request` is `Some(None)` for
/// `null` and `Some(Some(text))` for JavaScript's `String(value)`.
#[derive(Clone, Debug, Default)]
pub struct BuildPatch {
    pub name: Option<String>,
    pub special_request: Option<Option<String>>,
    pub archive: bool,
    pub touch: bool,
}

/// Why Node refuses to archive a Build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveRefusal {
    /// Units still to print, or none at all: "Archive only when print remaining is 0".
    Remaining,
    CompatibilityDirty,
    Uninitialized,
    Integrity,
}

/// Whether another Build of `tenant` already has `name`: the rule a Build
/// rename and Accept's header copy share (`duplicate_build_name`).
pub(crate) fn name_taken(
    tx: &Transaction<'_>,
    tenant: &str,
    name: &str,
    build: i64,
) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM build_profiles WHERE tenant_id=?1 AND name=?2 AND id<>?3)",
        params![tenant, name, build],
        |row| row.get(0),
    )?)
}

fn summary(
    tx: &Transaction<'_>,
    tenant: &str,
    build: i64,
    repos: &std::path::Path,
) -> Result<Option<ProfileSummary>> {
    read_model::graph::build_summary(
        tx,
        tenant,
        build,
        repos,
        &mut read_model::graph::Budget::default(),
    )
}

/// The legacy `/api/v1` contract refuses a Build whose accepted progress is
/// unavailable before anything is written; the caller presents that refusal.
fn legacy_refused(profile: &ProfileSummary, legacy: bool) -> bool {
    legacy && matches!(profile.accepted_progress, AcceptedProgress::Unavailable(_))
}

pub(super) fn update(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
    patch: &BuildPatch,
    legacy: bool,
    repos: &std::path::Path,
) -> Result<BuildOutcome> {
    let id = build.get() as i64;
    let Some(profile) = summary(tx, tenant, id, repos)? else {
        return Ok(BuildOutcome::MissingBuild);
    };
    if legacy_refused(&profile, legacy) {
        return Ok(BuildOutcome::Read {
            profile: Box::new(profile),
        });
    }
    if patch.special_request.is_some() {
        let Ok(view) = crate::features::resolve_for_tenant(tx, tenant, Some(build)) else {
            return Ok(BuildOutcome::FeatureTogglesInvalid);
        };
        if !view.effective().enabled(Feature::Orders) {
            return Ok(BuildOutcome::FeatureDisabled { feature: "orders" });
        }
    }
    if let Some(name) = &patch.name {
        let name = trim_ecmascript(name);
        if name.is_empty() {
            return Ok(BuildOutcome::NameRequired);
        }
        if name_taken(tx, tenant, name, id)? {
            return Ok(BuildOutcome::RenameTaken {
                name: name.to_owned(),
            });
        }
        tx.execute(
            "UPDATE build_profiles SET name=?1 WHERE tenant_id=?2 AND id=?3",
            params![name, tenant, id],
        )?;
    }
    if let Some(request) = &patch.special_request {
        let request = request
            .as_deref()
            .map(trim_ecmascript)
            .filter(|text| !text.is_empty());
        tx.execute(
            "UPDATE build_profiles SET special_request=?1 WHERE tenant_id=?2 AND id=?3",
            params![request, tenant, id],
        )?;
    }
    if patch.archive
        && let Err(refusal) = archive_in(tx, tenant, id, &profile)?
    {
        return Ok(BuildOutcome::ArchiveRefused(refusal));
    }
    if patch.touch {
        tx.execute(
            "UPDATE build_profiles SET last_used_at=?1 WHERE tenant_id=?2 AND id=?3",
            params![auth::catalog_timestamp(), tenant, id],
        )?;
    }
    updated(tx, tenant, id, repos)
}

pub(super) fn archive(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
    legacy: bool,
    repos: &std::path::Path,
) -> Result<BuildOutcome> {
    let id = build.get() as i64;
    let Some(profile) = summary(tx, tenant, id, repos)? else {
        return Ok(BuildOutcome::MissingBuild);
    };
    if legacy_refused(&profile, legacy) {
        return Ok(BuildOutcome::Read {
            profile: Box::new(profile),
        });
    }
    if let Err(refusal) = archive_in(tx, tenant, id, &profile)? {
        return Ok(BuildOutcome::ArchiveRefused(refusal));
    }
    updated(tx, tenant, id, repos)
}

fn updated(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    repos: &std::path::Path,
) -> Result<BuildOutcome> {
    let profile =
        summary(tx, tenant, id, repos)?.ok_or_else(|| anyhow!("Updated Build missing"))?;
    Ok(BuildOutcome::Touched {
        profile: Box::new(profile),
    })
}

/// Node's `archiveAcceptedPlan`: an archived Build stays as it is; otherwise
/// every unit of its accepted Plan must be printed.
fn archive_in(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    profile: &ProfileSummary,
) -> Result<std::result::Result<(), ArchiveRefusal>> {
    if profile.archived_at.is_some() {
        return Ok(Ok(()));
    }
    match profile.accepted_progress {
        AcceptedProgress::Ready {
            total_units,
            remaining_units: 0,
        } if total_units > 0 => {}
        AcceptedProgress::Ready { .. } | AcceptedProgress::Empty => {
            return Ok(Err(ArchiveRefusal::Remaining));
        }
        AcceptedProgress::Unavailable(AcceptedProgressUnavailable::CompatibilityDirty) => {
            return Ok(Err(ArchiveRefusal::CompatibilityDirty));
        }
        AcceptedProgress::Unavailable(AcceptedProgressUnavailable::Uninitialized) => {
            return Ok(Err(ArchiveRefusal::Uninitialized));
        }
        AcceptedProgress::Unavailable(AcceptedProgressUnavailable::Integrity) => {
            return Ok(Err(ArchiveRefusal::Integrity));
        }
    }
    tx.execute(
        "UPDATE build_profiles SET archived_at=?1 WHERE tenant_id=?2 AND id=?3",
        params![auth::catalog_timestamp(), tenant, id],
    )?;
    Ok(Ok(()))
}
