//! Bounded, re-scrubbed core-log records with stable file/offset cursors.
use super::{ARCHIVES, CORE, LINE_BYTES, platform, view_directory};
use serde_json::Value;
use std::{
    collections::{HashSet, VecDeque},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub type DiagnosticRecord = (String, std::sync::Arc<std::fs::File>, u64, Value);

impl super::CoreLog {
    /// Complete scrubbed rows and pinned (file identity, handle, end offset)
    /// cursors. Up to 1,000 rows and 64 KiB per file; unreadable files fail closed.
    pub fn diagnostic_records(data: &Path) -> std::io::Result<Vec<DiagnosticRecord>> {
        let directory = match view_directory(data) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let scrubber = crate::redact::Scrubber::default();
        // Pin active first: rotation must not move it behind the scan.
        let mut files = Vec::new();
        let mut seen = HashSet::new();
        for index in 0..=ARCHIVES {
            let name = if index == 0 {
                CORE.active()
            } else {
                CORE.archive(index)
            };
            let file = match platform::open_tail(&directory.join(name)) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(std::io::ErrorKind::InvalidInput.into());
            }
            let id = platform::file_id(&file)?;
            if seen.insert(id.clone()) {
                files.push((id, file));
            }
        }
        let mut records = Vec::new();
        for (id, mut file) in files.into_iter().rev() {
            let mut rows = VecDeque::new();
            let start = file.metadata()?.len().saturating_sub(65536);
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::new();
            (&mut file).take(65536).read_to_end(&mut bytes)?;
            let handle = std::sync::Arc::new(file);
            let mut offset = start;
            for row in bytes.split_inclusive(|byte| *byte == b'\n') {
                let row_start = offset;
                offset += row.len() as u64;
                if (start != 0 && row_start == start)
                    || row.len() > LINE_BYTES
                    || row.last() != Some(&b'\n')
                {
                    continue;
                }
                let Ok(mut value) = serde_json::from_slice::<Value>(row) else {
                    continue;
                };
                if !value
                    .get("level")
                    .and_then(Value::as_u64)
                    .is_some_and(|n| [10, 20, 30, 40, 50, 60].contains(&n))
                {
                    continue;
                }
                scrubber.json(&mut value);
                if rows.len() == 1000 {
                    rows.pop_front();
                }
                rows.push_back((id.clone(), handle.clone(), offset, value));
            }
            records.extend(rows);
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use crate::logs::{CoreLog, test_support};
    #[test]
    fn cursors_survive_rotation_and_identical_appends_and_refuse_symlinks() {
        let data = std::env::temp_dir().join(format!(
            "pp-log-reader-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let log = CoreLog::open(&data).unwrap();
        let row = "{\"time\":1,\"level\":30,\"token\":\"fixture-only\",\"message\":\"same\"}\n";
        let active = data.join("logs/core.log");
        std::fs::write(&active, row).unwrap();
        let before = CoreLog::diagnostic_records(&data).unwrap();
        std::fs::rename(&active, data.join("logs/core.1.log")).unwrap();
        std::fs::write(&active, format!("{row}{row}invalid\npartial")).unwrap();
        let after = CoreLog::diagnostic_records(&data).unwrap();
        assert_eq!(after.len(), 3);
        assert_eq!(before[0].0, after[0].0);
        assert_ne!(before[0].0, after[1].0);
        assert!(after[2].2 > after[1].2);
        assert_ne!(after[0].3["token"], "fixture-only");
        drop(before);
        drop(after);
        std::fs::remove_file(&active).unwrap();
        test_support::symlink_file(&data.join("logs/core.1.log"), &active);
        assert!(CoreLog::diagnostic_records(&data).is_err());
        std::fs::remove_file(&active).unwrap();
        std::fs::write(&active, "{\"level\":30}\n".repeat(1200)).unwrap();
        // Clear must see every retained file, even when the UI cap would
        // otherwise evict all rows of the older archive.
        assert_eq!(CoreLog::diagnostic_records(&data).unwrap().len(), 1001);
        drop(log);
        std::fs::remove_dir_all(data).unwrap();
    }
}
