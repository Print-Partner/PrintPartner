//! Accepted Plan exports: `GET /api/v2/plans/{id}/exports/checklist` (GRE-1447)
//! answers one self-contained HTML attachment and `.../exports/stl-pack`
//! (GRE-1525, [`pack`]) a ZIP of the STLs. [`managed`] exports the accepted
//! Plates as 3MF; [`direct`] exports selected Required units without placements.
//! Downloads don't store files; [`slicer`] delivers a managed Plate locally.

pub mod bambu_connect;
mod direct;
mod managed;
mod pack;
mod slicer;

use crate::{
    accepted::{AcceptedHttpConfig, Gate, build_id},
    loopback::PresentedCredential,
    problem,
};
use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{StatusCode, header},
    response::Response,
    routing::{get, post},
};
use pp_services::exports::{self, ChecklistPart, ChecklistPlan, Colour, pack::PackRefusal};
use pp_services::native_slicers::Handoff;
use pp_storage::{
    WriterFailure,
    accepted_artifacts::{ArtifactFailure, ArtifactOpenMode, OpenedArtifact},
    production::geometry::GeometryLimits,
    read_model::{AcceptedRead, Artifact, Part, ReadClient, Snapshot, views},
};
use serde_json::{Map, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

const MAX_CHECKLIST_THUMBNAIL_BYTES: usize = 8 * 1024 * 1024;
static CHECKLIST_RENDER_PERMITS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[derive(Clone)]
struct App {
    gate: Gate,
    client: ReadClient,
    three_mf_render: Arc<Semaphore>,
}

/// Mounts the export routes under `/api/v2` when `PP_DESKTOP_REBUILD` was
/// exactly `1` or `true`; otherwise nothing.
pub fn exports_router(config: AcceptedHttpConfig, client: ReadClient) -> Router {
    exports_router_with_slicer(config, client, None)
}

/// Adds local native slicer routes only when the runtime provides an adapter.
pub fn exports_router_with_slicer(
    config: AcceptedHttpConfig,
    client: ReadClient,
    slicer: Option<Arc<dyn Handoff>>,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let mut routes = Router::new()
        .route("/plans/{id}/exports/checklist", get(checklist))
        .route("/plans/{id}/exports/stl-pack", get(pack::stl_pack))
        .route("/plans/{id}/exports/managed-3mf", get(managed::download))
        .route("/plans/{id}/exports/direct-3mf", get(direct::download));
    if let Some(slicer) = slicer {
        routes = routes.merge(
            Router::new()
                .route("/slicer-instances", get(slicer::picker))
                .route(
                    "/plans/{id}/exports/slicer-handoff",
                    post(slicer::deliver).layer(DefaultBodyLimit::max(4096)),
                )
                .layer(Extension(slicer)),
        );
    }
    gate.guard(Router::new().nest("/api/v2", routes))
        .with_state(App {
            gate,
            client,
            three_mf_render: Arc::new(Semaphore::new(1)),
        })
}

/// Why an export was refused.
#[derive(Debug)]
enum ExportFailure {
    Storage(WriterFailure),
    BuildNotFound,
    NoAcceptedPlan,
    /// The accepted Plan needs compatibility repair or was never initialized.
    AcceptedPlanUnavailable,
    AcceptedPlanInconsistent,
    /// The blocking read did not finish.
    Interrupted,
    /// `scope` is neither `all` nor `remaining`.
    InvalidScope,
    /// The scope leaves no Required unit to pack.
    NothingToPack,
    /// Source files are missing or no longer match their accepted SHA-256.
    SourceFilesUnavailable(Vec<pack::UnavailableFile>),
    Pack(pp_services::exports::pack::PackRefusal),
}

impl ExportFailure {
    fn response(&self) -> Response {
        match self {
            Self::Storage(failure) => problem::writer_failure(failure),
            Self::BuildNotFound => problem::response(StatusCode::NOT_FOUND, "Build not found"),
            Self::NoAcceptedPlan => problem::coded(
                StatusCode::CONFLICT,
                "This Build has no accepted Plan",
                "no_accepted_plan",
            ),
            Self::AcceptedPlanUnavailable => problem::coded(
                StatusCode::CONFLICT,
                "The accepted Plan cannot be read until it is repaired",
                "accepted_plan_unavailable",
            ),
            Self::AcceptedPlanInconsistent => problem::response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Accepted Plan data is inconsistent",
            ),
            Self::Interrupted => {
                problem::response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }
            Self::InvalidScope => problem::coded(
                StatusCode::BAD_REQUEST,
                "scope must be all or remaining",
                "invalid_scope",
            ),
            Self::NothingToPack | Self::Pack(PackRefusal::NothingToPack) => problem::coded(
                StatusCode::CONFLICT,
                "No Required unit is left to pack",
                "nothing_to_pack",
            ),
            Self::SourceFilesUnavailable(files) => problem::build(
                problem::problem(
                    StatusCode::CONFLICT,
                    "Source files are missing or changed since the Plan was accepted",
                    Some("source_files_unavailable"),
                ),
                Map::from_iter([("files".into(), json!(files))]),
            ),
            Self::Pack(PackRefusal::TooManyEntries { entries, limit }) => problem::build(
                problem::problem(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "The pack has more entries than a ZIP holds",
                    Some("pack_too_large"),
                ),
                Map::from_iter([
                    ("entries".into(), json!(entries)),
                    ("limit".into(), json!(limit)),
                ]),
            ),
            Self::Pack(PackRefusal::TooLarge { bytes, limit }) => problem::build(
                problem::problem(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "The pack is larger than a ZIP holds",
                    Some("pack_too_large"),
                ),
                Map::from_iter([
                    ("bytes".into(), json!(bytes)),
                    ("limit".into(), json!(limit)),
                ]),
            ),
        }
    }
}

async fn checklist(State(app): State<App>, Path(id): Path<String>, request: Request) -> Response {
    let permit = match CHECKLIST_RENDER_PERMITS.acquire().await {
        Ok(permit) => permit,
        Err(_) => return ExportFailure::Interrupted.response(),
    };
    match accepted_snapshot(&app, &id, request).await {
        Ok(snapshot) => {
            let generated = time::OffsetDateTime::now_utc()
                .replace_nanosecond(0)
                .ok()
                .and_then(|now| {
                    now.format(&time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .unwrap_or_default();
            let client = app.client.clone();
            let rendered = match app
                .gate
                .blocking(move |cancelled, _| {
                    let plan = checklist_plan(&client, &snapshot, &cancelled);
                    (plan, permit)
                })
                .await
            {
                Ok(rendered) => rendered,
                Err(_) => return ExportFailure::Interrupted.response(),
            };
            match rendered {
                (Ok(plan), _permit) => {
                    let checklist = exports::checklist(&plan, &generated);
                    attachment(
                        "text/html; charset=utf-8",
                        &checklist.filename,
                        checklist.html.into_bytes(),
                    )
                }
                (Err(failure), _permit) => failure.response(),
            }
        }
        Err(failure) => failure.response(),
    }
}

/// The accepted Plan `id` names, read once on the reader pool with the
/// request's credential. An id that names no Build still checks the
/// credential, then answers not found.
async fn accepted_snapshot(
    app: &App,
    id: &str,
    request: Request,
) -> Result<Box<Snapshot>, ExportFailure> {
    ready_snapshot(read_accepted(app, id, request, ReadClient::read_plan).await?)
}

async fn read_accepted<T: Send + 'static>(
    app: &App,
    id: &str,
    request: Request,
    read: impl FnOnce(
        &ReadClient,
        pp_storage::read_model::Credential,
        Option<i64>,
        &AtomicBool,
        Duration,
    ) -> Result<T, WriterFailure>
    + Send
    + 'static,
) -> Result<T, ExportFailure> {
    let presented = request
        .extensions()
        .get::<PresentedCredential>()
        .cloned()
        .ok_or(ExportFailure::Storage(WriterFailure::Auth(
            pp_storage::auth::AuthFailure::SessionRequired,
        )))?;
    let build = build_id(id).and_then(|id| i64::try_from(id.get()).ok());
    let client = app.client.clone();
    app.gate
        .blocking(move |cancelled, wait| {
            read(
                &client,
                presented.into_credential(),
                build,
                &cancelled,
                wait,
            )
        })
        .await
        .map_err(|_| ExportFailure::Interrupted)?
        .map_err(ExportFailure::Storage)
}

fn ready_snapshot(read: AcceptedRead) -> Result<Box<Snapshot>, ExportFailure> {
    match read {
        AcceptedRead::Ready { snapshot } => Ok(snapshot),
        AcceptedRead::Missing => Err(ExportFailure::BuildNotFound),
        AcceptedRead::Empty { .. } => Err(ExportFailure::NoAcceptedPlan),
        AcceptedRead::CompatibilityDirty | AcceptedRead::Uninitialized => {
            Err(ExportFailure::AcceptedPlanUnavailable)
        }
        AcceptedRead::IntegrityFailure { .. } => Err(ExportFailure::AcceptedPlanInconsistent),
    }
}

fn required_members(plan: &Snapshot) -> HashMap<&str, (&Part, &str)> {
    plan.parts
        .iter()
        .filter(|part| part.included)
        .flat_map(|part| {
            part.units
                .iter()
                .filter(|unit| unit.required)
                .map(move |unit| (unit.token.as_str(), (part, unit.object_name.as_str())))
        })
        .collect()
}

/// The checklist input: included Parts, with the quantity and printed count
/// `GET /api/v2/plans/{id}/checkoff/units` reports for each.
fn checklist_plan(
    client: &ReadClient,
    snapshot: &Snapshot,
    cancelled: &AtomicBool,
) -> Result<ChecklistPlan, ExportFailure> {
    let mut remaining_thumbnail_bytes = MAX_CHECKLIST_THUMBNAIL_BYTES;
    let mut thumbnail_budget_exhausted = false;
    let mut parts = Vec::new();
    for part in snapshot.parts.iter().filter(|part| part.included) {
        if cancelled.load(Ordering::Acquire) {
            return Err(ExportFailure::Interrupted);
        }
        let thumbnail_png = if thumbnail_budget_exhausted {
            None
        } else {
            match accepted_thumbnail(client, &part.artifact, cancelled)? {
                Some(png) => {
                    let accepted = reserve_thumbnail(&mut remaining_thumbnail_bytes, png);
                    if accepted.is_none() || remaining_thumbnail_bytes == 0 {
                        thumbnail_budget_exhausted = true;
                    }
                    accepted
                }
                None => None,
            }
        };
        if cancelled.load(Ordering::Acquire) {
            return Err(ExportFailure::Interrupted);
        }
        parts.push(ChecklistPart {
            source: part.source_layer.clone(),
            relative_path: part.relative_path.clone(),
            filename: part.filename.clone(),
            role: part.effective_role.clone(),
            colour: colour(part),
            quantity: part.units.len() as u64,
            printed: part.units.iter().filter(|unit| unit.completed).count() as u64,
            notes: part.notes.clone(),
            thumbnail_png,
        });
    }
    Ok(ChecklistPlan {
        build_name: snapshot.profile.name.clone(),
        order_number: snapshot.profile.order_number.clone(),
        plan_version: snapshot.plan_version,
        parts,
    })
}

fn accepted_thumbnail(
    client: &ReadClient,
    artifact: &Artifact,
    cancelled: &AtomicBool,
) -> Result<Option<Vec<u8>>, ExportFailure> {
    let limits = GeometryLimits::default();
    let stl = match client.open_artifact(
        artifact,
        ArtifactOpenMode::Bounded {
            max_bytes: limits.max_bytes as u64,
            cancelled,
        },
    ) {
        Ok(OpenedArtifact::Bounded(stl)) => stl,
        Ok(OpenedArtifact::Stream { .. }) => return Ok(None),
        Err(ArtifactFailure::Cancelled) => return Err(ExportFailure::Interrupted),
        Err(_) => return Ok(None),
    };
    if cancelled.load(Ordering::Acquire) {
        return Err(ExportFailure::Interrupted);
    }
    Ok(pp_storage::production::thumbnail::render(&stl, limits).ok())
}

fn reserve_thumbnail(remaining: &mut usize, png: Vec<u8>) -> Option<Vec<u8>> {
    if png.len() > *remaining {
        *remaining = 0;
        None
    } else {
        *remaining -= png.len();
        Some(png)
    }
}

/// A catalog colour by name; otherwise the custom hex, the Spoolman spool or
/// the stored id as text.
fn colour(part: &Part) -> Colour {
    if let Some(color) = views::part_catalog_color(part.filament_color_id.as_deref()) {
        return Colour {
            label: color.display_name.clone(),
            hex: Some(color.hex.clone()),
        };
    }
    let present = |value: &Option<String>| value.clone().filter(|v| !v.trim().is_empty());
    let hex = present(&part.filament_custom_hex);
    let label = match (
        present(&part.spoolman_spool_id),
        present(&part.filament_color_id),
    ) {
        (Some(spool), _) => format!("Spoolman spool #{spool}"),
        (None, Some(id)) => id,
        (None, None) if hex.is_some() => "Custom".into(),
        (None, None) => String::new(),
    };
    Colour { label, hex }
}

fn attachment(content_type: &str, filename: &str, body: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .header(
            header::CONTENT_SECURITY_POLICY,
            exports::CONTENT_SECURITY_POLICY,
        )
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_LENGTH, body.len())
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| ExportFailure::Interrupted.response())
}

#[cfg(test)]
mod tests {
    use super::reserve_thumbnail;

    #[test]
    fn thumbnail_budget_stops_at_the_first_image_that_does_not_fit() {
        let mut remaining = 3;
        assert_eq!(
            reserve_thumbnail(&mut remaining, vec![1, 2]),
            Some(vec![1, 2])
        );
        assert_eq!(remaining, 1);
        assert_eq!(reserve_thumbnail(&mut remaining, vec![3, 4]), None);
        assert_eq!(remaining, 0);
        assert_eq!(reserve_thumbnail(&mut remaining, vec![5]), None);
    }
}
