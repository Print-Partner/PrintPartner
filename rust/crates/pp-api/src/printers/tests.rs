use super::*;
use crate::integrations::tests::refusal;
use pp_storage::printer_setup::{
    DocumentFault, Field, InvalidDocument, InvalidField, LoadedFilament, PrinterSetupError, Reason,
    SetupDocument,
};
use serde_json::{Value, json};

#[tokio::test]
async fn printer_errors_use_the_printer_codes_and_name_each_field() {
    let fault = InvalidDocument {
        document: SetupDocument::Printers,
        entry: Some(0),
        reason: DocumentFault::WrongShape,
    };
    let unreadable = PrinterSetupError::StoredDocumentInvalid(fault);
    let cases = [
        (PrinterSetupError::NotFound, 404, code::PRINTER_NOT_FOUND),
        (PrinterSetupError::LimitReached, 409, code::PRINTER_LIMIT),
        (
            PrinterSetupError::SlotHoldsFilament,
            409,
            code::FILAMENT_SLOT_IN_USE,
        ),
        (unreadable, 500, code::PRINTERS_UNREADABLE),
    ];
    for (error, status, code) in cases {
        let (answered, body) = refusal(error, &WORDING).await;
        assert_eq!((answered, &body["code"]), (status, &json!(code)));
    }
    let out_of_range = |field| InvalidField {
        field,
        reason: Reason::OutOfRange,
    };
    let not_a_host = InvalidField {
        field: Field::IntegrationId,
        reason: Reason::NotAPrinterHost,
    };
    let invalid = vec![
        out_of_range(Field::BedWidth),
        out_of_range(Field::Margin),
        out_of_range(Field::FilamentSlots),
        not_a_host,
    ];
    let (status, body) = refusal(PrinterSetupError::Invalid(invalid), &WORDING).await;
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

/// The 200 body carries the configuration and none of the fields the routes
/// do not set.
#[test]
fn a_printer_answers_as_its_configuration() {
    let filament = LoadedFilament {
        slot: 1,
        filament_color_id: Some("pla-black".into()),
        label: "PLA".into(),
    };
    let stored = Printer {
        id: "p-1".into(),
        name: "Voron".into(),
        model: "voron-2.4-350".into(),
        bed_width_mm: 350,
        bed_depth_mm: 300,
        bed_height_mm: None,
        margin_mm: 4,
        max_filament_slots: 2,
        loaded_filaments: vec![filament],
        integration_id: Some("moon-1".into()),
        device_id: Some("ha-device".into()),
        preset_id: None,
        preferred_slicer: Some("orca".into()),
    };
    assert_eq!(
        serde_json::to_value(PrinterConfig::from(stored)).unwrap(),
        json!({"id": "p-1", "name": "Voron", "model": "voron-2.4-350", "bed_width_mm": 350,
            "bed_depth_mm": 300, "bed_height_mm": null, "margin_mm": 4,
            "max_filament_slots": 2, "integration_id": "moon-1"})
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
