//! Node's PATCH /plans/{id} and POST /plans/{id}/archive through the Build
//! router (GRE-421 part 2).

use super::http::{App, Refusal, SummaryContract, failure, missing_build, run, unreadable};
use crate::loopback::PresentedCredential;
use axum::{
    body::to_bytes,
    extract::{Path, State},
    http::{Method, StatusCode, header},
    response::Response,
};
use pp_storage::build_graph::{
    ArchiveRefusal, BuildCommand, BuildPatch, PositiveId, SpecialRequest,
};
use serde::de::IgnoredAny;
use serde_json::Value;

/// Node reads the id with `Number(id)`, so `0x1` and `" 1 "` name Build 1.
fn parse_build_id(id: &str) -> Option<PositiveId> {
    let id = pp_storage::plates::build_id(id)??;
    PositiveId::new(u64::try_from(id).ok()?).ok()
}

pub(super) async fn update(
    State(app): State<App>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let path = request.uri().path().to_owned();
    let contract = SummaryContract::of(&request);
    let limit = app.body_limit();
    let Ok(bytes) = to_bytes(request.into_body(), limit).await else {
        return failure(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
    };
    let fields = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(fields)) => fields,
        Ok(Value::Null) | Err(_) => return unreadable(),
        Ok(_) => Default::default(),
    };
    if fields.get("archived") == Some(&Value::Bool(false)) {
        return failure(
            StatusCode::BAD_REQUEST,
            "Cannot unarchive; duplicate the archived template instead",
        );
    }
    let Some(build) = parse_build_id(&id) else {
        return missing_build(app, presented).await;
    };
    let patch = BuildPatch {
        name: fields
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        special_request: fields.get("special_request").map(|value| match value {
            Value::Null => SpecialRequest::Clear,
            value if uncoercible(value) => SpecialRequest::Uncoercible,
            value => SpecialRequest::Text(crate::catalog::wire::js_string(value)),
        }),
        archive: fields.get("archived") == Some(&Value::Bool(true)),
        touch: fields.get("touch_last_used") == Some(&Value::Bool(true)),
    };
    let legacy = contract == SummaryContract::LegacyV1;
    run(
        app,
        presented,
        Method::PATCH,
        path,
        contract,
        BuildCommand::Update {
            build,
            patch,
            legacy,
        },
    )
    .await
}

pub(super) async fn archive(
    State(app): State<App>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let path = request.uri().path().to_owned();
    let contract = SummaryContract::of(&request);
    let json = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"));
    let limit = app.body_limit();
    let Ok(bytes) = to_bytes(request.into_body(), limit).await else {
        return failure(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
    };
    if json && serde_json::from_slice::<IgnoredAny>(&bytes).is_err() {
        return unreadable();
    }
    let Some(build) = parse_build_id(&id) else {
        return missing_build(app, presented).await;
    };
    let legacy = contract == SummaryContract::LegacyV1;
    run(
        app,
        presented,
        Method::POST,
        path,
        contract,
        BuildCommand::Update {
            build,
            patch: BuildPatch {
                archive: true,
                ..BuildPatch::default()
            },
            legacy,
        },
    )
    .await
}

/// Whether JavaScript's `String(value)` throws: an object with its own
/// `toString` member, which JSON can only make non-callable, or an array
/// holding one.
fn uncoercible(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.contains_key("toString"),
        Value::Array(items) => items.iter().any(uncoercible),
        _ => false,
    }
}

pub(super) fn archive_refused(refusal: ArchiveRefusal) -> Response {
    match refusal {
        ArchiveRefusal::Remaining => failure(
            StatusCode::BAD_REQUEST,
            "Archive only when print remaining is 0",
        ),
        ArchiveRefusal::CompatibilityDirty => Refusal::CompatibilityDirty.response(),
        ArchiveRefusal::Uninitialized => Refusal::Uninitialized.response(),
        ArchiveRefusal::Integrity => Refusal::Integrity.response(),
    }
}
