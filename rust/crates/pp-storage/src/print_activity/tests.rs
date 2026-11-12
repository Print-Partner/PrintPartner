use super::*;
use crate::{
    Limits,
    auth::{FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    write::test_support::{WAIT, first_session, opened},
};

const POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::FirstAccountOnly,
    first_user: FirstUserTenant::NewUser,
    session_tenant: SessionTenantPolicy::SingleAccountDefault,
};
static IDLE: AtomicBool = AtomicBool::new(false);

/// One Build and its jobs: `(tenant, printer_id, status, grams, at)`.
fn seed(directory: &std::path::Path, jobs: &[(&str, &str, &str, Option<i64>, &str)]) {
    let database = directory.join(crate::archive::DATABASE_FILE);
    let connection = rusqlite::Connection::open(database).unwrap();
    connection.busy_timeout(WAIT).unwrap();
    connection
        .execute(
            "INSERT INTO build_profiles(id,tenant_id,name) VALUES(1,'default','B1')",
            [],
        )
        .unwrap();
    for (index, (tenant, printer, status, grams, at)) in jobs.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO print_jobs(id,tenant_id,profile_id,printer_id,status,filament_consumed_g,at) \
                 VALUES(?1,?2,1,?3,?4,?5,?6)",
                params![format!("job-{index}"), tenant, printer, status, grams, at],
            )
            .unwrap();
    }
}

#[test]
fn counts_each_printers_jobs_in_the_window() {
    let (directory, owner) = opened(Limits::default());
    let token = first_session(&owner);
    seed(
        directory.path(),
        &[
            (
                "default",
                "p-a",
                "completed",
                Some(120),
                "2026-11-12T08:00:00.000Z",
            ),
            ("default", "p-a", "failed", None, "2026-11-12T09:00:00.000Z"),
            (
                "default",
                "p-a",
                "completed",
                Some(50),
                "2026-11-11T08:00:00.000Z",
            ),
            ("default", " ", "sent", None, "2026-11-12T10:00:00.000Z"),
            (
                "foreign",
                "p-a",
                "completed",
                Some(999),
                "2026-11-12T10:00:00.000Z",
            ),
        ],
    );
    let client = owner.print_activity(POLICY).unwrap();
    let session = || Credential::Session(Secret::new(token.clone()));
    let counted = client
        .since(session(), "2026-11-12T00:00:00.000Z", &IDLE, WAIT)
        .unwrap();
    assert_eq!(
        counted,
        vec![
            PrinterActivity {
                printer_id: None,
                sent: 1,
                completed: 0,
                failed: 0,
                filament_consumed_g: 0,
            },
            PrinterActivity {
                printer_id: Some("p-a".into()),
                sent: 2,
                completed: 1,
                failed: 1,
                filament_consumed_g: 120,
            },
        ]
    );
    let wider = client
        .since(session(), "2026-11-11T00:00:00.000Z", &IDLE, WAIT)
        .unwrap();
    assert_eq!(wider[1].sent, 3);
    let empty = client
        .since(session(), "2026-11-13T00:00:00.000Z", &IDLE, WAIT)
        .unwrap();
    assert!(empty.is_empty());
}

#[test]
fn a_refused_credential_reads_nothing() {
    let (_directory, owner) = opened(Limits::default());
    let client = owner.print_activity(POLICY).unwrap();
    let stranger = Credential::Session(Secret::new("x".repeat(64)));
    let refused = client.since(stranger, "2026-11-12T00:00:00.000Z", &IDLE, WAIT);
    assert!(
        matches!(
            refused,
            Err(PrintActivityError::Reader(WriterFailure::Auth(_)))
        ),
        "{refused:?}"
    );
}
