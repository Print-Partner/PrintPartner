//! The Node-backed start refuses a schema Node cannot open before the Node
//! child starts, so the supervisor never restarts a child that would refuse
//! the same database again.

#[path = "support/release.rs"]
mod release;

use pp_core::{CoreRuntime, IncompatibleSchema};
use pp_testkit::{Fixture, Installation};

#[tokio::test]
async fn node_backed_start_refuses_unsupported_schema_before_the_child() {
    for found in [30, 999] {
        let installation = Installation::create(Fixture::NodeSchema34).unwrap();
        pp_testkit::node_schema::restamp_schema_version(&installation.database_path(), found)
            .unwrap();
        let database = std::fs::read(installation.database_path()).unwrap();
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
        println!("schema {found}: {error}");
        assert_eq!(
            std::fs::read(installation.database_path()).unwrap(),
            database
        );
        assert!(
            !installation.path().join("print-partner.db-wal").exists(),
            "nothing opened the database for writing"
        );
    }
}
