use super::*;
use pp_storage::{
    read_model::history,
    working_drafts::{
        RebaseConflict, WorkingDraftClient,
        autosave::{AutosaveCommand, AutosaveOutcome},
        refresh::{RefreshArgs, RefreshDraftReceipt, RefreshDraftRequest, RefreshError},
        save::Refusal,
    },
};

const ADVANCE: &str = "
    INSERT INTO source_revisions (id,tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at,completeness)
        SELECT 2,tenant_id,project_id,'new',manifest_digest,'1/revisions/new',synced_at,completeness FROM source_revisions WHERE id=1;
    UPDATE projects SET current_source_revision_id=2 WHERE id=1;";
const COMPETE: &str = "
    INSERT INTO plan_drafts (tenant_id,profile_id,base_revision_id,base_plan_version,state,digest_format,snapshot_digest,created_by,idempotency_key,created_at)
        SELECT tenant_id,profile_id,base_revision_id,base_plan_version,'open',digest_format,snapshot_digest,'other-author','other-key',created_at FROM plan_drafts WHERE id=2;";

fn request(f: &Fixture, version: Option<u32>) -> RefreshDraftRequest {
    observed_request(f, id(2), version)
}

fn observed_request(f: &Fixture, draft: PositiveId, version: Option<u32>) -> RefreshDraftRequest {
    let digest = f.rows(&format!(
        "SELECT snapshot_digest FROM plan_drafts WHERE id={}",
        draft.get()
    ));
    serde_json::from_value(json!({
        "expected_draft": {"draft_id":draft,"base":{"revision_id":1,"plan_version":1},
            "state":"open","lifecycle_version":0,"snapshot_digest":digest[0]["snapshot_digest"]},
        "expected_header_version": version,
    }))
    .unwrap()
}

fn refresh_client(
    f: &Fixture,
    owner: &WriterOwner,
    files: &[&str],
    during_scan: Option<&str>,
) -> WorkingDraftClient {
    owner
        .working_drafts_with_observations(
            policy(),
            f.reads(&json!(files), &json!(during_scan), None),
            PreparationLimits::default(),
        )
        .unwrap()
}

fn run(
    client: &WorkingDraftClient,
    request: RefreshDraftRequest,
) -> Result<RefreshDraftReceipt, RefreshError> {
    let draft_id = request.expected_draft.draft_id();
    let key = format!("refresh-{}", draft_id.get());
    client.refresh(
        session(),
        id(1),
        RefreshArgs {
            draft_id,
            request,
            idempotency_key: &key,
        },
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(5),
    )
}

fn set_quantity(client: &WorkingDraftClient, request: &RefreshDraftRequest) {
    let result = client
        .edit(
            session(),
            id(1),
            id(2),
            serde_json::from_value(json!({
                "expected_snapshot_digest":request.expected_draft.snapshot_digest(),
                "decisions":[{"kind":"set_quantity_override","draft_part_ids":[6],"value":7}]
            }))
            .unwrap(),
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(serde_json::to_value(result).unwrap()["kind"], "ready");
}

fn published(f: &Fixture, owner: &WriterOwner) -> Value {
    let history::Outcome::Page(page) = owner
        .build_history(policy())
        .unwrap()
        .read(
            session(),
            history::Request {
                build: Some(id(1)),
                cursor: None,
                limit: history::DEFAULT_LIMIT,
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
    else {
        panic!("missing Build history")
    };
    json!({
        "base":f.rows("SELECT accepted_plan_revision_id,accepted_plan_version FROM build_profiles WHERE id=1"),
        "revisions":f.rows("SELECT * FROM plan_revisions ORDER BY id"),
        "parts":f.rows("SELECT * FROM plan_revision_parts ORDER BY id"),
        "units":f.rows("SELECT * FROM required_units ORDER BY token"),
        "membership":f.rows("SELECT * FROM plan_revision_required_units ORDER BY revision_id,revision_part_id,unit_index"),
        "history":page,
    })
}

#[test]
fn stale_autosave_recovers_exact_successor_after_lost_response_and_restart() {
    let f = Fixture::new(&node("base-unchanged"));
    let owner = f.open();
    let client = refresh_client(
        &f,
        &owner,
        &["bracket.stl", "excluded.stl", "gear.stl", "new.stl"],
        None,
    );
    f.sql().execute_batch("INSERT INTO plan_draft_headers VALUES (2,'default','Draft name','PO-7','Customer','2026-12-31',3,'fixture')").unwrap();
    set_quantity(&client, &request(&f, Some(3)));
    let observed = request(&f, Some(3));
    f.sql().execute_batch(ADVANCE).unwrap();
    let before = published(&f, &owner);
    let autosave = |target: &RefreshDraftRequest| {
        let body = json!({"expected_base":target.expected_draft.base(),
            "expected_draft":target.expected_draft,"remap_checkoff_links":false,
            "decisions":[{"kind":"set_quantity_override","target":{"part_key":"gear.stl","relative_path":"gear.stl","source_layer":"base:Fixture Source"},"value":9}]});
        client
            .autosave(
                AutosaveCommand::new(
                    session(),
                    id(1),
                    serde_json::from_value(body).unwrap(),
                    "stale-save",
                )
                .unwrap(),
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
    };
    assert!(matches!(
        autosave(&observed),
        AutosaveOutcome::Refused {
            reason: Refusal::InputsChanged
        }
    ));
    // Selection can fail independently. Refresh neither attempts it nor needs it to recover.
    f.sql().execute_batch("CREATE TRIGGER fail_selection BEFORE INSERT ON plan_draft_required_unit_reconciliations BEGIN SELECT RAISE(ABORT,'fixture selection failure'); END").unwrap();
    run(&client, observed.clone()).unwrap();
    owner.shutdown().unwrap();
    let owner = f.open();
    let replay = refresh_client(&f, &owner, &[], None);
    let receipt = run(&replay, observed.clone()).unwrap();
    assert_eq!(receipt.successor_draft_id, id(3));
    let mut changed = observed;
    changed.expected_header_version = None;
    assert!(matches!(
        run(&replay, changed),
        Err(RefreshError::Draft(Refusal::IdempotencyConflict))
    ));
    assert_eq!(f.rows("SELECT id,state,lifecycle_version,rebased_from_draft_id FROM plan_drafts WHERE id>=2 ORDER BY id"),
        json!([{"id":2,"state":"abandoned","lifecycle_version":1,"rebased_from_draft_id":null},
               {"id":3,"state":"open","lifecycle_version":0,"rebased_from_draft_id":2}]));
    assert_eq!(f.rows("SELECT quantity_override FROM plan_draft_parts WHERE draft_id=3 AND part_key='gear.stl'"),
        json!([{"quantity_override":7}]));
    assert_eq!(f.rows("SELECT name,order_number,customer,due_date,header_version,updated_at FROM plan_draft_headers WHERE draft_id=2"),
               f.rows("SELECT name,order_number,customer,due_date,header_version,updated_at FROM plan_draft_headers WHERE draft_id=3"));
    assert_eq!(published(&f, &owner), before);
    owner.shutdown().unwrap();
}

#[test]
fn refresh_refuses_the_wrong_author_competitors_and_concurrent_changes() {
    for (before, during, expected) in [
        (
            "UPDATE plan_drafts SET created_by='other-author' WHERE id=2",
            None,
            "author",
        ),
        (COMPETE, None, "competitor"),
        ("", Some(COMPETE), "competitor"),
        (
            "",
            Some(
                "INSERT INTO app_settings (tenant_id,key,value) VALUES ('default','kit_manifest_1','{}')",
            ),
            "inputs",
        ),
        (
            "",
            Some("UPDATE projects SET current_source_revision_id=1 WHERE id=1"),
            "inputs",
        ),
        (
            "",
            Some(
                "INSERT INTO plan_draft_headers VALUES (2,'default','Concurrent header',NULL,NULL,NULL,1,'fixture')",
            ),
            "header",
        ),
        (
            "",
            Some("UPDATE plan_drafts SET state='abandoned',lifecycle_version=1 WHERE id=2"),
            "draft",
        ),
        (
            "",
            Some("UPDATE build_profiles SET accepted_plan_version=2 WHERE id=1"),
            "base",
        ),
    ] {
        let f = Fixture::new(&node("base-unchanged"));
        let owner = f.open();
        f.sql().execute_batch(ADVANCE).unwrap();
        f.sql().execute_batch(before).unwrap();
        let client = refresh_client(
            &f,
            &owner,
            &["bracket.stl", "excluded.stl", "gear.stl"],
            during,
        );
        let parts = f.rows("SELECT * FROM plan_draft_parts ORDER BY id");
        let refusal = run(&client, request(&f, None)).unwrap_err();
        let actual = match refusal {
            RefreshError::NotAuthor => "author",
            RefreshError::CompetingDraft => "competitor",
            RefreshError::Draft(Refusal::InputsChanged) => "inputs",
            RefreshError::HeaderChanged => "header",
            RefreshError::Draft(Refusal::DraftChanged) => "draft",
            RefreshError::Draft(Refusal::BaseChanged) => "base",
            other => panic!("{other:?}"),
        };
        assert_eq!(actual, expected);
        assert_eq!(f.rows("SELECT * FROM plan_draft_parts ORDER BY id"), parts);
        assert_eq!(
            f.rows("SELECT id FROM plan_drafts WHERE id>=2 AND state='open' ORDER BY id"),
            if expected == "competitor" {
                json!([{"id":2},{"id":3}])
            } else if expected == "draft" {
                json!([])
            } else {
                json!([{"id":2}])
            }
        );
        owner.shutdown().unwrap();
    }
}

#[test]
fn refresh_preserves_quantity_on_a_rename_and_refuses_a_reused_path() {
    for path_reused in [false, true] {
        let f = Fixture::new(&node("base-unchanged"));
        let owner = f.open();
        let mut files = vec!["bracket.stl", "excluded.stl", "renamed-gear.stl"];
        if path_reused {
            files.push("gear.stl");
        }
        let saved =
            f.rows("SELECT artifact_digest, quantity_override FROM plan_draft_parts WHERE id=6");
        assert_eq!(saved[0]["quantity_override"], 2);
        let digest = saved[0]["artifact_digest"].as_str().unwrap().to_owned();
        let reads = f.reads_with_digests(
            &json!(files),
            &Value::Null,
            None,
            [("renamed-gear.stl".into(), digest)].into(),
        );
        let client = owner
            .working_drafts_with_observations(policy(), reads, PreparationLimits::default())
            .unwrap();
        set_quantity(&client, &request(&f, None));
        f.sql().execute_batch(ADVANCE).unwrap();
        let before = f.recorded_rows();
        let result = run(&client, request(&f, None));
        if path_reused {
            assert!(
                matches!(result, Err(RefreshError::MergeConflicts(ref conflicts))
                if matches!(conflicts.as_slice(), [RebaseConflict::TargetAmbiguous { source_part_id: 6, .. }])),
                "{result:?}"
            );
            assert_eq!(f.recorded_rows(), before);
        } else {
            assert_eq!(result.unwrap().successor_draft_id, id(3));
            assert_eq!(
                f.rows("SELECT quantity_override, quantity_effective FROM plan_draft_parts WHERE draft_id=3 AND part_key='renamed-gear.stl'"),
                json!([{"quantity_override":7,"quantity_effective":7}])
            );
        }
        owner.shutdown().unwrap();
    }
}

#[test]
fn refresh_keeps_new_part_filament_or_refuses_if_preparation_would_change_it() {
    let f = Fixture::new(&node("base-unchanged"));
    let owner = f.open();
    let set_default = |color: &str| {
        f.sql().execute(
            "INSERT OR REPLACE INTO app_settings (tenant_id,key,value) VALUES ('default','role_filaments_1',?)",
            [json!({"primary":{"filament_custom_hex":color}}).to_string()],
        ).unwrap();
    };
    set_default("#123456");
    f.sql().execute_batch(ADVANCE).unwrap();
    let client = refresh_client(
        &f,
        &owner,
        &["bracket.stl", "excluded.stl", "gear.stl", "new.stl"],
        None,
    );
    let first = run(&client, request(&f, None)).unwrap();
    assert_eq!(f.rows("SELECT filament_custom_hex FROM plan_draft_parts WHERE draft_id=3 AND part_key='new.stl'"),
        json!([{"filament_custom_hex":"#123456"}]));
    f.sql().execute_batch(
        "INSERT INTO source_revisions (id,tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at,completeness)
         SELECT 3,tenant_id,project_id,'next',manifest_digest,'1/revisions/next',synced_at,completeness FROM source_revisions WHERE id=2;
         UPDATE projects SET current_source_revision_id=3 WHERE id=1;"
    ).unwrap();
    let observed = observed_request(&f, first.successor_draft_id, None);
    set_default("#abcdef");
    let before = f.recorded_rows();
    let result = run(&client, observed.clone());
    assert!(
        matches!(result, Err(RefreshError::MergeConflicts(ref conflicts))
        if matches!(conflicts.as_slice(), [RebaseConflict::UnsupportedPartDecision { .. }])),
        "{result:?}"
    );
    assert_eq!(f.recorded_rows(), before);
    set_default("#123456");
    assert_eq!(run(&client, observed).unwrap().successor_draft_id, id(4));
    assert_eq!(f.rows("SELECT filament_custom_hex FROM plan_draft_parts WHERE draft_id=4 AND part_key='new.stl'"),
        json!([{"filament_custom_hex":"#123456"}]));
    owner.shutdown().unwrap();
}

#[test]
fn refresh_keeps_the_original_manual_reconciliation() {
    use pp_storage::required_units::{Outcome, ReconciliationCommand};
    let f = Fixture::new(&node("base-unchanged"));
    std::fs::write(
        f.database(),
        include_bytes!("../fixtures/required-units/unsafe.db"),
    )
    .unwrap();
    let owner = f.open();
    let before_selection = request(&f, None);
    let command = ReconciliationCommand::new(
        id(1),
        id(2),
        serde_json::from_value(
            json!({"expected_snapshot_digest":before_selection.expected_draft.snapshot_digest(),
             "decisions":[{"kind":"replace","target_draft_part_id":4}]}),
        )
        .unwrap(),
        "manual-replace",
        session(),
    )
    .unwrap();
    let selected = owner
        .required_units_with_policy(policy())
        .unwrap()
        .reconcile(
            command,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(matches!(selected, Outcome::Ready { .. }));
    f.sql().execute_batch(ADVANCE).unwrap();
    let before = f.recorded_rows();
    let client = refresh_client(&f, &owner, &[], None);
    let result = run(&client, request(&f, None));
    assert!(
        matches!(result, Err(RefreshError::UnsupportedDecisions)),
        "{result:?}"
    );
    assert_eq!(f.recorded_rows(), before);
    owner.shutdown().unwrap();
}

#[test]
fn refresh_keeps_the_original_on_an_unmapped_edit_or_failed_successor_insert() {
    let f = Fixture::new(&node("base-unchanged"));
    let owner = f.open();
    let files = ["bracket.stl", "excluded.stl", "gear.stl"];
    let original = refresh_client(&f, &owner, &files, None);
    set_quantity(&original, &request(&f, None));
    f.sql().execute_batch(ADVANCE).unwrap();
    let observed = request(&f, None);
    let before = f.recorded_rows();
    let removed = refresh_client(&f, &owner, &files[..2], None);
    assert!(
        matches!(run(&removed, observed.clone()), Err(RefreshError::MergeConflicts(conflicts))
        if matches!(conflicts.as_slice(), [RebaseConflict::TargetMissing { source_part_id:6 }]))
    );
    assert_eq!(f.recorded_rows(), before);
    f.sql().execute_batch("CREATE TRIGGER fail_successor BEFORE INSERT ON plan_drafts BEGIN SELECT RAISE(ABORT,'fixture successor failure'); END").unwrap();
    assert!(matches!(
        run(&original, observed),
        Err(RefreshError::Writer(pp_storage::WriterFailure::Internal(_)))
    ));
    assert_eq!(f.recorded_rows(), before);
    owner.shutdown().unwrap();
}
