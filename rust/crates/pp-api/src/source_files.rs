//! `GET /sources/{id}/files` at `/` and `/api/v1`: the files of a Source's
//! current revision, for Library (GRE-430 part 1). Rust-only runtime: the
//! route mounts only when `PP_DESKTOP_REBUILD` is on, behind the read gate the
//! Accepted Plan routes use.
//!
//! Storage answers [`SourceFiles`] or [`SourceFilesError`]; this module turns
//! them into the response, once.

use crate::accepted::{AcceptedHttpConfig, Gate};
use crate::loopback::PresentedCredential;
use crate::problem::{self, Problem, ProblemCode, ProblemType, ToProblem};
use axum::{
    Extension, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use pp_storage::source_files::{
    FileKind, MAX_FILES, SourceFile, SourceFiles, SourceFilesClient, SourceFilesError,
};
use serde::Serialize;

/// Every `code` the route answers, as its [`ProblemCode`]'s wire value.
pub mod code {
    use crate::problem::ProblemCode;
    pub const SOURCE_NOT_FOUND: &str = ProblemCode::SourceNotFound.as_str();
    pub const SNAPSHOT_UNAVAILABLE: &str = ProblemCode::SourceSnapshotUnavailable.as_str();
    pub const TOO_MANY_FILES: &str = ProblemCode::SourceTooManyFiles.as_str();
}

/// The response body. `revision_id` is null, and `files` empty, for a Source
/// with no revision yet.
#[derive(Debug, Serialize)]
pub struct SourceFileList {
    pub source_id: i64,
    pub revision_id: Option<i64>,
    /// Sorted by `path`.
    pub files: Vec<SourceFileEntry>,
}

#[derive(Debug, Serialize)]
pub struct SourceFileEntry {
    /// Relative to the revision's root, with `/` between folders.
    pub path: String,
    pub kind: SourceFileKind,
    pub size_bytes: u64,
}

/// What a file is, by its extension.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFileKind {
    Stl,
    /// 3MF, STEP, OBJ, OpenSCAD or Fusion.
    Model,
    /// PDF, Markdown or plain text.
    Document,
    Image,
    Other,
}

impl From<SourceFiles> for SourceFileList {
    fn from(listed: SourceFiles) -> Self {
        Self {
            source_id: listed.source_id,
            revision_id: listed.revision_id,
            files: listed
                .files
                .into_iter()
                .map(SourceFileEntry::from)
                .collect(),
        }
    }
}

impl From<SourceFile> for SourceFileEntry {
    fn from(file: SourceFile) -> Self {
        let kind = match file.kind {
            FileKind::Stl => SourceFileKind::Stl,
            FileKind::Model => SourceFileKind::Model,
            FileKind::Document => SourceFileKind::Document,
            FileKind::Image => SourceFileKind::Image,
            FileKind::Other => SourceFileKind::Other,
        };
        Self {
            path: file.path,
            kind,
            size_bytes: file.size_bytes,
        }
    }
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: SourceFilesClient,
}

/// Mounted only when the rebuild flag was on at startup; otherwise empty.
pub fn source_files_router(config: AcceptedHttpConfig, client: SourceFilesClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/sources/{id}/files", get(list))
        .route("/api/v1/sources/{id}/files", get(list));
    gate.guard(routes).with_state(App { gate, client })
}

async fn list(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    // Like the catalog routes, an id that is not a positive integer names no
    // Source.
    let Some(id) = id.parse::<i64>().ok().filter(|id| *id > 0) else {
        return refused(&SourceFilesError::NotFound);
    };
    let client = app.client.clone();
    let listed = app
        .gate
        .blocking(move |cancelled, wait| {
            client.list(presented.into_credential(), id, &cancelled, wait)
        })
        .await;
    match listed {
        Ok(Ok(listed)) => axum::Json(SourceFileList::from(listed)).into_response(),
        Ok(Err(error)) => refused(&error),
        Err(_) => problem::response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}

/// The one mapping from the family's error to a problem.
impl ToProblem for SourceFilesError {
    fn to_problem(&self) -> Problem {
        match self {
            Self::NotFound => Problem::new(ProblemType::NotFound, 404, "Source not found")
                .with_problem_code(ProblemCode::SourceNotFound),
            Self::SnapshotUnavailable => Problem::new(
                ProblemType::Internal,
                500,
                "The Source's current revision cannot be read",
            )
            .with_problem_code(ProblemCode::SourceSnapshotUnavailable),
            Self::TooManyFiles => Problem::new(
                ProblemType::Validation,
                422,
                &format!("The revision holds more than {MAX_FILES} files"),
            )
            .with_problem_code(ProblemCode::SourceTooManyFiles),
            Self::Writer(failure) => failure.to_problem(),
        }
    }
}

fn refused(error: &SourceFilesError) -> Response {
    problem::refused(error)
}

#[cfg(test)]
mod tests;
