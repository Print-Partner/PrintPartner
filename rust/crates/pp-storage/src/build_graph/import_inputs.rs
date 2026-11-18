//! Attach evidence to a Build's planning brief (GRE-1300 parts 2, 3a and
//! 3b), the operation behind MCP `propose_import_build_inputs`: Library
//! Sources by id, model pages and other links by URL, and git repositories
//! by URL.
//!
//! [`BuildGraphClient::import_inputs`] runs in one write transaction on the
//! write seam: prove the credential, read the Build's planning brief, check
//! every input, create a Library Source for each git repository the Library
//! does not hold yet, put one evidence item per input in the brief, clear its
//! selected draft and store it. An existing Source is only read. The Build's
//! layers, drafts and Accepted Plan are not touched. Nothing is fetched.
//! [`BuildGraphClient::check_import_inputs`] runs the same checks on the read
//! seam and writes nothing.
//!
//! An item is keyed by its normalized URL, as Node keys it: the link's, or
//! `printpartner:source:<id>` for a Source. Importing the same input again
//! updates its one item in place, and so does another URL for a git
//! repository's Source. Unlike Node, that update keeps the item's
//! `source_role`. The caller normalizes a URL and gives the [`RepoKey`] that
//! matches a repository to its Source; this module stores them.
//!
//! Errors are [`ImportInputsError`]. This module holds no status codes and
//! no response text; `pp-api` maps the variants.

use super::{
    BuildGraphClient,
    create_with_brief::{BriefError, store_changed_brief, stored_brief},
};
use crate::{
    WriterFailure, auth,
    catalog::{self, CreateSource},
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
use rusqlite::{Transaction, params};
use sha2::{Digest, Sha256};
use std::{sync::atomic::AtomicBool, time::Duration};

/// One piece of evidence to attach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportInput {
    pub target: Target,
    /// `model_source` for a Source or model page, `mod` for a git
    /// repository, `informational_evidence` for a link, when absent.
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
    /// A git repository by URL. Its item attaches the Library Source with
    /// the same [`RepoKey`], or a new git Source made as `POST /sources`
    /// makes one, from `normalized` and `branch` (`main` when absent).
    Git {
        url: String,
        normalized: String,
        github: bool,
        branch: Option<String>,
    },
}

/// The caller's rule for which repository a URL names: the same key means
/// the same repository, `None` means not a git repository URL.
pub type RepoKey = fn(&str) -> Option<String>;

/// The non-git URL pages this module attaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// A model library page: its files must be uploaded into a Source.
    Model,
    Link,
}

/// An evidence item an input put in the brief, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    pub evidence_id: String,
    pub kind: EvidenceKind,
    pub source_id: Option<i64>,
    pub url: String,
    pub upload_required: bool,
    /// This import created the item's Library Source.
    pub source_created: bool,
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
    /// `POST /sources` would refuse the Source a git URL needs. Holds its
    /// reason.
    #[error("the git repository URL cannot be a Source: {0}")]
    GitSourceRefused(String),
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
        repo_key: RepoKey,
        wait: Duration,
    ) -> Result<(), ImportInputsError> {
        let policy = self.policy;
        let name = "build_graph.check_import_inputs";
        self.settings
            .read(name, &AtomicBool::new(false), wait, move |tx| {
                let installation = authorize(tx, &credential, policy)?;
                prepare(tx, installation.id(), build, &inputs, repo_key).map(drop)
            })?
    }

    /// Creates the git Sources the inputs need, puts one evidence item per
    /// input in `build`'s brief and clears its selected draft, in one
    /// transaction. Answers the items in input order.
    pub fn import_inputs(
        &self,
        credential: Credential,
        build: PositiveId,
        inputs: Vec<ImportInput>,
        repo_key: RepoKey,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Vec<Imported>, ImportInputsError> {
        let policy = self.policy;
        let repos = self.repos.clone();
        let name = "build_graph.import_inputs";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let tenant = installation.id();
            let Prepared {
                mut brief,
                mut items,
                new_sources,
            } = prepare(tx, tenant, build, &inputs, repo_key)?;
            let mut created = vec![false; items.len()];
            for (source, attached) in new_sources {
                let id = catalog::insert_source(tx, &repos, tenant, source)
                    .map_err(WriterFailure::from)?
                    .id;
                for index in attached {
                    items[index].source_id = Some(id);
                    created[index] = true;
                }
            }
            let imported = items
                .into_iter()
                .zip(created)
                .map(|(item, created)| Imported::of(upsert(&mut brief, item), created))
                .collect();
            store_changed_brief(tx, tenant, build, brief)?;
            Ok(imported)
        })?
    }
}

impl Imported {
    fn of(item: &BuildEvidence, source_created: bool) -> Self {
        Self {
            evidence_id: item.id.clone(),
            kind: item.kind,
            source_id: item.source_id,
            url: item.url.clone(),
            upload_required: item.upload_required == Some(true),
            source_created,
        }
    }
}

/// The brief and the item each input makes, after every check, and the git
/// Sources to create first. A git item's `source_id` is set here when the
/// Library holds its repository.
struct Prepared {
    brief: BuildPlanningBrief,
    items: Vec<BuildEvidence>,
    /// Each new Source, checked as `POST /sources` checks it, with the
    /// indexes of the items that attach it.
    new_sources: Vec<(CreateSource, Vec<usize>)>,
}

/// Derived-from ids are checked against the brief as stored, before this
/// import.
fn prepare(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
    inputs: &[ImportInput],
    repo_key: RepoKey,
) -> Result<Prepared, ImportInputsError> {
    let brief = stored_brief::<ImportInputsError>(tx, tenant, build)?;
    let mut items = Vec::with_capacity(inputs.len());
    let mut library: Option<Vec<(String, i64)>> = None;
    let mut new_sources: Vec<(CreateSource, Vec<usize>)> = Vec::new();
    let mut new_keys: Vec<String> = Vec::new();
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
            } => url_item(url, normalized, *page),
            Target::Git {
                url,
                normalized,
                github,
                branch,
            } => {
                let mut item = git_item(url, normalized);
                let key = repo_key(normalized).unwrap_or_else(|| normalized.clone());
                let library = match &mut library {
                    Some(library) => library,
                    None => library.insert(git_sources(tx, tenant, repo_key)?),
                };
                if let Some((_, id)) = library.iter().find(|(stored, _)| *stored == key) {
                    item.source_id = Some(*id);
                } else if let Some(planned) = new_keys.iter().position(|planned| *planned == key) {
                    new_sources[planned].1.push(items.len());
                } else {
                    let source = new_source(tx, tenant, &item, *github, branch, &new_sources)?;
                    new_sources.push((source, vec![items.len()]));
                    new_keys.push(key);
                }
                item
            }
        };
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
        items.push(item);
    }
    Ok(Prepared {
        brief,
        items,
        new_sources,
    })
}

/// Each Library Source with a git repository URL, by its key, oldest first.
fn git_sources(
    tx: &Transaction<'_>,
    tenant: &str,
    repo_key: RepoKey,
) -> Result<Vec<(String, i64)>, ImportInputsError> {
    let mut rows = tx.prepare("SELECT url, id FROM projects WHERE tenant_id=?1 ORDER BY id")?;
    let rows = rows.query_map(params![tenant], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut sources = Vec::new();
    for row in rows {
        let (url, id) = row?;
        if let Some(key) = repo_key(&url) {
            sources.push((key, id));
        }
    }
    Ok(sources)
}

/// The git Source `item` needs, named as Node names it: the last two path
/// segments joined by `-`, without `.git`, made unique with the start of the
/// evidence id and then a number. On GitHub the two segments are the owner
/// and repository, so a `/tree/<branch>` URL is not named `tree-<branch>`.
fn new_source(
    tx: &Transaction<'_>,
    tenant: &str,
    item: &BuildEvidence,
    github: bool,
    branch: &Option<String>,
    planned: &[(CreateSource, Vec<usize>)],
) -> Result<CreateSource, ImportInputsError> {
    let path = item
        .normalized_url
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    let segments: Vec<_> = path
        .split('/')
        .skip(3)
        .filter(|segment| !segment.is_empty())
        .collect();
    let two = if github {
        &segments[..segments.len().min(2)]
    } else {
        &segments[segments.len().saturating_sub(2)..]
    };
    let mut base = two.join("-");
    if base.to_lowercase().ends_with(".git") {
        base.truncate(base.len() - ".git".len());
    }
    if base.is_empty() {
        base = format!("Build-source-{}", item.id);
    }
    let taken = |name: &str| -> Result<bool, ImportInputsError> {
        let stored: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE tenant_id=?1 AND name=?2)",
            params![tenant, name],
            |row| row.get(0),
        )?;
        Ok(stored || planned.iter().any(|(source, _)| source.name == name))
    };
    let mut name = base.clone();
    let mut attempt = 1;
    while taken(&name)? {
        name = format!("{base}-{}", &item.id[..6]);
        if attempt > 1 {
            name = format!("{name}-{attempt}");
        }
        attempt += 1;
    }
    let source = CreateSource {
        name,
        url: Some(item.normalized_url.clone()),
        branch: branch
            .as_deref()
            .map(trim_js)
            .filter(|branch| !branch.is_empty())
            .map(str::to_owned),
        source_kind: Some(if github { "github" } else { "git" }.into()),
        source_type: Some("git".into()),
        ..CreateSource::default()
    };
    catalog::check_new_source(tx, tenant, source).map_err(refused)
}

/// A refusal `POST /sources` gives as an input error, or a storage failure.
fn refused(error: anyhow::Error) -> ImportInputsError {
    if let Some(catalog::CatalogFailure::Input(reason)) = error.downcast_ref() {
        return ImportInputsError::GitSourceRefused(reason.clone());
    }
    ImportInputsError::Writer(error.into())
}

/// The item Node's `classifyBuildUrl` makes for a model page or link.
fn url_item(url: &str, normalized: &str, page: Page) -> BuildEvidence {
    let model = page == Page::Model;
    BuildEvidence {
        id: evidence_id(normalized),
        url: url.to_owned(),
        normalized_url: normalized.to_owned(),
        kind: if model {
            EvidenceKind::ModelSource
        } else {
            EvidenceKind::InformationalEvidence
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
        source_role: None,
        title: None,
        extract: None,
        retrieved_at: None,
        content_hash: None,
        sync_status: None,
        pinned_revision: None,
    }
}

/// The item Node's `classifyBuildUrl` makes for a git repository, without
/// its Source yet.
fn git_item(url: &str, normalized: &str) -> BuildEvidence {
    BuildEvidence {
        kind: EvidenceKind::Mod,
        input_kind: Some(EvidenceInputKind::Url),
        source_role: Some(SourceRole::Addon),
        sync_status: Some(SyncStatus::Pending),
        ..url_item(url, normalized, Page::Link)
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
pub(crate) fn evidence_id(normalized_url: &str) -> String {
    let mut id = hex::encode(Sha256::digest(normalized_url.as_bytes()));
    id.truncate(20);
    id
}

/// Replaces the item with `item`'s normalized URL, or a git item for the
/// same Source, in place, or appends `item`. A replaced item keeps its
/// `source_role` when it has one, and a git item keeps its id and URLs.
/// Answers the stored item.
fn upsert(brief: &mut BuildPlanningBrief, mut item: BuildEvidence) -> &BuildEvidence {
    let git = item.input_kind == Some(EvidenceInputKind::Url) && item.source_id.is_some();
    let same = |kept: &BuildEvidence| {
        kept.normalized_url == item.normalized_url
            || git && kept.input_kind == item.input_kind && kept.source_id == item.source_id
    };
    let Some(index) = brief.evidence.iter().position(same) else {
        brief.evidence.push(item);
        return &brief.evidence[brief.evidence.len() - 1];
    };
    let kept = &mut brief.evidence[index];
    if kept.source_role.is_some() {
        item.source_role = kept.source_role;
    }
    if kept.normalized_url != item.normalized_url {
        item.id = std::mem::take(&mut kept.id);
        item.url = std::mem::take(&mut kept.url);
        item.normalized_url = std::mem::take(&mut kept.normalized_url);
    }
    *kept = item;
    kept
}

#[cfg(test)]
mod tests;
