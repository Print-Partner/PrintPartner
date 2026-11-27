use super::{
    super::web_discovery::{Discovery, Error, Hit, Page, Read},
    reads::{assert_needs_key, session, tool},
    *,
};

struct Fake {
    search: bool,
}

impl Discovery for Fake {
    fn has_search(&self) -> bool {
        self.search
    }

    fn search<'a>(&'a self, query: &'a str, site: Option<&'a str>) -> Read<'a, Vec<Hit>> {
        Box::pin(async move {
            assert!(self.search);
            assert_eq!(query, "kit docs");
            assert_eq!(site, Some("docs.example.com"));
            Ok(vec![Hit {
                title: "Kit".into(),
                url: "https://docs.example.com/kit".into(),
                snippet: "Remote snippet".into(),
            }])
        })
    }

    fn fetch<'a>(&'a self, url: &'a str) -> Read<'a, Page> {
        Box::pin(async move {
            match url {
                "https://docs.example.com/timeout" => Err(Error::Timeout),
                "https://docs.example.com/kit" => Ok(Page {
                    url: "https://docs.example.com/guide".into(),
                    title: None,
                    text: "Remote text".into(),
                    truncated: false,
                    links: Vec::new(),
                }),
                "https://docs.example.com/large" => Ok(Page {
                    url: url.into(),
                    title: None,
                    text: "Truncated".into(),
                    truncated: true,
                    links: Vec::new(),
                }),
                "https://docs.example.com/unsupported" => Err(Error::ContentType),
                "https://docs.example.com/denied" => Err(Error::Destination),
                "https://docs.example.com/readme" => Ok(Page {
                    url: "https://docs.example.com/guide#setup".into(),
                    title: None,
                    text: "Base: Fixture printer\nRequired add-ons: Test probe".into(),
                    truncated: false,
                    links: vec!["https://github.com/example/probe".into()],
                }),
                _ => panic!("invalid input reached web adapter"),
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_lists_opted_in_tools_and_refuses_disabled_calls() {
    for (discovery, expected) in [
        (None, [false, false]),
        (Some(Fake { search: false }), [false, true]),
        (Some(Fake { search: true }), [true, true]),
    ] {
        let server = McpServer::configured(Some("1"), "default", |mut config| {
            config.web_discovery = discovery.map(|fake| Arc::new(fake) as Arc<dyn Discovery>);
            config
        })
        .await;
        let headers = session(&server, &format!("Bearer {}", server.create_key().await.1)).await;
        let names = server
            .tool_names(
                &headers
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect::<Vec<_>>(),
            )
            .await;
        for (name, enabled) in [
            ("web_search", expected[0]),
            ("fetch_web_page", expected[1]),
            ("ingest_guide_url", expected[1]),
            ("ingest_guide_text", true),
            ("import_guide_notes", true),
        ] {
            assert_eq!(names.iter().any(|listed| listed == name), enabled, "{name}");
            if !enabled {
                let (result, refused) = tool(&server, &headers, name, json!({})).await;
                assert!(refused);
                assert_eq!(result["code"], "disabled");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guide_intake_is_bounded_and_keeps_untrusted_provenance_without_writing() {
    let server = McpServer::configured(Some("1"), "default", |mut config| {
        config.web_discovery = Some(Arc::new(Fake { search: false }));
        config
    })
    .await;
    let headers = session(&server, &format!("Bearer {}", server.create_key().await.1)).await;
    let database = server._directory.join("print-partner.db");
    let before = pp_testkit::mcp_planning::state(&database).unwrap();
    let db = rusqlite::Connection::open(&database).unwrap();
    let counts = || {
        db.query_row(
            "SELECT (SELECT count(*) FROM source_notes), (SELECT count(*) FROM pending_actions)",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap()
    };
    let before_counts = counts();
    let paste = json!({"text": format!(
        "Printer: Fixture printer\nInstall release `v2.1`\nReplace the spacer with a 2.5 mm spacer. Verify fit.\n**Replace the stock probe.** Verify fit before {}.\n{}",
        "installing more parts ".repeat(8), "é".repeat(7_000)
    )});
    assert_needs_key(&server, "ingest_guide_text", paste.clone()).await;
    let (result, refused) = tool(&server, &headers, "ingest_guide_text", paste).await;
    assert!(!refused, "{result}");
    assert!(result.get("url").is_none());
    assert_eq!(result["truncated"], true);
    assert!(result["untrusted_text"].as_str().unwrap().len() <= 12_000);
    assert_eq!(result["extract"]["tags_or_refs"], json!(["v2.1"]));
    assert_eq!(
        result["extract"]["replacements"],
        json!([
            "Replace the spacer with a 2.5 mm spacer",
            "Replace the stock probe"
        ])
    );
    let readme = json!({"url":"https://docs.example.com/readme"});
    assert_needs_key(&server, "ingest_guide_url", readme.clone()).await;
    let (result, refused) = tool(&server, &headers, "ingest_guide_url", readme).await;
    assert!(!refused, "{result}");
    assert_eq!(result["url"], "https://docs.example.com/guide#setup");
    assert_eq!(
        result["extract"]["detected_printer_or_base"],
        "Fixture printer"
    );
    assert_eq!(result["extract"]["required_addons"], json!(["Test probe"]));
    assert_eq!(result["extract"]["links"][0]["kind"], "github");
    assert!(result["banner"].as_str().unwrap().starts_with("Untrusted"));
    for (name, args, code) in [
        ("ingest_guide_text", json!({"text":"  "}), "invalid_input"),
        (
            "ingest_guide_text",
            json!({"text":"Kit\u{0}README"}),
            "invalid_input",
        ),
        (
            "ingest_guide_text",
            json!({"text":"é".repeat(24_001)}),
            "input_too_large",
        ),
        ("ingest_guide_url", json!({"url":""}), "invalid_input"),
        (
            "ingest_guide_url",
            json!({"url":"https://docs.example.com/denied"}),
            "destination_denied",
        ),
        (
            "ingest_guide_url",
            json!({"url":"https://docs.example.com/unsupported"}),
            "unsupported_content",
        ),
        (
            "ingest_guide_url",
            json!({"url":"https://docs.example.com/large"}),
            "input_too_large",
        ),
    ] {
        let (result, refused) = tool(&server, &headers, name, args).await;
        assert!(refused, "{result}");
        assert_eq!(result["code"], code);
    }
    assert_eq!(before, pp_testkit::mcp_planning::state(&database).unwrap());
    assert_eq!(before_counts, counts());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_web_reads_preserve_provenance_without_storing_evidence() {
    let server = McpServer::configured(Some("1"), "default", |mut config| {
        config.web_discovery = Some(Arc::new(Fake { search: true }));
        config
    })
    .await;
    let headers = session(&server, &format!("Bearer {}", server.create_key().await.1)).await;
    let database = server._directory.join("print-partner.db");
    let before = pp_testkit::mcp_planning::state(&database).unwrap();
    for (name, args, result, url) in [
        (
            "web_search",
            json!({"query":" kit docs ", "site":" docs.example.com "}),
            "hits",
            "https://docs.example.com/kit",
        ),
        (
            "fetch_web_page",
            json!({"url":" https://docs.example.com/kit "}),
            "text",
            "https://docs.example.com/guide",
        ),
    ] {
        assert_needs_key(&server, name, args.clone()).await;
        let (payload, refused) = tool(&server, &headers, name, args).await;
        assert!(!refused, "{payload}");
        assert!(
            payload["untrusted_banner"]
                .as_str()
                .unwrap()
                .starts_with("Untrusted")
        );
        if result == "hits" {
            assert_eq!(payload["hits"][0]["url"], url);
        } else {
            assert_eq!(payload["url"], url);
            assert_eq!(payload["text"], "Remote text");
        }
    }
    for (name, args, code) in [
        ("web_search", json!({"query":"  "}), "invalid_input"),
        (
            "web_search",
            json!({"query":"kit docs","site":"https://docs.example.com/path"}),
            "invalid_input",
        ),
        ("fetch_web_page", json!({"url":""}), "invalid_input"),
        (
            "fetch_web_page",
            json!({"url":"https://docs.example.com/timeout"}),
            "timeout",
        ),
    ] {
        let (payload, refused) = tool(&server, &headers, name, args).await;
        assert!(refused);
        assert_eq!(payload["code"], code);
    }
    assert_eq!(pp_testkit::mcp_planning::state(&database).unwrap(), before);
}
