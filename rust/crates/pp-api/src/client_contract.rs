//! Routes and DTOs that `web/packages/rust-client` is generated from.
//!
//! Each [`ClientRoute`] names a route registered by [`crate::builds`],
//! [`crate::drafts`], [`crate::workflow`], [`crate::accepted`], [`crate::planning`],
//! [`crate::farm`], [`crate::checkoff`], [`crate::catalog`], [`crate::modules`] or
//! [`crate::auth`]. `pp-core`'s
//! `client_routes_match_registrations` test sends every entry to the composed
//! routers and checks that it is registered with the stated authentication.
//!
//! The document's `descriptors` list holds constant tables the generator writes
//! out as data: today only the printer display descriptors.

/// What a request must present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(serde::Serialize))]
#[cfg_attr(feature = "codegen", serde(rename_all = "lowercase"))]
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
#[cfg_attr(feature = "codegen", derive(serde::Serialize))]
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
    route(
        "probePrinter",
        "POST",
        "/api/v2/printers/probe",
        Some("PrinterProbeRequest"),
        "PrinterProbeResult",
    ),
];

#[cfg(feature = "codegen")]
pub use codegen::{document, document_bytes};

#[cfg(feature = "codegen")]
mod codegen;
