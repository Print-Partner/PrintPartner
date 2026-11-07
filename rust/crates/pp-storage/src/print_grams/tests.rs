use super::*;
use rusqlite::Connection;

fn store() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(include_str!("schema.sql"))
        .unwrap();
    connection
}

fn row(connection: &Connection, link: &str) -> (f64, i64, String) {
    connection
        .query_row(
            "SELECT grams,units,basis FROM print_link_grams WHERE tenant_id='default' AND link_id=?1",
            [link],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

#[test]
fn gre_1165_measured_grams_replace_an_estimate_and_never_the_reverse() {
    let mut connection = store();
    let tx = connection.transaction().unwrap();
    let record = |link, grams, units, basis| record(&tx, "default", link, grams, units, basis);
    assert!(record("link", 40.5, 3, Basis::Estimated).unwrap());
    assert!(record("link", 38.0, 3, Basis::Estimated).unwrap());
    assert!(record("link", 44.25, 3, Basis::Measured).unwrap());
    assert!(!record("link", 12.0, 3, Basis::Estimated).unwrap());
    assert!(record("link", 45.0, 3, Basis::Measured).unwrap());
    assert!(record("other", 7.0, 1, Basis::Measured).unwrap());
    tx.commit().unwrap();
    assert_eq!(row(&connection, "link"), (45.0, 3, "measured".into()));
    assert_eq!(row(&connection, "other"), (7.0, 1, "measured".into()));
}

#[test]
fn gre_1165_grams_must_be_positive_and_cover_a_unit() {
    let mut connection = store();
    let tx = connection.transaction().unwrap();
    for (link, grams, units) in [
        ("link", 0.0, 1),
        ("link", -1.0, 1),
        ("link", f64::NAN, 1),
        ("link", f64::INFINITY, 1),
        ("link", 1.0, 0),
        ("", 1.0, 1),
    ] {
        assert!(record(&tx, "default", link, grams, units, Basis::Estimated).is_err());
    }
    let count: i64 = tx
        .query_row("SELECT count(*) FROM print_link_grams", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}
