//! Plan summary wire DTOs for HTTP adapters.
//!
//! `pp-api` may only depend on `pp-storage`. Re-export contract shapes here so
//! build reads can serialize the Node-compatible ProfileSummary without a
//! direct `pp-contracts` edge.

#[cfg(feature = "codegen")]
pub use pp_contracts::RequiredNullable;
pub use pp_contracts::build_identity::{
    AcceptedProgressSummary, AcceptedProgressUnavailableReason, CreateBuildRequest,
    FreshnessReasons, PlanFreshness, PlanStaleReason, PlanUntrackedReason, ProfileLayer,
    ProfileLayers, ProfileList, ProfileSummary, SourceAttachmentRequest,
};

use super::{
    AcceptedProgress, AcceptedProgressUnavailable, PlanFreshness as DomainFreshness,
    PlanStaleReason as DomainStaleReason, PlanUntrackedReason as DomainUntrackedReason,
    ProfileLayer as DomainLayer, ProfileSummary as DomainSummary,
};

pub fn layer(value: DomainLayer) -> ProfileLayer {
    ProfileLayer {
        id: value.id,
        layer_order: value.layer_order,
        layer_type: value.layer_type,
        project_id: value.project_id,
        project_name: value.project_name,
    }
}

pub fn profile_summary(value: DomainSummary) -> ProfileSummary {
    ProfileSummary {
        id: value.id,
        name: value.name,
        order_number: value.order_number,
        special_request: value.special_request,
        part_count: value.part_count,
        accepted_progress: match value.accepted_progress {
            AcceptedProgress::Ready {
                total_units,
                remaining_units,
            } => AcceptedProgressSummary::Ready {
                total_units,
                remaining_units,
            },
            AcceptedProgress::Empty => AcceptedProgressSummary::Empty,
            AcceptedProgress::Unavailable(reason) => AcceptedProgressSummary::Unavailable {
                reason: match reason {
                    AcceptedProgressUnavailable::CompatibilityDirty => {
                        AcceptedProgressUnavailableReason::CompatibilityDirty
                    }
                    AcceptedProgressUnavailable::Uninitialized => {
                        AcceptedProgressUnavailableReason::Uninitialized
                    }
                    AcceptedProgressUnavailable::Integrity => {
                        AcceptedProgressUnavailableReason::Integrity
                    }
                },
            },
        },
        build_stale: value.build_stale,
        freshness: match value.freshness {
            DomainFreshness::Unavailable { code } => PlanFreshness::Unavailable { code },
            DomainFreshness::Current {
                accepted_input_set_id,
                accepted_at,
            } => PlanFreshness::Current {
                accepted_input_set_id,
                accepted_at,
            },
            DomainFreshness::Stale {
                accepted_input_set_id,
                accepted_at,
                reasons,
                untracked_sources,
            } => PlanFreshness::Stale {
                accepted_input_set_id,
                accepted_at,
                reasons: FreshnessReasons::try_from(
                    reasons.into_iter().map(stale_reason).collect::<Vec<_>>(),
                )
                .unwrap_or_else(|_| {
                    FreshnessReasons::new(PlanStaleReason::PlanInputsInvalid, vec![])
                }),
                untracked_sources: untracked_sources
                    .into_iter()
                    .map(untracked_reason)
                    .collect(),
            },
            DomainFreshness::Untracked {
                accepted_input_set_id,
                accepted_at,
                reasons,
            } => PlanFreshness::Untracked {
                accepted_input_set_id,
                accepted_at,
                reasons: FreshnessReasons::try_from(
                    reasons
                        .into_iter()
                        .map(untracked_reason)
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_else(|_| {
                    FreshnessReasons::new(PlanUntrackedReason::NoAcceptedInputs, vec![])
                }),
            },
        },
        archived_at: value.archived_at,
        last_used_at: value.last_used_at,
        customer: value.customer,
        due_date: value.due_date,
    }
}

fn stale_reason(value: DomainStaleReason) -> PlanStaleReason {
    match value {
        DomainStaleReason::SourceRevisionChanged {
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
        DomainStaleReason::SourceRevisionUnavailable {
            source_id,
            source_name,
            accepted_revision_id,
        } => PlanStaleReason::SourceRevisionUnavailable {
            source_id,
            source_name,
            accepted_revision_id,
        },
        DomainStaleReason::NamingRulesChanged {
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
        DomainStaleReason::PlanInputsInvalid => PlanStaleReason::PlanInputsInvalid,
        DomainStaleReason::PlanConfigurationChanged => PlanStaleReason::PlanConfigurationChanged,
    }
}

fn untracked_reason(value: DomainUntrackedReason) -> PlanUntrackedReason {
    match value {
        DomainUntrackedReason::NoAcceptedInputs => PlanUntrackedReason::NoAcceptedInputs,
        DomainUntrackedReason::SourceRevisionUntracked {
            source_id,
            source_name,
        } => PlanUntrackedReason::SourceRevisionUntracked {
            source_id,
            source_name,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_graph::{AcceptedProgress, PlanFreshness as DomainFreshness};

    #[test]
    fn unavailable_freshness_preserves_integrity_code() {
        let summary = profile_summary(DomainSummary {
            id: 7,
            name: "Broken".into(),
            customer: None,
            due_date: None,
            order_number: None,
            special_request: None,
            part_count: 0,
            accepted_progress: AcceptedProgress::Unavailable(
                AcceptedProgressUnavailable::Integrity,
            ),
            build_stale: false,
            freshness: DomainFreshness::Unavailable {
                code: "accepted_inputs".into(),
            },
            archived_at: None,
            last_used_at: None,
        });
        assert_eq!(summary.id, 7);
        assert!(matches!(
            summary.accepted_progress,
            AcceptedProgressSummary::Unavailable {
                reason: AcceptedProgressUnavailableReason::Integrity
            }
        ));
        let wire = serde_json::to_value(&summary.freshness).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"status":"unavailable","code":"accepted_inputs"})
        );
    }
}
