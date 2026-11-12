use super::*;
use axum::body::to_bytes;
use pp_storage::printer_setup::{Field, InvalidField, Reason};
use serde_json::{Value, json};

async fn answered(error: PrinterSetupError) -> (u16, Value) {
    let response = refused(&error, &WORDING);
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn printer_errors_use_the_printer_codes_and_name_each_field() {
    let cases = [
        (PrinterSetupError::NotFound, 404, code::PRINTER_NOT_FOUND),
        (PrinterSetupError::LimitReached, 409, code::PRINTER_LIMIT),
        (
            PrinterSetupError::SlotHoldsFilament,
            409,
            code::FILAMENT_SLOT_IN_USE,
        ),
    ];
    for (error, status, code) in cases {
        let (answered_status, body) = answered(error).await;
        assert_eq!((answered_status, &body["code"]), (status, &json!(code)));
    }
    let out_of_range = |field| InvalidField {
        field,
        reason: Reason::OutOfRange,
    };
    let (status, body) = answered(PrinterSetupError::Invalid(vec![
        out_of_range(Field::BedWidth),
        out_of_range(Field::Margin),
        out_of_range(Field::FilamentSlots),
        InvalidField {
            field: Field::IntegrationId,
            reason: Reason::NotAPrinterHost,
        },
    ]))
    .await;
    assert_eq!(
        (status, &body["code"]),
        (400, &json!(code::INVALID_PRINTER))
    );
    assert_eq!(
        body["errors"],
        json!([
            {"pointer": "/bed_width_mm", "detail": "must be between 1 and 10000 mm"},
            {"pointer": "/margin_mm", "detail": "must be less than half the bed width and depth"},
            {"pointer": "/max_filament_slots", "detail": "must be between 1 and 4"},
            {"pointer": "/integration_id",
             "detail": "must name a stored Moonraker, PrusaLink or Bambu integration"},
        ])
    );
}

/// An edit tells an absent `integration_id` (keep) from a null one (unlink).
#[test]
fn an_edit_reads_absent_and_null_integration_id_apart() {
    let read = |body: Value| serde_json::from_value::<UpdatePrinterRequest>(body).unwrap();
    assert_eq!(read(json!({"name": "Voron"})).integration_id, None);
    assert_eq!(
        read(json!({"integration_id": null})).integration_id,
        Some(None)
    );
    let linked = read(json!({"integration_id": "moon-1"})).integration_id;
    assert_eq!(linked, Some(Some("moon-1".into())));
    // A negative or fractional size is refused by the body's type.
    for body in [json!({"bed_width_mm": -1}), json!({"bed_width_mm": 350.5})] {
        let errors = problem::parse_body::<UpdatePrinterRequest>(body.to_string().as_bytes());
        let errors = errors.err().expect("the body parsed");
        assert_eq!(errors[0].pointer, "/bed_width_mm");
    }
}
