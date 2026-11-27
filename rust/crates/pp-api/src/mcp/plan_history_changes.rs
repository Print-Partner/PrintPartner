use super::write_result::{self, Code, Refusal as McpRefusal};
use super::{pending_actions::Decision, *};
use pp_services::mcp_domain::ProposedAction;
use pp_storage::{
    build_graph::{
        ArchiveRefusal, BuildName,
        history::{Applied, Change, PlanDraftBasis, Refusal},
    },
    pending_actions::StagedBy,
    required_units::Outcome as WorkspaceOutcome,
    working_drafts::{
        restore::{
            RestoreCommand, RestoreMode, RestoreOutcome, RestoreRefusal, RestoreSnapshotRequest,
        },
        save,
    },
};
use serde::{Deserialize, Serialize};

const SNAPSHOT: &str = "create_plan_snapshot";
const RESTORE: &str = "propose_restore_snapshot";
const DUPLICATE: &str = "duplicate_plan";
const ARCHIVE: &str = "archive_plan";
pub(super) const TOOLS: [&str; 4] = [SNAPSHOT, RESTORE, DUPLICATE, ARCHIVE];

#[derive(Deserialize, Serialize)]
struct Proposed {
    expected_base: PlanDraftBasis,
    #[serde(flatten)]
    change: Change,
}

pub(super) fn describe(catalog: &mut Value) {
    let Some(tools) = catalog["tools"].as_array_mut() else {
        return;
    };
    for tool in tools {
        let (description, fields, required) = match tool["name"].as_str() {
            Some(SNAPSHOT) => (
                "Propose saving a named snapshot of the current Accepted Plan for Draft restore. Confirm with confirm_apply.",
                json!({"name":{"type":"string"}}),
                json!(["plan_id"]),
            ),
            Some(RESTORE) => (
                "Propose restoring a snapshot as a new Draft. An open Draft blocks restore, and only Accept publishes it. Confirm with confirm_apply.",
                json!({"snapshot_id":{"type":"integer","minimum":1}}),
                json!(["plan_id", "snapshot_id"]),
            ),
            Some(DUPLICATE) => (
                "Propose a detached experiment from this Plan. Accepted choices become a Draft; no Checkoff or print state is copied. Confirm with confirm_apply.",
                json!({"name":{"type":"string"}}),
                json!(["plan_id", "name"]),
            ),
            Some(ARCHIVE) => (
                "Propose retaining a completed Plan as an archived Build. Requires no remaining Required units. Confirm with confirm_apply.",
                json!({}),
                json!(["plan_id"]),
            ),
            _ => continue,
        };
        tool["description"] = json!(description);
        tool["inputSchema"]["properties"] = fields;
        tool["inputSchema"]["properties"]["plan_id"] = json!({"type":"integer","minimum":1});
        tool["inputSchema"]["required"] = required;
    }
}

fn parse(tool: &str, args: &Value) -> Result<Change, &'static str> {
    let name = || {
        args["name"]
            .as_str()
            .ok_or("name must be a string")
            .and_then(|name| BuildName::parse(name.to_owned()))
    };
    match tool {
        SNAPSHOT => Ok(Change::Snapshot {
            name: args.get("name").map(|_| name()).transpose()?,
        }),
        RESTORE => positive_id(args, "snapshot_id")
            .map(|snapshot_id| Change::Restore { snapshot_id })
            .ok_or("snapshot_id required"),
        DUPLICATE if args["clear_checkoff"] == false => {
            Err("A detached experiment cannot copy Checkoff")
        }
        DUPLICATE => Ok(Change::Duplicate { name: name()? }),
        ARCHIVE => Ok(Change::Archive),
        _ => Err("Unknown Plan history change"),
    }
}

fn description(change: &Change, plan: PositiveId) -> String {
    let plan = plan.get();
    match change {
        Change::Snapshot { name: Some(name) } => {
            format!("Save snapshot “{}” of Accepted Plan #{plan}", name.as_str())
        }
        Change::Snapshot { name: None } => format!("Save a snapshot of Accepted Plan #{plan}"),
        Change::Restore { snapshot_id } => format!(
            "Restore snapshot #{} of Plan #{plan} as a Draft, without publishing it",
            snapshot_id.get()
        ),
        Change::Duplicate { name } => format!(
            "Duplicate Plan #{plan} as experiment “{}”, without Checkoff",
            name.as_str()
        ),
        Change::Archive => format!("Archive Plan #{plan} without deleting its history"),
    }
}

impl ProductMcp {
    pub(super) async fn propose_history_change(
        &self,
        tool: &str,
        args: &Value,
        presented: PresentedCredential,
        staged_by: StagedBy,
    ) -> ToolResult {
        let Some(plan) = positive_id(args, "plan_id") else {
            return ToolResult::error(json!({"error":"plan_id required"}));
        };
        let change = match parse(tool, args) {
            Ok(change) => change,
            Err(error) => return ToolResult::error(json!({"error":error})),
        };
        let builds = self.reads.builds.clone();
        let wait = self.reads.wait;
        let credential = presented.clone().into_credential();
        let observed = change.clone();
        let expected_base = match tokio::task::spawn_blocking(move || {
            builds.preview_history_change(
                credential,
                plan,
                &observed,
                &AtomicBool::new(false),
                wait,
            )
        })
        .await
        {
            Ok(Ok(basis)) => basis,
            Ok(Err(error)) => {
                let McpRefusal { code, detail } = refusal(&error);
                return ToolResult::error(json!({
                    "error": detail, "code": code.as_str()
                }));
            }
            Err(_) => return ToolResult::error(json!({"error":"Plan history is unavailable"})),
        };
        let label = description(&change, plan);
        let Ok(Value::Object(params)) = serde_json::to_value(Proposed {
            expected_base,
            change,
        }) else {
            return ToolResult::error(json!({"error":"Could not describe the proposal"}));
        };
        let action = ProposedAction::new(
            tool,
            plan.get() as i64,
            label,
            "Requires confirmation.".into(),
            params,
        );
        let result = ToolResult::ok(action.proposed_content());
        self.stage_proposal(action, presented, staged_by, result)
            .await
    }

    pub(super) async fn confirm_history_change(
        &self,
        action: ProposedAction,
        presented: PresentedCredential,
    ) -> Decision {
        let (Ok(plan), Ok(proposed)) = (
            PositiveId::new(action.plan_id as u64),
            serde_json::from_value::<Proposed>(Value::Object(action.params)),
        ) else {
            return refused(
                Code::InvalidProposal,
                "The Plan history proposal is invalid",
            );
        };
        let tool = match &proposed.change {
            Change::Snapshot { .. } => SNAPSHOT,
            Change::Restore { .. } => RESTORE,
            Change::Duplicate { .. } => DUPLICATE,
            Change::Archive => ARCHIVE,
        };
        if action.action_type != tool {
            return refused(
                Code::InvalidProposal,
                "The proposal does not match its change",
            );
        }
        let credential = presented.into_credential();
        let wait = self.reads.wait;
        if let Change::Restore { snapshot_id } = proposed.change {
            let request = RestoreSnapshotRequest {
                mode: RestoreMode::Draft,
                expected_base: proposed.expected_base,
                expected_draft: None,
            };
            let Ok(command) =
                RestoreCommand::new(credential, (plan, snapshot_id), request, &action.id)
            else {
                return refused(
                    Code::InvalidProposal,
                    "The snapshot restore proposal is invalid",
                );
            };
            let drafts = self.reads.drafts.clone();
            return match tokio::task::spawn_blocking(move || {
                drafts.restore_snapshot(command, Arc::new(AtomicBool::new(false)), wait)
            })
            .await
            {
                Ok(Ok(RestoreOutcome::Restored(WorkspaceOutcome::Ready { workspace }))) => applied(
                    json!({"plan_id":plan,"snapshot_id":snapshot_id,"draft":workspace.draft,"reconciliation":workspace.reconciliation}),
                ),
                Ok(Ok(RestoreOutcome::Refused { reason })) => restore_refusal(reason),
                _ => Decision::unknown("Check Drafts for the restore before proposing again."),
            };
        }
        let builds = self.reads.builds.clone();
        match tokio::task::spawn_blocking(move || {
            builds.apply_history_change(
                credential,
                plan,
                proposed.expected_base,
                proposed.change,
                &AtomicBool::new(false),
                wait,
            )
        })
        .await
        {
            Ok(Ok(result)) => applied(match result {
                Applied::Snapshot { id, name } => {
                    json!({"plan_id":plan,"snapshot_id":id,"name":name})
                }
                Applied::Duplicate(copy) => json!(copy),
                Applied::Archive => json!({"plan_id":plan,"archived":true}),
            }),
            Ok(Err(error)) => Decision::confirmed(refusal(&error).confirmation()),
            Err(_) => Decision::unknown("Check Build history before proposing again."),
        }
    }
}

fn applied(result: Value) -> Decision {
    Decision::confirmed(json!({"ok":true,"result":result}))
}

fn refused(code: Code, detail: &'static str) -> Decision {
    Decision::confirmed(McpRefusal::new(code, detail).confirmation())
}

fn refusal(error: &Refusal) -> McpRefusal {
    let (code, detail) = match error {
        Refusal::Missing => (Code::PlanNotFound, "Plan not found"),
        Refusal::Changed => (
            Code::BaseChanged,
            "The Accepted Plan changed; propose again",
        ),
        Refusal::Archived => (Code::BuildArchived, "This Plan is archived"),
        Refusal::NotRestorable => (
            Code::SnapshotNotRestorable,
            "No restorable Accepted revision",
        ),
        Refusal::SnapshotMissing => (Code::SnapshotNotFound, "Snapshot not found for this Plan"),
        Refusal::OpenDraft => (
            Code::DraftChanged,
            "Close the open Draft before proposing a restore",
        ),
        Refusal::DuplicateName => (Code::NameTaken, "A Build with that name already exists"),
        Refusal::BuildLimitReached => (Code::BuildLimitReached, "Build limit reached"),
        Refusal::ExperimentInputsChanged => (
            Code::InputsChanged,
            "The Build's Source layers differ from its Accepted Plan. Reconcile them before duplicating.",
        ),
        Refusal::Archive(ArchiveRefusal::Remaining) => (
            Code::PrintRemaining,
            "Archive only when print remaining is 0",
        ),
        Refusal::Archive(ArchiveRefusal::CompatibilityDirty) => (
            Code::StalePlan,
            "The Accepted Plan needs compatibility repair",
        ),
        Refusal::Archive(ArchiveRefusal::Uninitialized | ArchiveRefusal::Integrity) => {
            (Code::Unavailable, "Accepted Plan progress is unavailable")
        }
        Refusal::Storage(failure) => return write_result::writer_failure(failure),
    };
    McpRefusal::new(code, detail)
}

fn restore_refusal(reason: RestoreRefusal) -> Decision {
    match reason {
        RestoreRefusal::Draft(save::Refusal::BaseChanged) => refused(
            Code::BaseChanged,
            "The Accepted Plan changed; propose again",
        ),
        RestoreRefusal::Draft(save::Refusal::DraftChanged) => {
            refused(Code::DraftChanged, "An open Draft blocks this restore")
        }
        RestoreRefusal::Draft(save::Refusal::BuildArchived) => {
            refused(Code::BuildArchived, "This Plan is archived")
        }
        RestoreRefusal::Draft(save::Refusal::NotFound) => {
            refused(Code::PlanNotFound, "Plan not found")
        }
        RestoreRefusal::SnapshotNotFound => {
            refused(Code::SnapshotNotFound, "Snapshot not found for this Plan")
        }
        RestoreRefusal::NotRestorable => refused(
            Code::SnapshotNotRestorable,
            "No restorable Accepted revision",
        ),
        RestoreRefusal::PrintedUnitsAffected { tokens } => Decision::confirmed(json!({
            "ok":false,"code":Code::PrintedUnitsAffected.as_str(),
            "detail":"The restored quantities cannot hold these printed Required units", "tokens":tokens,
        })),
        _ => refused(
            Code::SnapshotRestoreRefused,
            "The Working Draft owner refused the snapshot restore",
        ),
    }
}
