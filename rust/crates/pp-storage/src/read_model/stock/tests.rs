//! Cases the mounted HTTP test (`pp-core/tests/filament_shortfalls_http.rs`)
//! does not reach: the other unknown reasons, Part grouping, and the stored
//! inputs query.
use super::super::{Unit, graph};
use super::*;

/// The Node fixture database (full schema) in a scratch directory.
struct Store(std::path::PathBuf, Connection);

impl Store {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pp-stock-{}", rand::random::<u64>()));
        std::fs::create_dir_all(root.join("repos/1/revisions/accepted")).unwrap();
        let database = root.join("fixture.db");
        let fixture = include_bytes!("../../../tests/fixtures/accepted-plan-node.db");
        std::fs::write(&database, fixture).unwrap();
        let connection = Connection::open(database).unwrap();
        connection
            .execute_batch(include_str!("../../print_grams/schema.sql"))
            .unwrap();
        Self(root, connection)
    }

    fn run(&self, sql: &str) {
        self.1.execute_batch(sql).unwrap();
    }

    /// A send (Checkoff link `link`) with `grams` for `units` units, when
    /// it has grams, and one checked-off unit per entry of `parts`, recorded
    /// on `day` as Node's `appendPrintOutcomes` records them.
    fn send(
        &self,
        tenant: &str,
        link: &str,
        day: u32,
        grams: Option<(&str, f64, u32)>,
        parts: &[i64],
    ) {
        if let Some((basis, grams, units)) = grams {
            self.run(&format!(
                "INSERT INTO print_link_grams(tenant_id,link_id,grams,units,basis,updated_at)
                 VALUES('{tenant}','{link}',{grams},{units},'{basis}','x')"
            ));
        }
        let at = format!("2026-01-{day:02}T00:00:00.000Z");
        self.run(&format!(
            "INSERT INTO print_jobs(id,tenant_id,profile_id,at,status,filament_consumed_g,link_id)
             VALUES('{link}-job','{tenant}',1,'{at}','sent',NULL,'{link}')"
        ));
        for (index, part) in parts.iter().enumerate() {
            self.run(&format!(
                "INSERT INTO print_job_parts(id,tenant_id,job_id,at,profile_id,part_id,result,link_id)
                 VALUES('{link}-{index}','{tenant}','{link}-job','{at}',1,{part},'confirmed','{link}')"
            ));
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The fixture's included Part copied once per `(id, filament)`, each with
/// one completed and one remaining unit.
fn plan(parts: &[(i64, &str)]) -> Box<Snapshot> {
    let mut store = Store::new();
    let repos = store.0.join("repos");
    let tx = store.1.transaction().unwrap();
    let read = graph::read(&tx, "default", 1, &repos, &mut graph::Budget::default());
    let Ok(AcceptedRead::Ready { mut snapshot }) = read else {
        panic!("fixture is ready")
    };
    let template = snapshot.parts.iter().find(|p| p.included).unwrap().clone();
    let unit = |completed| Unit {
        completed,
        ..template.units[0].clone()
    };
    snapshot.parts = parts
        .iter()
        .map(|&(id, filament)| {
            let mut part = template.clone();
            part.projection_part_id = id;
            part.filament_color_id = Some(format!(" {filament} "));
            part.units = vec![unit(true), unit(false)];
            part
        })
        .collect();
    snapshot
}

/// Eight Parts measured at 10.5 g a unit, two integrations configured, and
/// `local`'s inventory.
fn scenario() -> (
    Box<Snapshot>,
    StockInputs,
    HashMap<String, Option<Inventory>>,
) {
    let snapshot = plan(&[
        (10, "spoolman:local:filament:2"),
        (11, "spoolman:local:filament:2"),
        (12, "spoolman:gone:filament:2"),
        (13, "spoolman:local:filament:404"),
        (14, "spoolman:local:filament:5"),
        (15, "pla-basic-black"),
        (16, ""),
        (17, "spoolman:local:filament:7"),
        (18, "spoolman:off:filament:2"),
    ]);
    let integration = |id: &str| SpoolmanIntegration {
        id: id.into(),
        base_url: "http://127.0.0.1:7912".into(),
        api_key: None,
        enabled: id != "off",
    };
    let grams = (10..=18)
        .map(|part| {
            let link_ids = vec!["shared".into(), format!("own-{part}")];
            let recorded = PartGrams {
                grams_per_unit: 10.5,
                basis: Basis::Measured,
                unconverted: false,
                link_ids,
            };
            (part, recorded)
        })
        .collect();
    let inputs = StockInputs::On {
        integrations: vec![
            integration("other"),
            integration("local"),
            integration("off"),
        ],
        grams,
    };
    let filament = |label: &str| InventoryFilament {
        label: label.into(),
        hex: None,
    };
    let spool = |id, filament_id, remaining_g| InventorySpool {
        id,
        filament_id,
        remaining_g,
    };
    let inventory = Inventory {
        filaments: HashMap::from([
            (2, filament("Black")),
            (5, filament("No weight")),
            (7, filament("Empty")),
        ]),
        spools: vec![
            spool(1, 2, Some(30.0)),
            spool(2, 5, Some(900.0)),
            spool(3, 5, None),
        ],
    };
    let inventories = HashMap::from([("local".to_owned(), Some(inventory))]);
    (snapshot, inputs, inventories)
}

#[test]
fn unknowns_never_read_as_zero_and_shared_jobs_count_once() {
    let (snapshot, inputs, inventories) = scenario();
    let read: Vec<String> = integrations_to_read(&snapshot, &inputs)
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(
        read,
        ["local"],
        "only configured, enabled, named integrations"
    );
    let accepted = AcceptedRead::Ready { snapshot };
    let Some(FilamentShortfalls::Ready { filaments, .. }) =
        shortfalls(1, &accepted, &inputs, &inventories)
    else {
        panic!("ready")
    };
    let by_id = |id: &str| {
        let found = filaments.iter().find(|f| f.filament_color_id == id);
        found.unwrap()
    };

    // Two Parts on one filament: ceil(10.5) each, and the shared job once.
    let black = by_id("spoolman:local:filament:2");
    let need = GramsNeeded::Measured {
        grams: 22,
        job_count: 3,
    };
    assert_eq!((black.parts.len(), &black.grams_needed), (2, &need));
    assert_eq!(black.status, Sufficiency::Sufficient);
    for (id, reason) in [
        (
            "spoolman:gone:filament:2",
            InventoryUnknown::IntegrationMissing,
        ),
        (
            "spoolman:off:filament:2",
            InventoryUnknown::IntegrationDisabled,
        ),
        (
            "spoolman:local:filament:404",
            InventoryUnknown::FilamentMissing,
        ),
        (
            "spoolman:local:filament:5",
            InventoryUnknown::SpoolWeightMissing,
        ),
        ("pla-basic-black", InventoryUnknown::NotSpoolman),
        ("", InventoryUnknown::NotSpoolman),
    ] {
        let need = by_id(id);
        let unknown = GramsAvailable::Unknown { reason };
        assert_eq!(need.grams_available, unknown, "{id:?}");
        assert_eq!(need.status, Sufficiency::Unknown, "{id:?}");
    }
    // Spoolman knows the filament and has no spools: zero grams, a real shortfall.
    let empty = by_id("spoolman:local:filament:7");
    let none = GramsAvailable::Spoolman {
        grams: 0,
        spool_ids: vec![],
    };
    let short = Sufficiency::Short { short_grams: 11 };
    assert_eq!((&empty.grams_available, &empty.status), (&none, &short));
}

#[test]
fn unknown_need_says_whether_any_grams_were_recorded() {
    let snapshot = plan(&[(10, "spoolman:local:filament:2"), (11, "")]);
    let accepted = AcceptedRead::Ready { snapshot };
    let need = |grams: HashMap<i64, PartGrams>| {
        let inputs = StockInputs::On {
            integrations: vec![],
            grams,
        };
        let Some(FilamentShortfalls::Ready { filaments, .. }) =
            shortfalls(1, &accepted, &inputs, &HashMap::new())
        else {
            panic!("ready")
        };
        filaments
            .into_iter()
            .map(|f| f.grams_needed)
            .collect::<Vec<_>>()
    };
    let unknown = |reason| GramsNeeded::Unknown { reason };
    let none = unknown(NeedUnknown::GramsNotRecorded);
    assert_eq!(need(HashMap::new()), [none.clone(), none]);
    let part_10 = |basis| PartGrams {
        grams_per_unit: 3.0,
        basis,
        unconverted: false,
        link_ids: vec!["link".into()],
    };
    let measured = GramsNeeded::Measured {
        grams: 3,
        job_count: 1,
    };
    let unprinted = unknown(NeedUnknown::PartNotPrinted);
    let recorded = HashMap::from([(10, part_10(Basis::Measured))]);
    assert_eq!(need(recorded), [unprinted.clone(), measured]);
    let estimated = GramsNeeded::Estimated {
        grams: 3,
        job_count: 1,
        reason: None,
    };
    let recorded = HashMap::from([(10, part_10(Basis::Estimated))]);
    assert_eq!(need(recorded), [unprinted, estimated]);
}

#[test]
fn an_estimated_part_makes_its_filament_estimated() {
    let part = |id, grams_needed| PartNeed {
        part_id: id,
        filename: String::new(),
        remaining_units: 1,
        grams_needed,
    };
    let measured = GramsNeeded::Measured {
        grams: 4,
        job_count: 1,
    };
    let estimated = GramsNeeded::Estimated {
        grams: 6,
        job_count: 1,
        reason: Some(EstimateKept::MeasuredLengthNotConvertible),
    };
    let parts = [
        (part(1, measured), vec!["shared"]),
        (part(2, estimated), vec!["shared", "send"]),
    ];
    let both = GramsNeeded::Estimated {
        grams: 10,
        job_count: 2,
        reason: Some(EstimateKept::MeasuredLengthNotConvertible),
    };
    assert_eq!(group_need(&parts), both);
}

#[test]
fn inputs_read_module_state_integrations_and_measured_grams() {
    let store = Store::new();
    let inputs = || read_inputs(&store.1, "default", 1).unwrap();
    assert_eq!(inputs(), StockInputs::Off(ModuleOffReason::DefaultOff));
    store.run(
        r#"INSERT INTO app_settings(tenant_id,key,value) VALUES
           ('default','feature_toggles','{"spoolman":true}'),
           ('default','integrations','[null,
             {"id":"printer","type":"moonraker","config":{"base_url":"http://p"}},
             {"id":"local","type":"spoolman","config":{"base_url":" http://h:7912/ ","api_key":" k "}},
             {"id":"alt","type":"spoolman","config":{"baseUrl":"http://10.0.0.2","apiKey":"","enabled":false}},
             {"id":"broken","type":"spoolman","config":{}}]')"#,
    );
    let measured = |grams, units| Some(("measured", grams, units));
    let estimated = |grams, units| Some(("estimated", grams, units));
    store.send("default", "old", 1, measured(90.0, 3), &[2, 2, 2]);
    store.send("default", "new", 3, measured(50.5, 2), &[2, 1]);
    // Ignored for Part 2: an estimate beside measured sends, a send with no
    // grams, and another tenant's send.
    store.send("default", "guess", 4, estimated(999.0, 1), &[2]);
    store.send("default", "none", 5, None, &[2]);
    store.send("other", "foreign", 6, measured(999.0, 1), &[2]);
    // Part 4 has only an estimate; Part 5 one checked-off unit of a ten-unit
    // send, which still costs a tenth of the send's grams.
    store.send("default", "est", 7, estimated(12.0, 4), &[4]);
    store.send("default", "partial", 8, measured(100.0, 10), &[5]);
    // Part 6's printer measured a length its file gave no factor for.
    store.send("default", "unconverted", 9, estimated(8.0, 2), &[6]);
    store.run("UPDATE print_link_grams SET measured_mm=1500 WHERE link_id='unconverted'");
    for n in 1..=7 {
        let grams = f64::from(10 * n);
        store.send(
            "default",
            &format!("p3-{n}"),
            10 + n,
            measured(grams, 1),
            &[3],
        );
    }
    let StockInputs::On {
        integrations,
        grams,
    } = inputs()
    else {
        panic!("module is on")
    };
    let local = SpoolmanIntegration {
        id: "local".into(),
        base_url: "http://h:7912/".into(),
        api_key: Some("k".into()),
        enabled: true,
    };
    let alt = SpoolmanIntegration {
        id: "alt".into(),
        base_url: "http://10.0.0.2".into(),
        api_key: None,
        enabled: false,
    };
    assert_eq!(integrations, [local, alt]);
    let debug = format!("{:?}", integrations[0]);
    assert!(!debug.contains("\"k\""), "Debug hides the key");
    // (50.5 / 2 + 90 / 3) / 2: a send's grams, fractional ones included,
    // split over all of its units.
    let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
    let part = |grams_per_unit, basis, links: &[&str]| PartGrams {
        grams_per_unit,
        basis,
        unconverted: false,
        link_ids: ids(links),
    };
    assert_eq!(grams[&2], part(27.625, Basis::Measured, &["new", "old"]));
    assert_eq!(grams[&1], part(25.25, Basis::Measured, &["new"]));
    // The five most recent of seven sends: 70, 60, 50, 40, 30.
    let recent = ["p3-7", "p3-6", "p3-5", "p3-4", "p3-3"];
    assert_eq!(grams[&3], part(50.0, Basis::Measured, &recent));
    assert_eq!(grams[&4], part(3.0, Basis::Estimated, &["est"]));
    assert_eq!(grams[&5], part(10.0, Basis::Measured, &["partial"]));
    let unconverted = PartGrams {
        unconverted: true,
        ..part(4.0, Basis::Estimated, &["unconverted"])
    };
    assert_eq!(grams[&6], unconverted);
    assert_eq!(grams.len(), 6);

    let off = r#"UPDATE app_settings SET value='{"spoolman":false}' WHERE key='feature_toggles'"#;
    store.run(off);
    assert_eq!(inputs(), StockInputs::Off(ModuleOffReason::TurnedOff));
}
