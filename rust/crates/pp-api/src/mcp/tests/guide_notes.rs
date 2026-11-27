use super::{
    super::guide::IMPORT,
    reads::{assert_needs_key, confirm, session, tool},
    *,
};

const SEED: &str = "
INSERT INTO projects (id, tenant_id, name, url) VALUES
  (101, 'default', 'Guide Note Fixture Source', 'https://example.com/kit'),
  (102, 'foreign', 'Guide Note Foreign Source', 'https://example.com/kit');
INSERT INTO source_notes (tenant_id, project_id, title, body_markdown, created_at, updated_at)
  VALUES ('foreign', 102, 'Guide: Guide Note Fixture Source', 'Untouched', '2026-01-01', '2026-01-01');
";

fn db(server: &McpServer) -> rusqlite::Connection {
    rusqlite::Connection::open(server._directory.join("print-partner.db")).unwrap()
}

fn notes(server: &McpServer) -> Vec<(i64, String, String)> {
    db(server)
        .prepare("SELECT project_id, title, body_markdown FROM source_notes WHERE project_id >= 101 ORDER BY project_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guide_note_requires_apply_and_keeps_source_and_url_provenance() {
    let server = McpServer::seeded(SEED).await;
    let bearer = format!("Bearer {}", server.create_key().await.1);
    let headers = session(&server, &bearer).await;
    let before = notes(&server);
    let args = json!({
        "source_name": "Guide Note Fixture Source", "body_markdown": "    first line\n    second line",
        "guide_url": "https://example.com/guide#setup",
    });
    assert_needs_key(&server, IMPORT, args.clone()).await;
    for (args, code) in [
        (
            json!({"source_name":"Guide Note Foreign Source", "body_markdown":"Note"}),
            "source_not_found",
        ),
        (
            json!({"source_name":"Guide Note Fixture Source", "body_markdown":"é".repeat(10_001)}),
            "invalid_proposal",
        ),
    ] {
        let (result, refused) = tool(&server, &headers, IMPORT, args).await;
        assert!(refused, "{result}");
        assert_eq!(result["code"], code);
    }
    let (proposal, refused) = tool(&server, &headers, IMPORT, args.clone()).await;
    assert!(!refused, "{proposal}");
    assert_eq!(notes(&server), before);
    let server = server.restart().await;
    let headers = session(&server, &bearer).await;
    let (result, refused) = confirm(&server, &headers, &proposal).await;
    assert!(!refused, "{result}");
    assert_eq!(
        result["result"],
        json!({
            "source_id": 101, "source_name": "Guide Note Fixture Source", "title": "Guide: Guide Note Fixture Source",
            "upserted": true,
        })
    );
    let saved = notes(&server);
    assert_eq!(saved.len(), 2);
    assert_eq!(
        saved[0].2,
        "    first line\n    second line\n\nGuide URL: https://example.com/guide#setup"
    );
    assert_eq!(saved[1], before[0]);
    let (proposal, refused) = tool(&server, &headers, IMPORT, args).await;
    assert!(!refused, "{proposal}");
    let (result, refused) = confirm(&server, &headers, &proposal).await;
    assert!(!refused, "{result}");
    assert_eq!(result["result"]["upserted"], false);
    assert_eq!(notes(&server), saved);

    let (proposal, refused) = tool(
        &server,
        &headers,
        IMPORT,
        json!({"source_name": "Guide Note Fixture Source", "body_markdown": "New notes"}),
    )
    .await;
    assert!(!refused, "{proposal}");
    db(&server)
        .execute("UPDATE projects SET name='Changed' WHERE id=101", [])
        .unwrap();
    let (result, refused) = confirm(&server, &headers, &proposal).await;
    assert!(refused, "{result}");
    assert_eq!(result["code"], "source_not_found");
    assert_eq!(notes(&server), saved);
}
