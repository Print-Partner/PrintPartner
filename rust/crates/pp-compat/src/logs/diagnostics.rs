//! Bounded, re-scrubbed core-log records for authenticated diagnostics.
use super::{CORE, LINE_BYTES, tail_rows, view_directory};
use serde_json::Value;
use std::path::Path;

/// Reads at most 64 KiB per file, including six archives. Invalid, partial,
/// oversized and non-pino-level rows are omitted, as in the desktop Logs view.
pub fn records(data: &Path) -> std::io::Result<Vec<Value>> {
    let directory = match view_directory(data) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let scrubber = crate::redact::Scrubber::default();
    let (rows, _) = tail_rows(&directory, CORE, 1000, LINE_BYTES, |row| {
        let mut value: Value = serde_json::from_slice(row).ok()?;
        if ![10, 20, 30, 40, 50, 60].contains(&value.get("level")?.as_u64()?) {
            return None;
        }
        scrubber.json(&mut value);
        Some(value)
    })?;
    Ok(rows.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_reader_scrubs_and_omits_partial_rows() {
        let data = std::env::current_dir()
            .unwrap()
            .join(format!(".diagnostics-test-{}", std::process::id()));
        std::fs::create_dir_all(data.join("logs")).unwrap();
        std::fs::write(
            data.join("logs/core.1.log"),
            "{\"time\":1,\"level\":30,\"message\":\"old\"}\n",
        )
        .unwrap();
        std::fs::write(data.join("logs/core.log"), "invalid\n{\"time\":2,\"level\":50,\"token\":\"fixture-only\",\"message\":\"new\"}\n{\"level\":30}").unwrap();
        let rows = records(&data).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["message"], "old");
        assert_ne!(rows[1]["token"], "fixture-only");
        std::fs::remove_dir_all(data).unwrap();
    }
}
