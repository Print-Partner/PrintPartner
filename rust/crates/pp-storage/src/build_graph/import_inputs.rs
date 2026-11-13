//! Attach evidence to a Build's planning brief (GRE-1300 parts 2, 3a and
//! 3b), the operation behind MCP `propose_import_build_inputs`: Library
//! Sources by id, and git repositories, model pages and other links by URL.
//!
//! [`BuildGraphClient::import_inputs`] runs in one write transaction on the
//! write seam: prove the credential, read the Build's planning brief, check
//! every input, find or create the Source behind each git URL
//! (`git_source`), put one evidence item per input in the brief, clear its
//! selected draft and store it. Nothing is fetched or synced. The Build's
//! layers, drafts and Accepted Plan are not touched.
//! [`BuildGraphClient::check_import_inputs`] runs the same checks on the read
//! seam and writes nothing.
//!
//! An item is keyed by its normalized URL, as Node keys it: the link's, or
//! `printpartner:source:<id>` for a Source. Importing the same input again
//! updates its one item in place. Unlike Node, that update keeps the item's
//! `source_role`. The caller normalizes a URL; this module stores it.
//!
//! Errors are [`ImportInputsError`]. This module holds no status codes and
//! no response text; `pp-api` maps the variants.

use super::{
    BuildGraphClient,
    create_with_brief::{BriefError, store_changed_brief, stored_brief},
};
use crate::{
    WriterFailure, auth, catalog,
    read_model::{
        Credential,
        planning::{
            BuildEvidence, BuildPlanningBrief, EvidenceInputKind, EvidenceKind, SourceRole,
            SyncStatus,
        },
    },
    write::authorize,
};
use pp_domain::{PositiveId, build_identity::trim_js};
use rusqlite::Transaction;
use sha2::{Digest, Sha256};
use std::{sync::atomic::AtomicBool, time::Duration};

mod git_source;

/// The repository a URL names, for matching git Sources; `None` for a URL
/// that names none. The caller's URL rules (`pp_services::build_url`), so
/// this crate needs no dependency on them.
pub type RepositoryKey = fn(&str) -> Option<String>;

/// One piece of evidence to attach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportInput {
    pub target: Target,
    /// `model_source` for a Source or model page, `informational_evidence`
    /// for a link, when absent.
    pub kind: Option<EvidenceKind>,
    /// The Source's name, or none for a URL, when absent.
    pub title: Option<String>,
    /// Text the assistant read from the page. Stored trimmed with its
    /// SHA-256 and the time it was stored; blank means none.
    pub extract: Option<String>,
    /// Evidence already in the brief, for example the model page a Source's
    /// files came from. Trimmed; blank means none. Stored on a Source's item
    /// only, as Node stores it.
    pub derived_from_evidence_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A Library Source. `filenames` are stored trimmed, without blanks or
    /// repeats.
    Source {
        id: PositiveId,
        filenames: Vec<String>,
    },
    /// A page by URL: `url` as sent and `normalized` as the caller's URL
    /// rules give it.
    Url {
        url: String,
        normalized: String,
        page: Page,
    },
}

/// What a URL input points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// A git repository, with its [`RepositoryKey`]. Attached through its
    /// Library Source, which is created when no Source has the repository.
    Git {
        key: String,
        github: bool,
        /// The branch a created Source tracks; `main` when absent, as for
        /// `POST /sources`.
        branch: Option<String>,
    },
    /// A model library page: its files must be uploaded into a Source.
    Model,
    Link,
}

/// What a confirm attached and created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOutcome {
    /// One item per input, in input order.
    pub evidence: Vec<Imported>,
    /// Sources created for git URLs, in input order.
    pub created_source_ids: Vec<i64>,
}

/// An evidence item an input put in the brief, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    pub evidence_id: String,
    pub kind: EvidenceKind,
    pub source_id: Option<i64>,
    pub url: String,
    pub upload_required: bool,
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
    /// The name for a new git Source, and the name with its suffix, are
    /// both another Source's.
    #[error("the new Source's name is taken")]
    SourceNameTaken,
    /// The Library refused the git URL as a Source location.
    #[error("the git URL is not a valid Source location")]
    InvalidRepository,
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
        inputs: Vec<ImportInput>,
        repository_key: RepositoryKey,
        wait: Duration,
    ) -> Result<(), ImportInputsError> {
        let policy = self.policy;
        let name = "build_graph.check_import_inputs";
        self.settings
            .read(name, &AtomicBool::new(false), wait, move |tx| {
                let installation = authorize(tx, &credential, policy)?;
                prepare(tx, installation.id(), build, &inputs, repository_key).map(drop)
            })?
    }

    /// Creates the Sources new git URLs need, puts one evidence item per
    /// input in `build`'s brief and clears its selected draft, in one
    /// transaction.
    pub fn import_inputs(
        &self,
        credential: Credential,
        build: PositiveId,
        inputs: Vec<ImportInput>,
        repository_key: RepositoryKey,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<ImportOutcome, ImportInputsError> {
        let policy = self.policy;
        let name = "build_graph.import_inputs";
        let repos = self.repos.clone();
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let tenant = installation.id();
            let (mut brief, mut items) = prepare(tx, tenant, build, &inputs, repository_key)?;
            let wanted = items.iter().filter_map(|(_, new)| new.clone()).collect();
            let (ids, created_source_ids) = git_source::create(tx, &repos, tenant, wanted)?;
            for (item, new) in &mut items {
                if let Some(new) = new {
                    item.source_id = ids.get(&new.key).copied();
                }
            }
            let evidence = items.iter().map(|(item, _)| Imported::of(item)).collect();
            for (item, _) in items {
                upsert(&mut brief, item);
            }
            store_changed_brief(tx, tenant, build, brief)?;
            Ok(ImportOutcome {
                evidence,
                created_source_ids,
            })
        })?
    }
}

impl Imported {
    fn of(item: &BuildEvidence) -> Self {
        Self {
            evidence_id: item.id.clone(),
            kind: item.kind,
            source_id: item.source_id,
            url: item.url.clone(),
            upload_required: item.upload_required == Some(true),
        }
    }
}

/// An input's item, and the Source to create for it when its git URL names a
/// repository no Source holds.
type Prepared = (BuildEvidence, Option<git_source::NewSource>);

/// The brief and what each input makes, after every check. Derived-from ids
/// are checked against the brief as stored, before this import.
fn prepare(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
    inputs: &[ImportInput],
    repository_key: RepositoryKey,
) -> Result<(BuildPlanningBrief, Vec<Prepared>), ImportInputsError> {
    let brief = stored_brief::<ImportInputsError>(tx, tenant, build)?;
    let mut items = Vec::with_capacity(inputs.len());
    for input in inputs {
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
        let mut item = match &input.target {
            Target::Source { id, filenames } => {
                let missing = ImportInputsError::SourceNotFound(id.get());
                let Ok(id) = i64::try_from(id.get()) else {
                    return Err(missing);
                };
                let source = catalog::get(tx, tenant, id)
                    .map_err(WriterFailure::from)?
                    .ok_or(missing)?;
                source_item(id, &source.name, filenames, derived)
            }
            Target::Url {
                url,
                normalized,
                page,
            } => url_item(url, normalized, page),
        };
        let mut new = None;
        if let Target::Url {
            page:
                Page::Git {
                    key,
                    github,
                    branch,
                },
            normalized,
            ..
        } = &input.target
        {
            item.source_id = git_source::existing(tx, tenant, key, repository_key)?;
            if item.source_id.is_none() {
                new = Some(git_source::NewSource {
                    key: key.clone(),
                    url: normalized.clone(),
                    github: *github,
                    branch: branch.clone(),
                    evidence_id: item.id.clone(),
                });
            }
        }
        if let Some(kind) = input.kind {
            item.kind = kind;
        }
        if let Some(title) = &input.title {
            item.title = Some(title.clone());
        }
        let extract = input
            .extract
            .as_deref()
            .map(trim_js)
            .filter(|text| !text.is_empty());
        if let Some(extract) = extract {
            item.content_hash = Some(hex::encode(Sha256::digest(extract.as_bytes())));
            item.extract = Some(extract.to_owned());
            item.retrieved_at = Some(auth::catalog_timestamp());
        }
        items.push((item, new));
    }
    Ok((brief, items))
}

/// The item Node's `classifyBuildUrl` makes for a URL. A git repository's
/// item is a `mod` and an `addon` until the assistant says otherwise; its
/// Source is set by the caller.
fn url_item(url: &str, normalized: &str, page: &Page) -> BuildEvidence {
    let model = *page == Page::Model;
    let git = matches!(page, Page::Git { .. });
    BuildEvidence {
        id: evidence_id(normalized),
        url: url.to_owned(),
        normalized_url: normalized.to_owned(),
        kind: match page {
            Page::Git { .. } => EvidenceKind::Mod,
            Page::Model => EvidenceKind::ModelSource,
            Page::Link => EvidenceKind::InformationalEvidence,
        },
        input_kind: Some(if model {
            EvidenceInputKind::ModelPage
        } else {
            EvidenceInputKind::Url
        }),
        filenames: None,
        artifacts: None,
        upload_required: model.then_some(true),
        derived_from_evidence_id: None,
        source_id: None,
        source_role: git.then_some(SourceRole::Addon),
        title: None,
        extract: None,
        retrieved_at: None,
        content_hash: None,
        sync_status: git.then_some(SyncStatus::Pending),
        pinned_revision: None,
    }
}

/// The item Node's `buildEvidenceFromUploadedSource` makes for Source `id`.
fn source_item(id: i64, name: &str, filenames: &[String], derived: Option<&str>) -> BuildEvidence {
    let url = format!("printpartner:source:{id}");
    let mut kept: Vec<String> = Vec::new();
    for name in filenames.iter().map(|name| trim_js(name)) {
        if !name.is_empty() && !kept.iter().any(|kept| kept == name) {
            kept.push(name.to_owned());
        }
    }
    BuildEvidence {
        id: evidence_id(&url),
        normalized_url: url.clone(),
        url,
        kind: EvidenceKind::ModelSource,
        input_kind: Some(EvidenceInputKind::Upload),
        filenames: Some(kept),
        artifacts: None,
        upload_required: None,
        derived_from_evidence_id: derived.map(str::to_owned),
        source_id: Some(id),
        source_role: None,
        title: Some(name.to_owned()),
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
/// `source_role` when it has one, or appends `item`.
fn upsert(brief: &mut BuildPlanningBrief, mut item: BuildEvidence) {
    match brief
        .evidence
        .iter_mut()
        .find(|kept| kept.normalized_url == item.normalized_url)
    {
        Some(kept) => {
            item.source_role = kept.source_role.or(item.source_role);
            *kept = item;
        }
        None => brief.evidence.push(item),
    }
}

#[cfg(test)]
mod tests;
