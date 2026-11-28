use pp_core::production::kit_recipe::{KitRecipeEncodingError, project};
use pp_services::kit::{build_editable_kit_bundle, kit_bundle_zip, stringify};
use pp_storage::{
    Limits, WriterFailure, WriterOwner,
    auth::{FirstUserTenant, Outcome, Request, Secret},
    build_graph::{PositiveId, kit_recipe::KitRecipeFailure},
    manifest_text::JsText,
    ownership_marker::WriterGate,
    read_model::Credential,
};
use pp_testkit::{Fixture, Installation};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    io::{Cursor, Read},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(5);

fn register(owner: &WriterOwner, email: &str) -> (String, Secret) {
    let outcome = owner
        .auth(FirstUserTenant::NewUser)
        .submit(
            Request::Register {
                email: email.into(),
                display_name: "Kit fixture".into(),
                password: Secret::new("disposable-passphrase".into()),
            },
            Arc::new(AtomicBool::new(false)),
            WAIT,
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let Outcome::Session { user, token } = outcome else {
        panic!("session expected");
    };
    (user.tenant_id, token)
}

fn seed(db: &Connection, owner: &str, other: &str) {
    db.execute(
        "INSERT INTO build_profiles(id,tenant_id,name,order_number)
         VALUES (1,?1,'Fixture Kit','ORDER-7'),(2,?2,'Other Kit',NULL),(3,?1,'Empty Kit',NULL)",
        params![owner, other],
    )
    .unwrap();
    db.execute(
        "INSERT INTO projects(id,tenant_id,name,url,local_path,role,metadata_json,imported_paths)
         VALUES (1,?1,'Frame','https://example.invalid/frame','/private/model-store','base',NULL,'[\"STLs/\"]'),
                (2,?1,'Local','file:///private/kit.zip','/private/extracted','addon','{\"category\":null}',NULL),
                (3,?2,'Other','https://example.invalid/other',NULL,'addon',NULL,NULL)",
        params![owner, other],
    )
    .unwrap();
    db.execute(
        "UPDATE projects SET branch='release',tag='v1',manifest_community_slug='fixture-frame' WHERE id=1",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id)
         VALUES (?1,1,40,'addon',3),(?1,1,10,'base',1),(?1,1,20,'addon',2),
                (?1,1,30,'addon',1),(?1,1,50,'addon',NULL),(?2,1,0,'base',3)",
        params![owner, other],
    )
    .unwrap();
    db.execute(
        "INSERT INTO parts(tenant_id,profile_id,match_key,relative_path,filename,source_layer,
                           role,status,quantity_auto,quantity_override,quantity_effective,
                           included,notes,geometry_same,filament_color_id,filament_custom_hex,
                           requirement,option_group_id,manifest_source,github_blob_url)
         VALUES (?1,1,'base:z','STLs/z.stl','z.stl','base','accent','matched',2,4,4,
                 0,'Saved edit',1,'blue','#123456',?3,'options','community','file:///private/z.stl'),
                (?1,1,'addon:a','a.stl','a.stl','addon','primary','new',1,NULL,1,
                 1,'',NULL,NULL,NULL,NULL,NULL,NULL,NULL),
                (?2,1,'foreign','other.stl','other.stl','base','primary','new',1,NULL,1,
                 1,'',NULL,NULL,NULL,NULL,NULL,NULL,NULL)",
        params![owner, other, "necessary"],
    )
    .unwrap();
    db.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES (?1,'kit_manifest_1',?2)",
        params![
            owner,
            r#"{"name":"Fixture manifest","selections":{"group":"variant"}}"#
        ],
    )
    .unwrap();
}

#[test]
fn captures_only_saved_owned_recipe_and_remains_stable_after_reopen() {
    let installation = Installation::create(Fixture::Empty).unwrap();
    let open = || {
        WriterOwner::open_with_gate(installation.path(), Limits::default(), WriterGate::SwitchOn)
            .unwrap()
            .0
    };
    let owner = open();
    let (tenant, token) = register(&owner, "kit-owner@example.test");
    let (other_tenant, other_token) = register(&owner, "kit-other@example.test");
    let db = Connection::open(installation.database_path()).unwrap();
    seed(&db, &tenant, &other_tenant);
    drop(db);

    let client = owner.accepted_reads().unwrap();
    let running = AtomicBool::new(false);
    let cancelled = AtomicBool::new(true);
    let plan = PositiveId::new(1).unwrap();
    let credential = || Credential::Session(Secret::new(token.expose().to_owned()));
    let read = |build, credential, cancelled| {
        client.editable_kit_recipe(credential, PositiveId::new(build).unwrap(), cancelled, WAIT)
    };
    let recipe = read(1, credential(), &running).unwrap();
    assert!(matches!(
        read(2, credential(), &running),
        Err(KitRecipeFailure::MissingPlan)
    ));
    assert!(matches!(
        read(1, Credential::Session(other_token), &running),
        Err(KitRecipeFailure::MissingPlan)
    ));
    assert!(matches!(
        read(
            1,
            Credential::Session(Secret::new("not-a-session".into())),
            &running
        ),
        Err(KitRecipeFailure::Storage(WriterFailure::Auth(_)))
    ));
    assert!(matches!(
        read(1, credential(), &cancelled),
        Err(KitRecipeFailure::Storage(WriterFailure::Cancelled))
    ));
    let empty = read(3, credential(), &running).unwrap();
    assert!(empty.layers.is_empty() && empty.sources.is_empty() && empty.parts.is_empty());
    assert_eq!(empty.manifest, Default::default());
    assert_eq!(
        std::fs::read_dir(installation.path().join("exports"))
            .unwrap()
            .count(),
        0
    );

    owner.shutdown().unwrap();
    let reopened = open();
    let reopened_recipe = reopened
        .accepted_reads()
        .unwrap()
        .editable_kit_recipe(credential(), plan, &running, WAIT)
        .unwrap();
    assert_eq!(reopened_recipe, recipe);
    let encode =
        |recipe| build_editable_kit_bundle(&project(recipe).unwrap(), "2026-11-01T12:00:00.000Z");
    let bundle = encode(reopened_recipe);
    assert_eq!(stringify(&bundle, true), stringify(&encode(recipe), true));
    assert_eq!(
        bundle["profile"],
        json!({"name":"Fixture Kit","order_number":"ORDER-7"})
    );
    let frame = json!({"name":"Frame","url":"https://example.invalid/frame","branch":"release",
        "tag":"v1","source_kind":"github","source_type":"git","role":"base","category":"Printer kits",
        "import_rules":["STLs/"],"manifest_community_slug":"fixture-frame"});
    let local = json!({"name":"Local","url":"","branch":"main","source_kind":"github",
        "source_type":"git","role":"addon","category":"unassigned","import_rules":[]});
    assert_eq!(bundle["sources"], json!([frame, local]));
    assert_eq!(
        bundle["layers"],
        json!([
            {"layer_order":10,"layer_type":"base","project":frame},
            {"layer_order":20,"layer_type":"addon","project":local},
            {"layer_order":30,"layer_type":"addon","project":frame},
            {"layer_order":40,"layer_type":"addon","project":null},
            {"layer_order":50,"layer_type":"addon","project":null},
        ])
    );
    assert_eq!(bundle["parts"].as_array().unwrap().len(), 2);
    assert_eq!(bundle["parts"][0]["filename"], "a.stl");
    assert_eq!(
        bundle["parts"][1],
        json!({
            "match_key":"base:z","relative_path":"STLs/z.stl","filename":"z.stl","source_layer":"base",
            "status":"matched","role":"accent","filament_color_id":"blue","filament_custom_hex":"#123456",
            "quantity_auto":2,"quantity_override":4,"quantity_effective":4,"included":false,
            "notes":"Saved edit","geometry_same":true,"requirement":"necessary","option_group_id":"options",
            "manifest_source":"community",
        })
    );
    assert_eq!(bundle["kit_manifest"]["name"], "Fixture manifest");
    assert_eq!(
        bundle["kit_manifest"]["selections"],
        json!({"group":"variant"})
    );
    let mut archive = zip::ZipArchive::new(Cursor::new(kit_bundle_zip(&bundle).unwrap())).unwrap();
    assert_eq!(archive.len(), 1);
    let mut document = String::new();
    archive
        .by_name("kit.json")
        .unwrap()
        .read_to_string(&mut document)
        .unwrap();
    assert_eq!(document, stringify(&bundle, true));
    assert!(!document.contains("/private/"));

    let db = Connection::open(installation.database_path()).unwrap();
    db.execute(
        "UPDATE parts SET requirement=?1 WHERE tenant_id=?2 AND filename='z.stl'",
        params![
            JsText::from_units(vec![0xd800]).sqlite_value().unwrap(),
            tenant
        ],
    )
    .unwrap();
    let non_scalar = reopened
        .accepted_reads()
        .unwrap()
        .editable_kit_recipe(credential(), plan, &running, WAIT)
        .unwrap();
    assert_eq!(
        non_scalar.parts[1]
            .manifest
            .requirement
            .as_ref()
            .unwrap()
            .units(),
        [0xd800]
    );
    assert!(matches!(
        project(non_scalar),
        Err(KitRecipeEncodingError::PartManifest("requirement"))
    ));
    reopened.shutdown().unwrap();
}
