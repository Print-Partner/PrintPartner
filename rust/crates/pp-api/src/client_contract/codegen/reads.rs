use super::*;

pub(super) fn printer_display_descriptors() -> Vec<PrinterDisplayDescriptor> {
    PrinterDisplayState::ALL
        .iter()
        .map(|state| state.descriptor())
        .collect()
}

/// One printer state per display summary.
pub(super) fn printer_states() -> Vec<PrinterStateView> {
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
pub(super) fn farm_bodies() -> (
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
pub(super) fn build_history() -> BuildHistoryPage {
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
pub(super) fn farm_log() -> FarmLogPage {
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
pub(super) fn match_suggestions() -> MatchSuggestionList {
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

pub(super) fn summary(
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

pub(super) fn summaries() -> Vec<ProfileSummary> {
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

pub(super) fn identity(state: &str, revision_id: Option<i64>) -> DraftIdentity {
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

pub(super) fn part(id: i64, filled: bool) -> PartView {
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

pub(super) fn workspaces() -> Vec<Workspace> {
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

pub(super) fn health(
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

pub(super) fn mixed_modules() -> Vec<pp_storage::features::modules::ModuleStatus> {
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
pub(super) fn build_workflows() -> Result<Vec<BuildWorkflow>, serde_json::Error> {
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
    .map(|facts| workflow::resolve(facts).map_err(<serde_json::Error as serde::ser::Error>::custom))
    .collect()
}

/// Every need, stock and verdict variant.
pub(super) fn shortfalls() -> Vec<FilamentShortfalls> {
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
