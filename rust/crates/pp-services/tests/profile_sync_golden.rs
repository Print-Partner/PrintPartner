//! T-60: profile sync scans and events, replayed from the Node golden.
//! Replays `fixtures/parity/profile-sync/node.json`: every record and event
//! Node's profile sync produced for the files under `roots/`.
use pp_services::profile_sync::{
    ProfileKind, ProfileSlicer, ProfileSyncEvent, SyncedProfile, WatchRoot, read_profile, scan_root,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/parity/profile-sync"
);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Root {
    kind: ProfileSlicer,
    base_dir: String,
    dirs: BTreeMap<String, String>,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(untagged)]
enum Outcome {
    Synced {
        path: String,
        #[serde(flatten)]
        record: Box<SyncedProfile>,
        event: ProfileSyncEvent,
    },
    Failed {
        path: String,
        failed: bool,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    roots: Vec<Root>,
    outcomes: Vec<Outcome>,
}

fn capture() -> Capture {
    serde_json::from_slice(&std::fs::read(Path::new(FIXTURES).join("node.json")).unwrap()).unwrap()
}

fn watch_roots(capture: &Capture) -> Vec<WatchRoot> {
    capture
        .roots
        .iter()
        .map(|root| WatchRoot {
            kind: root.kind,
            base_dir: Path::new(FIXTURES).join(&root.base_dir),
            // Node's object order: printer, process, filament.
            dirs: [
                ("printer", ProfileKind::Printer),
                ("process", ProfileKind::Process),
                ("filament", ProfileKind::Filament),
            ]
            .into_iter()
            .map(|(key, kind)| (kind, root.dirs[key].clone()))
            .collect(),
        })
        .collect()
}

/// A path relative to the fixture dir with `/` separators, as node.json stores it.
fn rel(path: &Path) -> String {
    path.strip_prefix(FIXTURES)
        .unwrap()
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn relativize(mut record: SyncedProfile) -> SyncedProfile {
    let path = match &mut record {
        SyncedProfile::Printer(p) => &mut p.source_path,
        SyncedProfile::Process(p) => &mut p.source_path,
        SyncedProfile::Filament(p) => &mut p.source_path,
    };
    *path = rel(Path::new(path.as_str()));
    record
}

fn rust_outcomes(roots: &[WatchRoot]) -> Vec<Outcome> {
    (0..roots.len())
        .flat_map(|i| scan_root(roots, i))
        .map(|scanned| match scanned.result {
            Ok((record, event)) => Outcome::Synced {
                path: rel(&scanned.path),
                record: Box::new(relativize(record)),
                event,
            },
            Err(_) => Outcome::Failed {
                path: rel(&scanned.path),
                failed: true,
            },
        })
        .collect()
}

#[test]
fn ticket_t_60_profile_sync_matches_node_capture() {
    let capture = capture();
    let roots = watch_roots(&capture);
    let rust = rust_outcomes(&roots);
    assert!(
        capture.outcomes.len() >= 43,
        "corpus shrank: {}",
        capture.outcomes.len()
    );
    let mut differences = Vec::new();
    for i in 0..capture.outcomes.len().max(rust.len()) {
        if capture.outcomes.get(i) != rust.get(i) {
            differences.push(format!(
                "#{i}\n  node: {:?}\n  rust: {:?}",
                capture.outcomes.get(i),
                rust.get(i)
            ));
        }
    }
    println!(
        "profile-sync parity: {} outcomes, {} differences",
        capture.outcomes.len(),
        differences.len()
    );
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn ticket_t_60_profile_sync_resolves_inherited_identity() {
    let roots = watch_roots(&capture());
    let base = Path::new(FIXTURES).join("roots/orca/.config/OrcaSlicer/user/default");
    let read = |path: PathBuf| read_profile(&roots, &path).unwrap().unwrap();

    // Case-insensitive parent lookup; own settings override the parent's.
    let (SyncedProfile::Printer(printer), event) = read(base.join("machine/My X1C.json")) else {
        panic!("printer expected");
    };
    let resolved: serde_json::Value = serde_json::from_str(&printer.resolved_flat_config).unwrap();
    assert_eq!(resolved["printer_model"], "Bambu Lab X1 Carbon");
    assert_eq!(resolved["retraction_length"], "0.6");
    assert_eq!(printer.nozzle_diameter_mm.as_deref(), Some("0.4"));
    assert_eq!(
        (event.kind, event.slicer, event.name.as_str()),
        (ProfileKind::Printer, ProfileSlicer::Orca, "My X1C")
    );

    // Filament numbers come from the file's own values, the material from its name.
    let (SyncedProfile::Filament(filament), _) = read(base.join("filament/PLA Matte.json")) else {
        panic!("filament expected");
    };
    assert_eq!(filament.material_type, "PLA");
    assert_eq!(
        (filament.nozzle_temp_c, filament.bed_temp_c),
        (Some(216.0), Some(60.0))
    );
    assert_eq!(filament.extrusion_multiplier, None);
    let resolved: serde_json::Value = serde_json::from_str(&filament.resolved_flat_config).unwrap();
    assert_eq!(resolved["filament_flow_ratio"], "0.98");

    // A `type` field outranks the directory.
    let (record, event) = read(base.join("process/typed filament.json"));
    assert!(matches!(record, SyncedProfile::Filament(_)));
    assert_eq!(event.kind, ProfileKind::Filament);

    // Malformed and unreadable files produce no record.
    for name in ["bom.json", "broken.json", "folder.json"] {
        assert!(
            read_profile(&roots, &base.join("process").join(name))
                .unwrap()
                .is_err(),
            "{name}"
        );
    }

    // A path under no configured root is ignored, as Node's locate does.
    assert!(read_profile(&roots, &Path::new(FIXTURES).join("generate.mjs")).is_none());
}

#[test]
fn ticket_t_60_profile_sync_reads_prusa_ini() {
    let roots = watch_roots(&capture());
    let base = Path::new(FIXTURES).join("roots/prusa/.config/PrusaSlicer");
    let (SyncedProfile::Filament(petg), event) =
        read_profile(&roots, &base.join("filament/Prusament PETG.ini"))
            .unwrap()
            .unwrap()
    else {
        panic!("filament expected");
    };
    // BOM and CRLF file: header version, trimmed keys, no raw JSON.
    assert_eq!(event.version.as_deref(), Some("2.7.4+win64"));
    assert_eq!(
        (
            petg.material_type.as_str(),
            petg.nozzle_temp_c,
            petg.bed_temp_c,
            petg.fan_pct
        ),
        ("PETG", Some(250.0), Some(90.0), Some(1.0))
    );
    assert!(petg.raw_json.is_none() && petg.raw_ini.is_some());
    let resolved: serde_json::Value = serde_json::from_str(&petg.resolved_flat_config).unwrap();
    assert_eq!(resolved["key with spaces"], "value with spaces");
}
