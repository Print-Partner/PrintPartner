//! SQLite `file:` URIs for paths on disk.
//!
//! SQLite decodes `%HH` in a URI path, so every byte outside the unreserved
//! set and `/` is escaped. On Unix the URI is `file:` followed by the escaped
//! path, unchanged from before. On Windows SQLite cannot open a URI that holds
//! a verbatim `\\?\` prefix, which `std::fs::canonicalize` returns, so the
//! prefix is removed, separators become `/`, and the path follows `file:///`:
//! `\\?\C:\data\db` becomes `file:///C:/data/db` and `\\?\UNC\host\share\db`
//! becomes `file:////host/share/db`.

use std::path::Path;

/// A read-only `immutable=1` URI for `path`, or `None` when the path is not
/// UTF-8.
pub fn immutable_uri(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let (prefix, text) = platform_path(text);
    let mut uri = String::from(prefix);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || b"/-._~".contains(&byte)
            || (cfg!(windows) && byte == b':')
        {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    Some(uri)
}

#[cfg(unix)]
fn platform_path(text: &str) -> (&'static str, std::borrow::Cow<'_, str>) {
    ("file:", text.into())
}

#[cfg(windows)]
fn platform_path(text: &str) -> (&'static str, std::borrow::Cow<'_, str>) {
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return ("file:////", unc.replace('\\', "/").into());
    }
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    if let Some(unc) = text.strip_prefix(r"\\") {
        return ("file:////", unc.replace('\\', "/").into());
    }
    ("file:///", text.replace('\\', "/").into())
}

#[cfg(test)]
mod tests {
    use super::immutable_uri;
    use std::path::Path;

    #[cfg(unix)]
    #[test]
    fn unix_paths_keep_the_file_prefix_and_escape_reserved_bytes() {
        assert_eq!(
            immutable_uri(Path::new("/data/Print Partner/a?b#c%.sqlite")).unwrap(),
            "file:/data/Print%20Partner/a%3Fb%23c%25.sqlite?immutable=1"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_drop_the_verbatim_prefix_and_use_forward_slashes() {
        assert_eq!(
            immutable_uri(Path::new(r"\\?\C:\Users\RUNNER~1\Print Partner\db.sqlite")).unwrap(),
            "file:///C:/Users/RUNNER~1/Print%20Partner/db.sqlite?immutable=1"
        );
        assert_eq!(
            immutable_uri(Path::new(r"C:\data\db.sqlite")).unwrap(),
            "file:///C:/data/db.sqlite?immutable=1"
        );
        assert_eq!(
            immutable_uri(Path::new(r"\\?\UNC\host\share\db.sqlite")).unwrap(),
            "file:////host/share/db.sqlite?immutable=1"
        );
        assert_eq!(
            immutable_uri(Path::new(r"\\host\share\a#b.sqlite")).unwrap(),
            "file:////host/share/a%23b.sqlite?immutable=1"
        );
    }

    #[test]
    fn opened_uri_reads_a_database_under_a_canonical_path() {
        let root = tempfile::tempdir().unwrap();
        let directory = std::fs::canonicalize(root.path()).unwrap().join("a b#c");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("db.sqlite");
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE t(v); INSERT INTO t VALUES (7);")
            .unwrap();
        let read = rusqlite::Connection::open_with_flags(
            immutable_uri(&path).unwrap(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .unwrap();
        let value: i64 = read
            .query_row("SELECT v FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, 7);
        assert!(!directory.join("db.sqlite-wal").exists());
    }
}
