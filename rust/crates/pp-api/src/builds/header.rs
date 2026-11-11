//! Node's PATCH /plans/{id} and POST /plans/{id}/archive through the Build
//! router (GRE-421 part 2).

use super::http::{
    App, Received, Refusal, SummaryContract, failure, missing_build, number_build_id, run,
    unreadable,
};
use axum::{
    extract::{Path, State},
    http::{Method, StatusCode},
    response::Response,
};
use pp_storage::build_graph::{ArchiveRefusal, BuildCommand, BuildPatch, SpecialRequest};
use serde::de::IgnoredAny;
use serde_json::Value;

/// Applies `patch` to the Build `id` names, or 404 when it names none.
async fn apply(
    app: App,
    received: Received,
    method: Method,
    id: &str,
    patch: BuildPatch,
) -> Response {
    let Some(build) = number_build_id(id) else {
        return missing_build(app, received.presented).await;
    };
    let legacy = received.contract == SummaryContract::LegacyV1;
    run(
        app,
        received.presented,
        method,
        received.path,
        received.contract,
        BuildCommand::Update {
            build,
            patch,
            legacy,
        },
    )
    .await
}

pub(super) async fn update(
    State(app): State<App>,
    Path(id): Path<String>,
    received: Received,
) -> Response {
    let fields = match serde_json::from_slice::<Value>(&received.bytes) {
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
    apply(app, received, Method::PATCH, &id, patch).await
}

pub(super) async fn archive(
    State(app): State<App>,
    Path(id): Path<String>,
    received: Received,
) -> Response {
    if received.json && serde_json::from_slice::<IgnoredAny>(&received.bytes).is_err() {
        return unreadable();
    }
    let patch = BuildPatch {
        archive: true,
        ..BuildPatch::default()
    };
    apply(app, received, Method::POST, &id, patch).await
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

/// Create a detached experiment; accepted choices become an editable draft.
pub(super) async fn duplicate(
    State(app): State<App>,
    Path(id): Path<String>,
    received: Received,
) -> Response {
    let Some(build) = number_build_id(&id) else {
        return missing_build(app, received.presented).await;
    };
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Duplicate {
        name: pp_storage::build_graph::BuildName,
    }
    let Ok(body) = serde_json::from_slice::<Duplicate>(&received.bytes) else {
        return unreadable();
    };
    if received.contract == SummaryContract::LegacyV1 {
        return failure(StatusCode::CONFLICT, "Duplicate this Build through /api/v2");
    }
    run(
        app,
        received.presented,
        Method::POST,
        received.path,
        received.contract,
        BuildCommand::Duplicate {
            build,
            name: body.name,
        },
    )
    .await
}
