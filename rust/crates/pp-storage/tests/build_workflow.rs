//! Build workflow reads (GRE-1123) on a disposable on-disk copy of the Node
//! accepted-Plan fixture: Build 1 has an Accepted Plan with required units and
//! no printer links.
#![cfg(feature = "desktop_rebuild")]

use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    build_graph::PositiveId,
    read_model::{
        Credential,
        build_workflow::{BuildInputs, Outcome, Request, TaskFeed, WorkflowInputs, rank},
        workflow::{
            self, Accepted, ActionKind, Build, Checkoff, Facts, PlateState, Production, Sources,
            StageId, StatusKind, Stock, Target, Working, Workspace,
        },
    },
};
use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "common/scratch.rs"]
mod scratch;

const SESSION: &str = "read-fixture-secret";

#[derive(Default)]
struct Inputs(HashMap<i64, BuildInputs>);

impl WorkflowInputs for Inputs {
    fn inputs(&self, _tenant: &str, build: i64) -> BuildInputs {
        self.0.get(&build).copied().unwrap_or_default()
    }
}

fn due(day: u8) -> Option<time::Date> {
    Some(time::Date::from_calendar_date(2026, time::Month::November, day).unwrap())
}

/// Build 1 of the Node fixture in a scratch directory. `owner` drops before
/// `root` removes the directory.
struct Fixture {
    owner: Option<WriterOwner>,
    root: scratch::Scratch,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch::Scratch::new("pp-build-workflow-");
        let database = include_bytes!("fixtures/accepted-plan-node.db");
        std::fs::write(root.join("print-partner.db"), database).unwrap();
        pp_os::create_private_dir_all(&root.join("repos/1/revisions/accepted")).unwrap();
        let owner = Some(WriterOwner::open(&root, Limits::default()).unwrap().0);
        Self { owner, root }
    }

    fn owner(&self) -> &WriterOwner {
        self.owner.as_ref().unwrap()
    }

    fn sql(&self, sql: &str) {
        let db = rusqlite::Connection::open(self.root.join("print-partner.db")).unwrap();
        db.busy_timeout(Duration::from_secs(3)).unwrap();
        db.execute_batch(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    fn links(&self, state: &str) {
        self.sql(&format!(
            "INSERT INTO app_settings (tenant_id,key,value) VALUES ('default','printer.checkoff_links',
             '[{{\"id\":\"link-1\",\"profile_id\":1,\"integration_id\":\"moonraker-1\",\"printer_id\":\"voron\",\"filename\":\"plate.gcode\",\"state\":\"{state}\"}}]')
             ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value"
        ));
    }

    fn read(&self, inputs: Inputs, secret: &str, request: Request) -> anyhow::Result<Outcome> {
        let policy = AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        };
        self.owner()
            .build_workflow(policy, Arc::new(inputs))
            .unwrap()
            .read(
                Credential::Session(Secret::new(secret.into())),
                request,
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
            )
    }

    fn workspace(&self, inputs: Inputs, id: u64) -> Workspace {
        let request = Request::Build(Some(PositiveId::new(id).unwrap()));
        match self.read(inputs, SESSION, request).unwrap() {
            Outcome::Workspace(workspace) => *workspace,
            other => panic!("{other:?}"),
        }
    }

    fn tasks(&self, inputs: Inputs) -> TaskFeed {
        match self.read(inputs, SESSION, Request::Tasks).unwrap() {
            Outcome::Tasks(feed) => feed,
            other => panic!("{other:?}"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.owner
            .take()
            .map(WriterOwner::shutdown)
            .transpose()
            .unwrap();
    }
}

#[test]
fn build_workflow_ranks_the_earlier_due_date_first() {
    let f = Fixture::new();
    f.sql("INSERT INTO build_profiles (tenant_id,name) VALUES ('default','Later order'),('default','Sooner order');");
    let inputs = || {
        Inputs(HashMap::from([
            (
                2,
                BuildInputs {
                    due_date: due(20),
                    stock: Stock::NotReady,
                },
            ),
            (
                3,
                BuildInputs {
                    due_date: due(10),
                    stock: Stock::NotReady,
                },
            ),
        ]))
    };
    let feed = f.tasks(inputs());
    let order: Vec<_> = feed
        .tasks
        .iter()
        .map(|task| {
            (
                task.plan_id,
                task.due_date.as_deref(),
                task.rank_reason.as_str(),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            (3, Some("2026-11-10"), "Due 2026-11-10."),
            (2, Some("2026-11-20"), "Due 2026-11-20."),
            (1, None, "No due date."),
        ]
    );
    // The feed's next action is the Build workflow's own, not a copy.
    for task in &feed.tasks {
        let workspace = f.workspace(inputs(), task.plan_id as u64);
        assert_eq!(task.next_action, workspace.next_action);
        assert_eq!(task.build_name, workspace.build.name);
        assert!(!task.attention);
    }
    assert_eq!(feed.tasks[0].next_action.kind, ActionKind::AttachSources);
}

#[test]
fn build_workflow_confirmed_bed_is_not_attention() {
    let f = Fixture::new();
    f.sql("UPDATE print_progress SET completed=1;");
    f.links("verified");
    // Sources change after every unit is checked off.
    f.owner()
        .local_source_catalog()
        .execute(pp_storage::catalog::Request::SaveImportRules {
            id: 1,
            rules: vec!["STLs".into()],
        })
        .unwrap();
    let workspace = f.workspace(Inputs::default(), 1);
    assert_eq!(workspace.next_action.kind, ActionKind::ViewCompletedBuild);
    assert!(matches!(
        workspace.sources,
        pp_storage::read_model::workflow::Sources::Stale { .. }
    ));
    for stage in &workspace.stages {
        assert!(
            !matches!(
                stage.status.kind,
                StatusKind::NeedsAttention | StatusKind::Stale
            ) && stage.status.task_count.is_none(),
            "{stage:?}"
        );
    }
    assert_eq!(workspace.stages[0].status.kind, StatusKind::Complete);
    assert!(f.tasks(Inputs::default()).tasks.is_empty());
    // An autosaved draft without changes leaves the Build finished.
    unchanged_draft(&f);
    let with_draft = f.workspace(Inputs::default(), 1);
    assert_eq!(with_draft.next_action, workspace.next_action);
    assert_eq!(with_draft.stages, workspace.stages);
    assert!(f.tasks(Inputs::default()).tasks.is_empty());

    // The same bed still waiting for confirmation is attention.
    f.links("awaiting_verify");
    let workspace = f.workspace(Inputs::default(), 1);
    assert_eq!(workspace.next_action.kind, ActionKind::VerifyPrints);
    let feed = f.tasks(Inputs::default());
    assert_eq!(feed.tasks.len(), 1);
    assert!(feed.tasks[0].attention);
    assert_eq!(feed.tasks[0].rank_reason, "Needs attention now.");
    assert_eq!(feed.tasks[0].next_action, workspace.next_action);
}

#[test]
fn build_workflow_without_printer_link_is_manual_checkoff() {
    let f = Fixture::new();
    let workspace = f.workspace(Inputs::default(), 1);
    let action = &workspace.next_action;
    assert_eq!(action.kind, ActionKind::CheckOffManually);
    assert_eq!(
        action.target,
        Target {
            plan_id: 1,
            tab: StageId::Checkoff,
            draft_id: None,
        }
    );
    let remaining = workspace.active_work.remaining_units;
    assert!(remaining > 0);
    assert_eq!(action.unit_count, Some(remaining));
    assert!(
        action
            .reason
            .starts_with("No printer is linked to this Build."),
        "{}",
        action.reason
    );
    assert_eq!(f.tasks(Inputs::default()).tasks[0].next_action, *action);

    // A dismissed link links nothing; a confirmed one means a printer is used.
    f.links("dismissed");
    assert_eq!(
        f.workspace(Inputs::default(), 1).next_action.kind,
        ActionKind::CheckOffManually
    );
    f.links("applied");
    assert_eq!(
        f.workspace(Inputs::default(), 1).next_action.kind,
        ActionKind::PrepareProduction
    );
}

#[test]
fn build_workflow_autosaved_draft_says_saved_not_accepted() {
    let f = Fixture::new();
    f.sql(&format!(
        "INSERT INTO plan_drafts (tenant_id,profile_id,base_revision_id,base_plan_version,state,digest_format,snapshot_digest,created_by,idempotency_key,created_at)
         VALUES ('default',1,1,1,'open','v1','{}','user','autosave-1','2026-11-04T12:00:00.000Z');",
        "a".repeat(64)
    ));
    let workspace = f.workspace(Inputs::default(), 1);
    let action = &workspace.next_action;
    assert_eq!(action.kind, ActionKind::ReviewWorkingPlan);
    let draft = action.draft_id.expect("draft id");
    assert_eq!(action.target.draft_id, Some(draft));
    assert_eq!(action.target.tab, StageId::Plan);
    let plan = &workspace.stages[1].status.summary;
    assert!(
        action.reason.contains("saved in a draft"),
        "{}",
        action.reason
    );
    assert!(plan.contains("saved in a draft"), "{plan}");
    for text in [&action.label, &action.reason, plan] {
        assert!(!text.to_lowercase().contains("accept"), "{text}");
    }
}

/// Opens an autosaved draft whose parts equal the Accepted Plan's.
fn unchanged_draft(f: &Fixture) {
    let fields = "part_key,relative_path,filename,source_layer,status,role_inferred,role_override,filament_color_id,filament_custom_hex,spoolman_spool_id,quantity_inferred,quantity_override,quantity_effective,included,notes,github_blob_url,geometry_same,requirement,option_group_id,manifest_source,artifact_digest";
    f.sql(&format!(
        "INSERT INTO plan_drafts (tenant_id,profile_id,base_revision_id,base_plan_version,state,digest_format,snapshot_digest,created_by,idempotency_key,created_at)
         VALUES ('default',1,1,1,'open','v1','{}','user','autosave-0','2026-11-04T12:00:00.000Z');
         INSERT INTO plan_draft_parts (tenant_id,draft_id,base_revision_part_id,{fields})
         SELECT tenant_id,last_insert_rowid(),id,{fields} FROM plan_revision_parts WHERE revision_id=1;",
        "a".repeat(64)
    ));
}

#[test]
fn build_workflow_unchanged_draft_keeps_the_build_next_action() {
    let f = Fixture::new();
    let before = f.workspace(Inputs::default(), 1);
    unchanged_draft(&f);
    let after = f.workspace(Inputs::default(), 1);
    assert!(matches!(
        after.working_plan,
        Working::Ready {
            change_count: 0,
            ..
        }
    ));
    assert_eq!(after.next_action, before.next_action);
    assert_eq!(after.stages[1].status, before.stages[1].status);
}

#[test]
fn build_workflow_takes_stock_shortfall_from_its_adapter() {
    let f = Fixture::new();
    let stock = |stock| {
        Inputs(HashMap::from([(
            1,
            BuildInputs {
                due_date: None,
                stock,
            },
        )]))
    };
    let pending = f.workspace(stock(Stock::NotReady), 1);
    assert_eq!(pending.stock, Stock::NotReady);
    assert_eq!(pending.next_action.kind, ActionKind::CheckOffManually);
    let enough = f.workspace(stock(Stock::Ready { shortfalls: 0 }), 1);
    assert_eq!(enough.next_action.kind, ActionKind::CheckOffManually);
    let short = f.workspace(stock(Stock::Ready { shortfalls: 2 }), 1);
    assert_eq!(short.next_action.kind, ActionKind::ResolveStockShortfall);
    assert_eq!(short.next_action.item_count, Some(2));
    let feed = f.tasks(stock(Stock::Ready { shortfalls: 2 }));
    assert!(feed.tasks[0].attention);
    assert_eq!(feed.tasks[0].next_action, short.next_action);
}

#[test]
fn build_workflow_reads_authenticate_before_answering() {
    let f = Fixture::new();
    let build = |id| Request::Build(PositiveId::new(id).ok());
    for request in [build(1), build(999), Request::Build(None), Request::Tasks] {
        assert!(
            f.read(Inputs::default(), "not-a-session", request).is_err(),
            "{request:?}"
        );
    }
    for request in [build(999), Request::Build(None)] {
        assert!(matches!(
            f.read(Inputs::default(), SESSION, request).unwrap(),
            Outcome::MissingBuild
        ));
    }
}

#[test]
fn build_workflow_task_feed_reports_an_unreadable_build() {
    let f = Fixture::new();
    f.sql("INSERT INTO build_profiles (tenant_id,name) VALUES ('default','Healthy order');");
    let healthy = f.workspace(Inputs::default(), 2);
    // Corrupt Build 1's Plan history past its immutability triggers.
    let db = rusqlite::Connection::open(f.root.join("print-partner.db")).unwrap();
    let drops: String = db
        .query_row(
            "SELECT group_concat('DROP TRIGGER \"' || name || '\";', '') FROM sqlite_master WHERE type='trigger' AND tbl_name='plan_revisions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(db);
    f.sql(&drops);
    f.sql("UPDATE plan_revisions SET parent_revision_id=id WHERE profile_id=1");
    let feed = f.tasks(Inputs::default());
    assert_eq!(feed.tasks.len(), 1);
    assert_eq!(feed.tasks[0].plan_id, 2);
    assert_eq!(feed.tasks[0].next_action, healthy.next_action);
    assert_eq!(feed.unavailable.len(), 1);
    assert_eq!(feed.unavailable[0].plan_id, 1);
    assert!(!feed.unavailable[0].reason.is_empty());
    assert!(
        f.read(
            Inputs::default(),
            SESSION,
            Request::Build(PositiveId::new(1).ok())
        )
        .is_err()
    );
}

#[test]
fn build_workflow_prepared_plates_without_printer_link_are_manual_checkoff() {
    let facts = |plate_state, printer_links| Facts {
        build: Build {
            id: 1,
            name: "Order".into(),
        },
        sources: Sources::Ready { attached_count: 1 },
        accepted_plan: Accepted::Ready {
            revision_id: 1,
            plan_version: 1,
            total_units: 3,
            remaining_units: 2,
        },
        working_plan: Working::None,
        production: Production {
            plate_state,
            queued_jobs: 0,
            sending_jobs: 0,
            printing_jobs: 0,
            failed_jobs: 0,
        },
        checkoff: Checkoff {
            awaiting_verification: 0,
            failed_verifications: 0,
            printer_links,
        },
        stock: Stock::NotReady,
    };
    let kind = |plate_state, printer_links| {
        workflow::resolve(&facts(plate_state, printer_links))
            .unwrap()
            .next_action
            .kind
    };
    assert_eq!(kind(PlateState::Ready, 0), ActionKind::CheckOffManually);
    assert_eq!(
        kind(PlateState::NotStarted, 0),
        ActionKind::CheckOffManually
    );
    assert_eq!(kind(PlateState::Ready, 1), ActionKind::PrepareProduction);
    assert_eq!(kind(PlateState::Stale, 0), ActionKind::PrepareProduction);
}

// GRE-1089: the ranked task feed (TSK-01 to TSK-05). Each test reads the feed
// through the same client as the Build workflow read.

/// Every table's rows as quoted text, so two dumps compare the logical
/// database rather than its pages.
fn task_feed_logical_database(f: &Fixture) -> Vec<String> {
    let db = rusqlite::Connection::open(f.root.join("print-partner.db")).unwrap();
    let queries: Vec<String> = db
        .prepare(
            "SELECT 'SELECT ' || (SELECT group_concat('quote(\"' || name || '\")', '||''|''||') FROM pragma_table_info(m.name)) || ' FROM \"' || m.name || '\" ORDER BY rowid' FROM sqlite_master m WHERE m.type='table' ORDER BY m.name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    queries
        .iter()
        .flat_map(|query| {
            let mut rows: Vec<String> = db
                .prepare(query)
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows.insert(0, query.clone());
            rows
        })
        .collect()
}

/// Managed files under the data directory. SQLite's `-shm` index changes on
/// every read lock, so it is left out; the database and WAL bytes are kept.
fn task_feed_files(f: &Fixture) -> pp_testkit::Snapshot {
    let mut files = pp_testkit::snapshot(&f.root).unwrap();
    files.retain(|path, _| !path.to_string_lossy().ends_with("-shm"));
    files
}

/// Builds 2 to 4 have no Sources, Build 5 is archived. Build 1 is short of
/// stock, Build 3 is due first, Build 2 second and Build 4 has no due date.
fn task_feed_farm(f: &Fixture) -> Inputs {
    f.sql(
        "INSERT INTO build_profiles (tenant_id,name) VALUES ('default','Later order'),('default','Sooner order'),('default','Undated order');
         INSERT INTO build_profiles (tenant_id,name,archived_at) VALUES ('default','Archived order','2026-11-01T00:00:00.000Z');",
    );
    let inputs = |due_date, stock| BuildInputs { due_date, stock };
    Inputs(HashMap::from([
        (1, inputs(None, Stock::Ready { shortfalls: 2 })),
        (2, inputs(due(20), Stock::NotReady)),
        (3, inputs(due(10), Stock::NotReady)),
        (5, inputs(due(1), Stock::NotReady)),
    ]))
}

#[test]
fn task_feed_tasks_name_build_kind_reason_and_target() {
    let f = Fixture::new();
    let inputs = task_feed_farm(&f);
    let feed = f.tasks(Inputs(inputs.0.clone()));
    assert_eq!(feed.tasks.len(), 4);
    for task in &feed.tasks {
        let workspace = f.workspace(Inputs(inputs.0.clone()), task.plan_id as u64);
        let action = &task.next_action;
        assert_eq!(task.build_name, workspace.build.name);
        assert!(!task.build_name.is_empty());
        assert_eq!(action.kind, workspace.next_action.kind);
        assert!(!action.label.is_empty() && !action.reason.is_empty());
        assert_eq!(action.reason, workspace.next_action.reason);
        assert_eq!(action.target.plan_id, task.plan_id);
        assert_eq!(action.target.tab, action.stage_id);
    }
    assert_eq!(
        feed.tasks[0].next_action.kind,
        ActionKind::ResolveStockShortfall
    );
    assert!(
        feed.tasks[0]
            .next_action
            .reason
            .starts_with("2 filaments are short"),
        "{}",
        feed.tasks[0].next_action.reason
    );
    assert_eq!(feed.tasks[1].next_action.kind, ActionKind::AttachSources);
}

#[test]
fn task_feed_order_is_the_projection_order() {
    let f = Fixture::new();
    let inputs = task_feed_farm(&f);
    let feed = f.tasks(Inputs(inputs.0.clone()));
    let order: Vec<_> = feed.tasks.iter().map(|task| task.plan_id).collect();
    assert_eq!(order, [1, 3, 2, 4]);
    // The feed is the shared `rank` over each Build's own workflow read.
    let resolved = (1..=4)
        .map(|id| {
            let due_date = inputs.inputs("default", id).due_date;
            (f.workspace(Inputs(inputs.0.clone()), id as u64), due_date)
        })
        .collect();
    assert_eq!(feed, rank(resolved, Vec::new()));
    assert!(feed.unavailable.is_empty());
}

#[test]
fn task_feed_stale_only_with_open_work_and_no_task_when_checked_off() {
    let f = Fixture::new();
    f.links("verified");
    f.owner()
        .local_source_catalog()
        .execute(pp_storage::catalog::Request::SaveImportRules {
            id: 1,
            rules: vec!["STLs".into()],
        })
        .unwrap();
    // Units remain: the changed Sources are a decision.
    let open = f.workspace(Inputs::default(), 1);
    assert_eq!(open.stages[0].status.kind, StatusKind::Stale);
    assert_eq!(f.tasks(Inputs::default()).tasks.len(), 1);
    // Every unit checked off: no stale stage and no task.
    f.sql("UPDATE print_progress SET completed=1;");
    let done = f.workspace(Inputs::default(), 1);
    assert!(
        done.stages
            .iter()
            .all(|stage| stage.status.kind != StatusKind::Stale)
    );
    assert!(f.tasks(Inputs::default()).tasks.is_empty());
}

#[test]
fn task_feed_offers_manual_checkoff_without_printer_links() {
    let f = Fixture::new();
    let feed = f.tasks(Inputs::default());
    assert_eq!(feed.tasks.len(), 1);
    assert_eq!(feed.tasks[0].next_action.kind, ActionKind::CheckOffManually);
    assert_eq!(feed.tasks[0].next_action.target.tab, StageId::Checkoff);
}

#[test]
fn task_feed_writes_nothing() {
    let f = Fixture::new();
    let inputs = task_feed_farm(&f);
    f.links("awaiting_verify");
    let database = task_feed_logical_database(&f);
    let files = task_feed_files(&f);
    for _ in 0..3 {
        assert_eq!(f.tasks(Inputs(inputs.0.clone())).tasks.len(), 4);
    }
    assert_eq!(task_feed_logical_database(&f), database);
    assert_eq!(task_feed_files(&f), files);
}
