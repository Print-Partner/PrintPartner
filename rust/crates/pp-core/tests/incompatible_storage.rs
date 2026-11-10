//! The Node-backed start refuses a schema Node cannot open before the Node
//! child starts, so the supervisor never restarts a child that would refuse
//! the same database again.

#[path = "support/release.rs"]
mod release;

use pp_core::{CoreRuntime, IncompatibleSchema};
use pp_testkit::{Fixture, Installation, node_schema};

#[tokio::test]
async fn node_backed_start_refuses_unsupported_schema_before_the_child() {
    // The last case also fails `PRAGMA integrity_check`; the version still
    // decides, so Node never opens it.
    for (found, failing_integrity) in [(30, false), (999, false), (999, true)] {
        let installation = Installation::create(Fixture::NodeSchema34).unwrap();
        let database = installation.database_path();
        node_schema::restamp_schema_version(&database, found).unwrap();
        if failing_integrity {
            node_schema::break_check_constraint(&database).unwrap();
        }
        let before = node_schema::database_files(installation.path());
        let error = CoreRuntime::start(release::launch(installation.path().to_path_buf()))
            .await
            .err()
            .unwrap();
        // Only the pre-child check returns this type; a Node child that refused
        // the database would surface as a readiness failure after restarts.
        assert_eq!(
            error.downcast_ref::<IncompatibleSchema>(),
            Some(&IncompatibleSchema {
                found,
                oldest: 31,
                newest: 34
            }),
            "{error:#}"
        );
        println!("schema {found} (integrity failing: {failing_integrity}): {error}");
        assert_eq!(
            node_schema::database_files(installation.path()),
            before,
            "schema {found}"
        );
    }
}
