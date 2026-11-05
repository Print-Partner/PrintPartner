//! Draft recompute, rebase and Accept, as Node's `plan-drafts.ts` serves them.
use super::*;
use pp_storage::{
    plan_publication::{ApplyRequest, PublicationCommand},
    required_units::{DraftIdentity, Workspace},
    working_drafts::{
        RebaseArgs, RebaseOutcome, RebaseRequest, RebaseWorkspaceOutcome, RecomputeOutcome,
        RecomputeRequest, RecomputeWorkspaceOutcome,
    },
};

/// Node's `POST /plans/:id/drafts/recompute`: a body with a boolean
/// `apply_manifest` and an `Idempotency-Key`. The new or existing draft is
/// selected as an edit's is.
pub(super) async fn recompute(
    Extension(app): Extension<App>,
    Extension(presented): Extension<PresentedCredential>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(profile) = recompute_target(uri.path()) else {
        return invalid_request();
    };
    let apply_manifest = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|body| body.get("apply_manifest")?.as_bool());
    let request = idempotency_key(&headers)
        .zip(apply_manifest)
        .and_then(|(key, apply_manifest)| RecomputeRequest::new(key, apply_manifest).ok());
    let Some(request) = request else {
        return invalid_request();
    };
    let credential = presented.into_credential();
    let work = move |client: &WorkingDraftClient, flag, wait| {
        client.recompute_and_select(credential, profile, request, flag, wait)
    };
    match run_write(&app, work).await {
        Ok(RecomputeWorkspaceOutcome::Selected(outcome)) => present_edit(outcome),
        Ok(RecomputeWorkspaceOutcome::Refused(outcome)) => present_recompute(outcome),
        Err(error) => write_failure(error, "recompute", profile, None),
    }
}

/// Node's `recomputeFailure` through `sendFailure`.
fn present_recompute(outcome: RecomputeOutcome) -> Response {
    match outcome {
        RecomputeOutcome::ProfileNotFound => profile_missing(),
        RecomputeOutcome::AcceptedBaselineRequired => conflict("accepted_baseline_required", None),
        RecomputeOutcome::BaseChanged => conflict("base_changed", None),
        RecomputeOutcome::InputsChanged => conflict("inputs_changed", None),
        RecomputeOutcome::IdempotencyConflict => conflict("idempotency_conflict", None),
        RecomputeOutcome::NoLayers => domain_error("no_layers"),
        RecomputeOutcome::NoStls => domain_error("no_stls"),
        RecomputeOutcome::WouldWipe => domain_error("would_wipe"),
        RecomputeOutcome::Created { .. } | RecomputeOutcome::Existing { .. } => {
            edit_internal_error()
        }
    }
}

/// Node's `POST /plans/:id/drafts/:draftId/rebase`.
pub(super) async fn rebase(
    Extension(app): Extension<App>,
    Extension(presented): Extension<PresentedCredential>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((profile, draft_id)) = write_target(uri.path(), "rebase") else {
        return invalid_request();
    };
    let Some(key) = idempotency_key(&headers).map(str::to_owned) else {
        return invalid_request();
    };
    let request = match crate::problem::parse_body::<RebaseRequest>(&body) {
        Ok(request) => request,
        Err(errors) => return invalid_body(&body, errors),
    };
    let credential = presented.into_credential();
    let work = move |client: &WorkingDraftClient, flag, wait| {
        let args = RebaseArgs {
            draft_id,
            idempotency_key: &key,
            request,
        };
        client.rebase_and_select(credential, profile, args, flag, wait)
    };
    match run_write(&app, work).await {
        Ok(RebaseWorkspaceOutcome::Workspace(outcome)) => present_edit(outcome),
        Ok(RebaseWorkspaceOutcome::Refused(outcome)) => present_rebase(outcome),
        Err(error) => write_failure(error, "rebase", profile, Some(draft_id)),
    }
}

/// Node's `rebaseFailure` through `sendFailure`, for refusals that carry no
/// workspace.
fn present_rebase(outcome: RebaseOutcome) -> Response {
    match outcome {
        RebaseOutcome::ProfileNotFound => profile_missing(),
        RebaseOutcome::NotFound => draft_missing(),
        RebaseOutcome::AcceptedBaselineRequired => conflict("accepted_baseline_required", None),
        RebaseOutcome::BaseUnchanged => conflict("base_unchanged", None),
        RebaseOutcome::BaseChanged => conflict("base_changed", None),
        RebaseOutcome::InputsChanged => conflict("inputs_changed", None),
        RebaseOutcome::IdempotencyConflict => conflict("idempotency_conflict", None),
        RebaseOutcome::NotAbandoned { .. } => conflict("not_open", None),
        RebaseOutcome::SourceConflict { .. } => conflict("draft_changed", None),
        RebaseOutcome::MergeConflicts { conflicts } => refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "merge_conflicts",
            [("conflicts", json!(conflicts))],
        ),
        RebaseOutcome::NoLayers => domain_error("no_layers"),
        RebaseOutcome::NoStls => domain_error("no_stls"),
        RebaseOutcome::WouldWipe => domain_error("would_wipe"),
        RebaseOutcome::Rebased { .. } | RebaseOutcome::Existing { .. } => edit_internal_error(),
    }
}

/// Node's `POST /plans/:id/drafts/:draftId/apply` (Accept). A refusal for a
/// missing Required-unit selection runs Node's `prepareForApply` and
/// retries once with the prepared draft's revision under the same key.
pub(super) async fn apply(
    Extension(app): Extension<App>,
    Extension(presented): Extension<PresentedCredential>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((profile, draft_id)) = write_target(uri.path(), "apply") else {
        return invalid_request();
    };
    let Some(key) = idempotency_key(&headers).map(str::to_owned) else {
        return invalid_request();
    };
    let request = match crate::problem::parse_body::<ApplyRequest>(&body) {
        Ok(request) => request,
        Err(errors) => return invalid_body(&body, errors),
    };
    let accept = Accept {
        presented,
        profile,
        draft_id,
        request,
        key,
    };
    match run_write(&app, move |client, flag, wait| {
        accept.run(client, flag, wait)
    })
    .await
    {
        Ok(response) => response,
        Err(error) => write_failure(error, "apply", profile, Some(draft_id)),
    }
}

struct Accept {
    presented: PresentedCredential,
    profile: PositiveId,
    draft_id: PositiveId,
    request: ApplyRequest,
    key: String,
}

impl Accept {
    fn run(
        self,
        client: &WorkingDraftClient,
        flag: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Response> {
        let publication = client.publication();
        let command = PublicationCommand::new(
            self.profile,
            self.draft_id,
            self.request.clone(),
            self.key.clone(),
            self.credential(),
        )?;
        let mut outcome = publication.apply(command, &flag, wait)?;
        if matches!(outcome, AcceptOutcome::ReconciliationRequired { .. }) {
            let workspace = match self.prepare(client, &flag, wait)? {
                WorkspaceOutcome::Ready { workspace } => workspace,
                refused => return Ok(present_edit(refused)),
            };
            let execution = serde_json::from_value(json!({
                "expected_snapshot_digest": workspace.draft.snapshot_digest,
                "expected_lifecycle_version": workspace.draft.lifecycle_version,
                "expected_base": workspace.draft.base,
                "remap_checkoff_links": self.request.remap_checkoff_links,
            }))?;
            let command = PublicationCommand::normalized_http(
                self.profile,
                self.draft_id,
                self.request.clone(),
                execution,
                self.key.clone(),
                self.credential(),
            )?;
            outcome = publication.apply(command, &flag, wait)?;
        }
        self.present(client, outcome, &flag, wait)
    }

    fn credential(&self) -> pp_storage::read_model::Credential {
        self.presented.clone().into_credential()
    }

    fn read(
        &self,
        client: &WorkingDraftClient,
        request: Request,
        flag: &Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Outcome> {
        client.execute(self.credential(), self.profile, request, flag.clone(), wait)
    }

    /// The draft's current workspace, if it still exists.
    fn current(
        &self,
        client: &WorkingDraftClient,
        flag: &Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Option<Box<Workspace>>> {
        let draft_id = self.draft_id;
        Ok(
            match self.read(client, Request::Workspace { draft_id }, flag, wait)? {
                Outcome::Workspace { workspace } => Some(workspace),
                _ => None,
            },
        )
    }

    /// Node's `prepareForApply` with the request's expected revision, then its
    /// `autoReconcile`.
    fn prepare(
        &self,
        client: &WorkingDraftClient,
        flag: &Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<WorkspaceOutcome> {
        let draft_id = self.draft_id;
        let draft = match self.read(client, Request::Read { draft_id }, flag, wait)? {
            Outcome::Read { draft } => draft,
            Outcome::ProfileNotFound => return Ok(WorkspaceOutcome::ProfileNotFound),
            _ => return Ok(WorkspaceOutcome::DraftNotFound),
        };
        let identity = draft.identity();
        if self.mismatch(&identity.workspace_identity()?).is_none()
            && !draft.has_required_unit_selection()
        {
            let command = ReconciliationCommand::automatic(
                self.profile,
                draft_id,
                identity.snapshot_digest(),
                self.credential(),
            )?;
            return client
                .reconciliation()
                .reconcile(command, flag.clone(), wait);
        }
        let Some(workspace) = self.current(client, flag, wait)? else {
            return Ok(WorkspaceOutcome::DraftNotFound);
        };
        // The workspace comes from a later read, so it is checked again: a
        // write between the two reads must not reach the retried Accept.
        Ok(match self.mismatch(&workspace.draft) {
            Some("not_open") => WorkspaceOutcome::NotOpen { workspace },
            Some("draft_changed") => WorkspaceOutcome::DraftChanged {
                workspace: Some(workspace),
            },
            Some(_) => WorkspaceOutcome::BaseChanged { workspace },
            None => WorkspaceOutcome::Ready { workspace },
        })
    }

    /// Node's `prepareForApply` checks of a draft against the request.
    fn mismatch(&self, draft: &DraftIdentity) -> Option<&'static str> {
        let request = &self.request;
        if draft.state != "open" {
            Some("not_open")
        } else if json!(draft.snapshot_digest) != json!(request.expected_snapshot_digest)
            || json!(draft.lifecycle_version) != json!(request.expected_lifecycle_version)
        {
            Some("draft_changed")
        } else if json!(draft.base) != json!(request.expected_base) {
            Some("base_changed")
        } else {
            None
        }
    }

    /// Node's `applyResult` through `sendFailure`, and its receipt body.
    fn present(
        &self,
        client: &WorkingDraftClient,
        outcome: AcceptOutcome,
        flag: &Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Response> {
        let current = |code| -> Result<Response> {
            let workspace = self.current(client, flag, wait)?;
            Ok(conflict(code, workspace.map(|workspace| json!(workspace))))
        };
        Ok(match outcome {
            AcceptOutcome::Applied { receipt }
            | AcceptOutcome::Existing { receipt }
            | AcceptOutcome::AlreadyApplied { receipt } => json_response(StatusCode::OK, receipt),
            AcceptOutcome::NotFound => match self.read(client, Request::List, flag, wait)? {
                Outcome::ProfileNotFound => profile_missing(),
                _ => draft_missing(),
            },
            AcceptOutcome::NotOpen { .. } => current("not_open")?,
            AcceptOutcome::DraftChanged => current("draft_changed")?,
            AcceptOutcome::BaseChanged => current("base_changed")?,
            AcceptOutcome::AcceptedBaselineRequired => conflict("accepted_baseline_required", None),
            AcceptOutcome::InputsChanged => conflict("inputs_changed", None),
            AcceptOutcome::IdempotencyConflict => conflict("idempotency_conflict", None),
            AcceptOutcome::BuildArchived => domain_error("build_archived"),
            AcceptOutcome::TokenAllocationFailed => domain_error("token_allocation_failed"),
            AcceptOutcome::ReconciliationRequired { reason } => refusal(
                StatusCode::UNPROCESSABLE_ENTITY,
                "reconciliation_required",
                [("reason", json!(reason))],
            ),
            AcceptOutcome::ProductionActive {
                checkoff_link_count,
                send_queue_item_count,
            } => refusal(
                StatusCode::LOCKED,
                "production_active",
                [
                    ("checkoff_link_count", json!(checkoff_link_count)),
                    ("send_queue_item_count", json!(send_queue_item_count)),
                ],
            ),
            AcceptOutcome::CheckoffRemapUnsafe { unmappable } => refusal(
                StatusCode::UNPROCESSABLE_ENTITY,
                "checkoff_remap_unsafe",
                [("unmappable", json!(unmappable))],
            ),
            AcceptOutcome::ExecutionConflict { operations } => refusal(
                StatusCode::CONFLICT,
                "execution_conflict",
                [("operations", json!(operations))],
            ),
            AcceptOutcome::TransactionUnavailable => unavailable("transaction_unavailable"),
        })
    }
}

fn recompute_target(path: &str) -> Option<PositiveId> {
    let path = path.strip_prefix("/api/v2").unwrap_or(path);
    let rest = path.strip_prefix("/plans/")?;
    path_id(rest.strip_suffix("/drafts/recompute")?)
}
