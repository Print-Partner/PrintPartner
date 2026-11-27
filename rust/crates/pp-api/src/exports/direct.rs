use super::{
    App, accepted_snapshot, attachment, exports,
    managed::{Failure, GeometryBudget, MAX_OUTPUT_BYTES, color},
    required_members,
};
use axum::{
    extract::{Path, Query, Request, State, rejection::QueryRejection},
    response::Response,
};
use pp_services::exports::three_mf::{DirectLayout, Model, Object, Refusal};
use pp_storage::{
    production::model::RequiredUnitToken,
    read_model::{ReadClient, Snapshot},
};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_SELECTED: usize = 256;

#[derive(Deserialize)]
pub(super) struct Selection {
    tokens: Option<String>,
}

pub(super) async fn download(
    State(app): State<App>,
    Path(id): Path<String>,
    query: Result<Query<Selection>, QueryRejection>,
    request: Request,
) -> Response {
    let plan = match accepted_snapshot(&app, &id, request).await {
        Ok(plan) => plan,
        Err(failure) => return failure.response(),
    };
    let selected = match query
        .map_err(|_| Failure::InvalidSelection)
        .and_then(|Query(query)| selection(query.tokens.as_deref().unwrap_or_default()))
    {
        Ok(selected) => selected,
        Err(failure) => return failure.response(),
    };
    let filename = format!(
        "{}-v{}-direct.3mf",
        exports::slug(&plan.profile.name),
        plan.plan_version,
    );
    let result = app
        .gate
        .blocking(move |cancelled, _| {
            let _permit = app
                .three_mf_render
                .try_acquire()
                .map_err(|_| Failure::Busy)?;
            render(&app.client, &plan, selected, &cancelled)
        })
        .await;
    match result {
        Ok(Ok(bytes)) => attachment("model/3mf", &filename, bytes),
        Ok(Err(failure)) => failure.response(),
        Err(_) => Failure::Encode.response(),
    }
}

fn selection(tokens: &str) -> Result<BTreeSet<RequiredUnitToken>, Failure> {
    let mut selected = BTreeSet::new();
    for (index, token) in tokens.split(',').enumerate() {
        if index == MAX_SELECTED {
            return Err(Failure::Limit("objects"));
        }
        let token = RequiredUnitToken::parse(token).map_err(|_| Failure::InvalidSelection)?;
        if !selected.insert(token) {
            return Err(Failure::InvalidSelection);
        }
    }
    Ok(selected)
}

fn render(
    client: &ReadClient,
    plan: &Snapshot,
    selected: BTreeSet<RequiredUnitToken>,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Failure> {
    let members = required_members(plan);
    let selected = selected
        .iter()
        .map(|token| {
            members
                .get(token.as_str())
                .map(|&(part, name)| (token.as_str(), part, name))
                .ok_or_else(|| Failure::UnknownUnit(token.as_str().into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut budget = GeometryBudget::new();
    let mut layout = DirectLayout::default();
    let mut model = Model::new(MAX_OUTPUT_BYTES).map_err(Failure::encode)?;
    for (token, part, name) in selected {
        let geometry = budget.load(client, token, &part.artifact, cancelled)?;
        let position_um = layout
            .place(geometry.dimensions)
            .map_err(|_| Failure::PackingConflict(token.into()))?;
        model
            .object(Object {
                token,
                name,
                color: color(
                    part.filament_custom_hex.as_deref(),
                    part.filament_color_id.as_deref(),
                ),
                position_um,
                minimum: geometry.minimum,
                vertices: &geometry.vertices,
            })
            .map_err(|error| match error {
                Refusal::NoTriangles => Failure::Geometry(token.into()),
                other => Failure::encode(other),
            })?;
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(Failure::Cancelled);
    }
    model.package(MAX_OUTPUT_BYTES).map_err(Failure::encode)
}
