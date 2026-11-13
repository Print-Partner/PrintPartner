//! Routes and DTOs that `web/packages/rust-client` is generated from.
//!
//! Each [`ClientRoute`] names a route registered by [`crate::builds`],
//! [`crate::drafts`], [`crate::workflow`], [`crate::accepted`], [`crate::planning`],
//! [`crate::farm`], [`crate::checkoff`], [`crate::catalog`], [`crate::modules`],
//! [`crate::integrations`], [`crate::printers`] or [`crate::auth`]. `pp-core`'s
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
    route(
        "readBuildHeader",
        "GET",
        "/api/v2/plans/{id}/drafts/header",
        None,
        "BuildHeaderView",
    ),
    route(
        "readDraftHeader",
        "GET",
        "/api/v2/plans/{id}/drafts/{draft_id}/header",
        None,
        "DraftHeaderView",
    ),
    ClientRoute {
        headers: &["Idempotency-Key"],
        ..route(
            "saveDraftHeader",
            "PUT",
            "/api/v2/plans/{id}/drafts/header",
            Some("SaveDraftHeaderRequest"),
            "DraftHeaderSaved",
        )
    },
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
    ClientRoute {
        headers: &["Idempotency-Key"],
        ..route(
            "restoreSnapshotAsDraft",
            "POST",
            "/api/v2/plans/{id}/snapshots/{snapshot_id}/restore",
            Some("RestoreSnapshotRequest"),
            "DraftWorkspace",
        )
    },
    ClientRoute {
        headers: &["Idempotency-Key"],
        ..route(
            "autosaveDraft",
            "POST",
            "/api/v2/plans/{id}/drafts/save",
            Some("SavePlanChoicesRequest"),
            "DraftAutosaved",
        )
    },
    ClientRoute {
        headers: &["Idempotency-Key"],
        ..route(
            "acceptDraft",
            "POST",
            "/api/v2/plans/{id}/drafts/{draft_id}/apply",
            Some("AcceptDraftRequest"),
            "AcceptDraftReceipt",
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
        "readBuildHistory",
        "GET",
        "/api/v2/plans/{id}/history",
        None,
        "BuildHistoryPage",
    ),
    ClientRoute {
        text_params: &["entry_id"],
        ..route(
            "undoBuildHistoryEntry",
            "POST",
            "/api/v2/plans/{id}/history/{entry_id}/undo",
            None,
            "BuildHistoryUndo",
        )
    },
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
    route(
        "readBuildCheckoffUnits",
        "GET",
        "/api/v2/plans/{id}/checkoff/units",
        None,
        "CheckoffUnits",
    ),
    route(
        "recordManualCheckoffBurst",
        "POST",
        "/api/v2/plans/{id}/checkoff/bursts",
        Some("ManualBurstRequest"),
        "ManualBurstReceipt",
    ),
    route(
        "undoManualCheckoffBurst",
        "POST",
        "/api/v2/plans/{id}/checkoff/bursts/undo",
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
    route("listSources", "GET", "/api/v1/sources", None, "SourceList"),
    route(
        "listSourceActivity",
        "GET",
        "/api/v1/sources/activity",
        None,
        "SourceActivity",
    ),
    ClientRoute {
        text_params: &["module"],
        ..route(
            "setModule",
            "PUT",
            "/api/v2/modules/{module}",
            Some("SetModuleRequest"),
            "ModuleList",
        )
    },
    route(
        "createIntegration",
        "POST",
        "/api/v2/integrations",
        Some("CreateIntegrationRequest"),
        "IntegrationView",
    ),
    route(
        "listIntegrations",
        "GET",
        "/api/v2/integrations",
        None,
        "IntegrationList",
    ),
    ClientRoute {
        text_params: &["id"],
        ..route(
            "readTlsPin",
            "GET",
            "/api/v1/integrations/{id}/tls-pin",
            None,
            "TlsPin",
        )
    },
    ClientRoute {
        text_params: &["id"],
        ..route(
            "resetTlsPin",
            "POST",
            "/api/v1/integrations/{id}/tls-pin/reset",
            None,
            "TlsPinReset",
        )
    },
    ClientRoute {
        text_params: &["id"],
        ..route(
            "deleteIntegration",
            "DELETE",
            "/api/v2/integrations/{id}",
            None,
            "IntegrationView",
        )
    },
    route(
        "createPrinter",
        "POST",
        "/api/v2/printers",
        Some("CreatePrinterRequest"),
        "PrinterConfig",
    ),
    ClientRoute {
        text_params: &["id"],
        ..route(
            "readPrinter",
            "GET",
            "/api/v2/printers/{id}",
            None,
            "PrinterConfig",
        )
    },
    ClientRoute {
        text_params: &["id"],
        ..route(
            "updatePrinter",
            "PATCH",
            "/api/v2/printers/{id}",
            Some("UpdatePrinterRequest"),
            "PrinterConfig",
        )
    },
    ClientRoute {
        text_params: &["id"],
        ..route(
            "deletePrinter",
            "DELETE",
            "/api/v2/printers/{id}",
            None,
            "PrinterConfig",
        )
    },
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
    ClientRoute {
        auth: ClientAuth::None,
        ..route("readAuthStatus", "GET", "/auth/status", None, "AuthStatus")
    },
    route("readCurrentUser", "GET", "/auth/me", None, "CurrentUser"),
    ClientRoute {
        auth: ClientAuth::None,
        ..route(
            "signIn",
            "POST",
            "/auth/login",
            Some("SignInRequest"),
            "SignedIn",
        )
    },
    ClientRoute {
        auth: ClientAuth::None,
        ..route(
            "registerOwner",
            "POST",
            "/auth/register",
            Some("RegisterRequest"),
            "SignedIn",
        )
    },
    ClientRoute {
        auth: ClientAuth::None,
        ..route("signOut", "POST", "/auth/logout", None, "SignedOut")
    },
];

#[cfg(feature = "codegen")]
pub use codegen::{document, document_bytes};

#[cfg(feature = "codegen")]
pub(crate) mod codegen {
    use super::CLIENT_ROUTES;
    use crate::problem::{FieldError, MAX_FIELD_ERRORS, Problem, ProblemType};
    use pp_storage::checkoff_progress::farm_log::{
        FarmLogEntry, FarmLogKind, FarmLogPage, FarmLogUndo, FarmUndoReason,
    };
    use pp_storage::printer_display_state::{
        BedClearState, PrinterDisplayDescriptor, PrinterDisplayState, PrinterExecutionState,
        PrinterStateView,
    };
    use pp_storage::read_model::history::{
        self, Actor, EntryType, Page as BuildHistoryPage, Undo, UndoReason, Undone,
    };
    use pp_storage::{
        build_graph::wire::{
            AcceptedProgressSummary, AcceptedProgressUnavailableReason, FreshnessReasons,
            PlanFreshness, PlanStaleReason, PlanUntrackedReason, ProfileLayer, ProfileLayers,
            ProfileList, ProfileSummary,
        },
        checkoff_progress::wire::{
            BurstBedReceipt, CheckoffPart, CheckoffUnitState, CheckoffUnits,
            ClaimUnattributedReceipt, ClaimUnattributedRequest, ClaimedLink,
            DismissUnattributedReceipt, DismissUnattributedRequest, ManualBurstPart,
            ManualBurstReceipt, ManualBurstRequest, UndoBatchReceipt, UndoBatchRequest, UnitPlate,
            VerifyBatchReceipt, VerifyBatchRequest,
        },
        plan_publication::preview::{
            AcceptPreview, BlockingOperation, DuplicateOrder, FieldChange, FieldValue,
            HeaderChange, InputChange, PartRef, PlateChanges, PlateRef, PreviewChanges,
            PreviewRevision, ProductionCheck, SnapshotNote, SpoolImpact, StockImpact, UnitChanges,
            UnitRef, Unmappable,
        },
        plan_publication::{ApplyPlanDraftReceipt, ApplyRequest},
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
        working_drafts::{
            AbandonRequest, EditRequest,
            autosave::{AutosaveReceipt, DraftAutosaved, SavePlanChoicesRequest},
            header::{
                BuildHeader, BuildHeaderView, DraftHeaderReceipt, DraftHeaderSaved,
                DraftHeaderState, HeaderView, SaveDraftHeaderRequest,
            },
            restore::RestoreSnapshotRequest,
        },
    };
    use pp_storage::{
        features::modules::{Module, ModuleOffReason},
        read_model::stock::{
            EstimateKept, FilamentNeed, FilamentShortfalls, GramsAvailable, GramsNeeded,
            InventoryUnknown, NeedUnknown, PartNeed, Sufficiency,
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
        schema("DraftHeaderView", schemars::schema_for!(HeaderView))?;
        schema("BuildHeaderView", schemars::schema_for!(BuildHeaderView))?;
        schema(
            "SaveDraftHeaderRequest",
            schemars::schema_for!(SaveDraftHeaderRequest),
        )?;
        schema("DraftHeaderSaved", schemars::schema_for!(DraftHeaderSaved))?;
        schema(
            "RestoreSnapshotRequest",
            schemars::schema_for!(RestoreSnapshotRequest),
        )?;
        schema(
            "SavePlanChoicesRequest",
            schemars::schema_for!(SavePlanChoicesRequest),
        )?;
        schema("DraftAutosaved", schemars::schema_for!(DraftAutosaved))?;
        schema("AcceptDraftRequest", schemars::schema_for!(ApplyRequest))?;
        // Node's receipt parser refuses unknown keys; as a response the
        // client strips them like every other answer.
        let mut receipt = schemars::schema_for!(ApplyPlanDraftReceipt);
        if let Some(object) = receipt.schema.object.as_mut() {
            object.additional_properties = None;
        }
        schema("AcceptDraftReceipt", receipt)?;
        schema("EditDraftPartsRequest", schemars::schema_for!(EditRequest))?;
        schema("AbandonDraftRequest", schemars::schema_for!(AbandonRequest))?;
        schema(
            "ReconcileDraftRequest",
            schemars::schema_for!(ReconciliationRequest),
        )?;
        schema("BuildWorkflow", schemars::schema_for!(BuildWorkflow))?;
        schema("TaskFeed", schemars::schema_for!(TaskFeed))?;
        schema("Health", schemars::schema_for!(crate::auth::Health))?;
        schema("AuthStatus", schemars::schema_for!(crate::auth::AuthStatus))?;
        schema(
            "CurrentUser",
            schemars::schema_for!(crate::auth::CurrentUser),
        )?;
        schema("SignedIn", schemars::schema_for!(crate::auth::SignedIn))?;
        schema("SignedOut", schemars::schema_for!(crate::auth::SignedOut))?;
        schema(
            "SignInRequest",
            schemars::schema_for!(crate::auth::SignInRequest),
        )?;
        schema(
            "RegisterRequest",
            schemars::schema_for!(crate::auth::RegisterRequest),
        )?;
        schema(
            "SetModuleRequest",
            schemars::schema_for!(crate::modules::SetModuleRequest),
        )?;
        schema(
            "ModuleList",
            schemars::schema_for!(crate::modules::ModuleList),
        )?;
        schema(
            "CreateIntegrationRequest",
            schemars::schema_for!(crate::integrations::CreateIntegrationRequest),
        )?;
        schema(
            "IntegrationView",
            schemars::schema_for!(crate::integrations::IntegrationView),
        )?;
        schema(
            "IntegrationList",
            schemars::schema_for!(crate::integrations::IntegrationList),
        )?;
        schema("TlsPin", schemars::schema_for!(crate::printer_host::TlsPin))?;
        schema(
            "TlsPinReset",
            schemars::schema_for!(crate::printer_host::TlsPinReset),
        )?;
        use crate::printers::{CreatePrinterRequest, UpdatePrinterRequest};
        let create = schemars::schema_for!(CreatePrinterRequest);
        schema("CreatePrinterRequest", create)?;
        let update = schemars::schema_for!(UpdatePrinterRequest);
        schema("UpdatePrinterRequest", update)?;
        // The view always serializes these two, null included.
        let mut config = schemars::schema_for!(crate::printers::PrinterConfig);
        if let Some(object) = config.schema.object.as_mut() {
            let nullable = ["bed_height_mm", "integration_id"];
            object.required.extend(nullable.map(String::from));
        }
        schema("PrinterConfig", config)?;
        schema(
            "FilamentShortfalls",
            schemars::schema_for!(FilamentShortfalls),
        )?;
        schema(
            "BuildPlanningResponse",
            schemars::schema_for!(BuildPlanningResponse),
        )?;
        schema("PlanDecisionList", schemars::schema_for!(PlanDecisionList))?;
        schema("BuildHistoryPage", schemars::schema_for!(BuildHistoryPage))?;
        schema("BuildHistoryUndo", schemars::schema_for!(Undone))?;
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
        schema(
            "SourceList",
            schemars::schema_for!(crate::catalog::wire::SourceList),
        )?;
        schema(
            "SourceActivity",
            schemars::schema_for!(crate::catalog::wire::SourceActivity),
        )?;
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
        schema("CheckoffUnits", schemars::schema_for!(CheckoffUnits))?;
        schema(
            "ManualBurstRequest",
            schemars::schema_for!(ManualBurstRequest),
        )?;
        schema(
            "ManualBurstReceipt",
            schemars::schema_for!(ManualBurstReceipt),
        )?;
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

    /// One entry per type, one burst per undo state, and a restorable snapshot.
    fn build_history() -> BuildHistoryPage {
        let refused = |reason| {
            Some(Undo {
                undoable: false,
                reason: Some(reason),
                detail: Some(UNICODE.into()),
                scope: None,
            })
        };
        let burst = |undo| (EntryType::CheckoffBurst, Actor::User, undo);
        let entries = [
            (EntryType::Revision, Actor::User, None),
            (EntryType::Snapshot, Actor::Assistant, None),
            (
                EntryType::Send,
                Actor::User,
                refused(UndoReason::PrinterCommand),
            ),
            burst(Some(Undo {
                undoable: true,
                reason: None,
                detail: None,
                scope: Some(UNICODE.into()),
            })),
            burst(refused(UndoReason::NoUndoToken)),
            burst(refused(UndoReason::AlreadyUndone)),
            burst(refused(UndoReason::StateChanged)),
            burst(refused(UndoReason::Archived)),
            (EntryType::Decision, Actor::Assistant, None),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (kind, actor, undo))| {
            let mut entry = history::contract_entry(
                kind,
                &(MAX_SAFE - index as i64).to_string(),
                "2026-11-05T01:00:00.000Z",
                actor,
                UNICODE,
                undo,
            );
            entry.restorable = (kind == EntryType::Snapshot).then_some(true);
            entry
        })
        .collect();
        BuildHistoryPage {
            plan_id: MAX_SAFE,
            entries,
            next_cursor: Some("djF8".into()),
        }
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

    pub(crate) fn served(
        dto: &str,
        name: &str,
        value: impl Serialize,
    ) -> Result<Value, serde_json::Error> {
        Ok(
            json!({"dto": dto, "name": name, "value": serde_json::to_value(value)?, "rust_accepts": true}),
        )
    }

    /// A request body and whether the Rust deserializer accepts it.
    pub(crate) fn request<T: DeserializeOwned>(dto: &str, name: &str, value: Value) -> Value {
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
            customer: text(UNICODE),
            due_date: text("2026-12-01"),
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

    /// Draft header reads, saves and save bodies (GRE-1197).
    fn header_fixtures() -> Result<Vec<Value>, serde_json::Error> {
        let full = BuildHeader {
            name: UNICODE.into(),
            order_number: Some("PO-7".into()),
            customer: Some(UNICODE.into()),
            due_date: Some("2026-12-31".into()),
        };
        let empty = BuildHeader {
            name: "Build".into(),
            order_number: None,
            customer: None,
            due_date: None,
        };
        let state = DraftHeaderState {
            header_version: 2_147_483_647,
            header: full.clone(),
            updated_at: "2026-11-06T12:00:00.000Z".into(),
        };
        let proposed = HeaderView {
            draft_id: MAX_SAFE,
            build: empty.clone(),
            draft: Some(state.clone()),
        };
        let body = |header: Value, version: Value| {
            json!({"expected_base": {"revision_id": null, "plan_version": 0},
                "expected_draft": null, "expected_header_version": version, "header": header})
        };
        let save = |name: &str, value: Value| {
            request::<SaveDraftHeaderRequest>("SaveDraftHeaderRequest", name, value)
        };
        Ok(vec![
            served(
                "DraftHeaderView",
                "unchanged",
                HeaderView {
                    draft_id: 1,
                    build: full.clone(),
                    draft: None,
                },
            )?,
            served("DraftHeaderView", "proposed", &proposed)?,
            served(
                "BuildHeaderView",
                "accepted",
                BuildHeaderView {
                    build: full.clone(),
                    base: DraftBase {
                        revision_id: Some(MAX_SAFE),
                        plan_version: MAX_SAFE,
                    },
                },
            )?,
            served(
                "BuildHeaderView",
                "never-accepted",
                BuildHeaderView {
                    build: empty.clone(),
                    base: DraftBase {
                        revision_id: None,
                        plan_version: 0,
                    },
                },
            )?,
            served(
                "DraftHeaderSaved",
                "saved",
                DraftHeaderSaved {
                    receipt: DraftHeaderReceipt {
                        profile_id: MAX_SAFE,
                        draft_id: MAX_SAFE,
                        header: state,
                    },
                    header: proposed,
                },
            )?,
            save("full", body(serde_json::to_value(&full)?, json!(1))),
            save("cleared", body(serde_json::to_value(&empty)?, Value::Null)),
            save(
                "missing-due-date",
                body(
                    json!({"name": "Build", "order_number": null, "customer": null}),
                    Value::Null,
                ),
            ),
            save(
                "extra-field",
                body(
                    json!({"name": "Build", "order_number": null, "customer": null,
                    "due_date": null, "notes": "x"}),
                    Value::Null,
                ),
            ),
            save(
                "negative-version",
                body(serde_json::to_value(&empty)?, json!(-1)),
            ),
        ])
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
            expected_header_version: revision_id.map(|_| u32::MAX),
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
            header: vec![
                HeaderChange {
                    field: "name".into(),
                    old: Some(UNICODE.into()),
                    new: Some("Bracket kit".into()),
                },
                HeaderChange {
                    field: "due_date".into(),
                    old: None,
                    new: Some("2026-12-01".into()),
                },
            ],
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
            header: vec![],
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

    /// A synced Source with every optional field, a Source never synced, and
    /// one activity event of each kind the catalog records.
    fn catalog_fixtures() -> Result<Vec<Value>, serde_json::Error> {
        use crate::catalog::wire::{SourceActivity, SourceList};
        use pp_storage::catalog::{SourceActivityEvent, SourceSummary};
        let source = |id: i64, filled: bool| {
            let text = |value: &str| filled.then(|| value.to_owned());
            SourceSummary {
                id,
                name: format!("Source {UNICODE}"),
                url: "https://example.com/kit.git".into(),
                source_kind: "git".into(),
                source_type: "kit".into(),
                role: "primary".into(),
                category: text(UNICODE),
                branch: "main".into(),
                tag: text("v1.2.0"),
                local_path: None,
                content_available: filled,
                last_synced_at: text("2026-11-03T20:00:00.000Z"),
                last_commit_sha: text(&"a".repeat(40)),
                current_source_revision_id: filled.then_some(MAX_SAFE),
                docs_url: text("https://example.com/docs"),
                manifest_community_slug: text("kit"),
                metadata: filled.then(|| Map::from_iter([("note".into(), json!(UNICODE))])),
                naming_use_defaults: !filled,
                update_status: text("updates_available"),
                update_checked_at: text("2026-11-03T21:00:00.000Z"),
                doc_count: if filled { MAX_SAFE } else { 0 },
                catalog_metadata_version: filled.then_some(1),
            }
        };
        let event = |id: i64, kind: &str, detail: Option<&str>| SourceActivityEvent {
            id,
            at: "2026-11-03T22:00:00.000Z".into(),
            kind: kind.into(),
            source_id: detail.is_none().then(|| MAX_SAFE.into()),
            source_name: format!("Source {UNICODE}"),
            detail: detail.map(str::to_owned),
        };
        let sources = vec![source(MAX_SAFE, true), source(1, false)];
        let events = vec![
            event(3, "source.sync_failed", Some("network unreachable")),
            event(2, "source.updated", None),
            event(1, "source.update_available", None),
        ];
        Ok(vec![
            served("SourceList", "filled-and-empty", SourceList { sources })?,
            served("SourceList", "none", SourceList { sources: vec![] })?,
            served("SourceActivity", "every-kind", SourceActivity { events })?,
            served("SourceActivity", "none", SourceActivity { events: vec![] })?,
        ])
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
    /// draft, blocked Production with a stock shortfall, a finished Build, a
    /// Build with a bed to clear and one with suggested unmatched prints.
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
                beds_to_clear: 0,
                match_suggestions: 0,
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
        let farm = |id: i64, awaiting: u64, beds_to_clear: u64, match_suggestions: u64| {
            let mut facts = facts(
                id,
                Sources::Ready { attached_count: 1 },
                accepted(2),
                Working::None,
                0,
                Stock::NotReady,
            );
            facts.checkoff = Checkoff {
                awaiting_verification: awaiting,
                failed_verifications: 0,
                printer_links: awaiting,
                beds_to_clear,
                match_suggestions,
            };
            facts
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
            farm(4, 2, 1, 0),
            farm(5, 0, 0, 2),
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
            reason: None,
        };
        filaments.push(need(estimated, stock(12), Sufficiency::Sufficient));
        let estimated = GramsNeeded::Estimated {
            grams: 12,
            job_count: 1,
            reason: Some(EstimateKept::MeasuredLengthNotConvertible),
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

    /// Autosave and Accept: their bodies at the bounds, and their answers.
    fn lifecycle_fixtures(workspace: &Workspace) -> Result<Vec<Value>, serde_json::Error> {
        let digest = "e".repeat(64);
        let base = json!({"revision_id": MAX_SAFE, "plan_version": MAX_SAFE});
        let choices = |name: &str, value: Value| {
            request::<SavePlanChoicesRequest>("SavePlanChoicesRequest", name, value)
        };
        let accept =
            |name: &str, value: Value| request::<ApplyRequest>("AcceptDraftRequest", name, value);
        let decision = json!({"kind": "set_quantity_override", "target": {"part_key": UNICODE, "relative_path": "a/b.stl", "source_layer": null}, "value": null});
        let revision = json!({"expected_snapshot_digest": digest, "expected_lifecycle_version": 2_147_483_646, "expected_base": base});
        let receipt: ApplyPlanDraftReceipt = serde_json::from_value(json!({
            "profile_id": MAX_SAFE, "draft_id": MAX_SAFE, "revision_id": MAX_SAFE,
            "plan_version": MAX_SAFE, "draft_lifecycle_version": MAX_SAFE,
            "revision_digest": digest, "required_unit_mapping_digest": digest,
            "applied_at": "2026-11-06T12:00:00.000Z"
        }))?;
        let with = |key: &str, value: Value| {
            let mut body = revision.clone();
            body[key] = value;
            body
        };
        Ok(vec![
            served(
                "DraftAutosaved",
                "saved",
                DraftAutosaved {
                    receipt: AutosaveReceipt {
                        profile_id: MAX_SAFE,
                        draft_id: MAX_SAFE,
                        reconciliation_id: MAX_SAFE,
                        snapshot_digest: digest.clone(),
                        saved_at: "2026-11-06T12:00:00.000Z".into(),
                    },
                    workspace: Box::new(workspace.clone()),
                },
            )?,
            choices(
                "new-draft",
                json!({"expected_base": {"revision_id": null, "plan_version": 0}, "expected_draft": null, "remap_checkoff_links": false, "decisions": [decision]}),
            ),
            choices(
                "missing-draft",
                json!({"expected_base": base, "remap_checkoff_links": true, "decisions": [decision]}),
            ),
            served("AcceptDraftReceipt", "applied", receipt)?,
            accept("revision", revision.clone()),
            accept(
                "header-version",
                with("expected_header_version", json!(u32::MAX)),
            ),
            accept(
                "null-header-version",
                with("expected_header_version", Value::Null),
            ),
            accept("remap", with("remap_checkoff_links", json!(true))),
            accept("draft-id", with("draft_id", json!(1))),
            accept(
                "stale-lifecycle",
                with("expected_lifecycle_version", json!(2_147_483_647_u64)),
            ),
        ])
    }

    fn printer_cases() -> Result<Vec<Value>, serde_json::Error> {
        let config = |filled: bool| crate::printers::PrinterConfig {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: UNICODE.into(),
            model: "voron-2.4-350".into(),
            bed_width_mm: 350,
            bed_depth_mm: 350,
            bed_height_mm: filled.then_some(340),
            margin_mm: 4,
            max_filament_slots: 4,
            integration_id: filled.then(|| "fedcba9876543210fedcba9876543210".into()),
        };
        use crate::printers::{CreatePrinterRequest, UpdatePrinterRequest};
        let mut cases = vec![
            served("PrinterConfig", "linked", config(true))?,
            served("PrinterConfig", "nulls", config(false))?,
        ];
        let voron = json!({"name": "Voron", "model": "voron-2.4-350", "bed_width_mm": 350,
            "bed_depth_mm": 350, "bed_height_mm": 340});
        let with = |key: &str, value: Value| {
            let mut body = voron.clone();
            body[key] = value;
            body
        };
        let short =
            json!({"name": "Voron", "model": "v", "bed_width_mm": 350, "bed_depth_mm": 350});
        for (name, value) in [
            ("minimal", voron.clone()),
            ("linked", with("integration_id", json!("moon-1"))),
            ("margin", with("margin_mm", json!(6))),
            ("slots", with("max_filament_slots", json!(2))),
            ("fractional-size", with("bed_width_mm", json!(350.5))),
            ("negative-size", with("bed_height_mm", json!(-1))),
            ("extra-key", with("nozzle", json!(1))),
            ("missing-height", short),
        ] {
            let dto = "CreatePrinterRequest";
            cases.push(request::<CreatePrinterRequest>(dto, name, value));
        }
        for (name, value) in [
            ("empty", json!({})),
            ("rename", json!({"name": "Big Voron"})),
            ("unlink", json!({"integration_id": null})),
            ("sizes-and-slots", with("max_filament_slots", json!(2))),
            ("margin", json!({"margin_mm": 6})),
            ("link", json!({"integration_id": "moon-1"})),
            ("fractional-margin", json!({"margin_mm": 2.5})),
            ("slots-over-a-byte", json!({"max_filament_slots": 256})),
            ("extra-key", json!({"id": "p-1"})),
        ] {
            let dto = "UpdatePrinterRequest";
            cases.push(request::<UpdatePrinterRequest>(dto, name, value));
        }
        Ok(cases)
    }

    fn integration_cases() -> Result<Vec<Value>, serde_json::Error> {
        use crate::integrations::{CreateIntegrationRequest, IntegrationList, IntegrationView};
        use crate::printer_host::{TlsPin, TlsPinReset};
        use pp_storage::printer_setup::ServiceKind;
        let view = |kind, filled: bool| IntegrationView {
            id: "0123456789abcdef0123456789abcdef".into(),
            kind,
            name: UNICODE.into(),
            enabled: filled,
            base_url: filled.then(|| "http://fixture.invalid:7125".into()),
            username: filled.then(|| "maker".into()),
            has_api_key: false,
            has_password: filled,
        };
        let list = |integrations| IntegrationList { integrations };
        let mut cases = vec![
            served("IntegrationList", "empty", list(vec![]))?,
            served(
                "IntegrationList",
                "mixed",
                list(vec![
                    view(ServiceKind::Moonraker, true),
                    view(ServiceKind::Bambu, false),
                ]),
            )?,
            served(
                "TlsPin",
                "pinned",
                TlsPin {
                    pinned: true,
                    sha256_prefix: Some("ab12cd34ef56".into()),
                    pinned_at: Some("2026-11-07T05:00:00.000Z".into()),
                },
            )?,
            served(
                "TlsPin",
                "unpinned",
                TlsPin {
                    pinned: false,
                    sha256_prefix: None,
                    pinned_at: None,
                },
            )?,
            served("TlsPinReset", "reset", TlsPinReset { reset: true })?,
            served("TlsPinReset", "none-stored", TlsPinReset { reset: false })?,
            served(
                "IntegrationView",
                "prusalink",
                view(ServiceKind::Prusalink, true),
            )?,
            served(
                "IntegrationView",
                "kept-kind",
                view(ServiceKind::Bambu, false),
            )?,
        ];
        let moonraker = json!({"kind": "moonraker", "name": "Voron", "base_url": "http://fixture.invalid:7125"});
        let with = |key: &str, value: Value| {
            let mut body = moonraker.clone();
            body[key] = value;
            body
        };
        for (name, value) in [
            ("minimal", moonraker.clone()),
            ("api-key", with("api_key", json!("fixture-key"))),
            ("nulls", with("username", Value::Null)),
            ("unknown-kind", with("kind", json!("octoprint"))),
            ("extra-key", with("token", json!("fixture-key"))),
            ("enabled-not-boolean", with("enabled", json!("yes"))),
            (
                "missing-name",
                json!({"kind": "spoolman", "base_url": "http://fixture.invalid"}),
            ),
        ] {
            let dto = "CreateIntegrationRequest";
            cases.push(request::<CreateIntegrationRequest>(dto, name, value));
        }
        Ok(cases)
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
            served(
                "ModuleList",
                "mixed",
                crate::modules::ModuleList {
                    modules: mixed_modules(),
                },
            )?,
        ];
        cases.extend(crate::auth::contract::fixtures()?);
        for (name, value) in [
            ("on", json!({"on": true})),
            ("off", json!({"on": false})),
            ("missing", json!({})),
            ("not-boolean", json!({"on": "true"})),
            ("extra-key", json!({"on": true, "plan": 1})),
        ] {
            cases.push(request::<crate::modules::SetModuleRequest>(
                "SetModuleRequest",
                name,
                value,
            ));
        }
        cases.extend(integration_cases()?);
        cases.extend(printer_cases()?);
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
        cases.extend(catalog_fixtures()?);
        let log = farm_log();
        for entry in &log.entries {
            cases.push(served("FarmLogEntry", &entry.id, entry)?);
        }
        cases.push(served("FarmLogPage", "every-undo", log)?);
        let history = build_history();
        cases.push(served(
            "BuildHistoryUndo",
            "manual-burst-stays",
            Undone {
                undone_at: "2026-11-05T01:00:00.000Z".into(),
                entry: history
                    .entries
                    .iter()
                    .find(|entry| {
                        entry.undo.as_ref().and_then(|undo| undo.reason)
                            == Some(UndoReason::AlreadyUndone)
                    })
                    .cloned(),
            },
        )?);
        cases.push(served(
            "BuildHistoryUndo",
            "printer-burst-removed",
            Undone {
                undone_at: "2026-11-05T01:00:00.000Z".into(),
                entry: None,
            },
        )?);
        cases.push(served("BuildHistoryPage", "every-type", history)?);
        cases.push(served(
            "BuildHistoryPage",
            "last",
            BuildHistoryPage {
                plan_id: 1,
                entries: vec![],
                next_cursor: None,
            },
        )?);
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
        cases.extend(header_fixtures()?);
        cases.extend(lifecycle_fixtures(&workspaces[0])?);
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
        let base = json!({"revision_id": 1, "plan_version": 1});
        let open = json!({"draft_id": MAX_SAFE, "state": "open", "lifecycle_version": 0,
            "snapshot_digest": digest, "base": base});
        for (name, value) in [
            (
                "no-open-draft",
                json!({"mode": "draft", "expected_base": base, "expected_draft": null}),
            ),
            (
                "open-draft",
                json!({"mode": "draft", "expected_base": base, "expected_draft": open}),
            ),
            (
                "other-mode",
                json!({"mode": "direct", "expected_base": base, "expected_draft": null}),
            ),
            (
                "missing-draft",
                json!({"mode": "draft", "expected_base": base}),
            ),
        ] {
            cases.push(request::<RestoreSnapshotRequest>(
                "RestoreSnapshotRequest",
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
        cases.extend(manual_burst_fixtures()?);
        cases.extend(unattributed_fixtures()?);
        Ok(cases)
    }

    /// The Checkoff tab's unit read, manual burst requests the generated client
    /// must accept or refuse as Rust does, and the receipts the routes serve.
    fn manual_burst_fixtures() -> Result<Vec<Value>, serde_json::Error> {
        let digest = "a".repeat(64);
        let basis = json!({"profile_id": MAX_SAFE, "plan_version": 1, "plan_revision_id": 2,
            "plan_revision_digest": digest, "required_unit_mapping_digest": digest});
        let token = "ppu_00112233445566778899aabbccddeeff";
        let parsed: pp_storage::checkoff_progress::Basis = serde_json::from_value(basis.clone())?;
        let parsed = parsed.into();
        let mut cases = vec![
            served(
                "CheckoffUnits",
                "ready",
                CheckoffUnits {
                    profile_id: MAX_SAFE,
                    basis: Some(parsed),
                    plate_revision_id: Some(MAX_SAFE),
                    parts: vec![CheckoffPart {
                        part_id: MAX_SAFE,
                        filename: UNICODE.into(),
                        relative_path: format!("parts/{UNICODE}.stl"),
                        source_layer: "base".into(),
                        role: "primary".into(),
                        quantity: 2,
                        printed_count: 1,
                        assembled_count: 1,
                        units: vec![
                            CheckoffUnitState {
                                token: token.into(),
                                unit_index: 0,
                                object_name: UNICODE.into(),
                                printed: true,
                                assembled: true,
                                plate: Some(UnitPlate {
                                    plate_id: "plate_00000000000000000000000000000001".into(),
                                    ordinal: 1,
                                    printer_name: UNICODE.into(),
                                }),
                            },
                            CheckoffUnitState {
                                token: token.into(),
                                unit_index: 1,
                                object_name: String::new(),
                                printed: false,
                                assembled: false,
                                plate: None,
                            },
                        ],
                    }],
                },
            )?,
            served(
                "CheckoffUnits",
                "no-accepted-plan",
                CheckoffUnits {
                    profile_id: 1,
                    basis: None,
                    plate_revision_id: None,
                    parts: vec![],
                },
            )?,
            served(
                "ManualBurstReceipt",
                "printed-and-assembled",
                ManualBurstReceipt {
                    idempotency_key: UNICODE.into(),
                    undo_token: "ppm_00112233445566778899aabbccddeeff".into(),
                    profile_id: MAX_SAFE,
                    printed_added: 3,
                    printed_removed: 0,
                    assembled_added: 1,
                    assembled_removed: 0,
                    parts: vec![ManualBurstPart {
                        part_id: MAX_SAFE,
                        quantity: 4,
                        printed_count: 3,
                        assembled_count: 1,
                    }],
                },
            )?,
        ];
        for (name, value) in [
            (
                "printed",
                json!({"idempotency_key": UNICODE, "expected": basis, "units": [{"token": token, "printed": true}]}),
            ),
            (
                "assembled-and-cleared",
                json!({"idempotency_key": "k", "expected": basis, "units": [
                    {"token": token, "printed": true, "assembled": true},
                    {"token": "ppu_ffeeddccbbaa99887766554433221100", "printed": false}]}),
            ),
            (
                "text-flag",
                json!({"idempotency_key": "k", "expected": basis, "units": [{"token": token, "printed": "yes"}]}),
            ),
            (
                "extra-unit-field",
                json!({"idempotency_key": "k", "expected": basis, "units": [{"token": token, "count": 3}]}),
            ),
            (
                "missing-basis",
                json!({"idempotency_key": "k", "units": [{"token": token, "printed": true}]}),
            ),
        ] {
            cases.push(request::<ManualBurstRequest>(
                "ManualBurstRequest",
                name,
                value,
            ));
        }
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
