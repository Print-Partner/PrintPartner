use super::{App, attachment, build_id, exports, problem};
use crate::{
    loopback::PresentedCredential,
    problem::{Problem, ProblemCode, ToProblem},
};
use axum::{
    extract::{Path, Query, Request, State},
    http::StatusCode,
    response::Response,
};
use pp_services::exports::three_mf::{Bundle, MAX_OBJECTS, Object, Refusal};
use pp_storage::{
    WriterFailure,
    accepted_artifacts::{ArtifactFailure, ArtifactOpenMode, OpenedArtifact},
    plates::{AcceptedMaterial, ManagedExport, ManagedExportFailure},
    production::geometry::{GeometryLimits, MeshFailure, ParsedMesh, dimensions_um, mesh},
    read_model::{Artifact, ReadClient, views::catalog_color},
};
use serde::Deserialize;
use serde_json::{Map, json};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TRIANGLES: usize = 5_000_000;
const MAX_OUTPUT_BYTES: usize = 128 * 1024 * 1024;

#[derive(Deserialize)]
pub(super) struct RevisionQuery {
    plate_revision_id: Option<String>,
}

pub(super) async fn download(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<RevisionQuery>,
    request: Request,
) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return problem::response(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let safe_id = |id: &str| build_id(id).and_then(|id| i64::try_from(id.get()).ok());
    let Some(revision) = query.plate_revision_id.as_deref().and_then(safe_id) else {
        return problem::refused(&Failure::RevisionRequired);
    };
    let profile = safe_id(&id);
    let result = app
        .gate
        .blocking(move |cancelled, wait| -> Result<_, Failure> {
            let captured = app
                .client
                .read_managed_export(
                    presented.into_credential(),
                    profile,
                    revision,
                    MAX_OBJECTS,
                    &cancelled,
                    wait,
                )
                .map_err(Failure::Capture)?;
            let _permit = app
                .managed_render
                .try_acquire()
                .map_err(|_| Failure::Busy)?;
            let filename = format!(
                "{}-v{}-plates-r{}.zip",
                exports::slug(&captured.plan.profile.name),
                captured.plan.plan_version,
                captured.plate_revision_number,
            );
            let bytes = render(&app.client, captured, None, &cancelled)?;
            Ok((filename, bytes))
        })
        .await;
    match result {
        Ok(Ok((filename, bytes))) => attachment("application/zip", &filename, bytes),
        Ok(Err(failure)) => failure.response(),
        Err(_) => problem::refused(&Failure::Encode),
    }
}

pub(super) fn render(
    client: &ReadClient,
    captured: ManagedExport,
    ordinal: Option<u32>,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Failure> {
    let plan = &captured.plan;
    let members: HashMap<_, _> = plan
        .parts
        .iter()
        .filter(|part| part.included)
        .flat_map(|part| {
            part.units
                .iter()
                .filter(|unit| unit.required)
                .map(move |unit| (unit.token.as_str(), (part, unit.object_name.as_str())))
        })
        .collect();
    let mut source_bytes = MAX_SOURCE_BYTES;
    let mut triangles = MAX_TRIANGLES;
    let mut bundle = Bundle::new(MAX_OUTPUT_BYTES);
    let mut plates = Vec::with_capacity(captured.plates.len());
    for plate in captured.plates {
        if ordinal.is_some_and(|ordinal| ordinal != plate.ordinal) {
            continue;
        }
        let info = plate.input;
        let mut model = bundle.model().map_err(Failure::encode)?;
        let mut units = Vec::with_capacity(info.units.len());
        for placement in info.units {
            let token = placement.token.as_str();
            let &(part, name) = members
                .get(token)
                .ok_or(Failure::Capture(ManagedExportFailure::InvalidLayout))?;
            let max_bytes = source_bytes.min(MAX_ARTIFACT_BYTES);
            let bytes = match client.open_artifact(
                &part.artifact,
                ArtifactOpenMode::Bounded {
                    max_bytes,
                    cancelled,
                },
            ) {
                Ok(OpenedArtifact::Bounded(bytes)) => bytes,
                Ok(_) => return Err(Failure::Encode),
                Err(ArtifactFailure::TooLarge { .. }) => {
                    return Err(Failure::Limit("source_bytes"));
                }
                Err(ArtifactFailure::Cancelled) => return Err(Failure::Cancelled),
                Err(reason) => {
                    return Err(Failure::Source {
                        token: token.into(),
                        reason,
                    });
                }
            };
            source_bytes -= bytes.len() as u64;
            let (vertices, bounds) = mesh(
                &bytes,
                GeometryLimits {
                    max_bytes: bytes.len(),
                    max_triangles: triangles,
                },
            )
            .map_err(|failure| match failure {
                MeshFailure::TooManyTriangles => Failure::Limit("triangles"),
                MeshFailure::Invalid(_) => Failure::Geometry(token.into()),
            })?;
            let count = vertices.len() / 3;
            triangles -= count;
            let dims = dimensions_um(&ParsedMesh {
                triangles: count,
                bounds,
            })
            .map_err(|_| Failure::Geometry(token.into()))?;
            if [
                dims.width_um.get(),
                dims.depth_um.get(),
                dims.height_um.get(),
            ] != [placement.width_um, placement.depth_um, placement.height_um]
                .map(|d| u64::from(d.get()))
            {
                return Err(Failure::Geometry(token.into()));
            }
            let material = captured
                .materials
                .get(&part.revision_part_id)
                .ok_or(Failure::Capture(ManagedExportFailure::InvalidLayout))?;
            let color = color(material);
            let object_id = model
                .object(Object {
                    token,
                    name,
                    color,
                    position_um: [placement.x_um.get(), placement.y_um.get()],
                    minimum: [bounds.min_x, bounds.min_y, bounds.min_z],
                    vertices: &vertices,
                })
                .map_err(|err| match err {
                    Refusal::NoTriangles => Failure::Geometry(token.into()),
                    other => Failure::encode(other),
                })?;
            let Artifact::Tracked {
                expected_sha256,
                source_id,
                source_revision_id,
                relative_path,
                ..
            } = &part.artifact
            else {
                return Err(Failure::Encode);
            };
            units.push(json!({
                "token": token, "object_name": name, "object_id": object_id,
                "filament_color_id": material.color_id, "filament_hex": color,
                "source_id": source_id, "source_revision_id": source_revision_id,
                "relative_path": relative_path, "artifact_sha256": expected_sha256,
            }));
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        if ordinal.is_some() {
            return model.into_3mf(MAX_OUTPUT_BYTES).map_err(Failure::encode);
        }
        let entry = bundle
            .plate(plate.ordinal, model)
            .map_err(Failure::encode)?;
        plates.push(json!({
            "ordinal": plate.ordinal, "plate_id": info.plate_id.as_str(), "entry": entry,
            "printer": { "id": info.printer_id, "name": info.printer_name, "model": info.printer_model },
            "units": units,
        }));
    }
    if ordinal.is_some() {
        return Err(Failure::PlateNotFound);
    }
    bundle
        .finish(&json!({
            "format": "accepted-plate-3mf-manifest-v1",
            "basis": {
                "profile_id": plan.profile.id, "plan_version": plan.plan_version,
                "plan_revision_id": plan.revision_id, "plan_revision_digest": plan.revision_digest,
                "required_unit_mapping_digest": plan.required_unit_mapping_digest,
                "plate_revision_id": captured.plate_revision_id,
                "plate_revision_number": captured.plate_revision_number,
                "layout_digest": captured.layout_digest,
            },
            "plates": plates,
        }))
        .map_err(Failure::encode)
}

fn color(material: &AcceptedMaterial) -> Option<&str> {
    material
        .custom_hex
        .as_deref()
        .or_else(|| {
            material
                .color_id
                .as_deref()
                .and_then(catalog_color)
                .map(|color| color.hex.as_str())
        })
        .filter(|hex| {
            hex.len() == 7
                && hex.starts_with('#')
                && hex.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
        })
}

#[derive(Debug)]
pub(super) enum Failure {
    Busy,
    RevisionRequired,
    PlateNotFound,
    Capture(ManagedExportFailure),
    Limit(&'static str),
    Source {
        token: String,
        reason: ArtifactFailure,
    },
    Geometry(String),
    Encode,
    Cancelled,
}

impl Failure {
    fn encode(error: Refusal) -> Self {
        match error {
            Refusal::OutputLimit => Self::Limit("output_bytes"),
            _ => Self::Encode,
        }
    }

    pub(super) fn response(&self) -> Response {
        let fields = match self {
            Self::Busy => return problem::overloaded("Another managed Plate export is running"),
            Self::Limit(limit) => json!({"limit": limit}),
            Self::Capture(ManagedExportFailure::TooManyObjects) => json!({"limit": "objects"}),
            Self::Source { token, reason } => json!({"token": token, "reason": reason.code()}),
            Self::Geometry(token) => json!({"token": token}),
            _ => return problem::refused(self),
        };
        problem::build(
            self.to_problem(),
            fields.as_object().cloned().unwrap_or_else(Map::new),
        )
    }
}

impl ToProblem for Failure {
    fn to_problem(&self) -> Problem {
        use ManagedExportFailure as Capture;
        use ProblemCode as Code;
        use StatusCode as Status;
        let (status, detail, code) = match self {
            Self::RevisionRequired => (
                Status::BAD_REQUEST,
                "plate_revision_id must name the Plate revision to export",
                Code::PlateRevisionRequired,
            ),
            Self::PlateNotFound => {
                return problem::problem(Status::NOT_FOUND, "Managed Plate not found", None);
            }
            Self::Capture(Capture::Storage(failure)) => return failure.to_problem(),
            Self::Capture(Capture::BuildNotFound) => {
                return problem::problem(Status::NOT_FOUND, "Build not found", None);
            }
            Self::Capture(Capture::NoAcceptedPlan) => {
                return problem::problem(
                    Status::CONFLICT,
                    "This Build has no accepted Plan",
                    Some("no_accepted_plan"),
                );
            }
            Self::Capture(Capture::PlanUnavailable) => {
                return problem::problem(
                    Status::CONFLICT,
                    "The accepted Plan cannot be read",
                    Some("accepted_plan_unavailable"),
                );
            }
            Self::Capture(Capture::PlatesNotPublished) => (
                Status::CONFLICT,
                "This Plan has no managed Plates",
                Code::PlatesNotPublished,
            ),
            Self::Capture(Capture::StalePlan) => (
                Status::CONFLICT,
                "These Plates are not based on the current accepted Plan",
                Code::StaleAcceptedPlan,
            ),
            Self::Capture(Capture::RevisionChanged) => (
                Status::CONFLICT,
                "The managed Plate revision changed",
                Code::PlateRevisionChanged,
            ),
            Self::Capture(Capture::InvalidLayout) => (
                Status::CONFLICT,
                "The managed Plate layout cannot be exported",
                Code::InvalidPlateLayout,
            ),
            Self::Capture(Capture::UnplacedUnits) => (
                Status::CONFLICT,
                "Place every unit on the managed Plates before exporting",
                Code::UnplacedUnits,
            ),
            Self::Limit(_) | Self::Capture(Capture::TooManyObjects) => (
                Status::UNPROCESSABLE_ENTITY,
                "The managed Plate export exceeds a size limit",
                Code::ThreeMfTooLarge,
            ),
            Self::Source { .. } => (
                Status::CONFLICT,
                "An accepted Source file is unavailable or changed",
                Code::SourceFilesUnavailable,
            ),
            Self::Geometry(_) => (
                Status::CONFLICT,
                "The accepted Source geometry does not match its placement",
                Code::ArtifactGeometryMismatch,
            ),
            Self::Encode => {
                return problem::problem(
                    Status::INTERNAL_SERVER_ERROR,
                    "Could not generate the managed Plate export",
                    None,
                );
            }
            Self::Busy => return WriterFailure::Busy.to_problem(),
            Self::Cancelled => return WriterFailure::Cancelled.to_problem(),
        };
        problem::problem(status, detail, None).with_problem_code(code)
    }
}
