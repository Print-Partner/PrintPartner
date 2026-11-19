//! `GET /sources/{id}/files` at `/` and `/api/v1`: the files of a Source's
//! current revision, for Library (GRE-430 part 1). `GET
//! /sources/{id}/thumbnail?path=` at the same prefixes: a 256 x 256 PNG of
//! one of those STL files, cached by storage (GRE-430 part 2a). Rust-only
//! runtime: the routes mount only when `PP_DESKTOP_REBUILD` is on, behind the
//! read gate the Accepted Plan routes use. `GET
//! /sources/{id}/step-conversion?path=` at the same prefixes: the part tree of
//! one STEP file, converted once in the background and answered from storage's
//! cache, or its queued, converting or failed state (GRE-1007).
//!
//! Storage answers [`SourceFiles`] or [`SourceFilesError`]; this module turns
//! them into the response, once.

use crate::accepted::{AcceptedHttpConfig, Gate};
use crate::loopback::PresentedCredential;
use crate::problem::{self, Problem, ProblemCode, ProblemType, ToProblem};
use axum::{
    Extension, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use pp_storage::source_files::{
    FileKind, MAX_FILES, PartNode, SourceFile, SourceFiles, SourceFilesClient, SourceFilesError,
    SourceThumbnail, StepConversion, StepFailure, THUMBNAIL_CONCURRENCY,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

/// Every `code` the route answers, as its [`ProblemCode`]'s wire value.
pub mod code {
    use crate::problem::ProblemCode;
    pub const SOURCE_NOT_FOUND: &str = ProblemCode::SourceNotFound.as_str();
    pub const SNAPSHOT_UNAVAILABLE: &str = ProblemCode::SourceSnapshotUnavailable.as_str();
    pub const TOO_MANY_FILES: &str = ProblemCode::SourceTooManyFiles.as_str();
    pub const FILE_NOT_FOUND: &str = ProblemCode::SourceFileNotFound.as_str();
    pub const UNRENDERABLE: &str = ProblemCode::SourceFileUnrenderable.as_str();
    pub const PATH_REQUIRED: &str = ProblemCode::SourceFilePathRequired.as_str();
}

/// The response body. `revision_id` is null, and `files` empty, for a Source
/// with no revision yet. The generated client's `listSourceFiles`.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SourceFileList {
    pub source_id: i64,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_storage::build_graph::wire::RequiredNullable<i64>")
    )]
    pub revision_id: Option<i64>,
    /// Sorted by `path`.
    pub files: Vec<SourceFileEntry>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SourceFileEntry {
    /// Relative to the revision's root, with `/` between folders.
    pub path: String,
    pub kind: SourceFileKind,
    pub size_bytes: u64,
}

/// What a file is, by its extension.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum SourceFileKind {
    Stl,
    // 3MF, STEP, OBJ, OpenSCAD or Fusion. A plain comment: a doc comment
    // on only some variants would split the client's string enum.
    Model,
    // PDF, Markdown or plain text.
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

/// One STEP file's part tree, or where its conversion stands. The generated
/// client's `readStepPartTree`. The UI asks again while the state is
/// `queued` or `converting`.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct StepPartTree {
    pub source_id: i64,
    pub path: String,
    pub state: StepConversionState,
    /// The SHA-256 of the file's bytes, which keys the cache; set when done.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_storage::build_graph::wire::RequiredNullable<String>")
    )]
    pub sha256: Option<String>,
    /// Why the file could not be converted; set when failed.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_storage::build_graph::wire::RequiredNullable<StepConversionFailure>")
    )]
    pub failure: Option<StepConversionFailure>,
    /// Every node of the tree, depth first: a node's children follow it, one
    /// level deeper. Empty unless done.
    pub parts: Vec<StepPart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum StepConversionState {
    Queued,
    Converting,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum StepConversionFailure {
    NotStep,
    NoProducts,
    TooLarge,
    Cycle,
    Unreadable,
    CacheUnwritable,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct StepPart {
    /// The product's name, or its id when unnamed.
    pub name: String,
    pub product_id: String,
    /// The name of this use of the part in its parent; null for a root.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_storage::build_graph::wire::RequiredNullable<String>")
    )]
    pub instance: Option<String>,
    /// 0 for a root.
    pub depth: u32,
}

impl StepPartTree {
    pub fn new(source_id: i64, path: String, conversion: StepConversion) -> Self {
        let mut tree = Self {
            source_id,
            path,
            state: StepConversionState::Queued,
            sha256: None,
            failure: None,
            parts: Vec::new(),
        };
        match conversion {
            StepConversion::Queued => {}
            StepConversion::Converting => tree.state = StepConversionState::Converting,
            StepConversion::Done {
                sha256,
                tree: parts,
            } => {
                tree.state = StepConversionState::Done;
                tree.sha256 = Some(sha256);
                flatten(parts.roots, 0, &mut tree.parts);
            }
            StepConversion::Failed(failure) => {
                tree.state = StepConversionState::Failed;
                tree.failure = Some(match failure {
                    StepFailure::NotStep => StepConversionFailure::NotStep,
                    StepFailure::NoProducts => StepConversionFailure::NoProducts,
                    StepFailure::TooLarge => StepConversionFailure::TooLarge,
                    StepFailure::Cycle => StepConversionFailure::Cycle,
                    StepFailure::Unreadable => StepConversionFailure::Unreadable,
                    StepFailure::CacheUnwritable => StepConversionFailure::CacheUnwritable,
                });
            }
        }
        tree
    }
}

/// Depth first. The reader caps the depth, so the recursion is bounded.
fn flatten(nodes: Vec<PartNode>, depth: u32, parts: &mut Vec<StepPart>) {
    for node in nodes {
        parts.push(StepPart {
            name: node.name,
            product_id: node.product_id,
            instance: node.instance,
            depth,
        });
        flatten(node.children, depth + 1, parts);
    }
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: SourceFilesClient,
    /// Thumbnail requests in flight, at most [`THUMBNAIL_CONCURRENCY`].
    renders: Arc<tokio::sync::Semaphore>,
}

/// `?path=`: the file's path in the revision, as the file list gives it.
#[derive(Deserialize)]
struct PathQuery {
    path: Option<String>,
}

/// Mounted only when the rebuild flag was on at startup; otherwise empty.
pub fn source_files_router(config: AcceptedHttpConfig, client: SourceFilesClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/sources/{id}/files", get(list))
        .route("/api/v1/sources/{id}/files", get(list))
        .route("/sources/{id}/thumbnail", get(thumbnail))
        .route("/api/v1/sources/{id}/thumbnail", get(thumbnail))
        .route("/sources/{id}/step-conversion", get(step_conversion))
        .route("/api/v1/sources/{id}/step-conversion", get(step_conversion));
    let renders = Arc::new(tokio::sync::Semaphore::new(THUMBNAIL_CONCURRENCY));
    gate.guard(routes).with_state(App {
        gate,
        client,
        renders,
    })
}

/// Runs one storage read for Source `id` on the blocking pool. Like the
/// catalog routes, an id that is not a positive integer names no Source.
async fn read<T: Send + 'static>(
    app: &App,
    id: &str,
    operation: impl FnOnce(
        &SourceFilesClient,
        i64,
        &AtomicBool,
        Duration,
    ) -> Result<T, SourceFilesError>
    + Send
    + 'static,
) -> Result<T, Box<Response>> {
    let Some(id) = id.parse::<i64>().ok().filter(|id| *id > 0) else {
        return Err(Box::new(refused(&SourceFilesError::NotFound)));
    };
    let client = app.client.clone();
    let done = app
        .gate
        .blocking(move |cancelled, wait| operation(&client, id, &cancelled, wait))
        .await;
    match done {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(Box::new(refused(&error))),
        Err(_) => Err(Box::new(problem::response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
        ))),
    }
}

async fn list(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    let listed = read(&app, &id, move |client, id, cancelled, wait| {
        client.list(presented.into_credential(), id, cancelled, wait)
    })
    .await;
    match listed {
        Ok(listed) => axum::Json(SourceFileList::from(listed)).into_response(),
        Err(refusal) => *refusal,
    }
}

async fn thumbnail(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(path) = query.path.filter(|path| !path.is_empty()) else {
        return path_required();
    };
    // The blocking task owns the permit, so a render keeps it until it ends,
    // even when the client has gone.
    let Ok(permit) = app.renders.clone().acquire_owned().await else {
        return problem::response(StatusCode::SERVICE_UNAVAILABLE, "Service unavailable");
    };
    let rendered = read(&app, &id, move |client, id, cancelled, wait| {
        let _permit = permit;
        client.thumbnail(presented.into_credential(), id, &path, cancelled, wait)
    })
    .await;
    match rendered {
        Ok(thumbnail) => png_response(thumbnail, &headers),
        Err(refusal) => *refusal,
    }
}

/// Queues the STEP file on first open; answers its state, or its part tree
/// once the conversion is cached. Never waits for the conversion.
async fn step_conversion(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Response {
    let Some(path) = query.path.filter(|path| !path.is_empty()) else {
        return path_required();
    };
    let requested = path.clone();
    let opened = read(&app, &id, move |client, id, cancelled, wait| {
        let conversion =
            client.step_conversion(presented.into_credential(), id, &requested, cancelled, wait)?;
        Ok((id, conversion))
    })
    .await;
    match opened {
        Ok((id, conversion)) => axum::Json(StepPartTree::new(id, path, conversion)).into_response(),
        Err(refusal) => *refusal,
    }
}

fn path_required() -> Response {
    problem::build(
        Problem::new(ProblemType::Validation, 400, "path is required")
            .with_problem_code(ProblemCode::SourceFilePathRequired),
        Default::default(),
    )
}

/// 200 with the PNG, or 304 when `If-None-Match` names its tag. The client
/// revalidates each time; an unchanged thumbnail costs a cache read.
fn png_response(thumbnail: SourceThumbnail, headers: &HeaderMap) -> Response {
    let etag = format!("\"{}\"", thumbnail.tag);
    let fresh = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|tag| matches!(tag.trim(), "*") || tag.trim() == etag);
    let Ok(etag) = HeaderValue::from_str(&etag) else {
        return problem::response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let cache = (
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    if fresh {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag), cache]).into_response();
    }
    let kind = (header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    (
        StatusCode::OK,
        [(header::ETAG, etag), cache, kind],
        thumbnail.png,
    )
        .into_response()
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
            Self::FileNotFound => Problem::new(
                ProblemType::NotFound,
                404,
                "The current revision has no such file at that path",
            )
            .with_problem_code(ProblemCode::SourceFileNotFound),
            Self::Unrenderable => Problem::new(
                ProblemType::Validation,
                422,
                "The STL file cannot be rendered",
            )
            .with_problem_code(ProblemCode::SourceFileUnrenderable),
            Self::Writer(failure) => failure.to_problem(),
        }
    }
}

fn refused(error: &SourceFilesError) -> Response {
    problem::refused(error)
}

#[cfg(test)]
mod tests;
