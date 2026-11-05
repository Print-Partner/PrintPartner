use super::*;

#[test]
fn farm_log_cursor_round_trips_and_rejects_foreign_values() {
    let key = (1_730_000_000_000_000_000_i128, "send:a\nb".to_owned());
    assert_eq!(decode(&encode(&key)), Some(key));
    for raw in [
        "",
        "zz",
        &hex::encode("v2\n1\nx"),
        &hex::encode("v1\nnope\nx"),
        &hex::encode("v1\n1"),
    ] {
        assert_eq!(decode(raw), None, "{raw}");
    }
}

#[test]
fn farm_log_unparsable_times_sort_oldest() {
    assert_eq!(sort_time("yesterday"), i128::MIN);
    assert!(sort_time("2026-11-05T00:00:00.000Z") > sort_time("2026-11-04T23:59:59.999Z"));
}

#[test]
fn farm_log_stored_entries_need_a_seq_kind_and_time() {
    let entry = json!({"seq": 3, "kind": UNDO, "at": "2026-11-05T00:00:00.000Z",
        "undo_token": "ppb_x", "undoes": "checkoff_burst:1", "build_ids": [2, 1], "printer_ids": ["p"]});
    let logged = logged(&entry).unwrap();
    assert_eq!(logged.id, "checkoff_burst_undo:3");
    assert_eq!(logged.undo.reason, Some(FarmUndoReason::UndoEntry));
    for missing in ["seq", "kind", "at"] {
        let mut broken = entry.clone();
        broken.as_object_mut().unwrap().remove(missing);
        assert_eq!(super::logged(&broken), None, "{missing}");
    }
    assert_eq!(
        super::logged(&json!({"seq": 1, "kind": "reflow", "at": "x"})),
        None
    );
}

#[test]
fn farm_log_expired_receipt_detail_is_the_undo_routes_404_text() {
    let undo = FarmLogUndo::from(burst::Blocked::NotFound);
    assert_eq!(undo.reason, Some(FarmUndoReason::ReceiptExpired));
    assert_eq!(undo.detail.as_deref(), Some("Checkoff burst not found"));
}
