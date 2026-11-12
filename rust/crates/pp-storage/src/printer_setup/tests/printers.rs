use super::*;

const FLEET: &str = "printer.fleet";

impl Fixture {
    fn read(&self, id: &str) -> Result<Printer, PrinterSetupError> {
        self.client.read_printer(self.session(), id, &IDLE, WAIT)
    }
}

#[test]
fn a_stored_printer_is_read_with_every_field_it_keeps() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.read("p-1"),
        Err(PrinterSetupError::NotFound)
    ));
    // The second row is as an import could leave it: no height, and the
    // fields no operation sets.
    let stored = json!([
        {"id": "p-1", "name": "Voron", "model": "voron-2.4-350", "bed_width_mm": 350,
         "bed_depth_mm": 350, "bed_height_mm": 340, "margin_mm": 4, "max_filament_slots": 1,
         "loaded_filaments": [], "integration_id": "moon-1"},
        {"id": "p-2", "name": "Old", "model": "mk3", "bed_width_mm": 250, "bed_depth_mm": 210,
         "bed_height_mm": null, "margin_mm": 4, "max_filament_slots": 4,
         "loaded_filaments": [{"slot": 3, "filament_color_id": "pla-black", "label": "PLA"}],
         "device_id": "ha-device", "preset_id": "prusa-mk3", "preferred_slicer": "prusa"}
    ]);
    seed_setting(&fixture.owner, FLEET, &stored.to_string());
    let voron = fixture.read("p-1").unwrap();
    let link = (
        voron.name.as_str(),
        voron.bed_height_mm,
        voron.integration_id.as_deref(),
    );
    assert_eq!(link, ("Voron", Some(340), Some("moon-1")));
    let old = fixture.read("p-2").unwrap();
    assert_eq!(
        (old.bed_height_mm, old.integration_id.as_deref()),
        (None, None)
    );
    // The typed rows write back what was stored.
    assert_eq!(serde_json::to_value([voron, old]).unwrap(), stored);
    let missing = fixture.read("p-3");
    assert!(
        matches!(missing, Err(PrinterSetupError::NotFound)),
        "{missing:?}"
    );

    let stranger = Credential::Session(Secret::new("not-a-session".into()));
    let refused = fixture.client.read_printer(stranger, "p-1", &IDLE, WAIT);
    let refused = format!("{refused:?}");
    assert_eq!(refused, "Err(Writer(Auth(SessionRequired)))");
}

#[test]
fn a_fleet_document_that_is_not_the_typed_shape_refuses_the_read_untouched() {
    use DocumentFault::{DuplicateId, NotAList, WrongShape};
    let fixture = Fixture::new();
    let row = json!({"id": "p-1", "name": "Voron", "model": "v", "bed_width_mm": 350,
        "bed_depth_mm": 350, "bed_height_mm": 340, "margin_mm": 4, "max_filament_slots": 1,
        "loaded_filaments": []});
    let with = |key: &str, value: Value| {
        let mut changed = row.clone();
        changed["id"] = json!("p-2");
        changed[key] = value;
        changed
    };
    let cases = [
        (json!({"id": "p-1"}), None, NotAList),
        (
            json!([row, with("nozzle", json!(0.4))]),
            Some(1),
            WrongShape,
        ),
        (
            json!([row, with("bed_width_mm", json!(350.5))]),
            Some(1),
            WrongShape,
        ),
        (
            json!([row, {"id": "p-2", "name": "No bed"}]),
            Some(1),
            WrongShape,
        ),
        (json!([row, with("id", json!(""))]), Some(1), WrongShape),
        (json!([row, row]), Some(1), DuplicateId),
    ];
    for (document, entry, reason) in cases {
        let text = document.to_string();
        seed_setting(&fixture.owner, FLEET, &text);
        let fault = InvalidDocument {
            document: SetupDocument::Printers,
            entry,
            reason,
        };
        let Err(PrinterSetupError::StoredDocumentInvalid(found)) = fixture.read("p-1") else {
            panic!("{text} was read")
        };
        assert_eq!(found, fault, "{text}");
        assert_eq!(fixture.stored_at(FLEET).as_deref(), Some(text.as_str()));
    }
}
