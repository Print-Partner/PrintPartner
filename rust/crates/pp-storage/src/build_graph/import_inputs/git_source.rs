//! The Library Source behind a git URL input: the one whose URL names the
//! same repository, or a new git Source made as `POST /sources` makes one,
//! in the brief's own write transaction. Nothing is fetched or synced.

use super::{ImportInputsError, RepositoryKey};
use crate::{
    WriterFailure,
    catalog::{self, CatalogFailure, CreateSource},
};
use rusqlite::{Transaction, params};
use std::{collections::HashMap, path::Path};

/// A git URL whose repository no Library Source holds yet.
#[derive(Clone, Debug)]
pub(super) struct NewSource {
    pub(super) key: String,
    pub(super) url: String,
    pub(super) github: bool,
    pub(super) branch: Option<String>,
    /// The evidence item's id, for Node's fallback name and its suffix.
    pub(super) evidence_id: String,
}

/// The Source whose stored URL has repository key `key`, oldest first.
pub(super) fn existing(
    tx: &Transaction<'_>,
    tenant: &str,
    key: &str,
    repository_key: RepositoryKey,
) -> Result<Option<i64>, ImportInputsError> {
    let mut rows =
        tx.prepare("SELECT id,url FROM projects WHERE tenant_id=?1 AND url<>'' ORDER BY id")?;
    let sources = rows.query_map(params![tenant], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    for source in sources {
        let (id, url) = source?;
        if repository_key(&url).as_deref() == Some(key) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Creates each new Source once, and answers the id for each repository key
/// in `wanted`, plus the ids created, in order.
pub(super) fn create(
    tx: &Transaction<'_>,
    repos: &Path,
    tenant: &str,
    wanted: Vec<NewSource>,
) -> Result<(HashMap<String, i64>, Vec<i64>), ImportInputsError> {
    let mut ids = HashMap::new();
    let mut created = Vec::new();
    for new in wanted {
        if ids.contains_key(&new.key) {
            continue;
        }
        let source = CreateSource {
            name: free_name(tx, tenant, &new)?,
            url: Some(new.url.clone()),
            branch: new.branch.clone(),
            source_kind: Some(if new.github { "github" } else { "git" }.into()),
            source_type: Some("git".into()),
            ..CreateSource::default()
        };
        let summary = catalog::rebuild::create_in(tx, repos, tenant, source).map_err(|error| {
            match error.downcast_ref::<CatalogFailure>() {
                Some(CatalogFailure::DuplicateName(_)) => ImportInputsError::SourceNameTaken,
                Some(CatalogFailure::Input(_)) => ImportInputsError::InvalidRepository,
                _ => WriterFailure::from(error).into(),
            }
        })?;
        ids.insert(new.key, summary.id);
        created.push(summary.id);
    }
    Ok((ids, created))
}

/// Node's name for a Source it creates from a URL: the last two path
/// segments joined with `-` without `.git` (on `github.com`, the owner and
/// repository), or `Build-source-<evidence id>`; with the first 6 id
/// characters added when another Source has the name.
fn free_name(
    tx: &Transaction<'_>,
    tenant: &str,
    new: &NewSource,
) -> Result<String, ImportInputsError> {
    let path = url::Url::parse(&new.url)
        .ok()
        .and_then(|url| {
            let segments: Vec<String> = url
                .path_segments()?
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect();
            let pair = if new.github {
                segments.get(..2)?
            } else {
                &segments[segments.len().saturating_sub(2)..]
            };
            let joined = pair.join("-");
            let cut = if joined.to_lowercase().ends_with(".git") {
                4
            } else {
                0
            };
            Some(joined[..joined.len() - cut].to_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("Build-source-{}", new.evidence_id));
    if !taken(tx, tenant, &path)? {
        return Ok(path);
    }
    let suffixed = format!("{path}-{}", &new.evidence_id[..6]);
    if taken(tx, tenant, &suffixed)? {
        return Err(ImportInputsError::SourceNameTaken);
    }
    Ok(suffixed)
}

fn taken(tx: &Transaction<'_>, tenant: &str, name: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE tenant_id=?1 AND name=?2)",
        params![tenant, name],
        |row| row.get(0),
    )
}
