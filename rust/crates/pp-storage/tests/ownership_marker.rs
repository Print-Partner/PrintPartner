//! The durable ownership marker on disposable pp-testkit installations and
//! copied Node schema 34 databases.

use pp_storage::ownership_marker::{
    self, EMPTY_ALLOWLIST, MARKER_FILE, MARKER_TEMP_PREFIX, MarkerRefusal, MarkerState,
    OwnershipRefusal,
};
use pp_testkit::{Corruption, Fixture, Installation, MarkerState as KitMarker, Spec};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Every regular file under `root` with its bytes, and every directory as `None`.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                entries.insert(relative, None);
                pending.push(path);
            } else {
                entries.insert(relative, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    entries
}

/// Corruption specs, plus each named fixture.
fn every_marker_spec() -> Vec<Spec> {
    let mut specs: Vec<Spec> = Fixture::ALL.iter().map(|fixture| fixture.spec()).collect();
    specs.extend(Corruption::ALL.map(|corruption| Spec {
        marker: KitMarker::Corrupt(corruption),
        ..Spec::default()
    }));
    specs
}

#[test]
fn present_but_invalid_markers_are_refused_like_the_testkit_oracle() {
    for corruption in Corruption::ALL {
        let installation = Installation::create(Spec {
            marker: KitMarker::Corrupt(corruption),
            ..Spec::default()
        })
        .unwrap();
        let state = ownership_marker::inspect(installation.path());
        assert!(
            matches!(state, MarkerState::Refused(_)),
            "{corruption:?}: {state:?}"
        );
        if let Some(bytes) = corruption.bytes() {
            assert!(pp_testkit::Marker::parse(&bytes).is_err());
        } else {
            assert_eq!(state, MarkerState::Refused(MarkerRefusal::NotRegularFile));
        }
    }
    let marked = Installation::create(Fixture::RustMarked).unwrap();
    assert_eq!(
        ownership_marker::inspect(marked.path()),
        MarkerState::Valid(ownership_marker::Marker {
            created_at: pp_testkit::FIXTURE_CREATED_AT.into()
        })
    );
    let empty = Installation::create(Fixture::Empty).unwrap();
    assert_eq!(ownership_marker::inspect(empty.path()), MarkerState::Absent);
}

#[cfg(unix)]
#[test]
fn symlinked_marker_is_refused() {
    let installation = Installation::create(Fixture::RustMarked).unwrap();
    let target = installation.path().join("real-marker");
    std::fs::rename(installation.marker_path(), &target).unwrap();
    std::os::unix::fs::symlink(&target, installation.marker_path()).unwrap();
    assert_eq!(
        ownership_marker::inspect(installation.path()),
        MarkerState::Refused(MarkerRefusal::NotRegularFile)
    );
}

#[test]
fn oversized_marker_is_refused() {
    let installation = Installation::create(Fixture::RustMarked).unwrap();
    std::fs::write(installation.marker_path(), vec![b' '; 4096]).unwrap();
    assert_eq!(
        ownership_marker::inspect(installation.path()),
        MarkerState::Refused(MarkerRefusal::TooLarge)
    );
}

#[test]
fn product_empty_rule_matches_the_testkit_rule() {
    let names = [
        "printpartner.lock",
        ".desktop-lease",
        ".desktop-lease-candidate-ab",
        ".desktop-lease-candidate-",
        ".desktop-lease-release-ab",
        ".desktop-lease-retired-ab",
        ".desktop-lease-hidden",
        ".desktop-owner.json",
        ".desktop-owner-release-ab.json",
        ".desktop-owner-release-.json",
        ".desktop-owner-release-ab",
        "lost+found",
        "print-partner.db",
        "print-partner.db-wal",
        "repos",
        MARKER_FILE,
        "desktop.toml",
    ];
    for name in names {
        assert_eq!(
            ownership_marker::allowed_in_empty(name),
            pp_testkit::allowed_in_empty(name),
            "{name}"
        );
    }
    for name in ["owner.json", "previous-marker.json", "other.json"] {
        assert_eq!(
            ownership_marker::allowed_in_node_lease(name),
            pp_testkit::allowed_in_node_lease(name),
            "{name}"
        );
    }
    // The marker writer's crash leftover is the one product-only entry.
    let leftover = format!("{MARKER_TEMP_PREFIX}0011223344556677");
    assert!(ownership_marker::allowed_in_empty(&leftover));
    assert!(!pp_testkit::allowed_in_empty(&leftover));
}

/// Lease names in `.desktop-*` and `printpartner.lock` spellings, with
/// template parts such as `${instance}` replaced by `*`.
fn lease_names(source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for token in [".desktop-", "printpartner.lock"] {
        for (start, _) in source.match_indices(token) {
            let rest = &source[start..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || "._-${}".contains(c)))
                .unwrap_or(rest.len());
            let mut name = String::new();
            let mut chars = rest[..end].trim_end_matches('.').chars();
            while let Some(c) = chars.next() {
                if c == '$' {
                    chars.by_ref().find(|c| *c == '}');
                    name.push('*');
                } else {
                    name.push(c);
                }
            }
            names.insert(name);
        }
    }
    names
}

#[test]
fn lease_names_in_rust_and_node_lease_code_and_the_marker_name_match_the_allowlist() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rust = std::fs::read_to_string(manifest.join("src/lease.rs")).unwrap();
    let rust = rust.split("\n#[cfg(test)]\nmod tests").next().unwrap();
    let node_root = manifest.join("../../../web/apps/server/src");
    let mut node = String::new();
    for file in ["storage-lease.ts", "desktop-context.ts"] {
        node += &std::fs::read_to_string(node_root.join(file)).unwrap();
    }
    let rust_names = lease_names(rust);
    let node_names = lease_names(&node);
    assert!(rust_names.contains(".desktop-lease") && rust_names.contains("printpartner.lock"));
    assert!(
        node_names.contains(".desktop-lease-candidate-*"),
        "{node_names:?}"
    );
    let found: BTreeSet<String> = rust_names.union(&node_names).cloned().collect();
    for name in &found {
        assert!(
            ownership_marker::allowed_in_empty(&name.replace('*', "0123abcd")),
            "lease name {name} is missing from the allowlist"
        );
    }
    let lease_part: BTreeSet<String> = EMPTY_ALLOWLIST
        .iter()
        .filter(|name| !matches!(**name, "lost+found") && !name.starts_with(MARKER_TEMP_PREFIX))
        .map(|name| name.to_string())
        .collect();
    assert_eq!(found, lease_part, "allowlist and lease code disagree");
    // Node keeps its previous owner record inside the lease directory.
    assert!(node.contains(r#"".desktop-lease", "previous-marker.json""#));
    assert!(ownership_marker::allowed_in_node_lease(
        "previous-marker.json"
    ));
    assert!(node.contains(r#""owner.json""#));
    assert!(ownership_marker::allowed_in_node_lease("owner.json"));
    // The marker itself never counts as empty: it marks the directory.
    assert_eq!(MARKER_FILE, pp_testkit::MARKER_FILE);
    assert!(!ownership_marker::allowed_in_empty(MARKER_FILE));
    assert!(!found.iter().any(|name| name.contains(MARKER_FILE)));
}

#[test]
fn node_backed_path_refuses_any_marker_and_leaves_unmarked_directories_alone() {
    for spec in every_marker_spec() {
        let installation = Installation::create(spec).unwrap();
        let before = snapshot(installation.path());
        let result = ownership_marker::refuse_marked(installation.path());
        match spec.marker {
            KitMarker::Absent => assert_eq!(result, Ok(()), "{spec:?}"),
            _ => assert_eq!(result, Err(OwnershipRefusal::RustOwned), "{spec:?}"),
        }
        assert_eq!(snapshot(installation.path()), before, "{spec:?}");
    }
}

#[test]
fn node_reads_schema_34_and_refuses_newer() {
    let server = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../web/apps/server/src/db");
    let schema = std::fs::read_to_string(server.join("schema.ts")).unwrap();
    assert!(schema.contains("export const currentSchemaVersion = 34;"));
    let client = std::fs::read_to_string(server.join("client.ts")).unwrap();
    assert!(client.contains("if (versionBeforeMigration > currentSchemaVersion) {"));
    assert!(client.contains("is newer than supported version"));
}

#[cfg(feature = "desktop_rebuild")]
mod writer {
    use super::*;
    use pp_storage::{Limits, WriterOwner, lease::StorageLease, ownership_marker::WriterGate};
    use pp_testkit::LeaseForm;
    use rusqlite::{Connection, OpenFlags};

    const NODE_CEILING: u64 = 34;

    fn open(installation: &Installation, gate: WriterGate) -> anyhow::Result<()> {
        let (owner, _) = WriterOwner::open_with_gate(installation.path(), Limits::default(), gate)?;
        owner.shutdown()
    }

    fn refusal(error: &anyhow::Error) -> Option<&OwnershipRefusal> {
        error.downcast_ref::<OwnershipRefusal>()
    }

    fn schema_version(database: &Path) -> Option<u64> {
        if !database.exists() {
            return None;
        }
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .map(|value| value.parse().unwrap())
    }

    /// Asserts a refused open changed nothing except the empty lease files
    /// (`printpartner.lock` and a Rust `.desktop-lease`), which the lease
    /// itself creates and removes.
    fn assert_unchanged(installation: &Installation, before: &BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let without_lease_files = |entries: &BTreeMap<PathBuf, Option<Vec<u8>>>| {
            entries
                .iter()
                .filter(|(path, value)| {
                    !(value.as_deref() == Some(&[][..])
                        && (*path == Path::new("printpartner.lock")
                            || *path == Path::new(".desktop-lease")))
                })
                .map(|(path, value)| (path.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let after = snapshot(installation.path());
        assert_eq!(
            without_lease_files(&after),
            without_lease_files(before),
            "{:?}",
            installation.spec()
        );
    }

    /// Copies of Node-written schema 34 databases in otherwise empty installations.
    fn copied_node_databases() -> Vec<Installation> {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut installations = vec![Installation::create(Fixture::NodeSchema34).unwrap()];
        for name in [
            "accepted-plan-node.db",
            "accepted-plate-node.db",
            "required-units/unchanged.db",
        ] {
            let installation = Installation::create(Fixture::Empty).unwrap();
            std::fs::copy(fixtures.join(name), installation.database_path()).unwrap();
            assert_eq!(
                schema_version(&installation.database_path()),
                Some(NODE_CEILING)
            );
            installations.push(installation);
        }
        installations
    }

    fn live_owner_record() -> String {
        let pid = std::process::id();
        let identity = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .zip(std::fs::read_to_string(format!("/proc/{pid}/stat")).ok())
            .map(|(boot, stat)| {
                let start = stat[stat.rfind(')').unwrap() + 2..]
                    .split_whitespace()
                    .nth(19)
                    .unwrap()
                    .to_owned();
                format!("{}:{start}", boot.trim())
            })
            .unwrap_or_else(|| "unavailable-off-linux".into());
        format!(
            r#"{{"pid":{pid},"kind":"standalone","process_identity":"{identity}","instance":"{}"}}"#,
            "ef".repeat(16)
        )
    }

    #[test]
    fn empty_directory_gets_a_marker_before_any_schema() {
        let specs = [
            Fixture::Empty.spec(),
            Spec {
                lease_leftovers: Some(LeaseForm::RustFile),
                ..Spec::default()
            },
        ];
        for spec in specs {
            let installation = Installation::create(spec).unwrap();
            open(&installation, WriterGate::SwitchOn).unwrap();
            let bytes = std::fs::read(installation.marker_path()).unwrap();
            let marker = pp_testkit::Marker::parse(&bytes).unwrap();
            assert_eq!(
                bytes,
                marker.to_bytes(),
                "product bytes match the kit format"
            );
            assert!(schema_version(&installation.database_path()).unwrap() > NODE_CEILING);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(installation.marker_path())
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600);
            }
            assert!(
                !installation
                    .entries()
                    .unwrap()
                    .iter()
                    .any(|name| name.starts_with(MARKER_TEMP_PREFIX))
            );
            // Reopening keeps the first marker.
            open(&installation, WriterGate::SwitchOn).unwrap();
            assert_eq!(std::fs::read(installation.marker_path()).unwrap(), bytes);
        }
    }

    #[test]
    fn dead_owner_record_counts_as_empty_and_a_live_one_does_not() {
        let dead = Installation::create(Fixture::Empty).unwrap();
        std::fs::write(
            dead.path().join(".desktop-owner.json"),
            br#"{"pid":2147483647,"kind":"standalone","process_identity":"gone"}"#,
        )
        .unwrap();
        open(&dead, WriterGate::SwitchOn).unwrap();
        assert!(matches!(
            ownership_marker::inspect(dead.path()),
            MarkerState::Valid(_)
        ));

        for record in [live_owner_record(), "{not json".to_owned()] {
            let live = Installation::create(Fixture::Empty).unwrap();
            std::fs::write(live.path().join(".desktop-owner.json"), &record).unwrap();
            let before = snapshot(live.path());
            let error = open(&live, WriterGate::SwitchOn).unwrap_err();
            assert_eq!(
                refusal(&error),
                Some(&OwnershipRefusal::NotEmptyUnmarked {
                    first_entry: ".desktop-owner.json".into()
                }),
                "{error:#}"
            );
            assert_unchanged(&live, &before);
        }
    }

    #[test]
    fn non_empty_unmarked_directory_is_refused_and_left_unchanged() {
        let mut installations = copied_node_databases();
        let stray = Installation::create(Fixture::Empty).unwrap();
        std::fs::write(stray.path().join("desktop.toml"), b"port = 43123\n").unwrap();
        installations.push(stray);
        for installation in installations {
            let before = snapshot(installation.path());
            let error = open(&installation, WriterGate::SwitchOn).unwrap_err();
            assert!(
                matches!(
                    refusal(&error),
                    Some(OwnershipRefusal::NotEmptyUnmarked { .. })
                ),
                "{error:#}"
            );
            assert_unchanged(&installation, &before);
        }
    }

    #[test]
    fn invalid_marker_is_refused_and_left_unchanged() {
        for corruption in Corruption::ALL {
            for node_database in [false, true] {
                let installation = Installation::create(Spec {
                    node_database,
                    marker: KitMarker::Corrupt(corruption),
                    ..Spec::default()
                })
                .unwrap();
                let before = snapshot(installation.path());
                let error = open(&installation, WriterGate::SwitchOn).unwrap_err();
                assert!(
                    matches!(refusal(&error), Some(OwnershipRefusal::Marker(_))),
                    "{corruption:?}: {error:#}"
                );
                assert_unchanged(&installation, &before);
            }
        }
    }

    #[test]
    fn switch_off_makes_no_schema_change() {
        let mut installations: Vec<Installation> = every_marker_spec()
            .into_iter()
            .map(|spec| Installation::create(spec).unwrap())
            .filter(|installation| installation.spec().lease_leftovers.is_none())
            .collect();
        installations.extend(copied_node_databases());
        for installation in installations {
            let before = snapshot(installation.path());
            let error = open(&installation, WriterGate::SwitchOff).unwrap_err();
            assert_eq!(
                refusal(&error),
                Some(&OwnershipRefusal::SwitchOff),
                "{error:#}"
            );
            assert_unchanged(&installation, &before);
        }
    }

    /// Invariant over every fixture, copied Node database and gate: a database
    /// above Node's schema 34 only ever exists next to a valid marker, and a
    /// refused open changes nothing.
    #[test]
    fn invariant_no_unmarked_database_above_the_node_ceiling() {
        for gate in [WriterGate::SwitchOn, WriterGate::SwitchOff] {
            let mut installations: Vec<Installation> = every_marker_spec()
                .into_iter()
                .map(|spec| Installation::create(spec).unwrap())
                .collect();
            installations.push(
                Installation::create(Spec {
                    lease_leftovers: Some(LeaseForm::RustFile),
                    node_database: true,
                    ..Spec::default()
                })
                .unwrap(),
            );
            installations.extend(copied_node_databases());
            for installation in installations {
                let before = snapshot(installation.path());
                let opened = open(&installation, gate);
                let marked = matches!(
                    ownership_marker::inspect(installation.path()),
                    MarkerState::Valid(_)
                );
                // Compare bytes before reading the schema: a reader of a WAL
                // database may checkpoint it.
                match &opened {
                    Ok(()) => assert!(marked && gate == WriterGate::SwitchOn),
                    Err(_) => assert_unchanged(&installation, &before),
                }
                let version = schema_version(&installation.database_path());
                assert!(
                    version.is_none_or(|version| version <= NODE_CEILING || marked),
                    "{gate:?} {:?}: unmarked schema {version:?}",
                    installation.spec()
                );
            }
        }
    }

    #[test]
    fn rust_writes_nothing_while_a_node_lease_is_held() {
        let installation = Installation::create(Fixture::Empty).unwrap();
        let lease = installation.path().join(".desktop-lease");
        std::fs::create_dir(&lease).unwrap();
        std::fs::write(lease.join("owner.json"), live_owner_record()).unwrap();
        std::fs::write(
            installation.path().join(".desktop-owner.json"),
            live_owner_record(),
        )
        .unwrap();
        for gate in [WriterGate::SwitchOn, WriterGate::SwitchOff] {
            let before = snapshot(installation.path());
            let error = open(&installation, gate).unwrap_err();
            assert!(
                format!("{error:#}").contains("standalone Node"),
                "{error:#}"
            );
            assert_eq!(
                snapshot(installation.path()),
                before,
                "no lock file, no marker"
            );
        }
        // The Node leftover shape blocks Rust the same way.
        let leftovers = Installation::create(Fixture::EmptyWithLeaseLeftovers).unwrap();
        let before = snapshot(leftovers.path());
        assert!(open(&leftovers, WriterGate::SwitchOn).is_err());
        assert_eq!(snapshot(leftovers.path()), before);
    }

    #[test]
    fn rust_writes_no_marker_while_another_rust_lease_is_held() {
        let installation = Installation::create(Fixture::Empty).unwrap();
        let held = StorageLease::acquire(installation.path()).unwrap();
        let before = snapshot(installation.path());
        let error = open(&installation, WriterGate::SwitchOn).unwrap_err();
        assert!(format!("{error:#}").contains("already owned"), "{error:#}");
        assert_eq!(snapshot(installation.path()), before);
        drop(held);
        open(&installation, WriterGate::SwitchOn).unwrap();
        assert!(matches!(
            ownership_marker::inspect(installation.path()),
            MarkerState::Valid(_)
        ));
    }

    #[test]
    fn valid_marker_opens_and_keeps_its_bytes() {
        let installation = Installation::create(Spec {
            node_database: true,
            marker: KitMarker::Valid,
            ..Spec::default()
        })
        .unwrap();
        let marker = std::fs::read(installation.marker_path()).unwrap();
        open(&installation, WriterGate::SwitchOn).unwrap();
        assert_eq!(std::fs::read(installation.marker_path()).unwrap(), marker);
        assert!(schema_version(&installation.database_path()).unwrap() > NODE_CEILING);
    }
}
