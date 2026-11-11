//! Admin file reads with Node-compatible JSON serialization (GRE-438).

use crate::source_scan::node_json::{Property, property};
use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

/// The owner's data directory, for reads of files Node writes there.
#[derive(Clone, Debug)]
pub struct AdminFiles {
    data_dir: PathBuf,
}

impl crate::WriterOwner {
    pub fn admin_files(&self) -> Result<AdminFiles> {
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow!("Storage owner is closed"))?;
        Ok(AdminFiles {
            data_dir: lease.data_dir().to_owned(),
        })
    }
}

impl AdminFiles {
    /// Admitted owner directory for core diagnostics.
    pub fn data_directory(&self) -> &Path {
        &self.data_dir
    }

    pub fn custom_filaments(&self) -> CustomFilaments {
        custom_filaments(&self.data_dir)
    }
}

/// The body Node's `GET /filaments/custom` sends: `{filaments}` from
/// `services/custom-filaments.ts::loadStore`.
#[derive(Debug, PartialEq, Eq)]
pub enum CustomFilaments {
    /// Exact JSON text of `JSON.stringify({filaments: store.filaments})`.
    Body(String),
    /// The store file is the JSON literal `null`; Node's handler throws.
    NullStore,
}

/// Reads `<data_dir>/custom_filaments.json` as `loadStore` does. A missing or
/// unreadable file, or text `JSON.parse` rejects, is the empty store. Bytes
/// that are not UTF-8 decode with U+FFFD, as `readFileSync(path, "utf8")`
/// does, and a byte order mark stays and fails the parse.
pub fn custom_filaments(data_dir: &Path) -> CustomFilaments {
    // ast-grep-ignore: no-writer-family-literal -- Read side of the filaments writer family.
    let text = match std::fs::read(data_dir.join("custom_filaments.json")) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => return CustomFilaments::Body(r#"{"filaments":[]}"#.into()),
    };
    match property(&text, "filaments") {
        Property::Unparsable => CustomFilaments::Body(r#"{"filaments":[]}"#.into()),
        Property::Null => CustomFilaments::NullStore,
        // `JSON.stringify` drops a member whose value is `undefined`.
        Property::Missing => CustomFilaments::Body("{}".into()),
        Property::Value(value) => CustomFilaments::Body(format!(r#"{{"filaments":{value}}}"#)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(contents: &[u8]) -> CustomFilaments {
        let dir = tempfile::tempdir().unwrap();
        // ast-grep-ignore: no-writer-family-literal -- Test fixture stands in for the Node-written store.
        std::fs::write(dir.path().join("custom_filaments.json"), contents).unwrap();
        custom_filaments(dir.path())
    }

    fn body(text: &str) -> CustomFilaments {
        CustomFilaments::Body(text.into())
    }

    #[test]
    fn ticket_t_62_admin_custom_filaments_follow_load_store() {
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(custom_filaments(empty.path()), body(r#"{"filaments":[]}"#));
        assert_eq!(read(b"not json"), body(r#"{"filaments":[]}"#));
        assert_eq!(read(b"\xEF\xBB\xBF{}"), body(r#"{"filaments":[]}"#));
        assert_eq!(read(b"null"), CustomFilaments::NullStore);
        assert_eq!(read(b"[]"), body("{}"));
        assert_eq!(read(b"{}"), body("{}"));
        assert_eq!(
            read(br##"{"filaments":[{"id":"custom-1","hex":"#AABBCC","n":1.50,"big":1e400}]}"##),
            body(r##"{"filaments":[{"id":"custom-1","hex":"#AABBCC","n":1.5,"big":null}]}"##)
        );
        assert_eq!(
            read(br#"{"filaments":1,"filaments":"\ud800"}"#),
            body(r#"{"filaments":"\ud800"}"#)
        );
        assert_eq!(
            read(b"{\"filaments\":\"\xFF\"}"),
            body("{\"filaments\":\"\u{FFFD}\"}")
        );
    }
}
