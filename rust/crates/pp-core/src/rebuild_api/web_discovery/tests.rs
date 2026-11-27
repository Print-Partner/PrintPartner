use super::*;
use axum::http;
use pp_net::outbound::{Resolution, Resolver};

fn content_type(value: &'static str) -> PageContent {
    PageContent::from_header(Some(&header::HeaderValue::from_static(value))).unwrap()
}

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

#[tokio::test]
async fn published_page_url_keeps_the_redirect_anchor_without_sending_it() {
    let (url, _) = follow_page("https://docs.example.com/start#original", |url| {
        assert!(url.fragment().is_none());
        let response = http::Response::builder();
        let response = match url.path() {
            "/start" => response.status(302).header(header::LOCATION, "/manual"),
            "/manual" => response
                .status(302)
                .header(header::LOCATION, "/guide#steps"),
            "/guide" => response.status(302).header(header::LOCATION, "/final"),
            "/final" => response.status(200),
            _ => panic!("unexpected outbound hop"),
        };
        std::future::ready(Ok(response.body("").unwrap().into()))
    })
    .await
    .unwrap();
    assert_eq!(url.as_str(), "https://docs.example.com/final#steps");
}

#[test]
fn bounds_and_attributes_remote_text() {
    let hits = search_hits(
        &serde_json::to_vec(&serde_json::json!({
            "web": {"results": (0..MAX_HITS + 3).map(|n| serde_json::json!({
                "url": format!("https://docs.example.com/{n}#section"),
                "title": "é".repeat(400),
                "description": "snippet"
            })).collect::<Vec<_>>()}
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(hits.len(), MAX_HITS);
    assert_eq!(hits[0].url, "https://docs.example.com/0#section");
    assert_eq!(hits[0].title.len(), 300);
    assert_eq!(hits[0].snippet, "snippet");
    assert_eq!(search_hits(b"<html>").err(), Some(Error::Unavailable));

    let page = page_text(
        hits[0].url.clone(),
        b"<title>Kit &amp; Guide</title><script>omit</script><p>Step one</p><p>Step two</p>",
        content_type("text/html"),
    )
    .unwrap();
    assert_eq!(page.title.as_deref(), Some("Kit & Guide"));
    assert_eq!(page.text, "Step one\n\nStep two");
    let page = page_text(
        page.url,
        "é".repeat(MAX_TEXT).as_bytes(),
        content_type("text/plain"),
    )
    .unwrap();
    assert_eq!(page.text.len(), MAX_TEXT);
    assert!(page.truncated);
}

#[test]
fn decodes_declared_page_charsets_without_corrupting_evidence() {
    for (content_type, prefix) in [
        ("text/html; charset=\"windows-1252\"", ""),
        ("text/html", "<meta charset=windows-1252>"),
        (
            "text/html",
            "<meta http-equiv=content-type content=\"text/html; charset=windows-1252\">",
        ),
    ] {
        let mut bytes = prefix.as_bytes().to_vec();
        bytes.extend_from_slice(b"<title>Caf\xe9</title><p>Price: \x801</p>");
        let content =
            PageContent::from_header(Some(&header::HeaderValue::from_static(content_type)))
                .unwrap();
        let page = page_text(String::new(), &bytes, content).unwrap();
        assert_eq!(page.title.as_deref(), Some("Café"));
        assert_eq!(page.text, "Price: €1");
    }
    assert_eq!(
        content_type("text/plain")
            .decode(b"\xff\xfec\x00a\x00f\x00\xe9\x00")
            .unwrap(),
        "café"
    );
    assert_eq!(
        content_type("text/plain").decode(b"Caf\xe9"),
        Err(Error::Encoding)
    );
}

#[test]
fn decodes_content_type_parameters_with_optional_whitespace_and_quoted_values() {
    for value in [
        "Text/Plain \t; charset=utf-8",
        r#"text/plain; charset="UTF-8"; version=1"#,
        r#"text/plain; note="ignore; charset=bogus; \""; charset="UTF\-8""#,
    ] {
        assert_eq!(
            content_type(value).decode("Café".as_bytes()).unwrap(),
            "Café"
        );
    }
}

#[test]
fn xhtml_uses_xml_encoding_when_no_http_charset_is_declared() {
    let body = b"<?xml version='1.0' encoding='windows-1252'?><html><title>Caf\xe9</title><body>\x801</body></html>";
    let page = page_text(String::new(), body, content_type("application/xhtml+xml")).unwrap();
    assert_eq!(page.title.as_deref(), Some("Café"));
    assert_eq!(page.text, "€1");
    assert_eq!(
        content_type("application/xhtml+xml; charset=utf-8").decode(body),
        Err(Error::Encoding)
    );
}

#[tokio::test(start_paused = true)]
async fn fake_responses_obey_body_limit_deadline_and_capacity() {
    let response = http::Response::builder()
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(vec![b'x'; MAX_BODY + 1])
        .unwrap()
        .into();
    assert!(!content_type("text/plain; charset=utf-8").is_markup());
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
