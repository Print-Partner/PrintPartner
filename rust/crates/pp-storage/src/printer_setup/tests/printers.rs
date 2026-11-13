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

/// A row as an import could leave it: no height, a loaded slot, and the
/// fields no operation here sets.
fn imported(id: &str, margin_mm: u32) -> Value {
    json!({"id": id, "name": "Old", "model": "mk3", "bed_width_mm": 250, "bed_depth_mm": 210,
        "bed_height_mm": null, "margin_mm": margin_mm, "max_filament_slots": 4,
        "loaded_filaments": [{"slot": 3, "filament_color_id": "pla-black", "label": "PLA"}],
        "device_id": "ha-device", "preset_id": "prusa-mk3", "preferred_slicer": "prusa"})
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
        imported("p-2", 4)
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
            // The margin is not compared with a bed that is itself refused.
            vec![
                (BedWidth, OutOfRange),
                (BedDepth, OutOfRange),
                (BedHeight, OutOfRange),
            ],
        ),
        (
            // Every invalid field is named together, the link included.
            NewPrinter {
                bed_width_mm: 0,
                ..voron(Some("no-such-id"))
            },
            vec![(BedWidth, OutOfRange), (IntegrationId, NotAPrinterHost)],
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
        let named = invalid_fields(fixture.add(request), index);
        assert_eq!(named, expected, "case {index}");
    }
    assert_eq!(fixture.stored_at(FLEET), None);
}

#[test]
fn stored_values_are_kept_and_only_new_values_are_checked() {
    let fixture = Fixture::new();
    // A row as an import could leave it: no height, a margin Accept refuses,
    // a loaded slot, and fields no operation here sets.
    let stored = json!([imported("p-1", 200)]);
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
    let unchanged = fixture.stored_at(FLEET);
    // Slot 3 holds filament, so it cannot be removed; slot 4 is empty.
    let refused = fixture.edit("p-1", slots(2));
    assert!(
        matches!(refused, Err(PrinterSetupError::SlotHoldsFilament)),
        "{refused:?}"
    );
    // An invalid value in the same edit is the answer, not the slot.
    let both = PrinterPatch {
        name: Some(" ".into()),
        ..slots(2)
    };
    let named = invalid_fields(fixture.edit("p-1", both), 0);
    assert_eq!(named, [(Field::Name, Reason::Empty)]);
    // Touching the bed compares the stored margin with it.
    let widen = PrinterPatch {
        bed_width_mm: Some(300),
        ..PrinterPatch::default()
    };
    let named = invalid_fields(fixture.edit("p-1", widen), 1);
    assert_eq!(named, [(Field::Margin, Reason::OutOfRange)]);
    assert_eq!(fixture.stored_at(FLEET), unchanged, "a refused edit stored");

    let trimmed = fixture.edit("p-1", slots(3)).unwrap();
    assert_eq!(trimmed.max_filament_slots, 3);
    let kept = trimmed.loaded_filaments[0].filament_color_id.as_deref();
    assert_eq!(
        (trimmed.loaded_filaments.len(), kept),
        (1, Some("pla-black"))
    );
}

/// A link is checked when it changes, and only then.
#[test]
fn an_edit_checks_a_link_only_when_it_changes() {
    let fixture = Fixture::new();
    let host = |name: &str| {
        let moon = new(ServiceKind::Moonraker, &format!("http://{name}.test"));
        fixture.create(moon).unwrap().id
    };
    let (first, second) = (host("first"), host("second"));
    let spool = new(ServiceKind::Spoolman, "http://spool.test");
    let spool = fixture.create(spool).unwrap().id;
    let printer = fixture.add(voron(Some(&first))).unwrap();
    let link = |id: &str| PrinterPatch {
        name: Some("Edited".into()),
        integration_id: Some(Some(id.to_owned())),
        ..PrinterPatch::default()
    };

    for (index, refused) in [spool.as_str(), "no-such-id"].into_iter().enumerate() {
        let named = invalid_fields(fixture.edit(&printer.id, link(refused)), index);
        assert_eq!(named, [(Field::IntegrationId, Reason::NotAPrinterHost)]);
    }
    assert_eq!(fixture.read(&printer.id).unwrap(), printer);
    let relinked = fixture.edit(&printer.id, link(&second)).unwrap();
    assert_eq!(relinked.integration_id.as_deref(), Some(second.as_str()));

    // Once its integration is deleted, the printer keeps the id and can still
    // be edited with that id sent back unchanged.
    fixture.delete(&second).unwrap();
    let edited = fixture.edit(&printer.id, link(&second)).unwrap();
    let kept = (edited.name.as_str(), edited.integration_id.as_deref());
    assert_eq!(kept, ("Edited", Some(second.as_str())));
}

#[test]
fn a_fleet_document_that_is_not_the_typed_shape_refuses_every_operation_untouched() {
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
        (json!([row, with("name", json!(7))]), Some(1), WrongShape),
        (
            json!([
                row,
                with(
                    "loaded_filaments",
                    json!([{"slot": 1, "label": "", "spool": 3}])
                )
            ]),
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

/// A row the import wrote with keys the shape does not name. Rust made
/// neither key, reads neither, and shows neither; an edit stores both back.
#[test]
fn keys_the_shape_does_not_name_are_never_shown_and_an_edit_keeps_them_byte_for_byte() {
    let fixture = Fixture::new();
    let unread = r#""nozzle_mm":0.4,"notes":{"token":"unread-canary","list":[1,2.5,null,"é\n"]}"#;
    let row = |id: &str, name: &str, slots: u8| {
        format!(
            r#"{{"id":"{id}","name":"{name}","model":"v","bed_width_mm":350,"bed_depth_mm":350,"bed_height_mm":340,"margin_mm":4,"max_filament_slots":{slots},"loaded_filaments":[],{unread}}}"#
        )
    };
    let other = row("p-2", "Other", 1);
    let stored = format!("[{},{other}]", row("p-1", "Voron", 1));
    seed_setting(&fixture.owner, FLEET, &stored);
    let hidden = |printer: &Printer| {
        let shown = format!("{printer:?}");
        assert!(shown.contains("Unread(2 keys)"), "{shown}");
        assert!(
            !shown.contains("nozzle") && !shown.contains("canary"),
            "{shown}"
        );
    };
    hidden(&fixture.read("p-1").unwrap());
    let patch = PrinterPatch {
        name: Some("Renamed".into()),
        max_filament_slots: Some(2),
        ..PrinterPatch::default()
    };
    hidden(&fixture.edit("p-1", patch).unwrap());
    // Only what the edit set differs; both rows keep their keys as stored.
    let edited = row("p-1", "Renamed", 2);
    assert_eq!(
        fixture.stored_at(FLEET),
        Some(format!("[{edited},{other}]"))
    );
    // A printer Rust makes has none.
    let made = fixture.add(voron(None)).unwrap();
    assert!(format!("{made:?}").contains("Unread(0 keys)"));
    hidden(&fixture.remove("p-2").unwrap());
    let left = fixture.stored_at(FLEET).unwrap();
    assert!(left.starts_with(&format!("[{edited},")), "{left}");
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
