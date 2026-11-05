//! Build workflow projection: one pure function from a Build's `Facts` to its
//! stages and next action. `GET /plans/{id}/workflow`, the ranked task feed
//! (`GET /api/v1/tasks`) and MCP `get_build_workflow` all serve this output,
//! so they cannot disagree about what to do next.
//!
//! `Facts` accept the camelCase field names Node's `resolveBuildWorkflow`
//! takes; the output is snake_case.

use serde::{Deserialize, Serialize};

const MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sources {
    Empty,
    Ready {
        #[serde(alias = "attachedCount")]
        attached_count: u64,
    },
    Stale {
        #[serde(alias = "attachedCount")]
        attached_count: u64,
        #[serde(alias = "issueCount")]
        issue_count: u64,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Accepted {
    None,
    Ready {
        #[serde(alias = "revisionId")]
        revision_id: i64,
        #[serde(alias = "planVersion")]
        plan_version: i64,
        #[serde(alias = "totalUnits")]
        total_units: u64,
        #[serde(alias = "remainingUnits")]
        remaining_units: u64,
    },
    Unavailable {
        reason: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Working {
    None,
    Ready {
        #[serde(alias = "draftId")]
        draft_id: i64,
        #[serde(alias = "changeCount")]
        change_count: u64,
    },
    NeedsAttention {
        #[serde(alias = "draftId")]
        draft_id: i64,
        #[serde(alias = "changeCount")]
        change_count: u64,
        #[serde(alias = "issueCount")]
        issue_count: u64,
    },
    Stale {
        #[serde(alias = "draftId")]
        draft_id: i64,
        #[serde(alias = "changeCount")]
        change_count: u64,
        #[serde(alias = "issueCount")]
        issue_count: u64,
    },
}
/// Stock shortfall for a Build's remaining units, from the separate Spoolman
/// shortfall computation. `NotReady` until that result exists; the projection
/// never computes stock itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stock {
    #[default]
    NotReady,
    /// `shortfalls` filaments lack enough stock for the remaining units.
    Ready { shortfalls: u64 },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlateState {
    NotStarted,
    Preparing,
    Ready,
    Stale,
    Error,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Production {
    pub plate_state: PlateState,
    pub queued_jobs: u64,
    pub sending_jobs: u64,
    pub printing_jobs: u64,
    pub failed_jobs: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkoff {
    pub awaiting_verification: u64,
    pub failed_verifications: u64,
    /// Printer Checkoff links for this Build in any state but dismissed. With
    /// none, Checkoff is manual.
    #[serde(default)]
    pub printer_links: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct Build {
    pub id: i64,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Facts {
    pub build: Build,
    pub sources: Sources,
    pub accepted_plan: Accepted,
    pub working_plan: Working,
    pub production: Production,
    pub checkoff: Checkoff,
    #[serde(default)]
    pub stock: Stock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StageId {
    Sources,
    Plan,
    Production,
    Checkoff,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StageGroup {
    Prepare,
    Make,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    NotStarted,
    Ready,
    InProgress,
    NeedsAttention,
    Stale,
    Complete,
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct StageStatus {
    pub kind: StatusKind,
    pub summary: String,
    /// Open decisions in this stage.
    #[cfg_attr(
        all(feature = "codegen", feature = "desktop_rebuild"),
        schemars(with = "pp_contracts::RequiredNullable<u64>")
    )]
    pub task_count: Option<u64>,
    /// Work running in this stage.
    #[cfg_attr(
        all(feature = "codegen", feature = "desktop_rebuild"),
        schemars(with = "pp_contracts::RequiredNullable<u64>")
    )]
    pub active_count: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct Stage {
    pub id: StageId,
    pub group: StageGroup,
    pub label: String,
    pub status: StageStatus,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    ReviewFailedPrints,
    RecoverPrinterJobs,
    VerifyPrints,
    MonitorProduction,
    ReviewProductionQueue,
    ReviewPlanStatus,
    AttachSources,
    ResolvePlanIssues,
    RefreshWorkingPlan,
    ReviewWorkingPlan,
    ReviewSourceChanges,
    CreateWorkingPlan,
    ResolveStockShortfall,
    PrepareProduction,
    CheckOffManually,
    ViewCompletedBuild,
}
impl ActionKind {
    /// Work blocked on the user, as opposed to the Build's next step.
    pub fn needs_attention(self) -> bool {
        matches!(
            self,
            Self::ReviewFailedPrints
                | Self::RecoverPrinterJobs
                | Self::VerifyPrints
                | Self::ReviewPlanStatus
                | Self::ResolvePlanIssues
                | Self::RefreshWorkingPlan
                | Self::ResolveStockShortfall
        )
    }
}
/// Where the next action opens: a Build workspace tab, and the draft when the
/// action is about one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct Target {
    pub plan_id: i64,
    pub tab: StageId,
    #[cfg_attr(
        all(feature = "codegen", feature = "desktop_rebuild"),
        schemars(with = "pp_contracts::RequiredNullable<i64>")
    )]
    pub draft_id: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct NextAction {
    pub kind: ActionKind,
    pub stage_id: StageId,
    pub label: String,
    pub reason: String,
    pub target: Target,
    #[cfg_attr(
        all(feature = "codegen", feature = "desktop_rebuild"),
        schemars(with = "pp_contracts::RequiredNullable<u64>")
    )]
    pub item_count: Option<u64>,
    #[cfg_attr(
        all(feature = "codegen", feature = "desktop_rebuild"),
        schemars(with = "pp_contracts::RequiredNullable<i64>")
    )]
    pub draft_id: Option<i64>,
    #[cfg_attr(
        all(feature = "codegen", feature = "desktop_rebuild"),
        schemars(with = "pp_contracts::RequiredNullable<u64>")
    )]
    pub issue_count: Option<u64>,
    #[cfg_attr(
        all(feature = "codegen", feature = "desktop_rebuild"),
        schemars(with = "pp_contracts::RequiredNullable<u64>")
    )]
    pub unit_count: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ActiveWork {
    pub queued_jobs: u64,
    pub sending_jobs: u64,
    pub printing_jobs: u64,
    pub failed_jobs: u64,
    pub awaiting_verification: u64,
    pub failed_verifications: u64,
    pub total_units: u64,
    pub remaining_units: u64,
}
/// One Build's workflow: the body of `GET /plans/{id}/workflow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct Workspace {
    pub build: Build,
    pub sources: Sources,
    pub accepted_plan: Accepted,
    pub working_plan: Working,
    pub stock: Stock,
    pub stages: Vec<Stage>,
    pub next_action: NextAction,
    pub active_work: ActiveWork,
}

fn plural(n: u64, s: &str) -> String {
    if n == 1 { s.into() } else { format!("{s}s") }
}
fn is_are(n: u64) -> &'static str {
    if n == 1 { "is" } else { "are" }
}
fn status(kind: StatusKind, summary: impl Into<String>) -> StageStatus {
    StageStatus {
        kind,
        summary: summary.into(),
        task_count: None,
        active_count: None,
    }
}
fn tasks(kind: StatusKind, summary: impl Into<String>, n: u64) -> StageStatus {
    StageStatus {
        task_count: Some(n),
        ..status(kind, summary)
    }
}
fn active(kind: StatusKind, summary: impl Into<String>, n: u64) -> StageStatus {
    StageStatus {
        active_count: Some(n),
        ..status(kind, summary)
    }
}
/// Every required unit of the Accepted Plan is checked off and no draft with
/// changes is open, so earlier Source changes are history rather than open
/// work.
fn finished(f: &Facts) -> bool {
    matches!(
        f.accepted_plan,
        Accepted::Ready {
            remaining_units: 0,
            ..
        }
    ) && matches!(
        f.working_plan,
        Working::None
            | Working::Ready {
                change_count: 0,
                ..
            }
    )
}
fn source_status(f: &Facts) -> StageStatus {
    use StatusKind::*;
    match f.sources {
        Sources::Empty => status(NotStarted, "No Sources attached."),
        Sources::Ready { attached_count: n } => {
            status(Complete, format!("{n} {} attached.", plural(n, "Source")))
        }
        Sources::Stale {
            attached_count: n, ..
        } if finished(f) => status(
            Complete,
            format!(
                "{n} {} attached. They have changed, and every required unit is checked off.",
                plural(n, "Source")
            ),
        ),
        Sources::Stale { issue_count, .. } => tasks(
            Stale,
            "Sources have changed since this Plan was accepted.",
            issue_count,
        ),
    }
}
fn plan_status(f: &Facts) -> StageStatus {
    use StatusKind::*;
    if let Accepted::Unavailable { reason } = &f.accepted_plan {
        return tasks(Error, reason, 1);
    }
    match &f.working_plan {
        Working::Ready {
            change_count: n, ..
        } if *n > 0 => status(
            Ready,
            format!("{n} Plan {} saved in a draft.", plural(*n, "change")),
        ),
        Working::NeedsAttention { issue_count: n, .. } => tasks(
            NeedsAttention,
            format!("{n} Plan {} to finish before saving.", plural(*n, "choice")),
            *n,
        ),
        Working::Stale { issue_count, .. } => tasks(
            Stale,
            "Open Plan to refresh its saved changes.",
            *issue_count,
        ),
        Working::None | Working::Ready { .. } => match &f.accepted_plan {
            Accepted::Ready { plan_version, .. } => {
                status(Complete, format!("Plan revision {plan_version} saved."))
            }
            _ if matches!(f.sources, Sources::Ready { .. }) => {
                status(Ready, "Ready to choose parts in Plan.")
            }
            _ => status(NotStarted, "Add Sources before choosing parts in Plan."),
        },
    }
}
fn production_status(f: &Facts) -> StageStatus {
    use StatusKind::*;
    let remaining = match &f.accepted_plan {
        Accepted::Unavailable { .. } => {
            return tasks(Error, "Production cannot read the saved Plan.", 1);
        }
        Accepted::None => {
            return status(
                NotStarted,
                "Publish a Plan to create Production's required units.",
            );
        }
        Accepted::Ready {
            remaining_units, ..
        } => *remaining_units,
    };
    let p = &f.production;
    if p.failed_jobs > 0 {
        let n = p.failed_jobs;
        return tasks(
            NeedsAttention,
            format!("{n} printer {} need attention.", plural(n, "job")),
            n,
        );
    }
    let running = p.queued_jobs + p.sending_jobs + p.printing_jobs;
    if running > 0 {
        return active(
            InProgress,
            format!("{running} Production {} active.", plural(running, "job")),
            running,
        );
    }
    if remaining == 0 {
        return status(Complete, "All required units have completed Production.");
    }
    match p.plate_state {
        PlateState::Error => tasks(Error, "Production preparation failed.", 1),
        PlateState::Stale => tasks(Stale, "Prepared plates do not match the saved Plan.", 1),
        PlateState::Preparing => active(InProgress, "Production plates are being prepared.", 1),
        PlateState::Ready => status(Ready, "Production plates are ready."),
        PlateState::NotStarted => status(
            Ready,
            format!(
                "{remaining} required {} ready for Production.",
                plural(remaining, "unit")
            ),
        ),
    }
}
fn checkoff_status(f: &Facts) -> StageStatus {
    use StatusKind::*;
    let (total, remaining) = match &f.accepted_plan {
        Accepted::Unavailable { .. } => {
            return tasks(Error, "Checkoff cannot read the saved Plan.", 1);
        }
        Accepted::None => {
            return status(
                NotStarted,
                "Publish a Plan to define the units Checkoff will verify.",
            );
        }
        Accepted::Ready {
            total_units,
            remaining_units,
            ..
        } => (*total_units, *remaining_units),
    };
    let c = &f.checkoff;
    if c.failed_verifications > 0 {
        let n = c.failed_verifications;
        return tasks(
            NeedsAttention,
            format!("{n} failed print {} need review.", plural(n, "result")),
            n,
        );
    }
    if c.awaiting_verification > 0 {
        let n = c.awaiting_verification;
        return tasks(
            NeedsAttention,
            format!("{n} print {} await verification.", plural(n, "result")),
            n,
        );
    }
    let printing = f.production.printing_jobs;
    if printing > 0 {
        return active(
            InProgress,
            format!("{printing} print {} in progress.", plural(printing, "job")),
            printing,
        );
    }
    if remaining == 0 {
        return status(Complete, "Every required unit is checked off.");
    }
    if remaining < total {
        let done = total - remaining;
        return active(
            InProgress,
            format!("{done} of {total} required units checked off."),
            done,
        );
    }
    status(NotStarted, "Waiting for print results.")
}

fn next(f: &Facts) -> NextAction {
    use ActionKind::*;
    let action = |kind, stage_id, label: &str, reason: String| NextAction {
        kind,
        stage_id,
        label: label.into(),
        reason,
        target: Target {
            plan_id: f.build.id,
            tab: stage_id,
            draft_id: None,
        },
        item_count: None,
        draft_id: None,
        issue_count: None,
        unit_count: None,
    };
    let items = |kind, stage_id, label: &str, reason: String, n: u64| NextAction {
        item_count: Some(n),
        ..action(kind, stage_id, label, reason)
    };
    let draft = |kind, label: &str, reason: String, draft_id: i64| {
        let mut a = action(kind, StageId::Plan, label, reason);
        a.draft_id = Some(draft_id);
        a.target.draft_id = Some(draft_id);
        a
    };
    let c = &f.checkoff;
    let p = &f.production;
    if c.failed_verifications > 0 {
        let n = c.failed_verifications;
        return items(
            ReviewFailedPrints,
            StageId::Checkoff,
            "Review failed print results",
            format!("{n} failed print {} need review.", plural(n, "result")),
            n,
        );
    }
    if p.failed_jobs > 0 {
        let n = p.failed_jobs;
        return items(
            RecoverPrinterJobs,
            StageId::Production,
            "Recover printer jobs",
            format!("{n} printer {} failed.", plural(n, "job")),
            n,
        );
    }
    if c.awaiting_verification > 0 {
        let n = c.awaiting_verification;
        return items(
            VerifyPrints,
            StageId::Checkoff,
            "Verify print results",
            format!(
                "{n} print {} {} waiting for verification.",
                plural(n, "result"),
                is_are(n)
            ),
            n,
        );
    }
    let monitored = p.sending_jobs + p.printing_jobs;
    if monitored > 0 {
        return items(
            MonitorProduction,
            StageId::Production,
            "Monitor Production",
            format!(
                "{monitored} Production {} {} active.",
                plural(monitored, "job"),
                is_are(monitored)
            ),
            monitored,
        );
    }
    if p.queued_jobs > 0 {
        let n = p.queued_jobs;
        return items(
            ReviewProductionQueue,
            StageId::Production,
            "Review Production queue",
            format!("{n} printer {} {} queued.", plural(n, "job"), is_are(n)),
            n,
        );
    }
    if let Accepted::Unavailable { reason } = &f.accepted_plan {
        return action(
            ReviewPlanStatus,
            StageId::Plan,
            "Review Plan status",
            reason.clone(),
        );
    }
    if matches!(f.sources, Sources::Empty) {
        return action(
            AttachSources,
            StageId::Sources,
            "Attach Sources",
            "This Build has no Sources yet.".into(),
        );
    }
    match f.working_plan {
        Working::NeedsAttention {
            draft_id,
            issue_count,
            ..
        } => {
            let label = format!(
                "Review {issue_count} Plan {}",
                plural(issue_count, "choice")
            );
            let mut a = draft(
                ResolvePlanIssues,
                &label,
                "Complete these choices so the Plan can save for Production.".into(),
                draft_id,
            );
            a.issue_count = Some(issue_count);
            return a;
        }
        Working::Stale { draft_id, .. } => {
            return draft(
                RefreshWorkingPlan,
                "Refresh Plan",
                "Open Plan to refresh changes against the current saved revision.".into(),
                draft_id,
            );
        }
        Working::Ready {
            draft_id,
            change_count: n,
        } if n > 0 => {
            return draft(
                ReviewWorkingPlan,
                "Review saved Plan changes",
                format!(
                    "{n} Plan {} {} saved in a draft and not used for Production yet. Open Plan to review {}.",
                    plural(n, "change"),
                    is_are(n),
                    if n == 1 { "it" } else { "them" }
                ),
                draft_id,
            );
        }
        Working::None | Working::Ready { .. } => {}
    }
    let Accepted::Ready {
        remaining_units: remaining,
        ..
    } = f.accepted_plan
    else {
        if let Sources::Stale { issue_count, .. } = f.sources {
            let mut a = action(
                ReviewSourceChanges,
                StageId::Sources,
                "Review Source changes",
                "Sources have changed. Open Sources to review the files for this Build.".into(),
            );
            a.issue_count = Some(issue_count);
            return a;
        }
        return action(
            CreateWorkingPlan,
            StageId::Plan,
            "Open Plan",
            "Sources are ready. Open Plan to choose files, quantities, and colors. Changes save automatically.".into(),
        );
    };
    if remaining == 0 {
        return action(
            ViewCompletedBuild,
            StageId::Checkoff,
            "View completed Build",
            "Every required unit in the saved Plan is checked off.".into(),
        );
    }
    if let Stock::Ready { shortfalls: n } = f.stock
        && n > 0
    {
        return items(
            ResolveStockShortfall,
            StageId::Production,
            "Resolve stock shortfall",
            format!(
                "{n} {} {} short for the {remaining} remaining required {}.",
                plural(n, "filament"),
                is_are(n),
                plural(remaining, "unit")
            ),
            n,
        );
    }
    let mut a = if c.printer_links == 0
        && matches!(p.plate_state, PlateState::NotStarted | PlateState::Ready)
    {
        action(
            CheckOffManually,
            StageId::Checkoff,
            "Check off printed units",
            format!(
                "No printer is linked to this Build. Check off the {remaining} remaining required {} by hand as {} finish.",
                plural(remaining, "unit"),
                if remaining == 1 { "it" } else { "they" }
            ),
        )
    } else {
        action(
            PrepareProduction,
            StageId::Production,
            "Prepare Production",
            format!(
                "{remaining} required {} remain in the Accepted Plan.",
                plural(remaining, "unit")
            ),
        )
    };
    a.unit_count = Some(remaining);
    a
}

fn validate(f: &Facts) -> anyhow::Result<()> {
    use anyhow::ensure;
    let safe_id = |id: i64| id > 0 && id as u64 <= MAX;
    let count = |n: u64| n > 0 && n <= MAX;
    ensure!(
        safe_id(f.build.id)
            && !f.build.name.is_empty()
            && f.build.name.encode_utf16().count() <= 500,
        "Invalid workflow Build"
    );
    let counts = [
        f.production.queued_jobs,
        f.production.sending_jobs,
        f.production.printing_jobs,
        f.production.failed_jobs,
        f.checkoff.awaiting_verification,
        f.checkoff.failed_verifications,
    ];
    ensure!(
        counts
            .iter()
            .try_fold(0u64, |sum, n| sum.checked_add(*n))
            .is_some_and(|n| n <= MAX)
            && f.checkoff.printer_links <= MAX
            && !matches!(f.stock, Stock::Ready { shortfalls } if shortfalls > MAX),
        "Invalid workflow counts"
    );
    ensure!(
        match f.sources {
            Sources::Empty => true,
            Sources::Ready { attached_count } => count(attached_count),
            Sources::Stale {
                attached_count,
                issue_count,
            } => count(attached_count) && count(issue_count),
        },
        "Invalid workflow Sources"
    );
    ensure!(
        match &f.accepted_plan {
            Accepted::None => true,
            Accepted::Ready {
                revision_id,
                plan_version,
                total_units,
                remaining_units,
            } => {
                safe_id(*revision_id)
                    && safe_id(*plan_version)
                    && *total_units <= MAX
                    && remaining_units <= total_units
            }
            Accepted::Unavailable { reason } => {
                !reason.is_empty() && reason.encode_utf16().count() <= 500
            }
        },
        "Invalid workflow accepted Plan"
    );
    ensure!(
        match f.working_plan {
            Working::None => true,
            Working::Ready {
                draft_id,
                change_count,
            } => safe_id(draft_id) && change_count <= MAX,
            Working::NeedsAttention {
                draft_id,
                change_count,
                issue_count,
            }
            | Working::Stale {
                draft_id,
                change_count,
                issue_count,
            } => safe_id(draft_id) && change_count <= MAX && count(issue_count),
        },
        "Invalid workflow Working Plan"
    );
    Ok(())
}

pub fn resolve(f: &Facts) -> anyhow::Result<Workspace> {
    validate(f)?;
    let (total_units, remaining_units) = match f.accepted_plan {
        Accepted::Ready {
            total_units,
            remaining_units,
            ..
        } => (total_units, remaining_units),
        _ => (0, 0),
    };
    let stage = |id, group, label: &str, status| Stage {
        id,
        group,
        label: label.into(),
        status,
    };
    Ok(Workspace {
        build: f.build.clone(),
        sources: f.sources.clone(),
        accepted_plan: f.accepted_plan.clone(),
        working_plan: f.working_plan.clone(),
        stock: f.stock,
        stages: vec![
            stage(
                StageId::Sources,
                StageGroup::Prepare,
                "Sources",
                source_status(f),
            ),
            stage(StageId::Plan, StageGroup::Prepare, "Plan", plan_status(f)),
            stage(
                StageId::Production,
                StageGroup::Make,
                "Production",
                production_status(f),
            ),
            stage(
                StageId::Checkoff,
                StageGroup::Make,
                "Checkoff",
                checkoff_status(f),
            ),
        ],
        next_action: next(f),
        active_work: ActiveWork {
            queued_jobs: f.production.queued_jobs,
            sending_jobs: f.production.sending_jobs,
            printing_jobs: f.production.printing_jobs,
            failed_jobs: f.production.failed_jobs,
            awaiting_verification: f.checkoff.awaiting_verification,
            failed_verifications: f.checkoff.failed_verifications,
            total_units,
            remaining_units,
        },
    })
}
