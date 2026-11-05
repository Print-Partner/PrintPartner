use serde_json::Value;

/// Parses JSON text as `JSON.parse` would, except that each lone surrogate
/// escape, which serde_json refuses, reads as U+FFFD, as Checkoff storage
/// reads it. Escaped surrogate pairs are kept.
pub fn parse(raw: &str) -> Value {
    let surrogate = |rest: &str, at: usize| {
        rest.get(at..at + 6)
            .filter(|escape| escape.starts_with("\\u"))
            .and_then(|escape| u16::from_str_radix(&escape[2..], 16).ok())
            .filter(|unit| (0xD800..0xE000).contains(unit))
    };
    let mut text = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find('\\') {
        text.push_str(&rest[..at]);
        let end = match (surrogate(rest, at), surrogate(rest, at + 6)) {
            (Some(high), Some(low)) if high < 0xDC00 && low >= 0xDC00 => {
                text.push_str(&rest[at..at + 12]);
                at + 12
            }
            (Some(_), _) => {
                text.push_str("\\ufffd");
                at + 6
            }
            _ => {
                text.push_str(&rest[at..at + 2]);
                at + 2
            }
        };
        rest = &rest[end..];
    }
    text.push_str(rest);
    serde_json::from_str(&text).unwrap()
}
