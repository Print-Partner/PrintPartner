//! One `multipart/form-data` upload read under `@fastify/multipart`'s
//! limits, for the routes that take a single print file.

use crate::accepted::failure;
use axum::{
    extract::{FromRequest, Multipart, multipart::Field},
    http::{StatusCode, header::CONTENT_TYPE},
    response::Response,
};

/// Field names `@fastify/multipart` refuses because they are
/// `Object.prototype` members.
const PROTOTYPE: [&str; 12] = [
    "__proto__",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

/// One route's limits and refusal texts.
pub(crate) struct Limits {
    /// Field parts allowed; one more is refused as unreadable.
    pub fields: usize,
    /// A field's value is cut to this many bytes.
    pub field_bytes: usize,
    pub file_bytes: usize,
    /// File part names the route keeps; other file parts are read and dropped.
    pub file_names: &'static [&'static str],
    pub second_file: &'static str,
    pub too_large: &'static str,
    pub unreadable: &'static str,
}

/// The form's fields in arrival order and the kept file part, as its file
/// name (when the part had one) and bytes.
#[derive(Default)]
pub(crate) struct Form {
    pub fields: Vec<(String, String)>,
    pub file: Option<(Option<String>, Vec<u8>)>,
}

impl Form {
    /// The last value sent for field `name`.
    pub(crate) fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .rev()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One part's bytes, or `Err(true)` past `cap` or the body limit and
/// `Err(false)` when the body cannot be read. A `truncate` part keeps its
/// first `cap` bytes instead, as busboy cuts a field at `fieldSize`.
async fn part_bytes(part: &mut Field<'_>, cap: usize, truncate: bool) -> Result<Vec<u8>, bool> {
    let mut bytes = Vec::new();
    while let Some(chunk) = part
        .chunk()
        .await
        .map_err(|error| error.status() == StatusCode::PAYLOAD_TOO_LARGE)?
    {
        if bytes.len() + chunk.len() > cap && !truncate {
            return Err(true);
        }
        let room = cap - bytes.len();
        bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
    Ok(bytes)
}

/// The multipart form under `limits`. A part is a file when it has a file
/// name or an `application/octet-stream` type, as busboy decides, and a part
/// named after an `Object.prototype` member is refused as
/// `@fastify/multipart` refuses it, when the form ends. A second file part
/// is refused, and Node drains a refused part without its size limit.
pub(crate) async fn read(
    request: axum::extract::Request,
    limits: &Limits,
) -> Result<Form, Box<Response>> {
    let multipart = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("multipart/form-data")
        });
    if !multipart {
        return Err(Box::new(failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Send the print file as multipart/form-data",
        )));
    }
    let bad = |detail: &str| Box::new(failure(StatusCode::BAD_REQUEST, detail));
    let refused = |too_large: bool| {
        if too_large {
            Box::new(failure(StatusCode::PAYLOAD_TOO_LARGE, limits.too_large))
        } else {
            bad(limits.unreadable)
        }
    };
    let mut parts = Multipart::from_request(request, &())
        .await
        .map_err(|_| bad(limits.unreadable))?;
    let mut form = Form::default();
    let (mut files, mut fields) = (0, 0);
    let mut prototype = false;
    loop {
        let mut part = match parts.next_field().await {
            Ok(Some(part)) => part,
            Ok(None) if prototype => return Err(bad(limits.unreadable)),
            Ok(None) => return Ok(form),
            Err(_) => return Err(bad(limits.unreadable)),
        };
        let name = part.name().map(str::to_owned);
        // @fastify/multipart records this refusal and raises it when the
        // stream ends, so a limit hit later still answers first.
        let proto = name
            .as_deref()
            .is_some_and(|name| PROTOTYPE.contains(&name));
        prototype |= proto;
        let octets = part.content_type().is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/octet-stream")
        });
        let file_name = part.file_name().map(str::to_owned);
        if file_name.is_none() && !octets {
            fields += 1;
            let value = part_bytes(&mut part, limits.field_bytes, true)
                .await
                .map_err(|_| bad(limits.unreadable))?;
            if fields > limits.fields {
                return Err(bad(limits.unreadable));
            }
            if let Some(name) = name {
                form.fields
                    .push((name, String::from_utf8_lossy(&value).into_owned()));
            }
            continue;
        }
        files += 1;
        if files > 1 {
            return Err(bad(limits.second_file));
        }
        let bytes = part_bytes(&mut part, limits.file_bytes, proto)
            .await
            .map_err(|too_large| refused(too_large && !proto))?;
        if name
            .as_deref()
            .is_some_and(|name| limits.file_names.contains(&name))
        {
            form.file = Some((file_name, bytes));
        }
    }
}
