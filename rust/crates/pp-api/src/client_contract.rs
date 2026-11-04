//! Routes and DTOs that `web/packages/rust-client` is generated from.
//!
//! Each [`ClientRoute`] names a route registered by [`crate::builds`],
//! [`crate::drafts`] or [`crate::auth`]. `pp-core`'s
//! `client_routes_match_registrations` test sends every entry to the composed
//! routers and checks that it is registered with the stated authentication.
//!
//! The document's `descriptors` list holds constant tables the generator writes
//! out as data: today only the printer display descriptors.

/// What a request must present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAuth {
    /// Loopback admission only; no session or API key.
    None,
    /// A `pp_session` cookie or an API key.
    Credential,
}

impl ClientAuth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Credential => "credential",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ClientRoute {
    /// The generated TypeScript route key.
    pub name: &'static str,
    pub method: &'static str,
    /// Path template with `{param}` segments, as registered.
    pub path: &'static str,
    pub auth: ClientAuth,
    /// DTO name of the JSON request body, if the route takes one.
    pub request: Option<&'static str>,
    /// DTO name of the 200 response body.
    pub response: &'static str,
}

const fn route(
    name: &'static str,
    method: &'static str,
    path: &'static str,
    request: Option<&'static str>,
    response: &'static str,
) -> ClientRoute {
    ClientRoute {
        name,
        method,
        path,
        auth: ClientAuth::Credential,
        request,
        response,
    }
}

pub const CLIENT_ROUTES: &[ClientRoute] = &[
    route("listBuilds", "GET", "/api/v2/plans", None, "ProfileList"),
    route(
        "readBuild",
        "GET",
        "/api/v2/plans/{id}",
        None,
        "ProfileSummary",
    ),
    route(
        "listBuildLayers",
        "GET",
        "/api/v2/plans/{id}/layers",
        None,
        "ProfileLayers",
    ),
    route(
        "listDrafts",
        "GET",
        "/api/v2/plans/{id}/drafts",
        None,
        "DraftList",
    ),
    route(
        "readDraftWorkspace",
        "GET",
        "/api/v2/plans/{id}/drafts/{draft_id}",
        None,
        "DraftWorkspace",
    ),
    route(
        "editDraftParts",
        "PATCH",
        "/api/v2/plans/{id}/drafts/{draft_id}/parts",
        Some("EditDraftPartsRequest"),
        "DraftWorkspace",
    ),
    route(
        "abandonDraft",
        "POST",
        "/api/v2/plans/{id}/drafts/{draft_id}/abandon",
        Some("AbandonDraftRequest"),
        "DraftIdentity",
    ),
    ClientRoute {
        name: "readHealth",
        method: "GET",
        path: "/health",
        auth: ClientAuth::None,
        request: None,
        response: "Health",
    },
];

#[cfg(feature = "codegen")]
pub use codegen::{document, document_bytes};

#[cfg(feature = "codegen")]
mod codegen {
    use super::CLIENT_ROUTES;
    use crate::problem::{FieldError, MAX_FIELD_ERRORS, Problem, ProblemType};
    use pp_storage::printer_display_state::{
        BedClearState, PrinterDisplayDescriptor, PrinterDisplayState, PrinterExecutionState,
        PrinterStateView,
    };
    use pp_storage::{
        build_graph::wire::{
            AcceptedProgressSummary, AcceptedProgressUnavailableReason, FreshnessReasons,
            PlanFreshness, PlanStaleReason, PlanUntrackedReason, ProfileLayer, ProfileLayers,
            ProfileList, ProfileSummary,
        },
        required_units::{
            AcceptedReference, Change, ConflictView, Diff, DraftBase, DraftIdentity,
            DraftListResponse, PartView, ReconciliationView, Workspace,
        },
        working_drafts::{AbandonRequest, EditRequest},
    };
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::{Map, Value, json};

    const MAX_SAFE: i64 = 9_007_199_254_740_991;
    const UNICODE: &str = "Ünïcødé 🧵 \u{200d} 層";

    /// Schemas, route metadata and Rust-serialized fixtures for the client
    /// generator. Fixtures are test input only; they do not reach the package.
    pub fn document() -> Result<Value, serde_json::Error> {
        let mut dtos = Map::new();
        let mut schema = |name: &str, value: schemars::schema::RootSchema| {
            serde_json::to_value(value).map(|value| dtos.insert(name.into(), value))
        };
        schema(
            "ProfileList",
            schemars::schema_for!(ProfileList<ProfileSummary>),
        )?;
        schema("ProfileSummary", schemars::schema_for!(ProfileSummary))?;
        schema("ProfileLayers", schemars::schema_for!(ProfileLayers))?;
        schema("DraftList", schemars::schema_for!(DraftListResponse))?;
        schema("DraftWorkspace", schemars::schema_for!(Workspace))?;
        schema("DraftIdentity", schemars::schema_for!(DraftIdentity))?;
        schema("EditDraftPartsRequest", schemars::schema_for!(EditRequest))?;
        schema("AbandonDraftRequest", schemars::schema_for!(AbandonRequest))?;
        schema("Health", schemars::schema_for!(crate::auth::Health))?;
        schema("PrinterState", schemars::schema_for!(PrinterStateView))?;
        schema(
            "PrinterDisplayDescriptor",
            schemars::schema_for!(PrinterDisplayDescriptor),
        )?;
        schema("Problem", schemars::schema_for!(Problem))?;
        schema("ProblemType", schemars::schema_for!(ProblemType))?;
        let routes: Vec<Value> = CLIENT_ROUTES
            .iter()
            .map(|route| {
                json!({"name": route.name, "method": route.method, "path": route.path,
                    "auth": route.auth.as_str(), "request": route.request, "response": route.response})
            })
            .collect();
        // Emitted as a TypeScript record keyed by `key`; rows in Rust order.
        let descriptors = json!([{"name": "printerDisplayDescriptors",
            "dto": "PrinterDisplayDescriptor", "key": "state",
            "values": printer_display_descriptors()}]);
        Ok(json!({
            "generator": "rust-client-zod/1",
            "schemars": "0.8.22",
            "dialect": "http://json-schema.org/draft-07/schema#",
            "dtos": dtos,
            "routes": routes,
            "descriptors": descriptors,
            "fixtures": fixtures()?,
        }))
    }

    fn printer_display_descriptors() -> Vec<PrinterDisplayDescriptor> {
        PrinterDisplayState::ALL
            .iter()
            .map(|state| state.descriptor())
            .collect()
    }

    /// One printer state per display summary.
    fn printer_states() -> Vec<PrinterStateView> {
        use BedClearState as Bed;
        use PrinterExecutionState as Execution;
        [
            (Execution::Printing, Bed::NeedsClearing),
            (Execution::Idle, Bed::NeedsClearing),
            (Execution::Idle, Bed::Clear),
            (Execution::Idle, Bed::Unknown),
            (Execution::Paused, Bed::Unknown),
            (Execution::Failed, Bed::NeedsClearing),
            (Execution::Unreachable, Bed::Unknown),
            (Execution::NotSetUp, Bed::Unknown),
            (Execution::Manual, Bed::Clear),
            (Execution::Unknown, Bed::Unknown),
        ]
        .into_iter()
        .map(|(execution, bed_clear)| PrinterStateView::new(execution, bed_clear))
        .collect()
    }

    /// Sorted, pretty JSON with a trailing newline.
    pub fn document_bytes() -> Result<String, serde_json::Error> {
        let mut value = document()?;
        value.sort_all_objects();
        Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
    }

    fn served(dto: &str, name: &str, value: impl Serialize) -> Result<Value, serde_json::Error> {
        Ok(
            json!({"dto": dto, "name": name, "value": serde_json::to_value(value)?, "rust_accepts": true}),
        )
    }

    /// A request body and whether the Rust deserializer accepts it.
    fn request<T: DeserializeOwned>(dto: &str, name: &str, value: Value) -> Value {
        let rust_accepts = serde_json::from_value::<T>(value.clone()).is_ok();
        json!({"dto": dto, "name": name, "value": value, "rust_accepts": rust_accepts})
    }

    fn summary(
        id: i64,
        filled: bool,
        accepted_progress: AcceptedProgressSummary,
        freshness: PlanFreshness,
    ) -> ProfileSummary {
        let text = |value: &str| filled.then(|| value.to_owned());
        ProfileSummary {
            id,
            name: format!("Build {UNICODE}"),
            order_number: text("#1001"),
            special_request: text(UNICODE),
            part_count: if filled { MAX_SAFE } else { 0 },
            accepted_progress,
            build_stale: filled,
            freshness,
            archived_at: text("2026-11-03T20:00:00.000Z"),
            last_used_at: text("2026-11-03T21:00:00.000Z"),
        }
    }

    fn summaries() -> Vec<ProfileSummary> {
        let source = |id| PlanUntrackedReason::SourceRevisionUntracked {
            source_id: id,
            source_name: UNICODE.into(),
        };
        vec![
            summary(
                MAX_SAFE,
                true,
                AcceptedProgressSummary::Ready {
                    total_units: MAX_SAFE,
                    remaining_units: 0,
                },
                PlanFreshness::Stale {
                    accepted_input_set_id: 3,
                    accepted_at: "2026-11-02T08:00:00.000Z".into(),
                    reasons: FreshnessReasons::new(
                        PlanStaleReason::SourceRevisionChanged {
                            source_id: 1,
                            source_name: UNICODE.into(),
                            accepted_revision_id: 2,
                            current_revision_id: MAX_SAFE,
                        },
                        vec![
                            PlanStaleReason::SourceRevisionUnavailable {
                                source_id: 4,
                                source_name: "Base".into(),
                                accepted_revision_id: 5,
                            },
                            PlanStaleReason::NamingRulesChanged {
                                source_id: 4,
                                source_name: "Base".into(),
                                accepted_digest: "a".repeat(64),
                                current_digest: "b".repeat(64),
                            },
                            PlanStaleReason::PlanInputsInvalid,
                            PlanStaleReason::PlanConfigurationChanged,
                        ],
                    ),
                    untracked_sources: vec![source(6)],
                },
            ),
            summary(
                2,
                false,
                AcceptedProgressSummary::Empty,
                PlanFreshness::Untracked {
                    accepted_input_set_id: None,
                    accepted_at: None,
                    reasons: FreshnessReasons::new(
                        PlanUntrackedReason::NoAcceptedInputs,
                        vec![source(7)],
                    ),
                },
            ),
            summary(
                3,
                true,
                AcceptedProgressSummary::Unavailable {
                    reason: AcceptedProgressUnavailableReason::ConcurrentUpdate,
                },
                PlanFreshness::Current {
                    accepted_input_set_id: 9,
                    accepted_at: "2026-11-01T08:00:00.000Z".into(),
                },
            ),
            summary(
                4,
                false,
                AcceptedProgressSummary::Unavailable {
                    reason: AcceptedProgressUnavailableReason::Integrity,
                },
                PlanFreshness::Unavailable {
                    code: "accepted_inputs".into(),
                },
            ),
        ]
    }

    fn identity(state: &str, revision_id: Option<i64>) -> DraftIdentity {
        DraftIdentity {
            draft_id: MAX_SAFE,
            state: state.into(),
            lifecycle_version: 2_147_483_647,
            snapshot_digest: "c".repeat(64),
            base: DraftBase {
                revision_id,
                plan_version: revision_id.map_or(0, |_| MAX_SAFE),
            },
        }
    }

    fn part(id: i64, filled: bool) -> PartView {
        PartView {
            draft_part_id: id,
            base_revision_part_id: filled.then_some(MAX_SAFE),
            part_key: format!("{UNICODE}/part-{id}"),
            filename: format!("{UNICODE}.stl"),
            relative_path: format!("plates/{UNICODE}.stl"),
            source_layer: "base".into(),
            role: "body".into(),
            quantity_inferred: 2,
            quantity_override: filled.then_some(10_000),
            quantity_effective: if filled { 10_000 } else { 2 },
            included: filled,
        }
    }

    fn workspaces() -> Vec<Workspace> {
        let reference = AcceptedReference {
            revision_part_id: MAX_SAFE,
            filename: format!("{UNICODE}.stl"),
            relative_path: "old/a.stl".into(),
            source_layer: "base".into(),
        };
        let ready = Workspace {
            profile_id: MAX_SAFE,
            draft: identity("open", Some(1)),
            parts: vec![part(1, true), part(2, false)],
            diff: Diff {
                base_is_current: true,
                added: vec![part(3, true)],
                removed: vec![reference.clone()],
                changed: vec![Change {
                    before: reference,
                    after: part(1, true),
                    fields: vec!["quantity", "included"],
                }],
            },
            reconciliation: ReconciliationView::Ready {
                reused_units: 3,
                new_units: 0,
                surplus_units: 1,
            },
        };
        let unresolved = Workspace {
            profile_id: 1,
            draft: identity("abandoned", None),
            parts: vec![],
            diff: Diff {
                base_is_current: false,
                added: vec![],
                removed: vec![],
                changed: vec![],
            },
            reconciliation: ReconciliationView::Unresolved {
                conflicts: vec![
                    ConflictView::AmbiguousExactMatch {
                        target_draft_part_id: 1,
                        candidate_revision_part_ids: vec![2, 9_007_199_254_740_991],
                    },
                    ConflictView::UnsafePredecessor {
                        target_draft_part_id: 3,
                        predecessor_revision_part_id: 4,
                    },
                    ConflictView::PredecessorClaimed {
                        target_draft_part_id: 5,
                        predecessor_revision_part_id: 6,
                    },
                ],
            },
        };
        vec![ready, unresolved]
    }

    fn health(degraded_detail: Option<&'static str>) -> crate::auth::Health {
        let filled = degraded_detail.is_some();
        crate::auth::Health {
            ok: true,
            authentication_required: true,
            authenticated: !filled,
            degraded_detail,
            capabilities: if filled {
                vec![]
            } else {
                vec!["github_oauth", "discord_oauth"]
            },
            multi_user: false,
            single_user_auth: true,
            single_user_setup_required: filled,
            registration_open: filled,
            owner_mapping_required: false,
            github_oauth_configured: !filled,
            discord_oauth_configured: !filled,
        }
    }

    fn fixtures() -> Result<Vec<Value>, serde_json::Error> {
        let summaries = summaries();
        let workspaces = workspaces();
        let layers = |filled: bool| ProfileLayers {
            profile_id: MAX_SAFE,
            layers: vec![ProfileLayer {
                id: 1,
                layer_order: 0,
                layer_type: "base".into(),
                project_id: filled.then_some(MAX_SAFE),
                project_name: filled.then(|| UNICODE.to_owned()),
            }],
        };
        let digest = "d".repeat(64);
        let edit = |value: Value| request::<EditRequest>("EditDraftPartsRequest", "", value);
        let mut cases = vec![
            served(
                "ProfileList",
                "all-variants",
                ProfileList {
                    profiles: summaries.clone(),
                },
            )?,
            served(
                "ProfileList",
                "empty",
                ProfileList::<ProfileSummary> { profiles: vec![] },
            )?,
            served("ProfileLayers", "filled", layers(true))?,
            served("ProfileLayers", "nulls", layers(false))?,
            served(
                "DraftList",
                "states",
                DraftListResponse {
                    profile_id: MAX_SAFE,
                    drafts: vec![identity("open", Some(1)), identity("consumed", None)],
                },
            )?,
            served("DraftIdentity", "open", identity("open", Some(MAX_SAFE)))?,
            served("DraftIdentity", "nulls", identity("abandoned", None))?,
            served("Health", "ready", health(None))?,
            served(
                "Health",
                "degraded",
                health(Some("Authentication required")),
            )?,
        ];
        for value in printer_states() {
            let name = format!("{:?}-{:?}", value.execution(), value.bed_clear());
            cases.push(served("PrinterState", &name, value)?);
        }
        for state in PrinterDisplayState::ALL {
            cases.push(served(
                "PrinterDisplayDescriptor",
                &format!("{state:?}"),
                state.descriptor(),
            )?);
        }
        for &kind in ProblemType::ALL {
            let name = format!("{kind:?}");
            cases.push(served("Problem", &name, Problem::new(kind, 400, UNICODE))?);
            cases.push(served("ProblemType", &name, kind)?);
        }
        let bare = Problem::new(ProblemType::Conflict, 409, "").without_detail();
        cases.push(served("Problem", "no-detail", bare.with_code("not_open"))?);
        let errors = (0..MAX_FIELD_ERRORS)
            .map(|index| FieldError::new(&format!("/decisions/{index}"), "is invalid"));
        cases.push(served(
            "Problem",
            "coded-with-errors",
            Problem::new(ProblemType::Validation, 400, "Request is invalid")
                .with_code("invalid_request")
                .with_errors(errors),
        )?);
        for (index, value) in summaries.into_iter().enumerate() {
            cases.push(served(
                "ProfileSummary",
                &format!("variant-{index}"),
                value,
            )?);
        }
        for (index, value) in workspaces.into_iter().enumerate() {
            cases.push(served(
                "DraftWorkspace",
                &format!("variant-{index}"),
                value,
            )?);
        }
        for (name, value) in [
            (
                "one-decision",
                json!({"expected_snapshot_digest": digest, "decision": {"kind": "set_included", "draft_part_ids": [1, 9_007_199_254_740_991_i64], "value": false}}),
            ),
            (
                "batch-null-override",
                json!({"expected_snapshot_digest": digest, "decisions": [{"kind": "set_quantity_override", "draft_part_ids": [1], "value": null}, {"kind": "set_quantity_override", "draft_part_ids": [2], "value": 10_000}]}),
            ),
            (
                "empty-batch",
                json!({"expected_snapshot_digest": digest, "decisions": []}),
            ),
            (
                "empty-targets",
                json!({"expected_snapshot_digest": digest, "decision": {"kind": "set_included", "draft_part_ids": [], "value": true}}),
            ),
            (
                "override-omitted",
                json!({"expected_snapshot_digest": digest, "decision": {"kind": "set_quantity_override", "draft_part_ids": [1]}}),
            ),
            (
                "override-too-large",
                json!({"expected_snapshot_digest": digest, "decision": {"kind": "set_quantity_override", "draft_part_ids": [1], "value": 10_001}}),
            ),
            (
                "unsafe-id",
                json!({"expected_snapshot_digest": digest, "decision": {"kind": "set_included", "draft_part_ids": [9_007_199_254_740_992_i64], "value": true}}),
            ),
            (
                "both-shapes",
                json!({"expected_snapshot_digest": digest, "decision": {"kind": "set_included", "draft_part_ids": [1], "value": true}, "decisions": []}),
            ),
            (
                "unknown-kind",
                json!({"expected_snapshot_digest": digest, "decision": {"kind": "rename", "draft_part_ids": [1], "value": true}}),
            ),
        ] {
            let mut case = edit(value);
            case["name"] = json!(name);
            cases.push(case);
        }
        for (name, value) in [
            ("max", json!({"expected_lifecycle_version": MAX_SAFE})),
            ("zero", json!({"expected_lifecycle_version": 0})),
            ("negative", json!({"expected_lifecycle_version": -1})),
            ("fraction", json!({"expected_lifecycle_version": 1.5})),
            ("null", json!({"expected_lifecycle_version": null})),
            ("missing", json!({})),
        ] {
            cases.push(request::<AbandonRequest>(
                "AbandonDraftRequest",
                name,
                value,
            ));
        }
        Ok(cases)
    }
}
