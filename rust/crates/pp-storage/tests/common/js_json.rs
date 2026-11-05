use serde_json::Value;

/// Parses JSON text as `JSON.parse` would, except that each lone surrogate
/// escape, which serde_json refuses, reads as U+FFFD, as Checkoff storage
/// reads it. `JSON.stringify` escapes only lone surrogates.
pub fn parse(raw: &str) -> Value {
    let mut text = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find('\\') {
        text.push_str(&rest[..at]);
        let lone = rest
            .get(at + 1..at + 6)
            .filter(|escape| escape.starts_with('u'))
            .and_then(|escape| u16::from_str_radix(&escape[1..], 16).ok())
            .is_some_and(|unit| (0xD800..0xE000).contains(&unit));
        let end = if lone { at + 6 } else { at + 2 };
        text.push_str(if lone { "\\ufffd" } else { &rest[at..end] });
        rest = &rest[end..];
    }
    text.push_str(rest);
    serde_json::from_str(&text).unwrap()
}
