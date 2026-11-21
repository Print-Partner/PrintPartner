//! Golden files that Rust writes and Rust tests compare byte for byte
//! (GRE-1116, `docs/development/golden-tests.md`). It needs only std and
//! `serde_json`, so a crate that does not depend on this one can include the
//! file with `#[path]`.
//!
//! A [`Goldens`] names a directory and every file it may hold. With
//! `PP_GOLDEN_UPDATE` set, [`Goldens::check`] writes the actual output and
//! still fails, so an update run never passes and CI cannot refresh a golden.

use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct Goldens {
    pub dir: PathBuf,
    /// Every file in `dir`. [`Goldens::assert_all_listed`] refuses any other
    /// file, so no golden sits in the directory unchecked.
    pub files: &'static [&'static str],
}

impl Goldens {
    pub fn new(dir: impl AsRef<Path>, files: &'static [&'static str]) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            files,
        }
    }

    /// Fails unless `actual` equals the golden `name` byte for byte, naming
    /// the first line that differs.
    pub fn check(&self, name: &str, actual: &str) {
        assert!(self.files.contains(&name), "{name} is not a listed golden");
        let path = self.dir.join(name);
        if std::env::var_os("PP_GOLDEN_UPDATE").is_some() {
            std::fs::write(&path, actual)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            panic!(
                "wrote {}: review the diff, then rerun without PP_GOLDEN_UPDATE",
                path.display()
            );
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "{}: {error}; write it with PP_GOLDEN_UPDATE=1",
                path.display()
            )
        });
        if let Some(line) = first_difference(&expected, actual) {
            panic!(
                "{name} differs at line {}:\n  golden: {:?}\n  actual: {:?}\n\
                 If the change is deliberate, rerun with PP_GOLDEN_UPDATE=1 and review the diff.",
                line + 1,
                expected
                    .split_inclusive('\n')
                    .nth(line)
                    .unwrap_or("<end of file>"),
                actual
                    .split_inclusive('\n')
                    .nth(line)
                    .unwrap_or("<end of file>"),
            );
        }
    }

    /// [`Goldens::check`] of `value` as [`json`] text.
    pub fn check_json(&self, name: &str, value: &Value) {
        self.check(name, &json(value));
    }

    /// Fails unless the directory holds exactly the listed files.
    pub fn assert_all_listed(&self) {
        let entries = std::fs::read_dir(&self.dir)
            .unwrap_or_else(|error| panic!("{}: {error}", self.dir.display()));
        let mut found: Vec<String> = entries
            .map(|entry| match entry {
                Ok(entry) => entry.file_name().to_string_lossy().into_owned(),
                Err(error) => panic!("{}: {error}", self.dir.display()),
            })
            .collect();
        found.sort();
        let mut listed: Vec<String> = self.files.iter().map(|name| name.to_string()).collect();
        listed.sort();
        assert_eq!(found, listed, "an unlisted golden is never compared");
    }
}

/// `value` as JSON text that does not depend on the build: object keys
/// sorted, so whether Cargo unifies `serde_json/preserve_order` into the build
/// does not matter, and any array or object that fits in 120 columns on one
/// line, so the diff of a change stays short.
pub fn json(value: &Value) -> String {
    let mut sorted = value.clone();
    sorted.sort_all_objects();
    let mut out = String::new();
    write_json(&sorted, 0, &mut out);
    out + "\n"
}

fn write_json(value: &Value, depth: usize, out: &mut String) {
    let compact = value.to_string();
    let (open, close, items): (_, _, Vec<(Option<&String>, &Value)>) = match value {
        Value::Array(items) => ('[', ']', items.iter().map(|item| (None, item)).collect()),
        Value::Object(map) => ('{', '}', map.iter().map(|(k, v)| (Some(k), v)).collect()),
        _ => ('\0', '\0', Vec::new()),
    };
    if items.is_empty() || 2 * depth + compact.len() <= 120 {
        out.push_str(&compact);
        return;
    }
    out.push(open);
    for (index, (key, item)) in items.iter().enumerate() {
        out.push('\n');
        out.push_str(&"  ".repeat(depth + 1));
        if let Some(key) = key {
            out.push_str(&Value::String(key.to_string()).to_string());
            out.push_str(": ");
        }
        write_json(item, depth + 1, out);
        if index + 1 < items.len() {
            out.push(',');
        }
    }
    out.push('\n');
    out.push_str(&"  ".repeat(depth));
    out.push(close);
}

/// The 0-based index of the first line, newline included, that differs.
pub fn first_difference(expected: &str, actual: &str) -> Option<usize> {
    if expected == actual {
        return None;
    }
    let mut expected_lines = expected.split_inclusive('\n');
    let mut actual_lines = actual.split_inclusive('\n');
    (0..).find(|_| expected_lines.next() != actual_lines.next())
}

#[cfg(test)]
mod tests {
    use super::{first_difference, json};

    #[test]
    fn golden_first_difference_finds_any_changed_byte() {
        let golden = "one\ntwo\nthree\n";
        assert_eq!(first_difference(golden, golden), None);
        assert_eq!(first_difference(golden, "one\ntwo\nthreE\n"), Some(2));
        assert_eq!(first_difference(golden, "one\ntwo\nthree"), Some(2));
        assert_eq!(first_difference(golden, "one\ntwo\nthree\n\n"), Some(3));
        assert_eq!(first_difference(golden, "one\r\ntwo\nthree\n"), Some(0));
    }

    #[test]
    fn golden_json_sorts_keys_and_wraps_only_long_values() {
        let value = serde_json::json!({"b": [1, 2], "a": {"long": "x".repeat(120)}});
        assert_eq!(
            json(&value),
            format!(
                "{{\n  \"a\": {{\n    \"long\": \"{}\"\n  }},\n  \"b\": [1,2]\n}}\n",
                "x".repeat(120)
            )
        );
    }
}
