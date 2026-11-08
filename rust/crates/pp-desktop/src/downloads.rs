//! Admission and placement for webview downloads.
//!
//! The main webview may download only from the bound origin, directly or
//! through a `blob:` URL that origin created. Every admitted file lands in the
//! user's Downloads directory under one sanitized name that no existing path
//! or earlier download uses. The webview's proposed path contributes only its
//! last component, because wry and WebView2 choose it from the response.
//!
//! The check sees the request URL. A same-origin response that redirects
//! elsewhere is not re-checked here.

use crate::AllowedOrigin;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::Url;

const MAIN_WEBVIEW: &str = "main";
const FALLBACK_NAME: &str = "download";
// Leaves room for a " (999)" suffix inside the common 255-byte name limit.
const NAME_BYTES: usize = 240;
const MAX_SUFFIX: u32 = 999;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadDenied {
    ForeignWebview,
    ForeignUrl,
    NoDownloadsDirectory,
    NoFreeName,
}

impl DownloadDenied {
    pub fn code(self) -> &'static str {
        match self {
            Self::ForeignWebview => "download_foreign_webview",
            Self::ForeignUrl => "download_foreign_url",
            Self::NoDownloadsDirectory => "download_no_downloads_directory",
            Self::NoFreeName => "download_no_free_name",
        }
    }
}

pub struct DownloadPolicy {
    origin: AllowedOrigin,
    directory: Option<PathBuf>,
    reserved: Mutex<HashSet<PathBuf>>,
}

impl DownloadPolicy {
    pub fn new(origin: AllowedOrigin, directory: Option<PathBuf>) -> Self {
        Self {
            origin,
            directory: directory.filter(|path| path.is_absolute()),
            reserved: Mutex::new(HashSet::new()),
        }
    }

    /// Returns the path the webview must write to, or why it must not.
    pub fn place(
        &self,
        webview: &str,
        url: &Url,
        proposed: &Path,
    ) -> Result<PathBuf, DownloadDenied> {
        if webview != MAIN_WEBVIEW {
            return Err(DownloadDenied::ForeignWebview);
        }
        if !self.admits(url) {
            return Err(DownloadDenied::ForeignUrl);
        }
        let directory = self
            .directory
            .as_deref()
            .ok_or(DownloadDenied::NoDownloadsDirectory)?;
        let name = file_name(proposed);
        let mut reserved = self.reserved.lock().unwrap_or_else(|e| e.into_inner());
        (0..=MAX_SUFFIX)
            .map(|n| directory.join(numbered(&name, n)))
            .find(|path| {
                !reserved.contains(path)
                    && matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
            })
            .inspect(|path| {
                reserved.insert(path.clone());
            })
            .ok_or(DownloadDenied::NoFreeName)
    }

    fn admits(&self, url: &Url) -> bool {
        if url.scheme() == "blob" {
            // `blob:` URLs have an opaque origin; the creator's URL is the path.
            return Url::parse(url.path()).is_ok_and(|inner| self.origin.admits(&inner));
        }
        self.origin.admits(url)
    }
}

/// Keeps the last component of `proposed`, splitting on both separators so a
/// Windows path cannot smuggle directories on Unix, then strips characters and
/// forms that Windows, macOS or Linux would treat as anything but a plain name.
fn file_name(proposed: &Path) -> String {
    let raw = proposed.to_string_lossy();
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
        .collect();
    let mut name = cleaned.trim_start_matches(['.', ' ']).to_owned();
    if name.len() > NAME_BYTES {
        let mut end = NAME_BYTES;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    let name = name.trim_end_matches(['.', ' ']);
    if name.is_empty() || reserved_on_windows(name) {
        FALLBACK_NAME.to_owned()
    } else {
        name.to_owned()
    }
}

fn reserved_on_windows(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
}

fn numbered(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_owned();
    }
    match name.rfind('.').filter(|&dot| dot > 0) {
        Some(dot) => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        None => format!("{name} ({n})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "http://127.0.0.1:46687";

    fn policy(directory: Option<PathBuf>) -> DownloadPolicy {
        DownloadPolicy::new(AllowedOrigin::parse(ORIGIN).unwrap(), directory)
    }

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }

    #[test]
    fn ticket_t_68_download_admits_bound_origin_and_its_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy(Some(dir.path().to_path_buf()));
        for (source, proposed, expected) in [
            (
                "http://127.0.0.1:46687/jobs/j1/artifact?x=1",
                "plate.3mf",
                "plate.3mf",
            ),
            (
                "blob:http://127.0.0.1:46687/00000000-0000-4000-8000-000000000001",
                "parts.csv",
                "parts.csv",
            ),
            (
                "BLOB:http://127.0.0.1:46687/00000000-0000-4000-8000-000000000002",
                "logs.json",
                "logs.json",
            ),
        ] {
            assert_eq!(
                policy.place("main", &url(source), Path::new(proposed)),
                Ok(dir.path().join(expected)),
                "{source}"
            );
        }
    }

    #[test]
    fn ticket_t_68_download_denies_foreign_urls_and_nested_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy(Some(dir.path().to_path_buf()));
        for source in [
            "http://127.0.0.1:46688/a.zip",
            "http://localhost:46687/a.zip",
            "https://127.0.0.1:46687/a.zip",
            "http://owner@127.0.0.1:46687/a.zip",
            "http://:secret@127.0.0.1:46687/a.zip",
            "https://example.com/a.zip",
            "blob:https://example.com/00000000-0000-4000-8000-000000000001",
            "blob:http://127.0.0.1:46688/00000000-0000-4000-8000-000000000001",
            "blob:http://owner@127.0.0.1:46687/00000000-0000-4000-8000-000000000001",
            "blob:blob:http://127.0.0.1:46687/00000000-0000-4000-8000-000000000001",
            "blob:null/00000000-0000-4000-8000-000000000001",
            "data:text/plain,hello",
            "file:///etc/passwd",
            "about:blank",
            "javascript:void(0)",
            "ws://127.0.0.1:46687/a.zip",
        ] {
            assert_eq!(
                policy.place("main", &url(source), Path::new("a.zip")),
                Err(DownloadDenied::ForeignUrl),
                "{source}"
            );
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn ticket_t_68_download_denies_other_webviews() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy(Some(dir.path().to_path_buf()));
        for webview in ["", "Main", "main-2", "sponsor"] {
            assert_eq!(
                policy.place(
                    webview,
                    &url("http://127.0.0.1:46687/a.zip"),
                    Path::new("a.zip")
                ),
                Err(DownloadDenied::ForeignWebview),
                "{webview}"
            );
        }
    }

    #[test]
    fn ticket_t_68_download_denies_without_an_absolute_downloads_directory() {
        for directory in [None, Some(PathBuf::from("relative/Downloads"))] {
            assert_eq!(
                policy(directory).place(
                    "main",
                    &url("http://127.0.0.1:46687/a.zip"),
                    Path::new("/home/user/Downloads/a.zip")
                ),
                Err(DownloadDenied::NoDownloadsDirectory)
            );
        }
    }

    #[test]
    fn ticket_t_68_download_keeps_one_sanitized_name_inside_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("{}.zip", "é".repeat(200));
        for (proposed, expected) in [
            ("/home/user/Downloads/plate.3mf", "plate.3mf"),
            ("/home/user/.ssh/authorized_keys", "authorized_keys"),
            ("../../../../etc/cron.d/x", "x"),
            ("C:\\Windows\\System32\\x.dll", "x.dll"),
            ("\\\\?\\C:\\x.zip", "x.zip"),
            ("a/b.zip", "b.zip"),
            ("..", "download"),
            (".", "download"),
            ("", "download"),
            (".hidden", "hidden"),
            ("report.csv. . ", "report.csv"),
            ("a:b.zip", "ab.zip"),
            ("a\u{0}b\nc.zip", "abc.zip"),
            ("what?<*>|\".txt", "what.txt"),
            ("CON", "download"),
            ("con.txt", "download"),
            ("COM1.png", "download"),
            ("lpt9", "download"),
            ("console.txt", "console.txt"),
            ("COM10.txt", "COM10.txt"),
        ] {
            let policy = policy(Some(dir.path().to_path_buf()));
            let placed = policy
                .place(
                    "main",
                    &url("http://127.0.0.1:46687/a"),
                    Path::new(proposed),
                )
                .unwrap();
            assert_eq!(placed, dir.path().join(expected), "{proposed:?}");
        }
        let policy = policy(Some(dir.path().to_path_buf()));
        let placed = policy
            .place("main", &url("http://127.0.0.1:46687/a"), Path::new(&long))
            .unwrap();
        let name = placed.file_name().unwrap().to_str().unwrap();
        assert_eq!(placed.parent(), Some(dir.path()));
        assert!(name.len() <= NAME_BYTES && name.starts_with('é'), "{name}");
    }

    #[test]
    fn ticket_t_68_download_never_reuses_an_existing_path_or_earlier_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plate.3mf"), b"kept").unwrap();
        #[cfg(unix)]
        use std::os::unix::fs::symlink as symlink_file;
        #[cfg(windows)]
        use std::os::windows::fs::symlink_file;
        symlink_file(
            dir.path().join("missing-target"),
            dir.path().join("plate (1).3mf"),
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("plate (2).3mf")).unwrap();
        let policy = policy(Some(dir.path().to_path_buf()));
        let source = url("http://127.0.0.1:46687/jobs/j1/artifact");
        let first = policy
            .place("main", &source, Path::new("plate.3mf"))
            .unwrap();
        let second = policy
            .place("main", &source, Path::new("plate.3mf"))
            .unwrap();
        assert_eq!(first, dir.path().join("plate (3).3mf"));
        assert_eq!(second, dir.path().join("plate (4).3mf"));
        assert_eq!(
            std::fs::read(dir.path().join("plate.3mf")).unwrap(),
            b"kept"
        );
        assert_eq!(
            policy.place("main", &source, Path::new("noext")).unwrap(),
            dir.path().join("noext")
        );
        assert_eq!(
            policy.place("main", &source, Path::new("noext")).unwrap(),
            dir.path().join("noext (1)")
        );
    }

    #[test]
    fn ticket_t_68_download_denies_when_every_name_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy(Some(dir.path().to_path_buf()));
        let source = url("http://127.0.0.1:46687/a");
        for _ in 0..=MAX_SUFFIX {
            policy.place("main", &source, Path::new("a.zip")).unwrap();
        }
        assert_eq!(
            policy.place("main", &source, Path::new("a.zip")),
            Err(DownloadDenied::NoFreeName)
        );
    }
}
