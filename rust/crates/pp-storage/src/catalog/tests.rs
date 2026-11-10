use super::*;
use crate::{Queue, ReaderPool, ReaderState, Shared};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
};

#[test]
fn ticket_t_59_catalog_private_full_queue_retains_unreleased_work() {
    let (reply, _) = mpsc::channel();
    let shared = Arc::new(Shared {
        queue: Mutex::new(Queue {
            #[cfg(feature = "desktop_rebuild")]
            auth_ready: VecDeque::new(),
            #[cfg(feature = "desktop_rebuild")]
            next: crate::QueueClass::Normal,
            closed: false,
            pending: VecDeque::from([Envelope::Catalog {
                command: Command::End(999),
                reply,
            }]),
        }),
        changed: Condvar::new(),
        capacity: 1,
        job_admission: Mutex::new(None),
        #[cfg(feature = "desktop_rebuild")]
        orphaned_source_leases: Mutex::new(Vec::new()),
        #[cfg(feature = "desktop_rebuild")]
        job_subscriptions: crate::jobs::JobSubscriptions::new(),
        #[cfg(feature = "desktop_rebuild")]
        import_epoch: std::sync::atomic::AtomicU64::new(0),
        #[cfg(feature = "desktop_rebuild")]
        import_quota: Mutex::new(None),
        #[cfg(feature = "desktop_rebuild")]
        domain_events: crate::domain_events::Feed::new(0),
        #[cfg(feature = "desktop_rebuild")]
        print_file_snapshots: Arc::default(),
    });
    let readers = Arc::new(ReaderPool {
        state: Mutex::new(ReaderState {
            closed: false,
            idle: vec![],
            active: 0,
        }),
        changed: Condvar::new(),
    });
    let client = SettingsClient {
        shared: shared.clone(),
        readers,
    };
    let mut state = State {
        issued_high_water: 16,
        ..Default::default()
    };
    state.activate(17, "default".into(), 1);
    let proof = state.proof(17).unwrap();
    let lease = SourceWorkLease {
        client: client.clone(),
        proof: Some(proof.clone()),
    };
    assert!(
        enqueue(
            &client,
            Command::End(17),
            &AtomicBool::new(false),
            Duration::ZERO
        )
        .is_err()
    );
    drop(lease);
    #[cfg(feature = "desktop_rebuild")]
    {
        let orphaned = std::mem::take(&mut *shared.orphaned_source_leases.lock().unwrap());
        assert_eq!(orphaned, [17]);
        state.reap(orphaned);
        assert!(!state.source_busy("default", 1));
    }
    let mut queue = shared.queue.lock().unwrap();
    assert_eq!(queue.pending.len(), 1);
    let Envelope::Catalog {
        command: Command::End(token),
        ..
    } = queue.pending.pop_front().unwrap()
    else {
        panic!("existing queued work must survive")
    };
    assert_eq!(token, 999);
    drop(queue);
    let lease = SourceWorkLease {
        client,
        proof: Some(proof),
    };
    drop(lease);
    let Envelope::Catalog {
        command: Command::End(token),
        ..
    } = shared.queue.lock().unwrap().pending.pop_front().unwrap()
    else {
        panic!("release must be serialized")
    };
    assert_eq!(token, 17);
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn issued_source_tokens_cover_full_u64_bounds_and_retire_idempotently() {
    let mut state = State {
        issued_high_water: u64::MAX - 1,
        ..Default::default()
    };
    assert_eq!(state.next_token().unwrap(), u64::MAX);
    state.activate(u64::MAX, "tenant".into(), 9);
    assert!(state.next_token().is_err());
    let proof = state.proof(u64::MAX).unwrap();
    let binding = SourceBinding {
        tenant: "tenant".into(),
        id: 9,
    };
    assert!(matches!(
        state.retire_issued(&proof, &binding).unwrap(),
        Retirement::RetiredNow
    ));
    assert!(matches!(
        state.retire_issued(&proof, &binding).unwrap(),
        Retirement::AlreadyRetired
    ));
    assert!(matches!(
        state.classify_issued(&proof.incarnation, 0),
        IssuedTokenState::NeverIssued
    ));
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn source_token_incarnation_prevents_restart_aliasing() {
    let mut first = State::default();
    let token = first.next_token().unwrap();
    first.activate(token, "tenant".into(), 9);
    let proof = first.proof(token).unwrap();
    let mut restarted = State::default();
    let reused = restarted.next_token().unwrap();
    restarted.activate(reused, "tenant".into(), 9);
    assert_eq!(token, reused);
    assert!(matches!(
        restarted.classify_issued(&proof.incarnation, proof.token),
        IssuedTokenState::WrongIncarnation
    ));
}

/// A live Source lease whose release could not be queued.
#[cfg(feature = "desktop_rebuild")]
struct OrphanedLease {
    root: PathBuf,
    owner: WriterOwner,
    catalog: SourceCatalogClient,
    id: i64,
    shared: Arc<Shared>,
}
#[cfg(feature = "desktop_rebuild")]
impl OrphanedLease {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let (owner, _) = WriterOwner::open(&root, crate::Limits::default()).unwrap();
        let catalog = owner.local_source_catalog();
        let Outcome::Source(Some(source)) = catalog
            .execute(Request::Create {
                source: CreateSource {
                    name: name.into(),
                    source_kind: Some("local".into()),
                    ..Default::default()
                },
            })
            .unwrap()
        else {
            panic!("Source expected")
        };
        let mut lease = catalog
            .begin_work(source.id, &AtomicBool::new(false), Duration::from_secs(2))
            .unwrap();
        let busy =
            match catalog.begin_work(source.id, &AtomicBool::new(false), Duration::from_secs(2)) {
                Ok(_) => panic!("live Source lease must exclude overlap"),
                Err(error) => error,
            };
        assert!(busy.downcast_ref::<SourceBusy>().is_some());
        let token = lease.proof.take().unwrap().token;
        let shared = lease.client.shared.clone();
        drop(lease);
        shared.orphaned_source_leases.lock().unwrap().push(token);
        Self {
            root,
            owner,
            catalog,
            id: source.id,
            shared,
        }
    }

    fn assert_reaped_and_free(self) {
        assert!(
            self.shared
                .orphaned_source_leases
                .lock()
                .unwrap()
                .is_empty()
        );
        let mut acquired = self
            .catalog
            .begin_work(self.id, &AtomicBool::new(false), Duration::from_secs(2))
            .unwrap();
        acquired.release().unwrap();
        self.owner.shutdown().unwrap();
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn build_graph_envelope_reaps_orphaned_source_work_before_dispatch() {
    let orphaned = OrphanedLease::new("build-source-orphan");
    let builds = orphaned
        .owner
        .build_graph_with_policy(auth::AuthPolicy {
            registration: auth::RegistrationPolicy::Open,
            first_user: auth::FirstUserTenant::NewUser,
            session_tenant: auth::SessionTenantPolicy::AccountTenant,
        })
        .unwrap();
    let error = builds
        .execute(
            crate::read_model::Credential::Session(auth::Secret::new("missing-session".into())),
            crate::build_graph::BuildCommand::List,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
        )
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<auth::AuthFailure>(),
        Some(auth::AuthFailure::SessionRequired)
    ));
    orphaned.assert_reaped_and_free();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn native_secrets_envelope_reaps_orphaned_source_work_before_dispatch() {
    let orphaned = OrphanedLease::new("native-secrets-source-orphan");
    let preparation = orphaned
        .owner
        .native_secret_migration()
        .prepare(&AtomicBool::new(false), Duration::from_secs(2))
        .unwrap();
    assert!(matches!(
        preparation,
        crate::native_secrets::MigrationPreparation::Absent
    ));
    orphaned.assert_reaped_and_free();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn source_work_release_acknowledges_idempotent_retirement_and_preserves_stopped_proof() {
    let root =
        std::env::temp_dir().join(format!("pp-source-release-ack-{}", rand::random::<u64>()));
    let (owner, _) = WriterOwner::open(&root, crate::Limits::default()).unwrap();
    let catalog = owner.local_source_catalog();
    let Outcome::Source(Some(source)) = catalog
        .execute(Request::Create {
            source: CreateSource {
                name: "Release acknowledgement".into(),
                source_kind: Some("local".into()),
                ..Default::default()
            },
        })
        .unwrap()
    else {
        panic!("Source expected")
    };
    let mut first = catalog
        .begin_work(source.id, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap();
    let mut repeated = SourceWorkLease {
        client: first.client.clone(),
        proof: first.proof.clone(),
    };
    first.release().unwrap();
    assert!(first.proof.is_none());
    repeated.release().unwrap();
    assert!(repeated.proof.is_none());
    let mut stopped = catalog
        .begin_work(source.id, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap();
    owner.shutdown().unwrap();
    assert!(stopped.release().is_err());
    assert!(stopped.proof.is_some());
    assert!(stopped.release().is_err());
    assert!(stopped.proof.is_some());
    stopped.proof = None;
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn unknown_source_work_lease_is_rejected_without_database_work() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut state = State::default();
    let error = match execute(
        &mut connection,
        &mut state,
        Command::End(17),
        crate::writer_admission::Deadline::at_dequeue(),
    ) {
        Err(error) => error,
        Ok(_) => panic!("unknown Source lease released"),
    };
    assert_eq!(error.root_cause().to_string(), "Unknown work lease");
    connection.execute_batch("ROLLBACK").unwrap();
}
