use super::*;
use crate::{
    Limits, WriterOwner,
    auth::Secret,
    restore::{CONFIRMATION, Refusal, RestoreError},
    write::test_support::{ACCOUNT_POLICY, WAIT, first_session, opened},
};
use std::path::Path;

struct Queue {
    actions: PendingActionsClient,
    session: String,
}

impl Queue {
    fn on(owner: &WriterOwner) -> Self {
        let session = first_session(owner);
        Self::with(owner, session)
    }

    fn with(owner: &WriterOwner, session: String) -> Self {
        let reads = owner.accepted_reads_with_policy(ACCOUNT_POLICY).unwrap();
        Self {
            actions: reads.pending_actions(),
            session,
        }
    }

    fn key(&self) -> Credential {
        Credential::Session(Secret::new(self.session.clone()))
    }

    fn stage(&self, id: &str, depends_on: &[&str]) -> Result<PendingAction, ActionError> {
        let action = NewAction {
            id: id.into(),
            action_type: "propose_create_build".into(),
            plan_id: 0,
            label: format!("create build {id}"),
            summary: "Create the Build.".into(),
            params: serde_json::from_str(r#"{"name":"Night lamp"}"#).unwrap(),
            depends_on: depends_on.iter().map(|id| (*id).into()).collect(),
            staged_by: StagedBy::label(" claude\u{7}-ai\n"),
        };
        self.actions
            .stage(self.key(), action, &AtomicBool::new(false), WAIT)
    }

    fn list(&self) -> Listing {
        self.actions
            .list(self.key(), &AtomicBool::new(false), WAIT)
            .unwrap()
    }

    fn states(&self) -> Vec<(String, State)> {
        let listing = self.list();
        listing
            .actions
            .into_iter()
            .map(|a| (a.id, a.state))
            .collect()
    }

    fn claim(&self, id: &str) -> Result<Claim, ActionError> {
        self.actions
            .claim(self.key(), id, &AtomicBool::new(false), WAIT)
    }

    fn settle(&self, token: &ClaimToken, end: End) -> bool {
        self.actions
            .settle(token, end, &AtomicBool::new(false), WAIT)
            .unwrap()
    }

    fn dismiss(&self, id: &str) -> Result<(), ActionError> {
        self.actions
            .dismiss(self.key(), id, &AtomicBool::new(false), WAIT)
    }
}

fn staged(chain: &[(&str, &[&str])]) -> (tempfile::TempDir, WriterOwner, Queue) {
    let (directory, owner) = opened(Limits::default());
    let queue = Queue::on(&owner);
    for (id, depends_on) in chain {
        queue.stage(id, depends_on).unwrap();
    }
    (directory, owner, queue)
}

fn sql(directory: &Path, statement: &str) {
    rusqlite::Connection::open(directory.join("print-partner.db"))
        .unwrap()
        .execute_batch(statement)
        .unwrap();
}

fn expire(directory: &Path, id: &str) {
    sql(
        directory,
        &format!("UPDATE pending_actions SET expires_at = created_at WHERE id = '{id}'"),
    );
}

#[test]
fn a_staged_action_is_listed_with_its_times_staged_by_and_dependencies() {
    let (_directory, owner) = opened(Limits::default());
    let queue = Queue::on(&owner);
    queue.stage("a", &[]).unwrap();
    let b = queue.stage("b", &["a", "a"]).unwrap();
    assert_eq!(b.depends_on, ["a"]);
    let listing = queue.list();
    assert_eq!(listing.omitted, 0);
    let [first, second] = listing.actions.try_into().unwrap();
    assert_eq!((first.id.as_str(), first.state), ("a", State::Pending));
    assert_eq!(second, b);
    assert_eq!(second.staged_by, "claude-ai");
    assert_eq!(second.params["name"], "Night lamp");
    assert_eq!(second.created_at.len(), "2026-11-22T23:04:05.123Z".len());
    let created = sqlite_unix_seconds(&second.created_at);
    assert_eq!(
        sqlite_unix_seconds(&second.expires_at) - created,
        8 * 60 * 60
    );
    assert!(matches!(queue.stage("a", &[]), Err(ActionError::IdTaken)));
    assert_eq!(StagedBy::label(" \t ").as_str(), "MCP client");
}

fn sqlite_unix_seconds(stamp: &str) -> i64 {
    rusqlite::Connection::open_in_memory()
        .unwrap()
        .query_row(
            "SELECT CAST(strftime('%s', ?1) AS INTEGER)",
            [stamp],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn queue_transitions_without_a_domain_write() {
    let (_directory, owner) = opened(Limits::default());
    let queue = Queue::on(&owner);
    queue.stage("a", &[]).unwrap();
    let (claimed, token) = queue.claim("a").unwrap().into_parts();
    assert_eq!(claimed.state, State::Applying);
    assert!(matches!(queue.claim("a"), Err(ActionError::InFlight)));
    assert!(matches!(queue.dismiss("a"), Err(ActionError::InFlight)));
    assert_eq!(queue.states(), [("a".into(), State::Applying)]);
    assert!(queue.settle(&token, End::Applied));
    assert!(!queue.settle(&token, End::Released));
    assert!(matches!(queue.claim("a"), Err(ActionError::AlreadyApplied { at }) if !at.is_empty()));
    assert!(matches!(
        queue.dismiss("a"),
        Err(ActionError::AlreadyApplied { .. })
    ));
    assert!(queue.list().actions.is_empty());
    assert!(matches!(queue.claim("missing"), Err(ActionError::NotFound)));
}

#[test]
fn a_released_action_is_pending_again_in_its_place() {
    let (_directory, _owner, queue) = staged(&[("a", &[]), ("b", &[])]);
    let claim = queue.claim("a").unwrap();
    assert!(queue.settle(claim.token(), End::Released));
    let pending = [("a".into(), State::Pending), ("b".into(), State::Pending)];
    assert_eq!(queue.states(), pending);
    assert_eq!(queue.claim("a").unwrap().action().state, State::Applying);
}

#[test]
fn stale_settle_from_earlier_claim_does_not_affect_later_claim() {
    let (_directory, _owner, queue) = staged(&[("a", &[])]);
    let first = queue.claim("a").unwrap();
    assert!(queue.settle(first.token(), End::Released));
    let second = queue.claim("a").unwrap();
    assert_eq!(second.action().state, State::Applying);
    for end in [End::Released, End::Applied, End::Unknown] {
        assert!(!queue.settle(first.token(), end));
        assert_eq!(queue.states(), [("a".into(), State::Applying)]);
        assert!(matches!(queue.claim("a"), Err(ActionError::InFlight)));
    }
    assert!(queue.settle(second.token(), End::Applied));
    assert!(!queue.settle(second.token(), End::Released));
}

#[test]
fn an_expired_action_is_listed_as_expired_and_claim_refuses_it() {
    let (directory, owner) = opened(Limits::default());
    let queue = Queue::on(&owner);
    let a = queue.stage("a", &[]).unwrap();
    queue.stage("b", &["a"]).unwrap();
    expire(directory.path(), "a");
    let expired = [("a".into(), State::Expired), ("b".into(), State::Pending)];
    assert_eq!(queue.states(), expired);
    let Err(ActionError::Expired { expires_at }) = queue.claim("a") else {
        panic!("an expired action was claimed")
    };
    assert_eq!(expires_at, a.created_at);
    let Err(ActionError::Blocked(first)) = queue.claim("b") else {
        panic!("b was claimed past its expired dependency")
    };
    assert_eq!((first.id.as_str(), first.state), ("a", State::Expired));
    assert!(matches!(
        queue.stage("c", &["a"]),
        Err(ActionError::Dependency { id, state: Some(State::Expired) }) if id == "a"
    ));
    queue.dismiss("a").unwrap();
    assert_eq!(queue.states(), [("b".into(), State::Pending)]);
}

#[test]
fn claim_names_the_root_dependency_that_must_go_first() {
    let (_directory, _owner, queue) = staged(&[("a", &[]), ("b", &["a"]), ("c", &["b"])]);
    let Err(ActionError::Blocked(first)) = queue.claim("c") else {
        panic!("c was claimed before a")
    };
    assert_eq!(
        first,
        Blocker {
            id: "a".into(),
            label: "create build a".into(),
            state: State::Pending
        }
    );
    let first_claim = queue.claim("a").unwrap();
    let Err(ActionError::Blocked(first)) = queue.claim("b") else {
        panic!("b was claimed while a applies")
    };
    assert_eq!((first.id.as_str(), first.state), ("a", State::Applying));
    assert!(queue.settle(first_claim.token(), End::Applied));
    let second_claim = queue.claim("b").unwrap();
    assert!(queue.settle(second_claim.token(), End::Applied));
    assert_eq!(queue.claim("c").unwrap().action().state, State::Applying);
}

#[test]
fn dismiss_accepts_an_unknown_action_once_and_it_can_no_longer_be_a_dependency() {
    let (_directory, owner) = opened(Limits::default());
    let queue = Queue::on(&owner);
    queue.stage("a", &[]).unwrap();
    let claim = queue.claim("a").unwrap();
    assert!(queue.settle(claim.token(), End::Unknown));
    assert_eq!(queue.states(), [("a".into(), State::Unknown)]);
    assert!(matches!(queue.claim("a"), Err(ActionError::OutcomeUnknown)));
    queue.dismiss("a").unwrap();
    assert!(matches!(
        queue.dismiss("a"),
        Err(ActionError::AlreadyDismissed { .. })
    ));
    assert!(matches!(
        queue.claim("a"),
        Err(ActionError::AlreadyDismissed { .. })
    ));
    assert!(matches!(
        queue.dismiss("missing"),
        Err(ActionError::NotFound)
    ));
    let refused = |depends_on: &[&str]| match queue.stage("b", depends_on) {
        Err(ActionError::Dependency { id, state }) => (id, state),
        other => panic!("{other:?}"),
    };
    assert_eq!(refused(&["b"]), ("b".into(), None));
    assert_eq!(refused(&["a"]), ("a".into(), Some(State::Dismissed)));
}

#[test]
fn an_apply_interrupted_by_a_crash_reads_as_unknown_after_reopen() {
    let (directory, owner, queue) = staged(&[("a", &[]), ("b", &[])]);
    let claim = queue.claim("a").unwrap();
    let session = queue.session.clone();
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(directory.path(), Limits::default()).unwrap();
    let queue = Queue::with(&owner, session);
    let after = [("a".into(), State::Unknown), ("b".into(), State::Pending)];
    assert_eq!(queue.states(), after);
    assert!(matches!(queue.claim("a"), Err(ActionError::OutcomeUnknown)));
    assert!(!queue.settle(claim.token(), End::Applied));
}

fn restore_fixture() -> (
    tempfile::TempDir,
    WriterOwner,
    crate::restore::InstallationClient,
    Queue,
) {
    let directory = tempfile::tempdir().unwrap();
    let gate = crate::ownership_marker::WriterGate::SwitchOn;
    let (owner, _) =
        WriterOwner::open_with_gate(directory.path(), Limits::default(), gate).unwrap();
    let installation = owner.installation().unwrap();
    let queue = Queue::on(&owner);
    (directory, owner, installation, queue)
}

fn export_backup(installation: &crate::restore::InstallationClient) -> Vec<u8> {
    std::fs::read(&installation.export_backup("test").unwrap().path).unwrap()
}

fn stage_backup(installation: &crate::restore::InstallationClient, archive: &[u8]) -> String {
    let mut upload = installation.begin_upload(1 << 30).unwrap();
    upload.write(archive).unwrap();
    installation.stage(upload).unwrap().restore_id
}

#[test]
fn restore_is_refused_while_an_action_applies() {
    let (_directory, _owner, installation, queue) = restore_fixture();
    queue.stage("a", &[]).unwrap();
    let archive = export_backup(&installation);
    let claim = queue.claim("a").unwrap();
    let restore_id = stage_backup(&installation, &archive);
    let activate = || installation.activate(&restore_id, CONFIRMATION);
    assert!(matches!(
        activate(),
        Err(RestoreError::Refused(Refusal::Busy))
    ));
    assert_eq!(queue.states(), [("a".into(), State::Applying)]);
    assert!(queue.settle(claim.token(), End::Applied));
    activate().unwrap();
    assert_eq!(queue.states(), [("a".into(), State::InvalidatedByRestore)]);
}

fn seed_restore_cases(queue: &Queue) -> Claim {
    for id in [
        "applied_after",
        "dismissed_after",
        "in_flight_at_backup",
        "unknown_at_backup",
        "applied_at_backup",
        "dismissed_at_backup",
    ] {
        queue.stage(id, &[]).unwrap();
    }
    let in_flight = queue.claim("in_flight_at_backup").unwrap();
    let unknown = queue.claim("unknown_at_backup").unwrap();
    assert!(queue.settle(unknown.token(), End::Unknown));
    let applied = queue.claim("applied_at_backup").unwrap();
    assert!(queue.settle(applied.token(), End::Applied));
    queue.dismiss("dismissed_at_backup").unwrap();
    in_flight
}

fn assert_restored_refusals(queue: &Queue, invalidated: [&str; 4], in_flight: &Claim) {
    for id in invalidated {
        assert!(matches!(
            queue.claim(id),
            Err(ActionError::InvalidatedByRestore { at }) if !at.is_empty()
        ));
    }
    assert!(!queue.settle(in_flight.token(), End::Applied));
    assert!(matches!(
        queue.claim("applied_at_backup"),
        Err(ActionError::AlreadyApplied { at }) if !at.is_empty()
    ));
    assert!(matches!(
        queue.claim("dismissed_at_backup"),
        Err(ActionError::AlreadyDismissed { at }) if !at.is_empty()
    ));
    assert!(matches!(
        queue.stage("dependent", &["applied_after"]),
        Err(ActionError::Dependency {
            id,
            state: Some(State::InvalidatedByRestore),
        }) if id == "applied_after"
    ));
}

#[test]
fn restore_invalidates_open_actions_in_backup_and_preserves_settled_states() {
    let (_directory, _owner, installation, queue) = restore_fixture();
    let in_flight = seed_restore_cases(&queue);
    let archive = export_backup(&installation);

    let applied_after = queue.claim("applied_after").unwrap();
    assert!(queue.settle(applied_after.token(), End::Applied));
    assert!(queue.settle(in_flight.token(), End::Applied));
    queue.dismiss("dismissed_after").unwrap();
    queue.stage("live_pending", &[]).unwrap();

    let restore_id = stage_backup(&installation, &archive);
    assert!(matches!(
        installation.activate(&restore_id, "wrong confirmation"),
        Err(RestoreError::Refused(Refusal::Confirmation))
    ));
    assert!(
        queue
            .states()
            .contains(&("live_pending".into(), State::Pending))
    );

    installation.activate(&restore_id, CONFIRMATION).unwrap();
    let invalidated = [
        "applied_after",
        "dismissed_after",
        "in_flight_at_backup",
        "unknown_at_backup",
    ];
    assert_eq!(
        queue.states(),
        invalidated.map(|id| (id.into(), State::InvalidatedByRestore))
    );
    assert_restored_refusals(&queue, invalidated, &in_flight);
    queue.dismiss("applied_after").unwrap();
    assert_eq!(
        queue.states(),
        [
            ("dismissed_after".into(), State::InvalidatedByRestore),
            ("in_flight_at_backup".into(), State::InvalidatedByRestore),
            ("unknown_at_backup".into(), State::InvalidatedByRestore),
        ]
    );
    let replacement = queue.stage("replacement", &[]).unwrap();
    assert_eq!(replacement.state, State::Pending);
}

fn version(database: &Path) -> String {
    rusqlite::Connection::open(database)
        .unwrap()
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn users(directory: &Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    let mut statement = connection
        .prepare("SELECT email FROM users ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn schema_51_keeps_a_schema_50_database_and_a_failed_step_leaves_it_there() {
    let (directory, owner) = opened(Limits::default());
    let session = first_session(&owner);
    owner.shutdown().unwrap();
    let path = directory.path();
    sql(
        path,
        "DROP TABLE plan_revision_spoolman_colors;
         DROP TABLE pending_actions;
         UPDATE app_settings SET value='50' WHERE tenant_id='default' AND key='schema_version';
         CREATE TRIGGER reject_schema51 BEFORE UPDATE OF value ON app_settings
         WHEN NEW.tenant_id = 'default' AND NEW.key = 'schema_version' AND NEW.value = '51'
         BEGIN SELECT RAISE(ABORT, 'schema51 stamp rejected'); END;",
    );
    let kept = users(path);
    assert_eq!(kept, ["owner@example.com"]);

    let Err(error) = WriterOwner::open(path, Limits::default()) else {
        panic!("step 51 committed past the stamp trigger");
    };
    assert!(
        format!("{error:#}").contains("schema51 stamp rejected"),
        "{error:#}"
    );
    let database = path.join("print-partner.db");
    assert_eq!(version(&database), "50");
    assert_eq!(users(path), kept);
    assert_eq!(version(&path.join("backups/pre-schema51.db")), "50");

    sql(path, "DROP TRIGGER reject_schema51");
    let (owner, ready) = WriterOwner::open(path, Limits::default()).unwrap();
    assert_eq!((ready.version, ready.previous_version), (52, 50));
    assert_eq!(version(&database), "52");
    assert_eq!(users(path), kept);
    let queue = Queue::with(&owner, session);
    queue.stage("a", &[]).unwrap();
    assert_eq!(queue.states(), [("a".into(), State::Pending)]);
}
