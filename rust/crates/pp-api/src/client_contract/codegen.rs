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

mod reads;
mod requests;
use reads::*;
use requests::*;

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
    schema(
        "SetModuleRequest",
        schemars::schema_for!(crate::modules::SetModuleRequest),
    )?;
    schema(
        "ModuleList",
        schemars::schema_for!(crate::modules::ModuleList),
    )?;
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

