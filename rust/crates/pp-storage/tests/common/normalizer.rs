use serde_json::Value;
use std::collections::HashMap;

/// The Node generator's normalizer: generated UUIDs become `<uuid:n>` in order of
/// first appearance, and ISO times outside the seeded year become `<now>`.
#[derive(Default)]
pub struct Normalizer(HashMap<String, String>);

impl Normalizer {
    fn uuid(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 36
            && b.iter().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => *c == b'-',
                14 => *c == b'4',
                19 => b"89ab".contains(c),
                _ => matches!(c, b'0'..=b'9' | b'a'..=b'f'),
            })
    }

    fn iso(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 24
            && b.iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b'T',
                13 | 16 => *c == b':',
                19 => *c == b'.',
                23 => *c == b'Z',
                _ => c.is_ascii_digit(),
            })
    }

    pub fn walk(&mut self, value: Value) -> Value {
        match value {
            Value::Array(items) => items.into_iter().map(|v| self.walk(v)).collect(),
            Value::Object(map) => {
                Value::Object(map.into_iter().map(|(k, v)| (k, self.walk(v))).collect())
            }
            Value::String(s) if Self::uuid(&s) => {
                let next = format!("<uuid:{}>", self.0.len());
                self.0.entry(s).or_insert(next).clone().into()
            }
            Value::String(s) if Self::iso(&s) && !s.starts_with("2020-") => "<now>".into(),
            other => other,
        }
    }
}
