use super::*;
use rusqlite::Connection;

fn store() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(include_str!("schema.sql"))
        .unwrap();
    connection
}

type Row = (f64, i64, String, Option<f64>, Option<f64>);

fn row(tx: &Transaction<'_>, link: &str) -> Option<Row> {
    tx.query_row(
        "SELECT grams,units,basis,grams_per_mm,measured_mm FROM print_link_grams WHERE tenant_id='default' AND link_id=?1",
        [link],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )
    .optional()
    .unwrap()
}

#[test]
fn gre_1165_a_measured_length_replaces_the_estimate_and_estimates_never_replace_it() {
    let mut connection = store();
    let tx = connection.transaction().unwrap();
    let estimate =
        |link, grams, factor| record_estimate(&tx, "default", link, grams, 3, factor).unwrap();
    assert!(estimate("link", 40.5, None));
    assert!(estimate("link", 38.0, Some(0.003)));
    let estimated = (38.0, 3, "estimated".to_owned(), Some(0.003), None);
    assert_eq!(row(&tx, "link"), Some(estimated));
    let measured = record_measured(&tx, "default", "link", 12_000.0).unwrap();
    assert_eq!(measured, Measurement::Recorded);
    let recorded = (36.0, 3, "measured".to_owned(), Some(0.003), Some(12_000.0));
    assert_eq!(row(&tx, "link"), Some(recorded.clone()));
    assert!(!estimate("link", 12.0, Some(0.002)));
    assert_eq!(row(&tx, "link"), Some(recorded));
    assert_eq!(
        record_measured(&tx, "other-tenant", "link", 5.0).unwrap(),
        Measurement::NoEstimate
    );
}

#[test]
fn gre_1165_a_length_without_a_factor_keeps_the_estimate() {
    let mut connection = store();
    let tx = connection.transaction().unwrap();
    record_estimate(&tx, "default", "link", 9.5, 2, None).unwrap();
    let measured = record_measured(&tx, "default", "link", 3_100.0).unwrap();
    assert_eq!(measured, Measurement::NoFactor);
    let kept = (9.5, 2, "estimated".to_owned(), None, Some(3_100.0));
    assert_eq!(row(&tx, "link"), Some(kept.clone()));
    assert!(!record_estimate(&tx, "default", "link", 1.0, 2, None).unwrap());
    assert_eq!(row(&tx, "link"), Some(kept));
    let missing = record_measured(&tx, "default", "none", 3_100.0).unwrap();
    assert_eq!(missing, Measurement::NoEstimate);
    assert_eq!(row(&tx, "none"), None);
}

#[test]
fn gre_1165_values_must_be_positive_and_cover_a_unit() {
    let mut connection = store();
    let tx = connection.transaction().unwrap();
    for (link, grams, units, factor) in [
        ("link", 0.0, 1, None),
        ("link", -1.0, 1, None),
        ("link", f64::NAN, 1, None),
        ("link", f64::INFINITY, 1, None),
        ("link", 1.0, 0, None),
        ("", 1.0, 1, None),
        ("link", 1.0, 1, Some(0.0)),
        ("link", 1.0, 1, Some(f64::NAN)),
    ] {
        assert!(record_estimate(&tx, "default", link, grams, units, factor).is_err());
    }
    record_estimate(&tx, "default", "link", 1.0, 1, Some(0.002)).unwrap();
    for length in [0.0, -3.0, f64::NAN, f64::INFINITY] {
        assert!(record_measured(&tx, "default", "link", length).is_err());
    }
    let estimated = (1.0, 1, "estimated".to_owned(), Some(0.002), None);
    assert_eq!(row(&tx, "link"), Some(estimated));
}
