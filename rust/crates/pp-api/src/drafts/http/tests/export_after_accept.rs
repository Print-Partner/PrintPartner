//! Exports follow a new Accept: the next HTML checklist (GRE-1447) states the
//! new Plan version, and the next STL pack (GRE-1525) is named with it and
//! holds the new quantities.
use super::*;

/// An export read from `server`'s storage through the export router on its
/// own loopback listener: status, `Content-Disposition` and body.
async fn download(server: &ReadServer, export: &str) -> (u16, String, Vec<u8>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let config =
        crate::accepted::AcceptedHttpConfig::from_startup(&origin, "default", Some("1")).unwrap();
    let router = crate::exports::exports_router(config, server.owner.accepted_reads().unwrap());
    let mut serving = crate::serving::Serving::start(listener, router);
    let response = server
        .client
        .get(format!("{origin}/api/v2/plans/1/exports/{export}"))
        .header("Origin", &origin)
        .header("Cookie", FIXTURE_COOKIE)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let name = response
        .headers()
        .get("content-disposition")
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let body = response.bytes().await.unwrap().to_vec();
    serving.stop().await.unwrap();
    (status, name, body)
}

async fn checklist(server: &ReadServer) -> (u16, String, String) {
    let (status, name, body) = download(server, "checklist").await;
    (status, name, String::from_utf8(body).unwrap())
}

#[tokio::test]
async fn gre_1447_checklist_states_the_version_a_new_accept_makes() {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    let (status, name, page) = checklist(&server).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(
        name,
        "attachment; filename=\"fixture-build-v1-checklist.html\""
    );
    assert!(page.contains("<dt>Plan version</dt><dd>1</dd>"), "{page}");

    let apply = keyed(&server, "/api/v2/plans/1/drafts/2/apply", Some("export-1"));
    json_reply(apply.json(&apply_body(DIGEST, 0)), 200).await;
    let (status, name, page) = checklist(&server).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(
        name,
        "attachment; filename=\"fixture-build-v2-checklist.html\""
    );
    assert!(page.contains("<dt>Plan version</dt><dd>2</dd>"), "{page}");
    server.close().await;
}

#[tokio::test]
async fn gre_1525_pack_holds_the_quantities_a_new_accept_makes() {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    // The bytes the fixture's accepted SHA-256 digests pin.
    let files = [
        ("bracket.stl", "solid bracket"),
        ("excluded.stl", "solid excluded"),
        ("gear.stl", "solid gear"),
    ];
    for (name, text) in files {
        std::fs::write(
            server
                .directory
                .join("repos/1/revisions/fixture")
                .join(name),
            text,
        )
        .unwrap();
    }
    // bracket.stl down to one unit, excluded.stl included.
    let parts = server.request(Method::PATCH, "/api/v2/plans/1/drafts/2/parts");
    let edited = json_reply(parts.json(&edit(DIGEST)), 200).await;
    let digest = edited["draft"]["snapshot_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    let apply = keyed(&server, "/api/v2/plans/1/drafts/2/apply", Some("pack-1"));
    json_reply(apply.json(&apply_body(&digest, 0)), 200).await;

    let (status, name, body) = download(&server, "stl-pack").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(name, "attachment; filename=\"fixture-build-v2-stl.zip\"");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(body)).unwrap();
    let mut entries = Vec::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut entry, &mut text).unwrap();
        entries.push((entry.name().unwrap().to_string(), text));
    }
    entries.sort();
    let expected: Vec<(String, String)> = [
        ("bracket_01", "solid bracket"),
        ("excluded_01", "solid excluded"),
        ("gear_01", "solid gear"),
        ("gear_02", "solid gear"),
    ]
    .iter()
    .map(|(stem, text)| (format!("primary/_root/{stem}.stl"), (*text).to_string()))
    .collect();
    assert_eq!(entries, expected);
    server.close().await;
}
