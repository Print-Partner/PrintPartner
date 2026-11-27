use dom_query::Document;
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED};
use pp_api::mcp::web_discovery::{Discovery, Error, Hit, Page, Read};
use pp_net::{
    network::{CheckedDns, Policy, checked_client},
    outbound::SystemResolver,
};
use reqwest::{Client, Response, Url, header};
use serde::Deserialize;
use std::{borrow::Cow, future::Future, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

const SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const MAX_BODY: usize = 512 * 1024;
const MAX_TEXT: usize = 20_000;
const MAX_HITS: usize = 5;
const MAX_URL: usize = 2_048;
const MAX_REDIRECTS: usize = 3;
const DEADLINE: Duration = Duration::from_secs(12);

pub(super) struct WebDiscovery {
    client: Client,
    brave_key: Option<header::HeaderValue>,
    limit: Semaphore,
}

impl WebDiscovery {
    pub(super) fn from_env() -> anyhow::Result<Option<Arc<dyn Discovery>>> {
        if std::env::var("PP_ASSISTANT_WEB_DISCOVERY").as_deref() != Ok("1") {
            return Ok(None);
        }
        let brave_key = brave_key(|name| std::env::var(name).ok());
        let client = checked_client(CheckedDns::new(
            Arc::new(SystemResolver),
            Policy::Public,
            "Web discovery needs public DNS addresses",
        ))?
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(0)
        .user_agent("PrintPartner-WebDiscovery/1.0")
        .build()?;
        Ok(Some(Arc::new(Self {
            client,
            brave_key,
            limit: Semaphore::new(4),
        })))
    }

    async fn bounded<T>(&self, work: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
        let _permit = self.limit.try_acquire().map_err(|_| Error::Busy)?;
        tokio::time::timeout(DEADLINE, work)
            .await
            .map_err(|_| Error::Timeout)?
    }

    async fn page(&self, url: &str) -> Result<Page, Error> {
        let (url, response) = follow_page(url, |url| async move {
            self.client
                .get(url)
                .header(header::ACCEPT, "text/html,text/plain")
                .send()
                .await
                .map_err(network_error)
        })
        .await?;
        let mime = page_type(&response)?;
        let bytes = body(response).await?;
        page_text(url.into(), &bytes, &mime)
    }

    fn brave_request(
        &self,
        query: &str,
        site: Option<&str>,
    ) -> Result<reqwest::RequestBuilder, Error> {
        let key = self.brave_key.clone().ok_or(Error::Disabled)?;
        let query = match site {
            Some(host) => format!("site:{host} {query}"),
            None => query.to_owned(),
        };
        let mut url = Url::parse(SEARCH_URL).map_err(|_| Error::Unavailable)?;
        url.query_pairs_mut()
            .append_pair("q", &query)
            .append_pair("count", &MAX_HITS.to_string());
        Ok(self
            .client
            .get(url)
            .header(header::ACCEPT, "application/json")
            .header("X-Subscription-Token", key))
    }

    async fn brave(&self, query: &str, site: Option<&str>) -> Result<Vec<Hit>, Error> {
        let response = self
            .brave_request(query, site)?
            .send()
            .await
            .map_err(network_error)?;
        search_hits(&body(success(response)?).await?)
    }
}

impl Discovery for WebDiscovery {
    fn has_search(&self) -> bool {
        self.brave_key.is_some()
    }

    fn search<'a>(&'a self, query: &'a str, site: Option<&'a str>) -> Read<'a, Vec<Hit>> {
        Box::pin(self.bounded(self.brave(query, site)))
    }

    fn fetch<'a>(&'a self, url: &'a str) -> Read<'a, Page> {
        Box::pin(self.bounded(self.page(url)))
    }
}

fn brave_key(env: impl Fn(&str) -> Option<String>) -> Option<header::HeaderValue> {
    if env("SEARCH_PROVIDER").as_deref() != Some("brave") {
        return None;
    }
    let mut key = ["SEARCH_API_KEY", "BRAVE_API_KEY"]
        .into_iter()
        .filter_map(env)
        .find(|key| !key.trim().is_empty())
        .and_then(|key| header::HeaderValue::from_str(key.trim()).ok())?;
    key.set_sensitive(true);
    Some(key)
}

fn public_url(raw: &str) -> Result<Url, Error> {
    let url = Url::parse(raw).map_err(|_| Error::Destination)?;
    if raw.len() > MAX_URL
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(Error::Destination);
    }
    let host = url.host_str().ok_or(Error::Destination)?;
    let allowed = match host.trim_matches(['[', ']']).parse() {
        Ok(ip) => Policy::Public.allows(ip),
        Err(_) => host.trim_end_matches('.').contains('.'),
    };
    if !allowed {
        return Err(Error::Destination);
    }
    Ok(url)
}

fn redirect_target(from: &Url, location: &header::HeaderValue) -> Result<Url, Error> {
    let location = location.to_str().map_err(|_| Error::Destination)?;
    let mut target = from.join(location).map_err(|_| Error::Destination)?;
    if from.scheme() == "https" && target.scheme() != "https" {
        return Err(Error::Destination);
    }
    if target.fragment().is_none() {
        target.set_fragment(from.fragment());
    }
    public_url(target.as_str())
}

async fn follow_page<F: Future<Output = Result<Response, Error>>>(
    raw: &str,
    get: impl Fn(Url) -> F,
) -> Result<(Url, Response), Error> {
    let mut url = public_url(raw)?;
    for hop in 0..=MAX_REDIRECTS {
        let mut request_url = url.clone();
        request_url.set_fragment(None);
        let response = get(request_url).await?;
        if !response.status().is_redirection() {
            return Ok((url, success(response)?));
        }
        if hop == MAX_REDIRECTS {
            break;
        }
        let location = response
            .headers()
            .get(header::LOCATION)
            .ok_or(Error::Destination)?;
        url = redirect_target(&url, location)?;
    }
    Err(Error::Destination)
}

fn network_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Unavailable
    }
}

fn success(response: Response) -> Result<Response, Error> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(Error::Unavailable)
    }
}

async fn body(mut response: Response) -> Result<Vec<u8>, Error> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(Error::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if chunk.len() > MAX_BODY - body.len() {
            return Err(Error::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn page_type(response: &Response) -> Result<mime::Mime, Error> {
    let mime: mime::Mime = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .ok_or(Error::ContentType)?;
    if matches!(
        mime.essence_str(),
        "text/html" | "application/xhtml+xml" | "text/plain"
    ) {
        Ok(mime)
    } else {
        Err(Error::ContentType)
    }
}

fn html_encoding(body: &[u8]) -> Option<&'static Encoding> {
    let prefix = String::from_utf8_lossy(&body[..body.len().min(1024)]);
    let document = Document::from(prefix.as_ref());
    document
        .select("head meta")
        .nodes()
        .iter()
        .find_map(|node| {
            let encoding = if let Some(label) = node.attr("charset") {
                Encoding::for_label_no_replacement(label.as_bytes())
            } else if node
                .attr("http-equiv")?
                .eq_ignore_ascii_case("content-type")
            {
                let content: mime::Mime = node.attr("content")?.parse().ok()?;
                Encoding::for_label_no_replacement(
                    content.get_param(mime::CHARSET)?.as_str().as_bytes(),
                )
            } else {
                None
            }?;
            Some(match encoding {
                e if e == UTF_16BE || e == UTF_16LE => UTF_8,
                e if e == X_USER_DEFINED => WINDOWS_1252,
                e => e,
            })
        })
}

fn decode_page<'a>(body: &'a [u8], mime: &mime::Mime) -> Result<Cow<'a, str>, Error> {
    let (encoding, skip) = if let Some(bom) = Encoding::for_bom(body) {
        bom
    } else if let Some(label) = mime.get_param(mime::CHARSET) {
        (
            Encoding::for_label_no_replacement(label.as_str().as_bytes()).ok_or(Error::Encoding)?,
            0,
        )
    } else if mime.essence_str() == "text/html" {
        (html_encoding(body).unwrap_or(UTF_8), 0)
    } else {
        (UTF_8, 0)
    };
    encoding
        .decode_without_bom_handling_and_without_replacement(&body[skip..])
        .ok_or(Error::Encoding)
}

fn cap(text: &str, bytes: usize) -> (&str, bool) {
    (&text[..text.floor_char_boundary(bytes)], text.len() > bytes)
}

fn page_text(url: String, body: &[u8], mime: &mime::Mime) -> Result<Page, Error> {
    let raw = decode_page(body, mime)?;
    let (title, text) = if mime.essence_str() != "text/plain" {
        let document = Document::from(raw.as_ref());
        let title = document.select_single("title").text();
        document
            .select("script, style, template, noscript")
            .remove();
        let text = document.select_single("body").formatted_text();
        (Some(cap(title.trim(), 300).0.to_owned()), text.to_string())
    } else {
        (None, raw.into_owned())
    };
    let (text, truncated) = cap(text.trim(), MAX_TEXT);
    Ok(Page {
        url,
        title: title.filter(|title| !title.is_empty()),
        text: text.into(),
        truncated,
    })
}

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveHit>,
}

#[derive(Deserialize)]
struct BraveHit {
    url: String,
    title: String,
    #[serde(default)]
    description: String,
}

fn search_hits(bytes: &[u8]) -> Result<Vec<Hit>, Error> {
    let response: BraveResponse = serde_json::from_slice(bytes).map_err(|_| Error::Unavailable)?;
    Ok(response
        .web
        .into_iter()
        .flat_map(|web| web.results)
        .filter_map(|hit| {
            Some(Hit {
                url: public_url(&hit.url).ok()?.into(),
                title: cap(&hit.title, 300).0.into(),
                snippet: cap(&hit.description, 500).0.into(),
            })
        })
        .take(MAX_HITS)
        .collect())
}

#[cfg(test)]
mod tests;
