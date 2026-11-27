use super::*;
use axum::http;
use pp_net::outbound::{Resolution, Resolver};

#[test]
fn accepts_public_web_urls_and_rechecks_each_redirect() {
    for url in [
        "https://docs.example.com/kit",
        "http://docs.example.com:80/kit",
        "https://[2606:4700::1111]/",
    ] {
        assert!(public_url(url).is_ok(), "{url}");
    }
    for url in [
        "file:///kit",
        "https://docs.example.com:9000/",
        "https://someone:secret@docs.example.com/",
        "https://localhost/",
        "http://127.0.0.1/",
        "http://192.168.0.10/",
        "http://169.254.0.10/",
        "http://[fc00::1]/",
        "http://[::ffff:127.0.0.1]/",
    ] {
        assert_eq!(public_url(url), Err(Error::Destination), "{url}");
    }
    let from = public_url("https://docs.example.com/start").unwrap();
    let target = |location: &'static str| {
        redirect_target(&from, &header::HeaderValue::from_static(location))
    };
    assert_eq!(
        target("/guide").unwrap().as_str(),
        "https://docs.example.com/guide"
    );
    assert_eq!(
        target("https://manuals.example.com/").unwrap().host_str(),
        Some("manuals.example.com")
    );
    for location in ["http://docs.example.com", "//127.0.0.1/", "//[fe80::1]/"] {
        assert_eq!(target(location), Err(Error::Destination));
    }
}

struct MixedDns;

impl Resolver for MixedDns {
    fn resolve(&self, _: String) -> Resolution {
        Box::pin(async { Ok(vec!["8.8.8.8".parse()?, "10.0.0.1".parse()?]) })
    }
}

#[test]
fn search_requires_explicit_provider_and_sends_key_only_to_brave() {
    assert!(brave_key(|_| Some("fixture-key".into())).is_none());
    let key = brave_key(|name| match name {
        "SEARCH_PROVIDER" => Some("brave".into()),
        "BRAVE_API_KEY" => Some(" fixture-key ".into()),
        _ => None,
    })
    .unwrap();
    let discovery = WebDiscovery {
        client: Client::new(),
        brave_key: Some(key),
        limit: Semaphore::new(1),
    };
    let request = discovery
        .brave_request("kit & guide", Some("docs.example.com"))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(request.url().host_str(), Some("api.search.brave.com"));
    assert_eq!(
        request
            .url()
            .query_pairs()
            .find(|(key, _)| key == "q")
            .unwrap()
            .1,
        "site:docs.example.com kit & guide"
    );
    let key = &request.headers()["x-subscription-token"];
    assert_eq!(key, "fixture-key");
    assert!(key.is_sensitive());
}

#[tokio::test]
async fn checked_resolver_refuses_mixed_public_private_answers_before_connecting() {
    use reqwest::dns::{Name, Resolve};
    let dns = CheckedDns::new(Arc::new(MixedDns), Policy::Public, "fixture denied");
    assert!(
        dns.resolve("docs.example.com".parse::<Name>().unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn fake_redirects_are_checked_before_the_next_request() {
    use std::cell::Cell;
    let calls = Cell::new(0);
    let result = follow_page("https://docs.example.com/start", |url| {
        calls.set(calls.get() + 1);
        let location = match url.path() {
            "/start" => "/guide",
            "/guide" => "https://127.0.0.1/",
            _ => panic!("unexpected outbound hop"),
        };
        std::future::ready(Ok(http::Response::builder()
            .status(302)
            .header(header::LOCATION, location)
            .body("")
            .unwrap()
            .into()))
    })
    .await;
    assert_eq!(calls.get(), 2);
    assert_eq!(result.err(), Some(Error::Destination));
}

#[test]
fn bounds_and_attributes_remote_text() {
    let hits = search_hits(
        &serde_json::to_vec(&serde_json::json!({
            "web": {"results": (0..MAX_HITS + 3).map(|n| serde_json::json!({
                "url": format!("https://docs.example.com/{n}"),
                "title": "é".repeat(400),
                "description": "snippet"
            })).collect::<Vec<_>>()}
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(hits.len(), MAX_HITS);
    assert_eq!(hits[0].url, "https://docs.example.com/0");
    assert_eq!(hits[0].title.len(), 300);
    assert_eq!(hits[0].snippet, "snippet");
    assert_eq!(search_hits(b"<html>").err(), Some(Error::Unavailable));

    let page = page_text(
        hits[0].url.clone(),
        b"<title>Kit &amp; Guide</title><script>omit</script><p>Step one</p><p>Step two</p>",
        true,
    );
    assert_eq!(page.title.as_deref(), Some("Kit & Guide"));
    assert_eq!(page.text, "Step one\n\nStep two");
    let page = page_text(page.url, "é".repeat(MAX_TEXT).as_bytes(), false);
    assert_eq!(page.text.len(), MAX_TEXT);
    assert!(page.truncated);
}

#[tokio::test(start_paused = true)]
async fn fake_responses_obey_body_limit_deadline_and_capacity() {
    let response = http::Response::builder()
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(vec![b'x'; MAX_BODY + 1])
        .unwrap()
        .into();
    assert_eq!(html_type(&response), Ok(false));
    assert_eq!(body(response).await, Err(Error::TooLarge));
    let response = http::Response::builder()
        .status(503)
        .body("unavailable")
        .unwrap()
        .into();
    assert_eq!(success(response).err(), Some(Error::Unavailable));
    let discovery = WebDiscovery {
        client: Client::new(),
        brave_key: None,
        limit: Semaphore::new(1),
    };
    assert_eq!(
        discovery
            .bounded(std::future::pending::<Result<(), Error>>())
            .await,
        Err(Error::Timeout)
    );
    let _permit = discovery.limit.acquire().await.unwrap();
    assert_eq!(discovery.bounded(async { Ok(()) }).await, Err(Error::Busy));
}
