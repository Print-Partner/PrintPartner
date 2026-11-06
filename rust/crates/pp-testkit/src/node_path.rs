//! Paths handed to a Node child process.
//!
//! `std::fs::canonicalize` returns verbatim `\\?\C:\...` paths on Windows.
//! Node's `fs.realpathSync` cannot walk them (`EISDIR: lstat 'C:'`), and its
//! ESM loader takes an absolute module path only as a `file:` URL. These
//! helpers give Node the spelling it accepts. On Unix they change nothing
//! beyond URL escaping.

use std::path::{Path, PathBuf};

/// On Windows, `path` without a verbatim prefix: `\\?\C:\x` becomes `C:\x`
/// and `\\?\UNC\host\share` becomes `\\host\share`. Other paths, and every
/// path on Unix, are unchanged.
pub fn node_path(path: &Path) -> PathBuf {
    if cfg!(not(windows)) {
        return path.to_owned();
    }
    let Some(text) = path.to_str() else {
        return path.to_owned();
    };
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_owned(),
    }
}

/// A `file:` URL for the absolute `path`, as Node's `pathToFileURL` spells
/// it: `/` separators, `file:///C:/x` for a drive path and every byte outside
/// the unreserved set, `/` and `:` escaped. `None` when the path is not UTF-8.
pub fn file_url(path: &Path) -> Option<String> {
    let text = node_path(path).to_str()?.to_owned();
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    };
    // A UNC path keeps its host as the URL authority, as pathToFileURL
    // spells it: \\host\share\x becomes file://host/share/x.
    let mut url = String::from(if text.starts_with("//") {
        "file:"
    } else if text.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::{file_url, node_path};
    use std::path::Path;

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_prefixes_are_removed_and_urls_use_forward_slashes() {
        assert_eq!(
            node_path(Path::new(r"\\?\C:\a b\c")),
            Path::new(r"C:\a b\c")
        );
        assert_eq!(
            node_path(Path::new(r"\\?\UNC\host\share\x")),
            Path::new(r"\\host\share\x")
        );
        assert_eq!(
            node_path(Path::new(r"\\?\Volume{1}\x")),
            Path::new(r"\\?\Volume{1}\x")
        );
        assert_eq!(
            file_url(Path::new(r"\\?\D:\a\loader.mjs")).unwrap(),
            "file:///D:/a/loader.mjs"
        );
        assert_eq!(
            file_url(Path::new(r"D:\a b\x.mjs")).unwrap(),
            "file:///D:/a%20b/x.mjs"
        );
        assert_eq!(
            file_url(Path::new(r"\\?\UNC\host\share\x.mjs")).unwrap(),
            "file://host/share/x.mjs"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_paths_are_kept_and_urls_escape_reserved_bytes() {
        assert_eq!(
            node_path(Path::new(r"/tmp/\\?\C:")),
            Path::new(r"/tmp/\\?\C:")
        );
        assert_eq!(
            file_url(Path::new("/tmp/a b#c.mjs")).unwrap(),
            "file:///tmp/a%20b%23c.mjs"
        );
        assert_eq!(
            file_url(Path::new(r"/tmp/a\b")).unwrap(),
            "file:///tmp/a%5Cb"
        );
    }
}
