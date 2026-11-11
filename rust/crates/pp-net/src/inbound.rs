//! Admission for an explicitly enabled IP-address HTTP listener.
use anyhow::{Result, ensure};
use axum::http::{Method, Request};
use std::net::IpAddr;

/// Validate the browser-visible authority, including a published Docker port.
/// DNS names and forwarded authority are outside this listener's contract.
pub fn origin<B>(request: &Request<B>) -> Result<(String, String)> {
    let headers = request.headers();
    ensure!(headers.get_all("host").iter().count() == 1, "Invalid Host");
    let host = headers["host"].to_str()?;
    let authority: axum::http::uri::Authority = host.parse()?;
    let ip: IpAddr = authority.host().trim_matches(['[', ']']).parse()?;
    ensure!(!ip.is_unspecified() && !ip.is_multicast(), "Invalid Host");
    ensure!(
        authority.port().is_none() || authority.port_u16().is_some(),
        "Invalid port"
    );
    ensure!(!host.contains('@'), "Invalid Host");
    ensure!(
        request
            .uri()
            .authority()
            .is_none_or(|value| value.as_str() == host),
        "Invalid authority"
    );
    ensure!(
        !headers
            .keys()
            .any(|key| key == "forwarded" || key.as_str().starts_with("x-forwarded-")),
        "Forwarded authority is unsupported"
    );
    let origin = format!("http://{host}");
    let presented = headers.get("origin");
    ensure!(
        headers.get_all("origin").iter().count() <= 1
            && presented.is_none_or(|value| value.as_bytes() == origin.as_bytes()),
        "Invalid Origin"
    );
    let reading = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    ensure!(
        (reading && !headers.contains_key("upgrade")) || presented.is_some(),
        "Origin required"
    );
    ensure!(
        !headers
            .get_all("sec-fetch-site")
            .iter()
            .any(|value| value == "cross-site"),
        "Cross-site request"
    );
    Ok((origin, host.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, host: &str, origin: Option<&str>) -> Request<()> {
        let mut builder = Request::builder()
            .method(method)
            .uri("/api/v1/plans")
            .header("host", host);
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn published_ports_and_ipv6_are_validated_against_the_browser_authority() {
        for host in [
            "127.0.0.1:18080",
            "192.168.1.5:8080",
            "[::1]:18080",
            "[fd00::1]:8080",
        ] {
            let expected = format!("http://{host}");
            assert_eq!(
                origin(&request("POST", host, Some(&expected))).unwrap(),
                (expected, host.to_owned())
            );
            assert!(origin(&request("GET", host, None)).is_ok());
        }
    }

    #[test]
    fn invalid_authority_and_cross_site_requests_are_refused() {
        for host in [
            "localhost:8080",
            "attacker.example",
            "user@127.0.0.1:8080",
            "0.0.0.0:8080",
            "[::]:8080",
            "127.0.0.1:99999",
        ] {
            assert!(origin(&request("GET", host, None)).is_err(), "{host}");
        }
        let host = "127.0.0.1:18080";
        assert!(origin(&request("POST", host, None)).is_err());
        assert!(origin(&request("POST", host, Some("http://127.0.0.1:8080"))).is_err());
        for (name, value) in [
            ("host", host),
            ("origin", "http://127.0.0.1:18080"),
            ("forwarded", "for=127.0.0.1"),
            ("x-forwarded-host", host),
            ("sec-fetch-site", "cross-site"),
        ] {
            let mut req = request("POST", host, Some("http://127.0.0.1:18080"));
            req.headers_mut().append(
                axum::http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
            assert!(origin(&req).is_err(), "{name}");
        }
        let mut req = request("GET", host, None);
        req.headers_mut()
            .insert("upgrade", "websocket".parse().unwrap());
        assert!(origin(&req).is_err());
        req.headers_mut().remove("upgrade");
        *req.uri_mut() = "http://192.168.1.5:8080/api/v1/plans".parse().unwrap();
        assert!(origin(&req).is_err());
    }
}
