//! Admission for an explicitly enabled IP-address HTTP listener.
use anyhow::{Result, ensure};
use axum::http::{Method, Request};
use std::net::IpAddr;

/// Validate the browser-visible authority, including a published Docker port.
/// DNS names and forwarded authority are outside this listener's contract.
/// An unsafe request or an upgrade must carry the matching Origin.
pub fn origin<B>(request: &Request<B>) -> Result<(String, String)> {
    let validated = authority(request)?;
    let reading = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    ensure!(
        (reading && !request.headers().contains_key("upgrade"))
            || request.headers().contains_key("origin"),
        "Origin required"
    );
    Ok(validated)
}

/// `origin` without its Origin-required rule, for MCP: MCP clients are not
/// browsers and send no Origin, so the MCP key guard decides whether an unsafe
/// request without one may continue. A presented Origin must still match.
pub fn authority<B>(request: &Request<B>) -> Result<(String, String)> {
    let headers = request.headers();
    ensure!(headers.get_all("host").iter().count() == 1, "Invalid Host");
    let host = headers["host"].to_str()?;
    let authority: axum::http::uri::Authority = host.parse()?;
    let ip: IpAddr = authority.host().trim_matches(['[', ']']).parse()?;
    let ip = ip.to_canonical();
    ensure!(!ip.is_unspecified() && !ip.is_multicast(), "Invalid Host");
    ensure!(
        host == authority.host() || host.parse::<std::net::SocketAddr>().is_ok(),
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

    /// `request` with one more header appended, possibly a second of its name.
    fn with_header(mut req: Request<()>, name: &str, value: &str) -> Request<()> {
        req.headers_mut().append(
            axum::http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        req
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
            "[::ffff:0.0.0.0]:8080",
            "[::ffff:224.0.0.1]:8080",
            "127.0.0.1:99999",
            "127.0.0.1:",
            "127.0.0.1:+8080",
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
            let req = request("POST", host, Some("http://127.0.0.1:18080"));
            assert!(origin(&with_header(req, name, value)).is_err(), "{name}");
        }
        let mut req = request("GET", host, None);
        req.headers_mut()
            .insert("upgrade", "websocket".parse().unwrap());
        assert!(origin(&req).is_err());
        req.headers_mut().remove("upgrade");
        *req.uri_mut() = "http://192.168.1.5:8080/api/v1/plans".parse().unwrap();
        assert!(origin(&req).is_err());
    }

    #[test]
    fn authority_drops_only_the_origin_required_rule() {
        let host = "192.168.1.5:8080";
        let expected = (format!("http://{host}"), host.to_owned());
        assert!(origin(&request("POST", host, None)).is_err());
        assert_eq!(authority(&request("POST", host, None)).unwrap(), expected);
        assert_eq!(
            authority(&request("POST", host, Some("http://192.168.1.5:8080"))).unwrap(),
            expected
        );
        assert!(authority(&request("POST", host, Some("http://192.168.1.6:8080"))).is_err());
        assert!(authority(&request("POST", "attacker.example:8080", None)).is_err());
        for (name, value) in [
            ("host", host),
            ("forwarded", "for=127.0.0.1"),
            ("x-forwarded-host", host),
            ("sec-fetch-site", "cross-site"),
        ] {
            let req = with_header(request("POST", host, None), name, value);
            assert!(authority(&req).is_err(), "{name}");
        }
    }
}
