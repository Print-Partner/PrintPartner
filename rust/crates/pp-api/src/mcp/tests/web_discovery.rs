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
        for (name, enabled) in ["web_search", "fetch_web_page"].into_iter().zip(expected) {
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
