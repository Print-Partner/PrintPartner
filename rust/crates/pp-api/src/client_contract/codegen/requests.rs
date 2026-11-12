use super::*;

/// Draft header reads, saves and save bodies (GRE-1197).
pub(super) fn header_fixtures() -> Result<Vec<Value>, serde_json::Error> {
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
pub(super) fn previews() -> Vec<(&'static str, AcceptPreview)> {
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
pub(super) fn catalog_fixtures() -> Result<Vec<Value>, serde_json::Error> {
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

/// Autosave and Accept: their bodies at the bounds, and their answers.
pub(super) fn lifecycle_fixtures(workspace: &Workspace) -> Result<Vec<Value>, serde_json::Error> {
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

/// Checkoff burst requests the generated client must accept or refuse as
/// Rust does, and the receipts the routes serve.
pub(super) fn burst_fixtures() -> Result<Vec<Value>, serde_json::Error> {
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
pub(super) fn manual_burst_fixtures() -> Result<Vec<Value>, serde_json::Error> {
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
pub(super) fn unattributed_fixtures() -> Result<Vec<Value>, serde_json::Error> {
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
pub(super) fn planning_fixtures() -> Result<Vec<Value>, serde_json::Error> {
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