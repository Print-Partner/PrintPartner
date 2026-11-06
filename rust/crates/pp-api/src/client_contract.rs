//! Routes and DTOs that `web/packages/rust-client` is generated from.
//!
//! Each [`ClientRoute`] names a route registered by [`crate::builds`],
//! [`crate::drafts`], [`crate::workflow`], [`crate::accepted`], [`crate::planning`],
//! [`crate::farm`], [`crate::checkoff`] or [`crate::auth`]. `pp-core`'s
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
    /// Path parameters that are opaque strings; the rest are positive integers.
    pub text_params: &'static [&'static str],
    /// Request headers the route refuses to run without, such as
    /// `Idempotency-Key`.
    pub headers: &'static [&'static str],
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
        text_params: &[],
        headers: &[],
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
        "previewDraftAccept",
        "GET",
        "/api/v2/plans/{id}/drafts/{draft_id}/accept-preview",
        None,
        "DraftAcceptPreview",
    ),
    route(
        "editDraftParts",
        "PATCH",
        "/api/v2/plans/{id}/drafts/{draft_id}/parts",
        Some("EditDraftPartsRequest"),
        "DraftWorkspace",
    ),
    ClientRoute {
        headers: &["Idempotency-Key"],
        ..route(
            "reconcileDraft",
            "PUT",
            "/api/v2/plans/{id}/drafts/{draft_id}/reconciliation",
            Some("ReconcileDraftRequest"),
            "DraftWorkspace",
        )
    },
    route(
        "readBuildWorkflow",
        "GET",
        "/api/v2/plans/{id}/workflow",
        None,
        "BuildWorkflow",
    ),
    route("listTasks", "GET", "/api/v2/tasks", None, "TaskFeed"),
    route(
        "abandonDraft",
        "POST",
        "/api/v2/plans/{id}/drafts/{draft_id}/abandon",
        Some("AbandonDraftRequest"),
        "DraftIdentity",
    ),
    route(
        "readFilamentShortfalls",
        "GET",
        "/api/v2/plans/{id}/filament-shortfalls",
        None,
        "FilamentShortfalls",
    ),
    route(
        "readBuildPlanning",
        "GET",
        "/api/v2/plans/{id}/build-planning",
        None,
        "BuildPlanningResponse",
    ),
    route(
        "listPlanDecisions",
        "GET",
        "/api/v2/plans/{id}/decisions",
        None,
        "PlanDecisionList",
    ),
    route(
        "listPrinters",
        "GET",
        "/api/v2/printers",
        None,
        "FarmPrinterList",
    ),
    route(
        "listPrinterCheckoff",
        "GET",
        "/api/v2/printer-checkoff",
        None,
        "CheckoffResultList",
    ),
    route(
        "listUnmatchedPrints",
        "GET",
        "/api/v2/printer-checkoff/unattributed",
        None,
        "UnmatchedPrintList",
    ),
    route(
        "listFarmLog",
        "GET",
        "/api/v2/farm-log",
        None,
        "FarmLogPage",
    ),
    ClientRoute {
        text_params: &["id"],
        ..route(
            "undoFarmLogEntry",
            "POST",
            "/api/v2/farm-log/{id}/undo",
            None,
            "FarmLogEntry",
        )
    },
    route(
        "listMatchSuggestions",
        "GET",
        "/api/v2/printer-checkoff/unattributed/suggestions",
        None,
        "MatchSuggestionList",
    ),
    route(
        "verifyCheckoffBurst",
        "POST",
        "/api/v2/printer-checkoff/verify-batch",
        Some("VerifyBatchRequest"),
        "VerifyBatchReceipt",
    ),
    route(
        "undoCheckoffBurst",
        "POST",
        "/api/v2/printer-checkoff/verify-batch/undo",
        Some("UndoBatchRequest"),
        "UndoBatchReceipt",
    ),
    ClientRoute {
        text_params: &["id"],
        ..route(
            "claimUnattributedPrint",
            "POST",
            "/api/v2/printer-checkoff/unattributed/{id}/claim",
            Some("ClaimUnattributedRequest"),
            "ClaimUnattributedReceipt",
        )
    },
    ClientRoute {
        text_params: &["id"],
        ..route(
            "dismissUnattributedPrint",
            "POST",
            "/api/v2/printer-checkoff/unattributed/{id}/dismiss",
            Some("DismissUnattributedRequest"),
            "DismissUnattributedReceipt",
        )
    },
    route(
        "readSendQueue",
        "GET",
        "/api/v2/printer-send-queue",
        None,
        "SendQueue",
    ),
    ClientRoute {
        name: "readHealth",
        method: "GET",
        path: "/health",
        auth: ClientAuth::None,
        request: None,
        response: "Health",
        text_params: &[],
        headers: &[],
    },
];

#[cfg(feature = "codegen")]
pub use codegen::{document, document_bytes};

#[cfg(feature = "codegen")]
mod codegen {
    use super::CLIENT_ROUTES;
    use crate::problem::{FieldError, MAX_FIELD_ERRORS, Problem, ProblemType};
    use pp_storage::checkoff_progress::farm_log::{
        FarmLogEntry, FarmLogKind, FarmLogPage, FarmLogUndo, FarmUndoReason,
    };
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
        checkoff_progress::wire::{
            BurstBedReceipt, ClaimUnattributedReceipt, ClaimUnattributedRequest, ClaimedLink,
            DismissUnattributedReceipt, DismissUnattributedRequest, UndoBatchReceipt,
            UndoBatchRequest, VerifyBatchReceipt, VerifyBatchRequest,
        },
        plan_publication::preview::{
            AcceptPreview, BlockingOperation, DuplicateOrder, FieldChange, FieldValue, InputChange,
            PartRef, PlateChanges, PlateRef, PreviewChanges, PreviewRevision, ProductionCheck,
            SnapshotNote, SpoolImpact, StockImpact, UnitChanges, UnitRef, Unmappable,
        },
        read_model::{
            build_workflow::{TaskFeed, UnavailableBuild, rank},
            farm::{
                CheckoffResult, CheckoffResultList, CheckoffResultState, CheckoffUnit, FarmPrinter,
                FarmPrinterList, FileNameEvidence, HostOutcome, IntegrationKind, LoadedFilament,
                MatchBasis, MatchConfidence, MatchEvidence, MatchSuggestion, MatchSuggestionList,
                ObjectListEvidence, PrinterIntegration, PrinterReason, QueuedSend, ResolvedUnit,
                SendQueue, SendState, SuggestedBuild, SuggestedPlate, TimeWindowEvidence,
                UnitResult, UnmatchedPrint, UnmatchedPrintList,
            },
            planning::{
                BuildPlanning, BuildPlanningBrief, BuildPlanningPhase, BuildPlanningReadiness,
                BuildPlanningResponse, PlanDecision, PlanDecisionList, ReadinessBlocker,
            },
            workflow::{
                self, Accepted, Build, Checkoff, Facts, PlateState, Production, Sources, Stock,
                Working, Workspace as BuildWorkflow,
            },
        },
        required_units::{
            AcceptedReference, Change, ConflictView, Diff, DraftBase, DraftIdentity,
            DraftListResponse, PartView, ReconciliationRequest, ReconciliationView, Workspace,
        },
        working_drafts::{AbandonRequest, EditRequest},
    };
    use pp_storage::{
        features::modules::{Module, ModuleOffReason},
        read_model::stock::{
            FilamentNeed, FilamentShortfalls, GramsAvailable, GramsNeeded, InventoryUnknown,
            NeedUnknown, PartNeed, Sufficiency,
        },
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
        schema("DraftAcceptPreview", schemars::schema_for!(AcceptPreview))?;
        schema("EditDraftPartsRequest", schemars::schema_for!(EditRequest))?;
        schema("AbandonDraftRequest", schemars::schema_for!(AbandonRequest))?;
        schema(
            "ReconcileDraftRequest",
            schemars::schema_for!(ReconciliationRequest),
        )?;
        schema("BuildWorkflow", schemars::schema_for!(BuildWorkflow))?;
        schema("TaskFeed", schemars::schema_for!(TaskFeed))?;
        schema("Health", schemars::schema_for!(crate::auth::Health))?;
        schema(
            "FilamentShortfalls",
            schemars::schema_for!(FilamentShortfalls),
        )?;
        schema(
            "BuildPlanningResponse",
            schemars::schema_for!(BuildPlanningResponse),
        )?;
        schema("PlanDecisionList", schemars::schema_for!(PlanDecisionList))?;
        schema("PrinterState", schemars::schema_for!(PrinterStateView))?;
        schema("FarmPrinterList", schemars::schema_for!(FarmPrinterList))?;
        schema(
            "CheckoffResultList",
            schemars::schema_for!(CheckoffResultList),
        )?;
        schema(
            "UnmatchedPrintList",
            schemars::schema_for!(UnmatchedPrintList),
        )?;
        schema(
            "MatchSuggestionList",
            schemars::schema_for!(MatchSuggestionList),
        )?;
        schema("SendQueue", schemars::schema_for!(SendQueue))?;
        schema("FarmLogPage", schemars::schema_for!(FarmLogPage))?;
        schema("FarmLogEntry", schemars::schema_for!(FarmLogEntry))?;
        schema(
            "VerifyBatchRequest",
            schemars::schema_for!(VerifyBatchRequest),
        )?;
        schema(
            "VerifyBatchReceipt",
            schemars::schema_for!(VerifyBatchReceipt),
        )?;
        schema("UndoBatchRequest", schemars::schema_for!(UndoBatchRequest))?;
        schema("UndoBatchReceipt", schemars::schema_for!(UndoBatchReceipt))?;
        schema(
            "ClaimUnattributedRequest",
            schemars::schema_for!(ClaimUnattributedRequest),
        )?;
        schema(
            "ClaimUnattributedReceipt",
            schemars::schema_for!(ClaimUnattributedReceipt),
        )?;
        schema(
            "DismissUnattributedRequest",
            schemars::schema_for!(DismissUnattributedRequest),
        )?;
        schema(
            "DismissUnattributedReceipt",
            schemars::schema_for!(DismissUnattributedReceipt),
        )?;
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
                    "auth": route.auth.as_str(), "request": route.request, "response": route.response,
                    "text_params": route.text_params, "headers": route.headers})
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

    /// Farm read bodies covering every enum variant, filled and null.
    fn farm_bodies() -> (
        FarmPrinterList,
        CheckoffResultList,
        UnmatchedPrintList,
        SendQueue,
    ) {
        use PrinterReason as R;
        let reasons = [
            None,
            Some(R::NotSetUp),
            Some(R::IntegrationMissing),
            Some(R::MonitoringDisabled),
            Some(R::MonitoringNotSupported),
            Some(R::AwaitingStatus),
            Some(R::Unreachable),
        ];
        let kinds = [
            IntegrationKind::Moonraker,
            IntegrationKind::Prusalink,
            IntegrationKind::Bambu,
        ];
        let printers = reasons
            .into_iter()
            .zip(printer_states().into_iter().cycle())
            .enumerate()
            .map(|(index, (reason, state))| {
                let filled = index % 2 == 0;
                FarmPrinter {
                    id: format!("printer-{index}"),
                    name: format!("Printer {UNICODE}"),
                    model: "Voron Micron".into(),
                    integration: filled.then(|| PrinterIntegration {
                        id: format!("integration-{index}"),
                        kind: kinds[index % kinds.len()],
                    }),
                    state,
                    reason,
                    last_heard_at: filled.then(|| "2026-11-05T02:00:00.000Z".into()),
                    progress_percent: filled.then_some(100),
                    loaded_filaments: vec![LoadedFilament {
                        slot: 1,
                        filament_color_id: filled.then(|| "pla-red".into()),
                        label: if filled {
                            UNICODE.into()
                        } else {
                            String::new()
                        },
                    }],
                }
            })
            .collect();
        use CheckoffResultState as S;
        let outcomes = [
            None,
            Some(HostOutcome::Unknown),
            Some(HostOutcome::Success),
            Some(HostOutcome::Failed),
            Some(HostOutcome::Cancelled),
        ];
        let results = [
            S::Watching,
            S::AwaitingVerify,
            S::HostFailed,
            S::Dismissed,
            S::Verified,
        ]
        .into_iter()
        .zip(outcomes)
        .enumerate()
        .map(|(index, (state, host_outcome))| {
            let filled = index % 2 == 0;
            CheckoffResult {
                id: format!("link-{index}"),
                profile_id: MAX_SAFE,
                printer_id: "printer-0".into(),
                integration_id: "integration-0".into(),
                filename: format!("{UNICODE}.gcode"),
                state,
                host_outcome,
                unit_count: u32::MAX,
                resolved_unit_count: 0,
                units: vec![CheckoffUnit {
                    part_id: MAX_SAFE,
                    unit_index: u32::MAX,
                    part_filename: filled.then(|| format!("{UNICODE}.stl")),
                }],
                resolved_units: if filled {
                    vec![
                        ResolvedUnit {
                            part_id: 1,
                            unit_index: 0,
                            result: UnitResult::Confirmed,
                        },
                        ResolvedUnit {
                            part_id: MAX_SAFE,
                            unit_index: u32::MAX,
                            result: UnitResult::Rejected,
                        },
                    ]
                } else {
                    vec![]
                },
                created_at: "2026-11-05T00:00:00.000Z".into(),
                completed_at: filled.then(|| "2026-11-05T01:00:00.000Z".into()),
                applied_at: filled.then(|| "2026-11-05T01:30:00.000Z".into()),
            }
        })
        .collect();
        let prints = vec![UnmatchedPrint {
            id: "print-0".into(),
            printer_id: "printer-0".into(),
            integration_id: "integration-0".into(),
            filename: format!("{UNICODE}.gcode"),
            completed_at: "2026-11-05T01:00:00.000Z".into(),
            object_count: 3,
            candidate_count: 0,
        }];
        let items = [
            SendState::Queued,
            SendState::Sending,
            SendState::Done,
            SendState::Error,
            SendState::Cancelled,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, state)| {
            let filled = index % 2 == 0;
            QueuedSend {
                id: format!("send-{index}"),
                filename: format!("{UNICODE}.gcode"),
                printer_id: "printer-0".into(),
                profile_id: filled.then_some(MAX_SAFE),
                state,
                start: filled,
                wait_for_idle: !filled,
                unit_count: 2,
                created_at: filled.then(|| "2026-11-05T00:00:00.000Z".into()),
                updated_at: filled.then(|| "2026-11-05T00:10:00.000Z".into()),
            }
        })
        .collect();
        (
            FarmPrinterList { printers },
            CheckoffResultList { results },
            UnmatchedPrintList { prints },
            SendQueue { items },
        )
    }

    /// One entry per kind and undo reason, filled and null.
    fn farm_log() -> FarmLogPage {
        let refused = |reason| FarmLogUndo {
            undoable: false,
            reason: Some(reason),
            detail: Some(UNICODE.into()),
        };
        let undos = [
            FarmLogUndo {
                undoable: true,
                reason: None,
                detail: None,
            },
            refused(FarmUndoReason::AlreadyUndone),
            refused(FarmUndoReason::ReceiptExpired),
            refused(FarmUndoReason::StateChanged),
            refused(FarmUndoReason::Archived),
            refused(FarmUndoReason::PrinterCommand),
            refused(FarmUndoReason::UndoEntry),
        ];
        let entries = undos
            .into_iter()
            .enumerate()
            .map(|(index, undo)| {
                let kind = match undo.reason {
                    Some(FarmUndoReason::PrinterCommand) => FarmLogKind::Send,
                    Some(FarmUndoReason::UndoEntry) => FarmLogKind::CheckoffBurstUndo,
                    _ => FarmLogKind::CheckoffBurst,
                };
                let filled = index % 2 == 0;
                FarmLogEntry {
                    id: format!("entry:{index}"),
                    kind,
                    at: "2026-11-05T01:00:00.000Z".into(),
                    summary: UNICODE.into(),
                    build_ids: vec![MAX_SAFE],
                    printer_ids: vec!["printer-0".into()],
                    units_confirmed: filled.then_some(u32::MAX),
                    units_rejected: filled.then_some(0),
                    undoes: (kind == FarmLogKind::CheckoffBurstUndo).then(|| "entry:0".into()),
                    send_state: (kind == FarmLogKind::Send).then_some(SendState::Cancelled),
                    undo,
                }
            })
            .collect();
        FarmLogPage {
            entries,
            next_cursor: Some("76310a310a78".into()),
        }
    }

    /// One suggestion per confidence, with every basis, filled and null.
    fn match_suggestions() -> MatchSuggestionList {
        let suggestions = [
            (
                MatchConfidence::Exact,
                vec![MatchBasis::UnitTokens, MatchBasis::FileName],
            ),
            (
                MatchConfidence::Likely,
                vec![MatchBasis::ObjectList, MatchBasis::TimeWindow],
            ),
            (MatchConfidence::None, vec![]),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (confidence, basis))| {
            let filled = confidence != MatchConfidence::None;
            MatchSuggestion {
                print_id: format!("print-{index}"),
                printer_id: "printer-0".into(),
                filename: format!("{UNICODE}.gcode"),
                completed_at: "2026-11-05T01:00:00.000Z".into(),
                confidence,
                starts_checked: confidence == MatchConfidence::Exact,
                build: filled.then(|| SuggestedBuild {
                    id: MAX_SAFE,
                    name: UNICODE.into(),
                }),
                plate: (confidence == MatchConfidence::Exact).then(|| SuggestedPlate {
                    plate_id: "plate_0123456789abcdef0123456789abcdef".into(),
                    ordinal: 1,
                }),
                evidence: MatchEvidence {
                    basis,
                    file_name: FileNameEvidence {
                        build_ids: filled.then_some(MAX_SAFE).into_iter().collect(),
                    },
                    object_list: ObjectListEvidence {
                        object_count: u32::MAX,
                        unit_token_count: 2,
                        unit_build_ids: vec![1, MAX_SAFE],
                        part_build_ids: vec![],
                    },
                    time_window: filled.then(|| TimeWindowEvidence {
                        start: "2026-11-02T01:00:00.000Z".into(),
                        end: "2026-11-05T01:00:00.000Z".into(),
                        build_ids: vec![MAX_SAFE],
                    }),
                },
            }
        })
        .collect();
        MatchSuggestionList { suggestions }
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

    /// An unchanged draft, and a changed one with every check filled.
    fn previews() -> Vec<(&'static str, AcceptPreview)> {
        let revision = |revision_id: Option<i64>| PreviewRevision {
            draft_id: MAX_SAFE,
            expected_snapshot_digest: "e".repeat(64),
            expected_lifecycle_version: 2_147_483_646,
            expected_base: DraftBase {
                revision_id,
                plan_version: if revision_id.is_some() { MAX_SAFE } else { 0 },
            },
        };
        let snapshot = SnapshotNote {
            saved_before_accept: true,
            detail: UNICODE.into(),
        };
        let unit = |token: Option<&str>, printed: bool| UnitRef {
            token: token.map(Into::into),
            filename: format!("{UNICODE}.stl"),
            unit_index: 0,
            printed,
            assembled: printed,
        };
        let part = PartRef {
            part_id: MAX_SAFE,
            filename: UNICODE.into(),
            quantity_effective: 10_000,
            included: false,
        };
        let changes = PreviewChanges {
            fields: vec![
                FieldChange {
                    draft_part_id: 1,
                    filename: UNICODE.into(),
                    field: "quantity_override".into(),
                    old: Some(FieldValue::Integer(3)),
                    new: None,
                },
                FieldChange {
                    draft_part_id: 1,
                    filename: UNICODE.into(),
                    field: "included".into(),
                    old: Some(FieldValue::Flag(false)),
                    new: Some(FieldValue::Flag(true)),
                },
            ],
            parts_added: vec![part.clone()],
            parts_removed: vec![part],
            inputs: vec![InputChange {
                source_id: MAX_SAFE,
                field: "source_layer".into(),
                old: None,
                new: Some(FieldValue::Text(UNICODE.into())),
            }],
            required_units: UnitChanges {
                added: vec![unit(None, false)],
                removed: vec![unit(Some("ppu_1"), true)],
                printed_affected: vec![unit(Some("ppu_1"), true)],
            },
            plates: PlateChanges {
                cleared: vec![PlateRef {
                    plate_id: UNICODE.into(),
                    ordinal: 1,
                    printer_name: UNICODE.into(),
                    unit_count: MAX_SAFE,
                }],
            },
            stock: StockImpact::On {
                spools: vec![SpoolImpact {
                    spool_id: "7".into(),
                    unprinted_before: 2,
                    unprinted_after: 0,
                }],
            },
            duplicate_orders: vec![DuplicateOrder {
                profile_id: MAX_SAFE,
                name: UNICODE.into(),
                order_number: "#1001".into(),
            }],
            production: Some(ProductionCheck {
                checkoff_link_count: 1,
                send_queue_item_count: 0,
                unmappable: vec![Unmappable {
                    link_id: "link-1".into(),
                    filename: UNICODE.into(),
                    reason: UNICODE.into(),
                }],
            }),
            blocked_by: vec![BlockingOperation {
                operation_id: "op-1".into(),
                kind: "printer_upload".into(),
                state: "running".into(),
            }],
        };
        let quiet = PreviewChanges {
            fields: vec![],
            parts_added: vec![],
            parts_removed: vec![],
            inputs: vec![],
            required_units: UnitChanges {
                added: vec![],
                removed: vec![],
                printed_affected: vec![],
            },
            plates: PlateChanges { cleared: vec![] },
            stock: StockImpact::Off,
            duplicate_orders: vec![],
            production: None,
            blocked_by: vec![],
        };
        vec![
            (
                "no-change",
                AcceptPreview::NoChange {
                    revision: revision(None),
                    snapshot: snapshot.clone(),
                },
            ),
            (
                "changed",
                AcceptPreview::Changed {
                    revision: revision(Some(MAX_SAFE)),
                    changes: Box::new(changes),
                    snapshot: snapshot.clone(),
                },
            ),
            (
                "changed-quiet",
                AcceptPreview::Changed {
                    revision: revision(Some(1)),
                    changes: Box::new(quiet),
                    snapshot,
                },
            ),
        ]
    }

    fn health(
        degraded_detail: Option<&'static str>,
        modules: Option<Vec<pp_storage::features::modules::ModuleStatus>>,
    ) -> crate::auth::Health {
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
            modules,
        }
    }

    fn mixed_modules() -> Vec<pp_storage::features::modules::ModuleStatus> {
        use pp_storage::features::modules::ModuleStatus;
        vec![
            ModuleStatus::On {
                module: Module::Farm,
            },
            ModuleStatus::Off {
                module: Module::Spoolman,
                reason: ModuleOffReason::DefaultOff,
            },
            ModuleStatus::Off {
                module: Module::Orders,
                reason: ModuleOffReason::TurnedOff,
            },
            ModuleStatus::Off {
                module: Module::Bambu,
                reason: ModuleOffReason::SettingUnreadable,
            },
            ModuleStatus::On {
                module: Module::AssemblyTracking,
            },
        ]
    }

    /// Workflows resolved by the projection itself: a new Build, a saved
    /// draft, blocked Production with a stock shortfall, and a finished Build.
    fn build_workflows() -> Result<Vec<BuildWorkflow>, serde_json::Error> {
        let facts = |id: i64, sources, accepted_plan, working_plan, failed: u64, stock| Facts {
            build: Build {
                id,
                name: format!("Build {UNICODE}"),
            },
            sources,
            accepted_plan,
            working_plan,
            production: Production {
                plate_state: PlateState::NotStarted,
                queued_jobs: 0,
                sending_jobs: 0,
                printing_jobs: 0,
                failed_jobs: failed,
            },
            checkoff: Checkoff {
                awaiting_verification: failed,
                failed_verifications: 0,
                printer_links: failed,
            },
            stock,
        };
        let accepted = |remaining_units| Accepted::Ready {
            revision_id: MAX_SAFE,
            plan_version: 2,
            total_units: 4,
            remaining_units,
        };
        let stale = Sources::Stale {
            attached_count: 2,
            issue_count: 1,
        };
        [
            facts(
                1,
                Sources::Empty,
                Accepted::None,
                Working::None,
                0,
                Stock::NotReady,
            ),
            facts(
                2,
                Sources::Ready { attached_count: 1 },
                accepted(4),
                Working::Ready {
                    draft_id: MAX_SAFE,
                    change_count: 3,
                },
                0,
                Stock::NotReady,
            ),
            facts(
                3,
                stale.clone(),
                accepted(1),
                Working::None,
                2,
                Stock::Ready { shortfalls: 1 },
            ),
            facts(
                MAX_SAFE,
                stale,
                accepted(0),
                Working::None,
                0,
                Stock::Ready { shortfalls: 0 },
            ),
        ]
        .iter()
        .map(|facts| {
            workflow::resolve(facts).map_err(<serde_json::Error as serde::ser::Error>::custom)
        })
        .collect()
    }

    /// Every need, stock and verdict variant.
    fn shortfalls() -> Vec<FilamentShortfalls> {
        let measured = |grams| GramsNeeded::Measured {
            grams,
            job_count: 5,
        };
        let need = |grams_needed: GramsNeeded, grams_available, status| FilamentNeed {
            filament_color_id: "spoolman:local:filament:2".into(),
            label: format!("Prusament PLA · {UNICODE}"),
            grams_needed: grams_needed.clone(),
            grams_available,
            status,
            parts: vec![PartNeed {
                part_id: MAX_SAFE,
                filename: format!("{UNICODE}.stl"),
                remaining_units: 10_000,
                grams_needed,
            }],
        };
        let stock = |grams| GramsAvailable::Spoolman {
            grams,
            spool_ids: vec![7, MAX_SAFE],
        };
        let short = Sufficiency::Short {
            short_grams: MAX_SAFE as u64,
        };
        let mut filaments = vec![
            need(measured(40), stock(752), Sufficiency::Sufficient),
            need(measured(MAX_SAFE as u64), stock(0), short),
        ];
        let estimated = GramsNeeded::Estimated {
            grams: 12,
            job_count: 1,
        };
        filaments.push(need(estimated, stock(12), Sufficiency::Sufficient));
        for reason in [NeedUnknown::GramsNotRecorded, NeedUnknown::PartNotPrinted] {
            let unknown = GramsNeeded::Unknown { reason };
            filaments.push(need(unknown, stock(1), Sufficiency::Unknown));
        }
        for reason in [
            InventoryUnknown::NotSpoolman,
            InventoryUnknown::IntegrationMissing,
            InventoryUnknown::IntegrationDisabled,
            InventoryUnknown::Unreachable,
            InventoryUnknown::FilamentMissing,
            InventoryUnknown::SpoolWeightMissing,
        ] {
            let unknown = GramsAvailable::Unknown { reason };
            filaments.push(need(measured(1), unknown, Sufficiency::Unknown));
        }
        let ready = |id, filaments| FilamentShortfalls::Ready {
            profile_id: id,
            revision_id: id,
            plan_version: id,
            filaments,
        };
        vec![
            ready(MAX_SAFE, filaments),
            ready(1, vec![]),
            FilamentShortfalls::NoAcceptedPlan { profile_id: 1 },
            FilamentShortfalls::ModuleOff {
                module: Module::Spoolman,
                reason: ModuleOffReason::DefaultOff,
            },
        ]
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
            served("Health", "ready", health(None, Some(mixed_modules())))?,
            served(
                "Health",
                "degraded",
                health(Some("Authentication required"), Some(mixed_modules())),
            )?,
            served("Health", "node-mode-no-modules", health(None, None))?,
            served(
                "Health",
                "modules-unreadable",
                health(None, Some(pp_storage::features::modules::unreadable())),
            )?,
        ];
        for (index, value) in shortfalls().into_iter().enumerate() {
            cases.push(served(
                "FilamentShortfalls",
                &format!("variant-{index}"),
                value,
            )?);
        }
        for value in printer_states() {
            let name = format!("{:?}-{:?}", value.execution(), value.bed_clear());
            cases.push(served("PrinterState", &name, value)?);
        }
        let (printers, results, prints, queue) = farm_bodies();
        cases.push(served("FarmPrinterList", "every-reason", printers)?);
        cases.push(served(
            "FarmPrinterList",
            "empty",
            FarmPrinterList { printers: vec![] },
        )?);
        cases.push(served("CheckoffResultList", "every-state", results)?);
        cases.push(served("UnmatchedPrintList", "one", prints)?);
        cases.push(served(
            "MatchSuggestionList",
            "every-confidence",
            match_suggestions(),
        )?);
        cases.push(served(
            "MatchSuggestionList",
            "empty",
            MatchSuggestionList {
                suggestions: vec![],
            },
        )?);
        cases.push(served("SendQueue", "every-state", queue)?);
        let log = farm_log();
        for entry in &log.entries {
            cases.push(served("FarmLogEntry", &entry.id, entry)?);
        }
        cases.push(served("FarmLogPage", "every-undo", log)?);
        cases.push(served(
            "FarmLogPage",
            "last",
            FarmLogPage {
                entries: vec![],
                next_cursor: None,
            },
        )?);
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
        let workflows = build_workflows()?;
        let due = time::Date::from_calendar_date(2026, time::Month::November, 6).ok();
        let feed = rank(
            workflows
                .iter()
                .cloned()
                .zip([None, due, None, due])
                .collect(),
            vec![UnavailableBuild {
                plan_id: 9,
                build_name: "Damaged order".into(),
                reason: "Build pointer is invalid".into(),
            }],
        );
        cases.push(served("TaskFeed", "ranked", feed)?);
        cases.push(served(
            "TaskFeed",
            "empty",
            TaskFeed {
                tasks: vec![],
                unavailable: vec![],
            },
        )?);
        for (index, value) in workflows.into_iter().enumerate() {
            cases.push(served("BuildWorkflow", &format!("variant-{index}"), value)?);
        }
        for (name, value) in previews() {
            cases.push(served("DraftAcceptPreview", name, value)?);
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
        let reconcile =
            |decisions: Value| json!({"expected_snapshot_digest": digest, "decisions": decisions});
        for (name, value) in [
            (
                "every-decision",
                reconcile(json!([
                    {"kind": "select_exact_predecessor", "target_draft_part_id": 1, "predecessor_revision_part_id": MAX_SAFE},
                    {"kind": "accept_prior_completion", "target_draft_part_id": 2, "predecessor_revision_part_id": 3},
                    {"kind": "replace", "target_draft_part_id": MAX_SAFE},
                ])),
            ),
            ("no-decisions", reconcile(json!([]))),
            (
                "short-digest",
                json!({"expected_snapshot_digest": "d".repeat(63), "decisions": []}),
            ),
            (
                "unknown-kind",
                reconcile(json!([{"kind": "retire", "target_draft_part_id": 1}])),
            ),
            (
                "missing-predecessor",
                reconcile(json!([{"kind": "accept_prior_completion", "target_draft_part_id": 1}])),
            ),
            (
                "zero-target",
                reconcile(json!([{"kind": "replace", "target_draft_part_id": 0}])),
            ),
            (
                "missing-decisions",
                json!({"expected_snapshot_digest": digest}),
            ),
        ] {
            cases.push(request::<ReconciliationRequest>(
                "ReconcileDraftRequest",
                name,
                value,
            ));
        }
        cases.extend(burst_fixtures()?);
        cases.extend(planning_fixtures()?);
        Ok(cases)
    }

    /// Checkoff burst requests the generated client must accept or refuse as
    /// Rust does, and the receipts the routes serve.
    fn burst_fixtures() -> Result<Vec<Value>, serde_json::Error> {
        let decision = json!({"part_id": MAX_SAFE, "unit_index": 0, "result": "confirmed"});
        let mut cases = vec![
            served(
                "VerifyBatchReceipt",
                "two-beds",
                VerifyBatchReceipt {
                    idempotency_key: UNICODE.into(),
                    undo_token: "ppb_00112233445566778899aabbccddeeff".into(),
                    units_confirmed: 3,
                    units_rejected: 1,
                    beds: vec![
                        BurstBedReceipt {
                            units_confirmed: 3,
                            units_rejected: 0,
                        },
                        BurstBedReceipt {
                            units_confirmed: 0,
                            units_rejected: 1,
                        },
                    ],
                },
            )?,
            served(
                "UndoBatchReceipt",
                "undone",
                UndoBatchReceipt {
                    undo_token: "ppb_00112233445566778899aabbccddeeff".into(),
                    undone_at: "2026-11-05T02:00:00.000Z".into(),
                },
            )?,
        ];
        for (name, value) in [
            (
                "one-bed",
                json!({"idempotency_key": UNICODE, "beds": [{"link_id": "l1", "decisions": [decision]}]}),
            ),
            (
                "rejected-unit",
                json!({"idempotency_key": "k", "beds": [{"link_id": "l1", "decisions": [
                    {"part_id": 1, "unit_index": 4, "result": "rejected", "reason": "warping", "note": "lifted corner"}]}]}),
            ),
            (
                "unknown-result",
                json!({"idempotency_key": "k", "beds": [{"link_id": "l1", "decisions": [
                    {"part_id": 1, "unit_index": 0, "result": "printed"}]}]}),
            ),
            (
                "negative-index",
                json!({"idempotency_key": "k", "beds": [{"link_id": "l1", "decisions": [
                    {"part_id": 1, "unit_index": -1, "result": "confirmed"}]}]}),
            ),
            (
                "extra-field",
                json!({"idempotency_key": "k", "beds": [], "dry_run": true}),
            ),
            ("missing-beds", json!({"idempotency_key": "k"})),
        ] {
            cases.push(request::<VerifyBatchRequest>(
                "VerifyBatchRequest",
                name,
                value,
            ));
        }
        for (name, value) in [
            ("token", json!({"undo_token": "ppb_1"})),
            ("missing", json!({})),
            ("number", json!({"undo_token": 1})),
        ] {
            cases.push(request::<UndoBatchRequest>("UndoBatchRequest", name, value));
        }
        cases.extend(unattributed_fixtures()?);
        Ok(cases)
    }

    /// Unattributed claim and dismiss bodies and the receipts their routes serve.
    fn unattributed_fixtures() -> Result<Vec<Value>, serde_json::Error> {
        let mut cases = vec![
            served(
                "ClaimUnattributedReceipt",
                "claimed",
                ClaimUnattributedReceipt {
                    ok: true,
                    link: ClaimedLink {
                        id: UNICODE.into(),
                        profile_id: MAX_SAFE,
                        printer_id: "sim-1".into(),
                        filename: UNICODE.into(),
                        state: CheckoffResultState::AwaitingVerify,
                    },
                },
            )?,
            served(
                "DismissUnattributedReceipt",
                "dismissed",
                DismissUnattributedReceipt { ok: true },
            )?,
        ];
        for (name, value) in [
            ("profile", json!({"profile_id": MAX_SAFE})),
            (
                "selected",
                json!({"profile_id": 1, "selected_stl_basenames": [UNICODE, "gear.stl"]}),
            ),
            ("text-profile", json!({"profile_id": "1"})),
            ("missing", json!({})),
            ("extra-field", json!({"profile_id": 1, "force": true})),
        ] {
            cases.push(request::<ClaimUnattributedRequest>(
                "ClaimUnattributedRequest",
                name,
                value,
            ));
        }
        for (name, value) in [
            ("empty", json!({})),
            ("extra-field", json!({"reason": "x"})),
        ] {
            cases.push(request::<DismissUnattributedRequest>(
                "DismissUnattributedRequest",
                name,
                value,
            ));
        }
        Ok(cases)
    }

    /// Every planning phase over a brief that sets every optional field, a
    /// minimal brief, no brief, and decisions with and without results.
    fn planning_fixtures() -> Result<Vec<Value>, serde_json::Error> {
        let full: BuildPlanningBrief = serde_json::from_value(json!({
            "version": 1, "build_id": MAX_SAFE, "special_request": UNICODE,
            "requirements": [{"key": "size", "value": "350", "status": "unverified", "detail": UNICODE}],
            "evidence": [{"id": "e1", "url": "https://example.com/a", "normalized_url": "https://example.com/a",
                "kind": "canonical_design", "input_kind": "upload", "filenames": ["a.stl"],
                "artifacts": [{"path": "a.stl", "format": "3mf", "printable": true, "byte_size": MAX_SAFE, "sha256": "e".repeat(64)}],
                "upload_required": false, "derived_from_evidence_id": "e0", "source_id": MAX_SAFE,
                "source_role": "structural_base", "title": UNICODE, "extract": UNICODE,
                "retrieved_at": "2026-11-04T00:00:00.000Z", "content_hash": "f".repeat(64),
                "sync_status": "synced", "pinned_revision": "abc"}],
            "contributions": [{"id": "c1", "evidence_id": "e1", "slot": "probe", "responsibility": "printable_parts",
                "path_scopes": ["probe/"], "confidence": "high", "evidence_text": UNICODE, "status": "proposed"}],
            "compatibility_findings": [{"id": "f1", "subject": "probe", "status": "incompatible", "detail": UNICODE, "evidence_ids": ["e1"]}],
            "differences": [{"id": "d1", "group_id": "g1", "family": "probe", "kind": "renamed", "source_a": "A", "source_b": "B",
                "path_a": "a.stl", "path_b": "b.stl", "detail": UNICODE}],
            "resolutions": {"g1": {"resolution": "custom", "rationale": UNICODE, "custom_resolution": UNICODE, "resolved_at": "2026-11-04T00:00:00.000Z"}},
            "role_filaments": [{"role": "primary", "requested_brand": "B", "requested_name": "N", "inventory_kind": "substitute",
                "inventory_id": "7", "color_hex": "#A0B1C2", "substitution_confirmed": false}],
            "checklist_items": [{"id": "k1", "title": UNICODE, "detail": UNICODE, "category": "pre_print", "required": true, "completed": false}],
            "managed_source_ids": [MAX_SAFE], "draft_source_revisions": {"1": "abc"}, "draft_id": MAX_SAFE,
            "draft_review_blockers": [UNICODE],
            "created_at": "2026-11-04T00:00:00.000Z", "updated_at": "2026-11-04T00:00:00.000Z"
        }))?;
        let minimal: BuildPlanningBrief = serde_json::from_value(json!({
            "version": 1, "build_id": 1, "special_request": "", "requirements": [], "evidence": [],
            "contributions": [], "differences": [], "resolutions": {}, "role_filaments": [],
            "draft_review_blockers": [], "created_at": "2026-11-04T00:00:00.000Z", "updated_at": "2026-11-04T00:00:00.000Z"
        }))?;
        let readiness = BuildPlanningReadiness {
            ready: false,
            blockers: vec![ReadinessBlocker {
                code: "draft_missing".into(),
                detail: UNICODE.into(),
            }],
        };
        let planning = |brief: &BuildPlanningBrief, planning_phase| BuildPlanningResponse {
            planning: Some(BuildPlanning {
                brief: brief.clone(),
                planning_phase,
                readiness: readiness.clone(),
                grouped_difference_count: brief.differences.len(),
                difference_count: brief.differences.len(),
            }),
        };
        let mut cases = vec![served(
            "BuildPlanningResponse",
            "no-brief",
            BuildPlanningResponse { planning: None },
        )?];
        for (name, phase) in [
            ("preparing", BuildPlanningPhase::Preparing),
            ("draft", BuildPlanningPhase::Draft { draft_id: MAX_SAFE }),
            (
                "applied",
                BuildPlanningPhase::Applied {
                    draft_id: 1,
                    revision_id: Some(MAX_SAFE),
                },
            ),
            (
                "applied-no-revision",
                BuildPlanningPhase::Applied {
                    draft_id: 1,
                    revision_id: None,
                },
            ),
            ("abandoned", BuildPlanningPhase::Abandoned { draft_id: 1 }),
            (
                "missing-draft",
                BuildPlanningPhase::MissingDraft { draft_id: 1 },
            ),
        ] {
            cases.push(served(
                "BuildPlanningResponse",
                name,
                planning(&full, phase),
            )?);
        }
        cases.push(served(
            "BuildPlanningResponse",
            "minimal-brief",
            planning(&minimal, BuildPlanningPhase::Preparing),
        )?);
        let object = |value: Value| match value {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        let decision = |id: i64, result: Option<Map<String, Value>>| PlanDecision {
            id,
            plan_id: MAX_SAFE,
            created_at: "2026-11-04T00:00:00.000Z".into(),
            actor: "assistant".into(),
            kind: "applied".into(),
            action_type: result.as_ref().map(|_| "set_quantity".into()),
            params: object(
                json!({"part_id": MAX_SAFE, "nested": {"list": [1, 2.5, null, true, UNICODE]}}),
            ),
            label: UNICODE.into(),
            summary: UNICODE.into(),
            rationale: result.as_ref().map(|_| UNICODE.into()),
            result,
        };
        cases.push(served(
            "PlanDecisionList",
            "with-and-without-results",
            PlanDecisionList {
                decisions: vec![
                    decision(1, None),
                    decision(MAX_SAFE, Some(object(json!({"ok": true})))),
                ],
            },
        )?);
        cases.push(served(
            "PlanDecisionList",
            "empty",
            PlanDecisionList { decisions: vec![] },
        )?);
        Ok(cases)
    }
}
