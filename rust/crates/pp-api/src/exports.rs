//! Accepted Plan exports (GRE-1447): `GET /api/v2/plans/{id}/exports/checklist`
//! answers one self-contained HTML attachment, built from one reader-pool read
//! per request. No job runs and nothing is stored.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, build_id},
    loopback::PresentedCredential,
    problem,
};
use axum::{
    Router,
    extract::{Path, Request, State},
    http::{StatusCode, header},
    response::Response,
    routing::get,
};
use pp_services::exports::{self, ChecklistPart, ChecklistPlan, Colour};
use pp_storage::{
    WriterFailure,
    read_model::{AcceptedRead, Part, ReadClient, Snapshot, views},
};

#[derive(Clone)]
struct App {
    gate: Gate,
    client: ReadClient,
}

/// Mounts the export routes under `/api/v2` when `PP_DESKTOP_REBUILD` was
/// exactly `1` or `true`; otherwise nothing.
pub fn exports_router(config: AcceptedHttpConfig, client: ReadClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new().route("/plans/{id}/exports/checklist", get(checklist));
    gate.guard(Router::new().nest("/api/v2", routes))
        .with_state(App { gate, client })
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
        }
    }
}

async fn checklist(State(app): State<App>, Path(id): Path<String>, request: Request) -> Response {
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
            let checklist = exports::checklist(&checklist_plan(&snapshot), &generated);
            attachment(
                "text/html; charset=utf-8",
                &checklist.filename,
                checklist.html.into_bytes(),
            )
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
    let presented = request
        .extensions()
        .get::<PresentedCredential>()
        .cloned()
        .ok_or(ExportFailure::Storage(WriterFailure::Auth(
            pp_storage::auth::AuthFailure::SessionRequired,
        )))?;
    let build = build_id(id).and_then(|id| i64::try_from(id.get()).ok());
    let client = app.client.clone();
    let read = app
        .gate
        .blocking(move |cancelled, wait| {
            client.read_plan(presented.into_credential(), build, &cancelled, wait)
        })
        .await
        .map_err(|_| ExportFailure::Interrupted)?
        .map_err(ExportFailure::Storage)?;
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

/// The checklist input: included Parts, with the quantity and printed count
/// `GET /api/v2/plans/{id}/checkoff/units` reports for each.
fn checklist_plan(snapshot: &Snapshot) -> ChecklistPlan {
    ChecklistPlan {
        build_name: snapshot.profile.name.clone(),
        order_number: snapshot.profile.order_number.clone(),
        plan_version: snapshot.plan_version,
        parts: snapshot
            .parts
            .iter()
            .filter(|part| part.included)
            .map(|part| ChecklistPart {
                source: part.source_layer.clone(),
                relative_path: part.relative_path.clone(),
                filename: part.filename.clone(),
                role: part.effective_role.clone(),
                colour: colour(part),
                quantity: part.units.len() as u64,
                printed: part.units.iter().filter(|unit| unit.completed).count() as u64,
                notes: part.notes.clone(),
            })
            .collect(),
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
