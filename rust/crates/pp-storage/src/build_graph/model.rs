#[derive(Debug, Clone)]
pub struct ProfileSummary {
    pub id: i64,
    pub name: String,
    pub order_number: Option<String>,
    pub special_request: Option<String>,
    pub part_count: i64,
    pub accepted_progress: AcceptedProgress,
    pub build_stale: bool,
    pub freshness: PlanFreshness,
    pub archived_at: Option<String>,
    pub last_used_at: Option<String>,
    pub customer: Option<String>,
    pub due_date: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AcceptedProgress {
    Ready {
        total_units: i64,
        remaining_units: i64,
    },
    Empty,
    Unavailable(AcceptedProgressUnavailable),
}

#[derive(Debug, Clone, Copy)]
pub enum AcceptedProgressUnavailable {
    CompatibilityDirty,
    Uninitialized,
    Integrity,
}

#[derive(Debug, Clone)]
pub enum PlanFreshness {
    Unavailable {
        code: String,
    },
    Current {
        accepted_input_set_id: i64,
        accepted_at: String,
    },
    Stale {
        accepted_input_set_id: i64,
        accepted_at: String,
        reasons: Vec<PlanStaleReason>,
        untracked_sources: Vec<PlanUntrackedReason>,
    },
    Untracked {
        accepted_input_set_id: Option<i64>,
        accepted_at: Option<String>,
        reasons: Vec<PlanUntrackedReason>,
    },
}

#[derive(Debug, Clone)]
pub enum PlanStaleReason {
    SourceRevisionChanged {
        source_id: i64,
        source_name: String,
        accepted_revision_id: i64,
        current_revision_id: i64,
    },
    SourceRevisionUnavailable {
        source_id: i64,
        source_name: String,
        accepted_revision_id: i64,
    },
    NamingRulesChanged {
        source_id: i64,
        source_name: String,
        accepted_digest: String,
        current_digest: String,
    },
    PlanInputsInvalid,
    PlanConfigurationChanged,
}

#[derive(Debug, Clone)]
pub enum PlanUntrackedReason {
    NoAcceptedInputs,
    SourceRevisionUntracked { source_id: i64, source_name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileLayer {
    pub id: i64,
    pub layer_order: i64,
    pub layer_type: String,
    pub project_id: Option<i64>,
    pub project_name: Option<String>,
}
