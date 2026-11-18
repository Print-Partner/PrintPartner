//! The routes `client_routes_match_registrations` sends, and the JSON body it
//! sends to each route that takes one.
//!
//! [`SAMPLE_BODIES`] has one row per body route, keyed by the route's generated
//! name ([`ClientRoute::name`]) and sorted by it. A new body route adds its own
//! row; `sample_bodies_cover_every_body_route` fails when a row is missing,
//! out of order, or names no body route.

use pp_api::client_contract::ClientRoute;
use serde_json::{Value, json};

/// Every route the contract test sends: the shared client contract plus the
/// tables the probe, catalog and send queue modules own.
pub(super) fn contract_routes() -> impl Iterator<Item = &'static ClientRoute> {
    pp_api::client_contract::CLIENT_ROUTES
        .iter()
        .chain(std::iter::once(&pp_api::printer_host::probe::CLIENT_ROUTE))
        .chain(&pp_api::catalog::contract::CLIENT_ROUTES)
        .chain(&pp_api::send_queue::CLIENT_ROUTES)
}

/// Builds one sample body.
type Sample = fn() -> Value;

/// A body each route's request DTO accepts. Where a sample names an id, no
/// stored row has it, so the route answers its own not-found or conflict.
const SAMPLE_BODIES: &[(&str, Sample)] = &[
    ("abandonDraft", || json!({"expected_lifecycle_version": 0})),
    ("acceptDraft", || {
        json!({"expected_snapshot_digest": "0".repeat(64),
            "expected_lifecycle_version": 0,
            "expected_base": {"revision_id": null, "plan_version": 0}})
    }),
    ("autosaveDraft", || {
        json!({"expected_base": {"revision_id": null, "plan_version": 0},
            "expected_draft": null, "remap_checkoff_links": false, "decisions": [{"kind": "set_included",
                "target": {"part_key": "client.stl", "relative_path": "client.stl", "source_layer": null},
                "value": true}]})
    }),
    ("claimUnattributedPrint", || json!({"profile_id": 1})),
    (
        "createIntegration",
        || json!({"kind": "moonraker", "name": "Client contract", "base_url": "http://fixture.invalid"}),
    ),
    ("createPrinter", || {
        json!({"name": "Client contract", "model": "fixture",
            "bed_width_mm": 250, "bed_depth_mm": 210, "bed_height_mm": 220})
    }),
    ("createBuild", || json!({"name": "Client contract create"})),
    (
        "createSource",
        || json!({"name": "Client contract", "source_kind": "archive"}),
    ),
    ("dismissUnattributedPrint", || json!({})),
    ("editDraftParts", || {
        json!({"expected_snapshot_digest": "0".repeat(64),
            "decision": {"kind": "set_included", "draft_part_ids": [1], "value": true}})
    }),
    // An unsupported kind opens no outbound connection.
    (
        "probePrinter",
        || json!({"kind": "unsupported", "address": "http://fixture.invalid"}),
    ),
    (
        "reconcileDraft",
        || json!({"expected_snapshot_digest": "0".repeat(64), "decisions": []}),
    ),
    (
        "reconcileQueuedSend",
        || json!({"printed": false, "job_id": null}),
    ),
    (
        "reconcileSpoolmanDeduction",
        || json!({"answer": "dismiss"}),
    ),
    ("recordManualCheckoffBurst", || {
        json!({"idempotency_key": "client-contract",
            "expected": {"profile_id": 1, "plan_version": 1, "plan_revision_id": 1,
                "plan_revision_digest": "0".repeat(64), "required_unit_mapping_digest": "0".repeat(64)},
            "units": [{"token": format!("ppu_{}", "0".repeat(32)), "printed": true}]})
    }),
    // The single owner already exists, so the handler refuses a second one.
    (
        "registerOwner",
        || json!({"email": "second@example.com", "password": "password-1234"}),
    ),
    // A wrong confirmation is refused before any stage is looked up.
    (
        "restoreInstallation",
        || json!({"restore_id": "0".repeat(32), "confirmation": "yes"}),
    ),
    ("restoreSnapshotAsDraft", || {
        json!({"mode": "draft",
            "expected_base": {"revision_id": null, "plan_version": 0}, "expected_draft": null})
    }),
    ("saveDraftHeader", || {
        json!({"expected_base": {"revision_id": null, "plan_version": 0},
            "expected_draft": null, "expected_header_version": null,
            "header": {"name": "Client contract", "order_number": null, "customer": null, "due_date": null}})
    }),
    // No Source has id 1, so the handler answers its own not-found.
    ("setBuildBase", || json!({"project_id": 1})),
    ("setModule", || json!({"on": true})),
    (
        "signIn",
        || json!({"email": "client@example.com", "password": "password-1234"}),
    ),
    ("startSourceSync", || json!({"project_ids": [1]})),
    ("undoCheckoffBurst", || json!({"undo_token": "ppb_unknown"})),
    ("undoDismissUnattributedPrint", || json!({})),
    (
        "undoManualCheckoffBurst",
        || json!({"undo_token": "ppb_unknown"}),
    ),
    ("updateIntegration", || json!({"name": "Client contract"})),
    ("updatePrinter", || json!({"name": "Client contract"})),
    ("verifyCheckoffBurst", || {
        json!({"idempotency_key": "client-contract",
            "beds": [{"link_id": "no-such-link", "decisions": [
                {"part_id": 1, "unit_index": 0, "result": "confirmed"}]}]})
    }),
];

/// The sample body for the route named `name`.
pub(super) fn sample(name: &str) -> Value {
    let (_, body) = SAMPLE_BODIES
        .iter()
        .find(|(route, _)| *route == name)
        .unwrap_or_else(|| panic!("no sample body for route {name}"));
    body()
}

/// The body to send to `route`: its sample if it takes one, otherwise none.
pub(super) fn sample_body(route: &ClientRoute) -> Option<Value> {
    route.request.map(|_| sample(route.name))
}

#[test]
fn sample_bodies_cover_every_body_route() {
    let mut body_routes: Vec<&str> = contract_routes()
        .filter(|route| route.request.is_some())
        .map(|route| route.name)
        .collect();
    body_routes.sort_unstable();
    let rows: Vec<&str> = SAMPLE_BODIES.iter().map(|(name, _)| *name).collect();
    assert!(
        rows.windows(2).all(|pair| pair[0] < pair[1]),
        "SAMPLE_BODIES must be sorted by route name with no repeats: {rows:?}"
    );
    assert_eq!(rows, body_routes, "SAMPLE_BODIES rows against body routes");
}
