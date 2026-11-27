use super::Error;
use dom_query::Document;
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED};
use quick_xml::{Reader, events::Event};
use reqwest::header::HeaderValue;
use std::borrow::Cow;

#[derive(Clone, Copy)]
enum Format {
    Html,
    Xhtml,
    Plain,
}

#[derive(Clone, Copy)]
pub(super) struct PageContent {
    format: Format,
    charset: Result<Option<&'static Encoding>, Error>,
}

impl PageContent {
    pub(super) fn from_header(header: Option<&HeaderValue>) -> Result<Self, Error> {
        let value = header
            .and_then(|value| value.to_str().ok())
            .ok_or(Error::ContentType)?;
        let (media_type, parameters) = value.split_once(';').unwrap_or((value, ""));
        let format = match media_type.trim().to_ascii_lowercase().as_str() {
            "text/html" => Format::Html,
            "application/xhtml+xml" => Format::Xhtml,
            "text/plain" => Format::Plain,
            _ => return Err(Error::ContentType),
        };
        Ok(Self {
            format,
            charset: declared_charset(parameters),
        })
    }

    pub(super) fn is_markup(self) -> bool {
        !matches!(self.format, Format::Plain)
    }

    pub(super) fn decode(self, body: &[u8]) -> Result<Cow<'_, str>, Error> {
        let (encoding, skip) = if let Some(bom) = Encoding::for_bom(body) {
            bom
        } else {
            let encoding = match self.charset? {
                Some(encoding) => Some(encoding),
                None => match self.format {
                    Format::Html => html_charset(body),
                    Format::Xhtml => xml_charset(body)?,
                    Format::Plain => None,
                },
            };
            (encoding.unwrap_or(UTF_8), 0)
        };
        encoding
            .decode_without_bom_handling_and_without_replacement(&body[skip..])
            .ok_or(Error::Encoding)
    }
}

fn declared_charset(parameters: &str) -> Result<Option<&'static Encoding>, Error> {
    let mut quoted = false;
    let mut escaped = false;
    let parameter = parameters.split_inclusive(|ch| {
        if escaped {
            escaped = false;
        } else if ch == '\\' && quoted {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch == ';' && !quoted {
            return true;
        }
        false
    });
    let label = parameter
        .filter_map(|part| part.trim_end_matches(';').split_once('='))
        .find_map(|(name, label)| {
            name.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| label.trim())
        });
    label.map(parse_charset).transpose()
}

fn parse_charset(label: &str) -> Result<&'static Encoding, Error> {
    let label = match label.strip_prefix('"') {
        Some(label) => {
            let label = label.strip_suffix('"').ok_or(Error::Encoding)?;
            let mut chars = label.chars();
            Cow::Owned(
                std::iter::from_fn(|| match chars.next()? {
                    '\\' => chars.next(),
                    ch => Some(ch),
                })
                .collect::<String>(),
            )
        }
        None => Cow::Borrowed(label),
    };
    Encoding::for_label_no_replacement(label.as_bytes()).ok_or(Error::Encoding)
}

fn html_charset(body: &[u8]) -> Option<&'static Encoding> {
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
                let content = node.attr("content")?;
                declared_charset(content.split_once(';')?.1).ok().flatten()
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

fn xml_charset(body: &[u8]) -> Result<Option<&'static Encoding>, Error> {
    if !body.starts_with(b"<?xml") {
        return Ok(None);
    }
    let Event::Decl(declaration) = Reader::from_reader(body)
        .read_event()
        .map_err(|_| Error::Encoding)?
    else {
        return Ok(None);
    };
    declaration
        .encoding()
        .map(|label| parse_charset(&label.map_err(|_| Error::Encoding)?))
        .transpose()
}
