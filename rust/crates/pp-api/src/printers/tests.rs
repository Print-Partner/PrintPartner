use super::*;
use axum::body::to_bytes;
use pp_storage::printer_setup::{
    DocumentFault, InvalidDocument, LoadedFilament, PrinterSetupError, SetupDocument,
};
use serde_json::{Value, json};

#[tokio::test]
async fn a_refused_printer_read_answers_with_the_printer_codes() {
    let fault = InvalidDocument {
        document: SetupDocument::Printers,
        entry: Some(0),
        reason: DocumentFault::WrongShape,
    };
    let cases = [
        (PrinterSetupError::NotFound, 404, code::PRINTER_NOT_FOUND),
        (
            PrinterSetupError::StoredDocumentInvalid(fault),
            500,
            code::PRINTERS_UNREADABLE,
        ),
    ];
    for (error, status, code) in cases {
        let response = refused(&error, &WORDING);
        let answered = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!((answered, &body["code"]), (status, &json!(code)));
    }
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
