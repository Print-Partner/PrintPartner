#![cfg(test)]

use super::*;
use crate::tests::platform;
use pp_storage::read_model::{Input, Part, Profile};
use serde_json::json;

struct Fixture {
    root: PathBuf,
    snapshot: Snapshot,
    observer: SnapshotReviewObserver,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-review-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let repos = root.join("repos");
        let source = repos.join("tenant/source/revision");
        fs::create_dir_all(&source).unwrap();
        // ast-grep-ignore: no-writer-family-literal -- Test fixture stands in for Node-rendered thumbnails.
        fs::create_dir_all(root.join("thumbs")).unwrap();
        fs::write(source.join("part.stl"), b"solid test").unwrap();
        let snapshot = Snapshot {
            format: "accepted-build-v1",
            profile: Profile {
                id: 1,
                name: "test".into(),
                order_number: None,
                special_request: None,
                archived_at: None,
            },
            plan_version: 1,
            revision_id: 1,
            revision_number: 1,
            revision_digest: "digest".into(),
            accepted_at: "now".into(),
            required_unit_mapping_digest: "mapping".into(),
            provenance: Provenance::Tracked {
                input_set_id: 1,
                input_set_digest: "input".into(),
                inputs: vec![Input {
                    input_id: 7,
                    source_id: 1,
                    source_layer: "source".into(),
                    layer_order: 0,
                    effective_naming_digest: "name".into(),
                    tracking_kind: "revision".into(),
                    source_revision_id: Some(1),
                    manifest_digest: Some("manifest".into()),
                    snapshot_root: Some(source.to_str().unwrap().into()),
                    snapshot_path: Some(source.to_str().unwrap().into()),
                    source_synced_at: None,
                }],
            },
            parts: vec![Part {
                revision_part_id: 1,
                projection_part_id: 9,
                part_key: "part".into(),
                relative_path: "part.stl".into(),
                filename: "part.stl".into(),
                source_layer: "source".into(),
                status: "ready".into(),
                role_inferred: "body".into(),
                role_override: None,
                effective_role: "body".into(),
                filament_color_id: None,
                filament_custom_hex: None,
                spoolman_spool_id: None,
                quantity_inferred: 1,
                quantity_override: None,
                quantity_effective: 1,
                included: true,
                notes: String::new(),
                github_blob_url: None,
                geometry_same: None,
                requirement: None,
                option_group_id: None,
                manifest_source: None,
                manifest: Default::default(),
                artifact: Artifact::Tracked {
                    source_id: 1,
                    source_revision_id: 1,
                    snapshot_root: source.to_str().unwrap().into(),
                    snapshot_path: source.to_str().unwrap().into(),
                    relative_path: "part.stl".into(),
                    expected_sha256:
                        "4e6931576efead51995a7cd8d36d2ef2aae3d1bf36e6f1827c71e32dde079ba7".into(),
                },
                units: vec![],
            }],
        };
        // ast-grep-ignore: no-writer-family-literal -- Test fixture stands in for Node-rendered thumbnails.
        let observer = SnapshotReviewObserver::new(repos, Some(root.join("thumbs"))).unwrap();
        Self {
            root,
            snapshot,
            observer,
        }
    }
    fn source(&self) -> PathBuf {
        self.root.join("repos/tenant/source/revision")
    }
    fn observe(&self) -> ReviewObservations {
        self.observer
            .review_observations(&self.snapshot, &AtomicBool::new(false))
            .unwrap()
    }
    fn artifact_mut(&mut self) -> (&mut String, &mut String, &mut String) {
        let Artifact::Tracked {
            snapshot_root,
            relative_path,
            expected_sha256,
            ..
        } = &mut self.snapshot.parts[0].artifact
        else {
            panic!("tracked fixture")
        };
        (snapshot_root, relative_path, expected_sha256)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn review_reports_artifacts_thumbnails_and_included_parts() {
    let mut f = Fixture::new();
    let observed = f.observe();
    assert_eq!(observed.available_input_roots, HashSet::from([7]));
    assert!(!observed.media_by_part_id[&9].artifact_missing);
    assert!(observed.media_by_part_id[&9].thumb_empty);
    let digest = &f.artifact_mut().2.clone();
    let name = hex::encode(Sha256::digest(format!(
        "accepted-thumbnail-v3\0thumbnail\0{digest}\0body\0"
    )));
    fs::write(
        // ast-grep-ignore: no-writer-family-literal -- Test fixture stands in for Node-rendered thumbnails.
        f.root.join("thumbs").join(format!("{name}.png")),
        [137, 80, 78, 71, 13, 10, 26, 10],
    )
    .unwrap();
    assert!(!f.observe().media_by_part_id[&9].thumb_empty);
    fs::write(f.source().join("part.stl"), "changed").unwrap();
    let observed = f.observe();
    assert!(observed.media_by_part_id[&9].artifact_missing);
    assert!(!observed.media_by_part_id[&9].thumb_empty);
    f.snapshot.parts[0].effective_role = "ÜPPER".into();
    fs::write(f.source().join("part.stl"), "solid test").unwrap();
    let name = hex::encode(Sha256::digest(format!(
        "accepted-thumbnail-v3\0thumbnail\0{digest}\0üpper\0"
    )));
    fs::write(
        // ast-grep-ignore: no-writer-family-literal -- Test fixture stands in for Node-rendered thumbnails.
        f.root.join("thumbs").join(format!("{name}.png")),
        [137, 80, 78, 71, 13, 10, 26, 10],
    )
    .unwrap();
    for custom in ["abcdef", "##abcdef"] {
        f.snapshot.parts[0].filament_custom_hex = Some(custom.into());
        assert!(!f.observe().media_by_part_id[&9].thumb_empty);
    }
    f.snapshot.parts[0].included = false;
    assert!(f.observe().media_by_part_id.is_empty());
}

#[test]
fn review_matches_unique_case_insensitive_paths() {
    let mut f = Fixture::new();
    *f.artifact_mut().1 = "PART.STL".into();
    assert!(!f.observe().media_by_part_id[&9].artifact_missing);
    fs::write(f.source().join("PART.STL"), "solid test").unwrap();
    if fs::read_dir(f.source()).unwrap().count() == 2 {
        assert!(f.observe().media_by_part_id[&9].artifact_missing);
    } else {
        assert!(!f.observe().media_by_part_id[&9].artifact_missing);
    }
}

#[test]
fn review_rejects_traversal_and_symlink_artifacts() {
    let mut f = Fixture::new();
    for path in [
        "../part.stl",
        "/part.stl",
        "a\\part.stl",
        "a:part.stl",
        "./part.stl",
    ] {
        *f.artifact_mut().1 = path.into();
        assert!(f.observe().media_by_part_id[&9].artifact_missing, "{path}");
    }
    *f.artifact_mut().1 = "part.stl".into();
    fs::remove_file(f.source().join("part.stl")).unwrap();
    fs::write(f.root.join("outside.stl"), "solid test").unwrap();
    platform::symlink_file(f.root.join("outside.stl"), f.source().join("part.stl")).unwrap();
    assert!(f.observe().media_by_part_id[&9].artifact_missing);
    fs::create_dir(f.root.join("outside")).unwrap();
    platform::symlink_dir(f.root.join("outside"), f.root.join("repos/link")).unwrap();
    *f.artifact_mut().0 = f.root.join("repos/link").to_str().unwrap().into();
    assert!(f.observe().media_by_part_id[&9].artifact_missing);
    *f.artifact_mut().0 = f.root.to_str().unwrap().into();
    assert!(f.observe().media_by_part_id[&9].artifact_missing);
}

#[test]
fn review_enforces_cancellation_and_shared_read_budgets() {
    let f = Fixture::new();
    let error = f
        .observer
        .review_observations(&f.snapshot, &AtomicBool::new(true))
        .unwrap_err();
    assert_eq!(error.to_string(), "Review observation cancelled");
    let observer = f
        .observer
        .clone()
        .with_observation_limits(ObservationLimits {
            artifact_bytes: 9,
            ..Default::default()
        })
        .unwrap();
    let error = observer
        .review_observations(&f.snapshot, &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Review observation artifact budget exceeded"
    );
    fs::write(f.source().join(pp_source::MANIFEST), "{}").unwrap();
    let observer = f
        .observer
        .clone()
        .with_observation_limits(ObservationLimits {
            entries: 1,
            ..Default::default()
        })
        .unwrap();
    assert!(
        !observer
            .review_observations(&f.snapshot, &AtomicBool::new(false))
            .unwrap()
            .media_by_part_id[&9]
            .artifact_missing
    );
    fs::write(f.source().join("other.stl"), "solid other").unwrap();
    let error = observer
        .review_observations(&f.snapshot, &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Review observation entry budget exceeded"
    );
    assert!(
        f.observer
            .clone()
            .with_observation_limits(ObservationLimits {
                elapsed: Duration::ZERO,
                ..Default::default()
            })
            .is_err()
    );
}

#[tokio::test]
async fn filament_provider_reads_local_http_and_falls_back_on_failure() {
    use axum::{Router, routing::get};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/api/v1/filament",
            get(|| async { axum::Json(json!({"results": [
                {"id":"2","name":"Red","vendor":{"name":"Maker"},"material":"PLA","color_hex":"FF0000"},
                {"id":-1,"name":"invalid"}
            ]})) }),
        )
        .route(
            "/api/v1/spool",
            get(|| async {
                axum::Json(json!([
                    {"id":3,"filament_id":2,"remaining_weight":"12.5"},
                    {"id":4,"filament":{"id":2},"remaining_weight":8},
                    {"id":5,"filament_id":2,"remaining_weight":9,"archived":true},
                    {"id":6,"filament_id":2,"remaining_weight":"NaN"}
                ]))
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut f = Fixture::new();
    f.snapshot.parts[0].filament_color_id = Some(" spoolman:local:filament:2 ".into());
    f.observer = f.observer.clone().with_filament_provider(
        FilamentProviderConfig::new(&format!("http://{address}/api/v1"), "local", None).unwrap(),
    );
    let lookup = f
        .observer
        .filament_lookup(&f.snapshot, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    let resolved = lookup
        .for_part(Some(" spoolman:local:filament:2 "), None)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.combo_label, "Maker PLA · Red");
    assert_eq!(resolved.hex.as_deref(), Some("#ff0000"));
    assert_eq!(
        resolved
            .spools
            .iter()
            .map(|s| (s.spool_id, s.remaining_g))
            .collect::<Vec<_>>(),
        vec![(3, 12.5), (4, 8.0)]
    );
    let selected = lookup
        .for_part(
            Some("spoolman:local:filament:2"),
            Some(" spoolman:local:spool:4 "),
        )
        .unwrap()
        .unwrap();
    assert_eq!(selected.spools[0].spool_id, 4);
    assert!(
        lookup
            .for_part(Some("spoolman:foreign:filament:2"), None)
            .unwrap()
            .is_none()
    );
    let error = f
        .observer
        .filament_lookup(&f.snapshot, Arc::new(AtomicBool::new(true)))
        .await
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "Filament observation cancelled");
    server.abort();
    let _ = server.await;
    let lookup = f
        .observer
        .filament_lookup(&f.snapshot, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(
        lookup
            .for_part(Some("spoolman:local:filament:2"), None)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn filament_http_bounds_redirects_origin_and_response_bytes() {
    use axum::{Router, http::StatusCode, response::Redirect, routing::get};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/api/v1/filament",
            get(|| async { Redirect::temporary("/rows") }),
        )
        .route("/rows", get(|| async { "[{\"id\":2,\"name\":\"Red\"}]" }))
        .route(
            "/foreign/api/v1/filament",
            get(|| async { Redirect::temporary("http://127.0.0.1:1/rows") }),
        )
        .route(
            "/error/api/v1/filament",
            get(|| async { StatusCode::UNAUTHORIZED }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let cancelled = AtomicBool::new(false);
    let config = FilamentProviderConfig::new(&format!("http://{address}"), "local", None).unwrap();
    assert_eq!(
        fetch_json(&client, &config, "filament", &cancelled)
            .await
            .unwrap(),
        json!([{"id":2,"name":"Red"}])
    );
    let limited = config
        .clone()
        .with_limits(Duration::from_secs(1), 4, 1)
        .unwrap();
    assert_eq!(
        fetch_json(&client, &limited, "filament", &cancelled)
            .await
            .unwrap_err()
            .to_string(),
        "Filament response too large"
    );
    let limited = config.with_limits(Duration::from_secs(1), 1024, 0).unwrap();
    assert_eq!(
        fetch_json(&client, &limited, "filament", &cancelled)
            .await
            .unwrap_err()
            .to_string(),
        "Too many filament redirects"
    );
    let foreign = FilamentProviderConfig::new(
        &format!("http://{address}/foreign"),
        "local",
        Some("secret".into()),
    )
    .unwrap();
    assert_eq!(
        fetch_json(&client, &foreign, "filament", &cancelled)
            .await
            .unwrap_err()
            .to_string(),
        "Filament redirect changed origin"
    );
    let error =
        FilamentProviderConfig::new(&format!("http://{address}/error"), "local", None).unwrap();
    assert_eq!(
        fetch_json(&client, &error, "filament", &cancelled)
            .await
            .unwrap_err()
            .to_string(),
        "Filament provider refused read"
    );
    server.abort();
    let _ = server.await;
}

#[test]
fn filament_config_rejects_ambiguous_urls_and_limit_expansion() {
    for url in [
        "file:///tmp/provider",
        "http://user@localhost",
        "http://localhost?query",
        "http://localhost#fragment",
        "http://169.254.169.254",
        "http://[fd00:ec2::254]",
        "http://[::ffff:169.254.169.254]",
    ] {
        assert!(
            FilamentProviderConfig::new(url, "local", None).is_err(),
            "{url}"
        );
    }
    assert!(FilamentProviderConfig::new("http://localhost", "bad:identity", None).is_err());
    assert!(FilamentProviderConfig::new("http://localhost", "local", Some(" ".into())).is_err());
    let config = FilamentProviderConfig::new("http://localhost", "local", None).unwrap();
    assert_eq!(
        config.endpoint("filament").as_str(),
        "http://localhost/api/v1/filament"
    );
    assert!(config.with_limits(Duration::from_secs(9), 1024, 1).is_err());
}

/// Mode bits only exist on Unix. Windows pins Source roots by handle; its
/// counterpart is the linked-root case in
/// `review_rejects_traversal_and_symlink_artifacts`.
#[cfg(unix)]
#[test]
fn review_regression_unpinned_roots_are_unavailable() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    fs::set_permissions(f.source(), fs::Permissions::from_mode(0o777)).unwrap();
    let observed = f.observe();
    assert!(observed.available_input_roots.is_empty());
    assert!(observed.media_by_part_id[&9].artifact_missing);
}

#[test]
fn review_regression_unicode_paths_match_and_collisions_are_missing() {
    let mut f = Fixture::new();
    fs::rename(f.source().join("part.stl"), f.source().join("CAFÉ.STL")).unwrap();
    *f.artifact_mut().1 = "café.stl".into();
    assert!(!f.observe().media_by_part_id[&9].artifact_missing);
    fs::write(f.source().join("cafe\u{301}.stl"), "solid test").unwrap();
    if fs::read_dir(f.source()).unwrap().count() == 2 {
        assert!(f.observe().media_by_part_id[&9].artifact_missing);
    } else {
        assert!(!f.observe().media_by_part_id[&9].artifact_missing);
    }
}

#[test]
fn review_regression_artifact_removal_after_scan_is_missing() {
    let f = Fixture::new();
    let cancelled = AtomicBool::new(false);
    let mut budget = ObservationBudget::new(&cancelled, ObservationLimits::default());
    let mut inventory = Inventory::scan(&f.source(), &mut budget).unwrap();
    fs::remove_file(f.source().join("part.stl")).unwrap();
    let Artifact::Tracked {
        expected_sha256, ..
    } = &f.snapshot.parts[0].artifact
    else {
        unreachable!()
    };
    assert!(artifact_missing(&mut inventory, "part.stl", expected_sha256, &mut budget).unwrap());
}

#[test]
fn review_regression_colorless_provider_uses_neutral_swatch() {
    let lookup = SnapshotFilamentLookup {
        integration_id: "local".into(),
        inventory: parse_inventory(
            &json!([
                {"id":2,"name":"Natural","color_hex":" "},
                {"id":3,"name":"Short","color_hex":"#abc"}
            ]),
            &json!([]),
        ),
    };
    assert_eq!(
        lookup
            .for_part(Some("spoolman:local:filament:2"), None)
            .unwrap()
            .unwrap()
            .hex
            .as_deref(),
        Some("#888888")
    );
    assert_eq!(
        lookup
            .for_part(Some("spoolman:local:filament:3"), None)
            .unwrap()
            .unwrap()
            .hex
            .as_deref(),
        Some("#abc")
    );
}

#[tokio::test]
async fn review_regression_provider_deadline_covers_redirects() {
    use axum::{Router, response::Redirect, routing::get};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/api/v1/filament",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(70)).await;
                Redirect::temporary("/rows")
            }),
        )
        .route(
            "/rows",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(70)).await;
                "[]"
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = FilamentProviderConfig::new(&format!("http://{address}"), "local", None)
        .unwrap()
        .with_limits(Duration::from_millis(100), 1024, 1)
        .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(config.timeout)
        .build()
        .unwrap();
    let result = fetch_json(&client, &config, "filament", &AtomicBool::new(false)).await;
    server.abort();
    let _ = server.await;
    assert_eq!(
        result.unwrap_err().to_string(),
        "Filament observation timed out"
    );
}

#[tokio::test]
async fn review_cancelled_no_provider_is_an_error() {
    let f = Fixture::new();
    assert_eq!(
        f.observer
            .filament_lookup(&f.snapshot, Arc::new(AtomicBool::new(true)))
            .await
            .err()
            .unwrap()
            .to_string(),
        "Filament observation cancelled"
    );
}

#[tokio::test]
async fn filament_dns_rejects_metadata_answers_and_pins_lan_answers() {
    use pp_net::outbound::{Resolution, Resolver};
    use reqwest::dns::Resolve;
    struct Answers(Vec<std::net::IpAddr>);
    impl Resolver for Answers {
        fn resolve(&self, _: String) -> Resolution {
            let answers = self.0.clone();
            Box::pin(async move { Ok(answers) })
        }
    }
    let allowed = "127.0.0.1".parse().unwrap();
    let denied = "169.254.169.254".parse().unwrap();
    let dns = FilamentDns(Arc::new(Answers(vec![allowed, denied])));
    assert_eq!(
        dns.resolve("example.test".parse().unwrap())
            .await
            .err()
            .unwrap()
            .to_string(),
        "Prohibited filament DNS answer"
    );
    let dns = FilamentDns(Arc::new(Answers(vec![allowed])));
    let addresses = dns
        .resolve("example.test".parse().unwrap())
        .await
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(addresses, vec!["127.0.0.1:0".parse().unwrap()]);
}

#[test]
fn review_rejects_ambiguous_directory_prefixes() {
    let mut f = Fixture::new();
    fs::remove_file(f.source().join("part.stl")).unwrap();
    fs::create_dir_all(f.source().join("A")).unwrap();
    fs::create_dir_all(f.source().join("a")).unwrap();
    fs::write(f.source().join("A/x.stl"), "solid test").unwrap();
    fs::write(f.source().join("a/y.stl"), "other").unwrap();
    *f.artifact_mut().1 = "a/x.stl".into();
    if fs::read_dir(f.source()).unwrap().count() == 2 {
        assert!(f.observe().media_by_part_id[&9].artifact_missing);
    } else {
        assert!(!f.observe().media_by_part_id[&9].artifact_missing);
    }
}

#[test]
fn review_preserves_unicode16_persisted_path_identity() {
    let mut f = Fixture::new();
    fs::remove_file(f.source().join("part.stl")).unwrap();
    let legacy = "a\u{1ADD}\u{0300}.stl";
    let composed = "\u{00E0}\u{1ADD}.stl";
    fs::write(f.source().join(legacy), "solid test").unwrap();
    fs::write(f.source().join(composed), "other").unwrap();
    *f.artifact_mut().1 = legacy.into();
    if fs::read_dir(f.source()).unwrap().count() == 2 {
        assert!(!f.observe().media_by_part_id[&9].artifact_missing);
        *f.artifact_mut().1 = composed.into();
        *f.artifact_mut().2 = hex::encode(Sha256::digest(b"other"));
        assert!(!f.observe().media_by_part_id[&9].artifact_missing);
    } else {
        assert!(f.observe().media_by_part_id[&9].artifact_missing);
    }
}

/// Windows counterpart: `review_never_reads_an_alternate_data_stream`.
#[cfg(unix)]
#[test]
fn review_reads_accepted_unix_colon_names() {
    let mut f = Fixture::new();
    fs::create_dir(f.source().join("parts")).unwrap();
    fs::rename(
        f.source().join("part.stl"),
        f.source().join("parts/body:v2.stl"),
    )
    .unwrap();
    *f.artifact_mut().1 = "parts/body:v2.stl".into();
    assert!(!f.observe().media_by_part_id[&9].artifact_missing);
}

/// NTFS reads `body:v2.stl` as the stream `v2.stl` of the file `body`, so
/// a colon name is missing on Windows instead of naming that stream.
#[cfg(windows)]
#[test]
fn review_never_reads_an_alternate_data_stream() {
    let mut f = Fixture::new();
    fs::create_dir(f.source().join("parts")).unwrap();
    fs::rename(f.source().join("part.stl"), f.source().join("parts/body")).unwrap();
    fs::write(f.source().join("parts/body:v2.stl"), "solid test").unwrap();
    *f.artifact_mut().1 = "parts/body:v2.stl".into();
    assert!(f.observe().media_by_part_id[&9].artifact_missing);
}

#[test]
fn review_cancelled_empty_snapshot_is_an_error() {
    let mut f = Fixture::new();
    f.snapshot.provenance = Provenance::Legacy;
    f.snapshot.parts.clear();
    assert_eq!(
        f.observer
            .review_observations(&f.snapshot, &AtomicBool::new(true))
            .unwrap_err()
            .to_string(),
        "Review observation cancelled"
    );
}
