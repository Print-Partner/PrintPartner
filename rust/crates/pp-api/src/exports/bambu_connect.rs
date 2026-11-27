//! Stages a local file and returns the official app's import link. Opening the
//! link is a separate user action; Print Partner does not dispatch a send.

use crate::{
    NetworkAdmission,
    accepted::{AcceptedHttpConfig, Gate, build_id, failure, internal, ok_json},
    loopback::PresentedCredential,
    multipart_form::{self, Limits},
    problem::{self, Problem, ProblemType, ToProblem},
};
use axum::{
    Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{StatusCode, header},
    response::Response,
    routing::post,
};
use pp_storage::{
    build_graph::PositiveId, checkoff_progress::print_file::MAX_UPLOAD_BYTES,
    features::BambuConnectAccess, read_model::ReadClient,
};
use serde::Serialize;
use std::{path::PathBuf, sync::Arc};

const MAX_DISPLAY_NAME_CHARS: usize = 120;
const FORM: Limits = Limits {
    fields: 0,
    field_bytes: 0,
    file_bytes: MAX_UPLOAD_BYTES,
    file_names: &["file"],
    second_file: "Choose one sliced print file",
    too_large: "Print file exceeds the 64 MiB upload limit",
    unreadable: "That upload could not be read",
};

/// A nonempty upload with a supported extension, admitted for this Build.
pub struct PrintFile {
    build: PositiveId,
    extension: &'static str,
    bytes: Vec<u8>,
}

impl PrintFile {
    pub fn build(&self) -> PositiveId {
        self.build
    }

    pub fn extension(&self) -> &'static str {
        self.extension
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Local filesystem adapter. Returns the newly staged or byte-identical
/// existing file's absolute path. Never opens the link or contacts a printer.
pub trait Stage: Send + Sync + 'static {
    fn stage(&self, print: PrintFile) -> Result<PathBuf, StageFailure>;
}

#[derive(Debug)]
pub enum StageFailure {
    Conflict,
    Full,
    Unavailable,
}

impl ToProblem for StageFailure {
    fn to_problem(&self) -> Problem {
        let (kind, status, detail) = match self {
            Self::Conflict => (
                ProblemType::Conflict,
                StatusCode::CONFLICT,
                "The existing handoff file does not match this upload",
            ),
            Self::Full => (
                ProblemType::Overloaded,
                StatusCode::SERVICE_UNAVAILABLE,
                "The local Bambu Connect staging area is full. Try again after old files expire.",
            ),
            Self::Unavailable => (
                ProblemType::Overloaded,
                StatusCode::SERVICE_UNAVAILABLE,
                "Local Bambu Connect staging is unavailable",
            ),
        };
        Problem::new(kind, status.as_u16(), detail)
    }
}

#[derive(Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub(crate) struct BambuConnectStage {
    pub(crate) filename: String,
    pub(crate) connect_url: String,
}

#[derive(Clone)]
struct App {
    gate: Gate,
    reads: ReadClient,
    files: Arc<dyn Stage>,
    uploads: Arc<tokio::sync::Semaphore>,
}

/// Mount only when a native desktop supplies a local staging adapter.
pub fn bambu_connect_router(
    config: AcceptedHttpConfig,
    reads: ReadClient,
    files: Arc<dyn Stage>,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/api/v2/plans/{id}/bambu-connect-handoff", post(handoff))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 64 * 1024));
    gate.guard(routes).with_state(App {
        gate,
        reads,
        files,
        uploads: Arc::new(tokio::sync::Semaphore::new(2)),
    })
}

async fn handoff(State(app): State<App>, Path(id): Path<String>, request: Request) -> Response {
    let Some(presented @ PresentedCredential::Session(_)) =
        request.extensions().get::<PresentedCredential>()
    else {
        return failure(StatusCode::FORBIDDEN, "Desktop session required");
    };
    if request.extensions().get::<NetworkAdmission>().is_some() {
        return failure(StatusCode::FORBIDDEN, "Local desktop only");
    }
    let build = build_id(&id);
    let credential = presented.clone().into_credential();
    let reads = app.reads.clone();
    let build = match app
        .gate
        .blocking(move |cancelled, wait| {
            reads.bambu_connect_access(&credential, build, &cancelled, wait)
        })
        .await
    {
        Ok(Ok(BambuConnectAccess::MissingBuild)) => {
            return failure(StatusCode::NOT_FOUND, "Build not found");
        }
        Ok(Ok(BambuConnectAccess::Disabled)) => {
            return failure(
                StatusCode::FORBIDDEN,
                "Bambu Connect is disabled for this Build",
            );
        }
        Ok(Ok(BambuConnectAccess::Enabled(build))) => build,
        Ok(Err(error)) => return problem::refused(&error),
        Err(_) => return internal(),
    };
    let Ok(permit) = app.uploads.try_acquire_owned() else {
        return problem::overloaded("Too many Bambu Connect uploads");
    };
    let form = match multipart_form::read(request, &FORM).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    let Some((Some(name), bytes)) = form.file.filter(|(_, bytes)| !bytes.is_empty()) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "Choose a nonempty sliced print file",
        );
    };
    let Some((stem, extension)) = print_name(&name) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "Choose a .3mf, .gcode.3mf, .gcode or .gco file",
        );
    };
    let filename = format!("{stem}{extension}");
    let print = PrintFile {
        build,
        extension,
        bytes,
    };
    match app
        .gate
        .blocking(move |_, _| {
            let _permit = permit;
            let path = app.files.stage(print)?;
            connect_url(&path, &stem).map(|connect_url| BambuConnectStage {
                filename,
                connect_url,
            })
        })
        .await
    {
        Ok(Ok(staged)) => {
            let mut response = ok_json(&staged);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Ok(Err(error)) => problem::refused(&error),
        Err(_) => internal(),
    }
}

fn print_name(raw: &str) -> Option<(String, &'static str)> {
    let name = raw.rsplit(['/', '\\']).next()?.trim();
    let extension = [".gcode.3mf", ".3mf", ".gcode", ".gco"]
        .into_iter()
        .find(|ext| {
            name.get(name.len().saturating_sub(ext.len())..)
                .is_some_and(|tail| tail.eq_ignore_ascii_case(ext))
        })?;
    let stem = name[..name.len() - extension.len()].trim_matches([' ', '.']);
    let stem: String = stem
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DISPLAY_NAME_CHARS)
        .collect();
    Some((
        if stem.is_empty() {
            "print".into()
        } else {
            stem
        },
        extension,
    ))
}

fn connect_url(path: &std::path::Path, name: &str) -> Result<String, StageFailure> {
    let path = path
        .to_str()
        .filter(|_| path.is_absolute())
        .ok_or(StageFailure::Unavailable)?;
    let mut url = reqwest::Url::parse("bambu-connect://import-file")
        .map_err(|_| StageFailure::Unavailable)?;
    url.query_pairs_mut()
        .extend_pairs([("path", path), ("name", name), ("version", "1.0.0")])
        .finish();
    Ok(url.into())
}
