use super::*;

const FLEET: &str = "printer.fleet";

fn voron(integration_id: Option<&str>) -> NewPrinter {
    NewPrinter {
        name: " Voron 2.4 ".into(),
        model: "voron-2.4-350".into(),
        bed_width_mm: 350,
        bed_depth_mm: 350,
        bed_height_mm: 340,
        margin_mm: 4,
        max_filament_slots: 2,
        integration_id: integration_id.map(str::to_owned),
    }
}

impl Fixture {
    fn add(&self, new: NewPrinter) -> Result<Printer, PrinterSetupError> {
        self.client.create_printer(self.session(), new, &IDLE, WAIT)
    }

    fn read(&self, id: &str) -> Result<Printer, PrinterSetupError> {
        self.client.read_printer(self.session(), id, &IDLE, WAIT)
    }

    fn edit(&self, id: &str, patch: PrinterPatch) -> Result<Printer, PrinterSetupError> {
        let id = id.to_owned();
        self.client
            .update_printer(self.session(), id, patch, &IDLE, WAIT)
    }

    fn remove(&self, id: &str) -> Result<Printer, PrinterSetupError> {
        self.client
            .delete_printer(self.session(), id.into(), &IDLE, WAIT)
    }
}

#[test]
fn a_printer_is_created_read_edited_and_deleted_and_its_integration_stays() {
    let fixture = Fixture::new();
    let moon = fixture
        .create(new(ServiceKind::Moonraker, "http://moon.test"))
        .unwrap();
    let printer = fixture.add(voron(Some(&moon.id))).unwrap();
    assert_eq!((printer.id.len(), printer.name.as_str()), (32, "Voron 2.4"));
    assert_eq!(fixture.read(&printer.id).unwrap(), printer);

    // The stored row has the field names the readers take, and the host
    // lookup resolves the link.
    let rows: Value = serde_json::from_str(&fixture.stored_at(FLEET).unwrap()).unwrap();
    assert_eq!(
        rows,
        json!([{"id": printer.id, "name": "Voron 2.4", "model": "voron-2.4-350",
            "bed_width_mm": 350, "bed_depth_mm": 350, "bed_height_mm": 340,
            "margin_mm": 4, "max_filament_slots": 2, "loaded_filaments": [],
            "integration_id": moon.id}])
    );
    let link = printer_hosts::printer_link(&fixture.owner.client(), WAIT, &printer.id).unwrap();
    assert!(matches!(link, printer_hosts::PrinterLink::Linked(host) if host.id == moon.id));

    // An edit changes only what it sends; `Some(None)` unlinks.
    let renamed = PrinterPatch {
        name: Some("Big Voron".into()),
        margin_mm: Some(6),
        ..PrinterPatch::default()
    };
    let expected = Printer {
        name: "Big Voron".into(),
        margin_mm: 6,
        ..printer.clone()
    };
    assert_eq!(fixture.edit(&printer.id, renamed).unwrap(), expected);
    let unlink = PrinterPatch {
        integration_id: Some(None),
        ..PrinterPatch::default()
    };
    let unlinked = fixture.edit(&printer.id, unlink).unwrap();
    assert_eq!(unlinked.integration_id, None);
    assert_eq!(fixture.read(&printer.id).unwrap(), unlinked);

    assert_eq!(fixture.remove(&printer.id).unwrap(), unlinked);
    assert_eq!(fixture.stored_at(FLEET).as_deref(), Some("[]"));
    for gone in [fixture.read(&printer.id), fixture.remove(&printer.id)] {
        assert!(matches!(gone, Err(PrinterSetupError::NotFound)), "{gone:?}");
    }
    assert!(fixture.stored().unwrap().contains(&moon.id));
}

#[test]
fn an_invalid_printer_value_names_its_field_and_stores_nothing() {
    use Field::{BedDepth, BedHeight, BedWidth, FilamentSlots, IntegrationId, Margin, Model, Name};
    use Reason::{Empty, NotAPrinterHost, OutOfRange, TooLong};
    let fixture = Fixture::new();
    let spool = fixture
        .create(new(ServiceKind::Spoolman, "http://spool.test"))
        .unwrap();
    let cases = [
        (
            NewPrinter {
                bed_width_mm: 0,
                bed_depth_mm: 10_001,
                bed_height_mm: 0,
                ..voron(None)
            },
            // The margin is checked against the bed it would sit on.
            vec![
                (BedWidth, OutOfRange),
                (BedDepth, OutOfRange),
                (BedHeight, OutOfRange),
                (Margin, OutOfRange),
            ],
        ),
        (
            NewPrinter {
                margin_mm: 175,
                max_filament_slots: 5,
                ..voron(None)
            },
            vec![(Margin, OutOfRange), (FilamentSlots, OutOfRange)],
        ),
        (
            NewPrinter {
                name: "  ".into(),
                model: "m".repeat(201),
                max_filament_slots: 0,
                ..voron(None)
            },
            vec![(Name, Empty), (Model, TooLong), (FilamentSlots, OutOfRange)],
        ),
        (
            voron(Some("no-such-id")),
            vec![(IntegrationId, NotAPrinterHost)],
        ),
        (
            voron(Some(&spool.id)),
            vec![(IntegrationId, NotAPrinterHost)],
        ),
    ];
    for (index, (request, expected)) in cases.into_iter().enumerate() {
        let Err(PrinterSetupError::Invalid(invalid)) = fixture.add(request) else {
            panic!("case {index} was not refused as invalid")
        };
        let invalid: Vec<_> = invalid.iter().map(|one| (one.field, one.reason)).collect();
        assert_eq!(invalid, expected, "case {index}");
    }
    assert_eq!(fixture.stored_at(FLEET), None);
}

#[test]
fn stored_values_are_kept_and_only_new_values_are_checked() {
    let fixture = Fixture::new();
    // A row as an import could leave it: no height, a margin Accept refuses,
    // a loaded slot, and fields no operation here sets.
    let stored = json!([{"id": "p-1", "name": "Old", "model": "mk3", "bed_width_mm": 250,
        "bed_depth_mm": 210, "bed_height_mm": null, "margin_mm": 200, "max_filament_slots": 4,
        "loaded_filaments": [{"slot": 3, "filament_color_id": "pla-black", "label": "PLA"}],
        "device_id": "ha-device", "preset_id": "prusa-mk3", "preferred_slicer": "prusa"}]);
    seed_setting(&fixture.owner, FLEET, &stored.to_string());
    let rename = PrinterPatch {
        name: Some("Renamed".into()),
        ..PrinterPatch::default()
    };
    let renamed = fixture.edit("p-1", rename).unwrap();
    assert_eq!((renamed.bed_height_mm, renamed.margin_mm), (None, 200));
    let mut expected = stored.clone();
    expected[0]["name"] = json!("Renamed");
    let rows: Value = serde_json::from_str(&fixture.stored_at(FLEET).unwrap()).unwrap();
    assert_eq!(rows, expected);

    let slots = |slots| PrinterPatch {
        max_filament_slots: Some(slots),
        ..PrinterPatch::default()
    };
    // Slot 3 holds filament, so it cannot be removed; slot 4 is empty.
    let refused = fixture.edit("p-1", slots(2));
    assert!(
        matches!(refused, Err(PrinterSetupError::SlotHoldsFilament)),
        "{refused:?}"
    );
    assert_eq!(fixture.edit("p-1", slots(3)).unwrap().max_filament_slots, 3);
    // Touching the bed re-checks the stored margin against it.
    let widen = PrinterPatch {
        bed_width_mm: Some(300),
        ..PrinterPatch::default()
    };
    let refused = fixture.edit("p-1", widen);
    assert!(
        matches!(refused, Err(PrinterSetupError::Invalid(_))),
        "{refused:?}"
    );
}

#[test]
fn a_fleet_document_that_is_not_the_typed_shape_refuses_every_operation_untouched() {
    use DocumentFault::{DuplicateId, NotAList, WrongShape};
    let fixture = Fixture::new();
    let row = json!({"id": "p-1", "name": "Voron", "model": "v", "bed_width_mm": 350,
        "bed_depth_mm": 350, "bed_height_mm": 340, "margin_mm": 4, "max_filament_slots": 1,
        "loaded_filaments": []});
    let mut extra = row.clone();
    extra["id"] = json!("p-2");
    extra["nozzle"] = json!(0.4);
    let cases = [
        (json!({"id": "p-1"}), None, NotAList),
        (json!([row, extra]), Some(1), WrongShape),
        (
            json!([row, {"id": "p-2", "name": "No bed"}]),
            Some(1),
            WrongShape,
        ),
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
        for refused in [
            fixture.add(voron(None)),
            fixture.read("p-1"),
            fixture.edit("p-1", PrinterPatch::default()),
            fixture.remove("p-1"),
        ] {
            let Err(PrinterSetupError::StoredDocumentInvalid(found)) = refused else {
                panic!("{text} took an operation: {refused:?}")
            };
            assert_eq!(found, fault, "{text}");
        }
        assert_eq!(fixture.stored_at(FLEET).as_deref(), Some(text.as_str()));
    }
}

#[test]
fn the_printer_limit_and_a_refused_credential_store_nothing() {
    let fixture = Fixture::new();
    let stranger = || Credential::Session(Secret::new("not-a-session".into()));
    let refused = [
        fixture
            .client
            .create_printer(stranger(), voron(None), &IDLE, WAIT),
        fixture.client.read_printer(stranger(), "p-1", &IDLE, WAIT),
    ];
    for refused in refused {
        let refused = format!("{refused:?}");
        assert_eq!(refused, "Err(Writer(Auth(SessionRequired)))");
    }
    assert_eq!(fixture.stored_at(FLEET), None);

    for _ in 0..MAX_PRINTERS {
        fixture.add(voron(None)).unwrap();
    }
    let full = fixture.stored_at(FLEET);
    let over = fixture.add(voron(None));
    assert!(
        matches!(over, Err(PrinterSetupError::LimitReached)),
        "{over:?}"
    );
    assert_eq!(fixture.stored_at(FLEET), full);
}
