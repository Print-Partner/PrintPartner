use super::model::{
    AcceptedProgress, AcceptedProgressUnavailable, PlanFreshness, PlanStaleReason,
    PlanUntrackedReason, ProfileSummary,
};
use crate::read_model::graph::{
    AcceptedProgressFacts, BuildSummaryFacts, FreshnessFacts, StaleReasonFacts,
    UntrackedReasonFacts,
};

fn stale_reason(reason: StaleReasonFacts) -> PlanStaleReason {
    match reason {
        StaleReasonFacts::SourceRevisionChanged {
            source_id,
            source_name,
            accepted_revision_id,
            current_revision_id,
        } => PlanStaleReason::SourceRevisionChanged {
            source_id,
            source_name,
            accepted_revision_id,
            current_revision_id,
        },
        StaleReasonFacts::SourceRevisionUnavailable {
            source_id,
            source_name,
            accepted_revision_id,
        } => PlanStaleReason::SourceRevisionUnavailable {
            source_id,
            source_name,
            accepted_revision_id,
        },
        StaleReasonFacts::NamingRulesChanged {
            source_id,
            source_name,
            accepted_digest,
            current_digest,
        } => PlanStaleReason::NamingRulesChanged {
            source_id,
            source_name,
            accepted_digest,
            current_digest,
        },
        StaleReasonFacts::PlanInputsInvalid => PlanStaleReason::PlanInputsInvalid,
        StaleReasonFacts::PlanConfigurationChanged => PlanStaleReason::PlanConfigurationChanged,
    }
}

fn untracked_reason(reason: UntrackedReasonFacts) -> PlanUntrackedReason {
    match reason {
        UntrackedReasonFacts::NoAcceptedInputs => PlanUntrackedReason::NoAcceptedInputs,
        UntrackedReasonFacts::SourceRevisionUntracked {
            source_id,
            source_name,
        } => PlanUntrackedReason::SourceRevisionUntracked {
            source_id,
            source_name,
        },
    }
}

pub(crate) fn profile_summary(facts: BuildSummaryFacts) -> ProfileSummary {
    let accepted_progress = match facts.accepted_progress {
        AcceptedProgressFacts::Ready {
            total_units,
            remaining_units,
        } => AcceptedProgress::Ready {
            total_units,
            remaining_units,
        },
        AcceptedProgressFacts::Empty => AcceptedProgress::Empty,
        AcceptedProgressFacts::CompatibilityDirty => {
            AcceptedProgress::Unavailable(AcceptedProgressUnavailable::CompatibilityDirty)
        }
        AcceptedProgressFacts::Uninitialized => {
            AcceptedProgress::Unavailable(AcceptedProgressUnavailable::Uninitialized)
        }
        AcceptedProgressFacts::Integrity => {
            AcceptedProgress::Unavailable(AcceptedProgressUnavailable::Integrity)
        }
    };
    let freshness = match facts.freshness {
        FreshnessFacts::Unavailable { code } => PlanFreshness::Unavailable { code },
        FreshnessFacts::Current {
            accepted_input_set_id,
            accepted_at,
        } => PlanFreshness::Current {
            accepted_input_set_id,
            accepted_at,
        },
        FreshnessFacts::Stale {
            accepted_input_set_id,
            accepted_at,
            reasons,
            untracked_sources,
        } => PlanFreshness::Stale {
            accepted_input_set_id,
            accepted_at,
            reasons: reasons.into_iter().map(stale_reason).collect(),
            untracked_sources: untracked_sources
                .into_iter()
                .map(untracked_reason)
                .collect(),
        },
        FreshnessFacts::Untracked {
            accepted_input_set_id,
            accepted_at,
            reasons,
        } => PlanFreshness::Untracked {
            accepted_input_set_id,
            accepted_at,
            reasons: reasons.into_iter().map(untracked_reason).collect(),
        },
    };
    ProfileSummary {
        id: facts.id,
        name: facts.name,
        order_number: facts.order_number,
        special_request: facts.special_request,
        part_count: facts.part_count,
        accepted_progress,
        build_stale: matches!(freshness, PlanFreshness::Stale { .. }),
        freshness,
        archived_at: facts.archived_at,
        last_used_at: facts.last_used_at,
        customer: facts.customer,
        due_date: facts.due_date,
    }
}
