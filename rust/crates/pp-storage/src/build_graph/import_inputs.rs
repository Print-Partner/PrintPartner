//! Attach Library Sources to a Build's planning brief as evidence (GRE-1300
//! part 2), the operation behind MCP `propose_import_build_inputs` for
//! `source_id` inputs.
//!
//! [`BuildGraphClient::import_inputs`] runs in one write transaction on the
//! write seam: prove the credential, read the Build's planning brief, check
//! every input, put one evidence item per Source in the brief, clear its
//! selected draft and store it. A Source is only read. The Build's layers,
//! drafts and Accepted Plan are not touched.
//! [`BuildGraphClient::check_import_inputs`] runs the same checks on the read
//! seam and writes nothing.
//!
//! An item is keyed by its normalized URL, `printpartner:source:<id>`, as
//! Node keys it, so importing a Source again updates its one item in place.
//! Unlike Node, that update keeps the item's `source_role`.
//!
//! Errors are [`ImportInputsError`]. This module holds no status codes and
//! no response text; `pp-api` maps the variants.

use super::{
    BuildGraphClient,
    create_with_brief::{BriefError, store_changed_brief, stored_brief},
};
use crate::{
    WriterFailure, catalog,
    read_model::{
        Credential,
        planning::{
            BuildEvidence, BuildPlanningBrief, EvidenceInputKind, EvidenceKind, SyncStatus,
        },
    },
    write::authorize,
};
use pp_domain::{PositiveId, build_identity::trim_js};
use rusqlite::Transaction;
use sha2::{Digest, Sha256};
use std::{sync::atomic::AtomicBool, time::Duration};

/// One Library Source to attach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceInput {
    pub source_id: PositiveId,
    /// `model_source` when absent.
    pub kind: Option<EvidenceKind>,
    /// The Source's name when absent.
    pub title: Option<String>,
    /// Stored trimmed, without blanks or repeats.
    pub filenames: Vec<String>,
    /// Evidence already in the brief, for example the model page the files
    /// came from. Trimmed; blank means none.
    pub derived_from_evidence_id: Option<String>,
}

/// An evidence item an input put in the brief, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    pub evidence_id: String,
    pub kind: EvidenceKind,
    pub source_id: i64,
    pub url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportInputsError {
    #[error("the Build has no planning brief")]
    NoBrief,
    /// The stored brief does not parse or names another Build.
    #[error("the stored planning brief is invalid")]
    BriefInvalid,
    #[error("no Library Source has this id")]
    SourceNotFound(u64),
    /// No evidence item in the brief, before this import, has this id.
    #[error("unknown derived-from evidence id")]
    DerivedEvidenceNotFound(String),
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

impl From<rusqlite::Error> for ImportInputsError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Writer(error.into())
    }
}

impl BriefError for ImportInputsError {
    fn no_brief() -> Self {
        Self::NoBrief
    }
    fn invalid() -> Self {
        Self::BriefInvalid
    }
}

impl BuildGraphClient {
    /// Runs every check of [`Self::import_inputs`] in a read transaction.
    /// `Ok` is no promise: confirm checks again.
    pub fn check_import_inputs(
        &self,
        credential: Credential,
        build: PositiveId,
        inputs: Vec<SourceInput>,
        wait: Duration,
    ) -> Result<(), ImportInputsError> {
        let policy = self.policy;
        let name = "build_graph.check_import_inputs";
        self.settings
            .read(name, &AtomicBool::new(false), wait, move |tx| {
                let installation = authorize(tx, &credential, policy)?;
                prepare(tx, installation.id(), build, &inputs).map(drop)
            })?
    }

    /// Puts one evidence item per input in `build`'s brief and clears its
    /// selected draft, in one transaction. Answers the items in input order.
    pub fn import_inputs(
        &self,
        credential: Credential,
        build: PositiveId,
        inputs: Vec<SourceInput>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Vec<Imported>, ImportInputsError> {
        let policy = self.policy;
        let name = "build_graph.import_inputs";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let tenant = installation.id();
            let (mut brief, items) = prepare(tx, tenant, build, &inputs)?;
            let imported = items.iter().map(Imported::of).collect();
            for item in items {
                upsert(&mut brief, item);
            }
            store_changed_brief(tx, tenant, build, brief)?;
            Ok(imported)
        })?
    }
}

impl Imported {
    fn of(item: &BuildEvidence) -> Self {
        Self {
            evidence_id: item.id.clone(),
            kind: item.kind,
            source_id: item.source_id.unwrap_or_default(),
            url: item.url.clone(),
        }
    }
}

/// The brief and the item each input makes, after every check. Derived-from
/// ids are checked against the brief as stored, before this import.
fn prepare(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
    inputs: &[SourceInput],
) -> Result<(BuildPlanningBrief, Vec<BuildEvidence>), ImportInputsError> {
    let brief = stored_brief::<ImportInputsError>(tx, tenant, build)?;
    let mut items = Vec::with_capacity(inputs.len());
    for input in inputs {
        let missing = ImportInputsError::SourceNotFound(input.source_id.get());
        let Ok(id) = i64::try_from(input.source_id.get()) else {
            return Err(missing);
        };
        let source = catalog::get(tx, tenant, id)
            .map_err(WriterFailure::from)?
            .ok_or(missing)?;
        let derived = input
            .derived_from_evidence_id
            .as_deref()
            .map(trim_js)
            .filter(|id| !id.is_empty());
        if let Some(derived) = derived
            && !brief.evidence.iter().any(|item| item.id == derived)
        {
            return Err(ImportInputsError::DerivedEvidenceNotFound(derived.into()));
        }
        items.push(evidence(id, &source.name, input, derived));
    }
    Ok((brief, items))
}

/// The item Node's `buildEvidenceFromUploadedSource` makes for Source `id`.
fn evidence(id: i64, name: &str, input: &SourceInput, derived: Option<&str>) -> BuildEvidence {
    let url = format!("printpartner:source:{id}");
    let mut filenames: Vec<String> = Vec::new();
    for name in input.filenames.iter().map(|name| trim_js(name)) {
        if !name.is_empty() && !filenames.iter().any(|kept| kept == name) {
            filenames.push(name.to_owned());
        }
    }
    BuildEvidence {
        id: evidence_id(&url),
        normalized_url: url.clone(),
        url,
        kind: input.kind.unwrap_or(EvidenceKind::ModelSource),
        input_kind: Some(EvidenceInputKind::Upload),
        filenames: Some(filenames),
        artifacts: None,
        upload_required: None,
        derived_from_evidence_id: derived.map(str::to_owned),
        source_id: Some(id),
        source_role: None,
        title: Some(input.title.clone().unwrap_or_else(|| name.to_owned())),
        extract: None,
        retrieved_at: None,
        content_hash: None,
        sync_status: Some(SyncStatus::Pending),
        pinned_revision: None,
    }
}

/// Node's evidence id: the first 20 hex digits of the SHA-256 of the
/// normalized URL. Stored data, so it must match Node's.
fn evidence_id(normalized_url: &str) -> String {
    let mut id = hex::encode(Sha256::digest(normalized_url.as_bytes()));
    id.truncate(20);
    id
}

/// Replaces the item with `item`'s normalized URL in place, keeping its
/// `source_role`, or appends `item`.
fn upsert(brief: &mut BuildPlanningBrief, mut item: BuildEvidence) {
    match brief
        .evidence
        .iter_mut()
        .find(|kept| kept.normalized_url == item.normalized_url)
    {
        Some(kept) => {
            item.source_role = kept.source_role;
            *kept = item;
        }
        None => brief.evidence.push(item),
    }
}

#[cfg(test)]
mod tests;
