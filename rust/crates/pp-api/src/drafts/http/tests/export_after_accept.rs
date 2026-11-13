//! The HTML checklist (GRE-1447) follows a new Accept: the next download
//! states the new Plan version and is named with it.
use super::*;

/// The checklist read from `server`'s storage through the export router on
/// its own loopback listener: status, `Content-Disposition` and page.
async fn checklist(server: &ReadServer) -> (u16, String, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let config =
        crate::accepted::AcceptedHttpConfig::from_startup(&origin, "default", Some("1")).unwrap();
    let router = crate::exports::exports_router(config, server.owner.accepted_reads().unwrap());
    let mut serving = crate::serving::Serving::start(listener, router);
    let response = server
        .client
        .get(format!("{origin}/api/v2/plans/1/exports/checklist"))
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
    let page = response.text().await.unwrap();
    serving.stop().await.unwrap();
    (status, name, page)
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
