//! The URL a Build's planning brief keeps as evidence: Node's `normalizedUrl`
//! and `classifyBuildUrl` (`web/apps/server/src/services/build-planning.ts`).
//!
//! An evidence item's id is a hash of the normalized URL, and briefs imported
//! from Node hold those ids, so [`normalize`] must give Node's string for the
//! same input. Both parse with the WHATWG URL rules: Node's `URL` and the
//! `url` crate. Pure: no network call, no I/O.

use url::Url;

/// What a URL points at, as Node sorts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlClass {
    /// A git repository: a `github.com` URL, or a path ending `.git`.
    Git { github: bool },
    /// A page on a model library, whose files the user downloads and uploads.
    ModelPage,
    /// Any other page.
    Link,
}

/// A classified evidence URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildUrl {
    pub normalized: String,
    pub class: UrlClass,
}

/// Hosts whose pages are model pages, matched with their subdomains.
const MODEL_LIBRARIES: [&str; 6] = [
    "printables.com",
    "makerworld.com",
    "thingiverse.com",
    "thangs.com",
    "cults3d.com",
    "myminifactory.com",
];

/// Node's `normalizedUrl`: the trimmed URL, http or https only, without its
/// fragment or trailing slashes on its path. `None` for anything else.
pub fn normalize(raw: &str) -> Option<String> {
    parse(raw).map(String::from)
}

/// Node's `classifyBuildUrl` sorting of `raw`. `None` when [`normalize`]
/// refuses it.
pub fn classify(raw: &str) -> Option<BuildUrl> {
    let url = parse(raw)?;
    let host = url.host_str().unwrap_or_default();
    let class = if MODEL_LIBRARIES
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        UrlClass::ModelPage
    } else if host == "github.com" || url.path().to_lowercase().ends_with(".git") {
        UrlClass::Git {
            github: host == "github.com",
        }
    } else {
        UrlClass::Link
    };
    Some(BuildUrl {
        normalized: url.into(),
        class,
    })
}

fn parse(raw: &str) -> Option<Url> {
    let mut url =
        Url::parse(raw.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(if path.is_empty() { "/" } else { &path });
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// `[raw, normalized, evidence id, class]` from Node's own functions:
    /// `normalizedUrl(raw)` and `analyzeBuildRequest("input", [raw]).evidence[0]`
    /// (its `id` and `kind`), run on main at 75cfb363e with
    /// `web/node_modules/.bin/tsx` and a script importing
    /// `web/apps/server/src/services/build-planning.ts`.
    const NODE: [(&str, &str, &str, UrlClass); 13] = [
        (
            "https://www.printables.com/model/12345-kit/files#comments",
            "https://www.printables.com/model/12345-kit/files",
            "e23eb2aaca54431c33ba",
            UrlClass::ModelPage,
        ),
        (
            "  HTTPS://MakerWorld.com/en/models/678/  ",
            "https://makerworld.com/en/models/678",
            "e16aee20b7489c02899c",
            UrlClass::ModelPage,
        ),
        (
            "https://thingiverse.com/thing:42",
            "https://thingiverse.com/thing:42",
            "30f42b5d48041ca05f51",
            UrlClass::ModelPage,
        ),
        (
            "https://sub.thangs.com/x",
            "https://sub.thangs.com/x",
            "62ba071fff4b9c719409",
            UrlClass::ModelPage,
        ),
        (
            "http://cults3d.com/en/3d-model/a",
            "http://cults3d.com/en/3d-model/a",
            "45a4fcf54eec7a534802",
            UrlClass::ModelPage,
        ),
        (
            "https://www.myminifactory.com/object/3d-print-1",
            "https://www.myminifactory.com/object/3d-print-1",
            "e41ec34f3c67ddf54348",
            UrlClass::ModelPage,
        ),
        (
            "https://example.com",
            "https://example.com/",
            "0f115db062b7c0dd030b",
            UrlClass::Link,
        ),
        (
            "https://Example.COM/Docs/Guide///?q=Ab#frag",
            "https://example.com/Docs/Guide?q=Ab",
            "73c6eab824d58a05cc41",
            UrlClass::Link,
        ),
        (
            "http://example.com:80/a/../b/./c",
            "http://example.com/b/c",
            "800d8155c5a77b740649",
            UrlClass::Link,
        ),
        (
            "https://example.com/a b",
            "https://example.com/a%20b",
            "f785dc530cf18312e858",
            UrlClass::Link,
        ),
        (
            "https://github.com/Owner/Repo/",
            "https://github.com/Owner/Repo",
            "8d325f3875d513dbc4e3",
            UrlClass::Git { github: true },
        ),
        (
            "https://gitlab.example/owner/repo.git",
            "https://gitlab.example/owner/repo.git",
            "259d2ec3017d023ad06f",
            UrlClass::Git { github: false },
        ),
        (
            "https://notprintables.com/model/1",
            "https://notprintables.com/model/1",
            "1e7227882f1bcb068c9c",
            UrlClass::Link,
        ),
    ];

    #[test]
    fn build_urls_normalize_and_classify_as_node_does() {
        for (raw, normalized, id, class) in NODE {
            assert_eq!(
                classify(raw),
                Some(BuildUrl {
                    normalized: normalized.into(),
                    class
                }),
                "{raw}"
            );
            assert_eq!(normalize(raw).as_deref(), Some(normalized), "{raw}");
            let hash = hex::encode(Sha256::digest(normalized.as_bytes()));
            assert_eq!(&hash[..20], id, "{raw}");
        }
    }

    #[test]
    fn build_urls_refuse_what_node_refuses() {
        // Node throws "Only public HTTP(S) URLs are supported" or "Invalid URL".
        for raw in [
            "ftp://example.com/x",
            "mailto:a@b.c",
            "not a url",
            "",
            "javascript:alert(1)",
        ] {
            assert_eq!(normalize(raw), None, "{raw}");
            assert_eq!(classify(raw), None, "{raw}");
        }
    }
}
