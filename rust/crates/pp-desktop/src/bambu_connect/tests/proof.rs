use super::{BambuConnect, DENIED, IMPORT_URL, OPEN_FAILED};
use crate::AllowedOrigin;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tauri::{Url, ipc::Origin};

const MAIN: &str = "http://127.0.0.1:46687";
const OTHER: &str = "http://127.0.0.1:46688";
const ROOT: &str = "bambu-connect-handoff";
const NAME: &str = "Plate A & B #2 + print";
const PRINT: &[u8] = b"; synthetic staged print\n";
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Fixture {
    _directory: tempfile::TempDir,
    data: PathBuf,
    path: PathBuf,
    current: Url,
    link: Url,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().canonicalize().unwrap();
        let staging = data.join(ROOT);
        fs::create_dir(&staging).unwrap();
        let path = staging.join(format!("7-{DIGEST}.gcode"));
        fs::write(&path, PRINT).unwrap();
        let current = Url::parse(&format!("{MAIN}/production?plan=7#send")).unwrap();
        let link = import_link(&path);
        Self {
            _directory: directory,
            data,
            path,
            current,
            link,
        }
    }

    fn action(&self, origin: &str) -> BambuConnect {
        BambuConnect::new(AllowedOrigin::parse(origin).unwrap(), self.data.clone())
    }

    fn assert_unchanged(&self) {
        assert_eq!(fs::read(&self.path).unwrap(), PRINT);
        let entries = fs::read_dir(self.data.join(ROOT))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(entries, [self.path.clone()]);
    }
}

fn import_link(path: &Path) -> Url {
    let mut url = Url::parse(IMPORT_URL).unwrap();
    url.query_pairs_mut()
        .append_pair("path", path.to_str().unwrap())
        .append_pair("name", NAME)
        .append_pair("version", "1.0.0");
    url
}

#[derive(Default)]
struct Opener {
    calls: Vec<Url>,
    fail: bool,
}

impl Opener {
    async fn invoke(
        &mut self,
        action: &BambuConnect,
        window: &str,
        webview: &str,
        current: &Url,
        link: &Url,
    ) -> Result<(), &'static str> {
        action
            .handoff(window, webview, current, link.as_str(), |link| {
                self.calls.push(link);
                std::future::ready(if self.fail {
                    Err("fake opener failed")
                } else {
                    Ok(())
                })
            })
            .await
    }

    async fn from_main(
        &mut self,
        action: &BambuConnect,
        fixture: &Fixture,
    ) -> Result<(), &'static str> {
        self.invoke(action, "main", "main", &fixture.current, &fixture.link)
            .await
    }
}

#[tokio::test]
async fn explicit_native_request_passes_one_staged_url_to_the_fake_opener() {
    let staged = Fixture::new();
    let action = staged.action(MAIN);
    let mut opener = Opener::default();
    assert_eq!(opener.from_main(&action, &staged).await, Ok(()));
    let [opened] = opener.calls.as_slice() else {
        panic!("expected one native open: {:?}", opener.calls);
    };
    assert_eq!(opened.as_str(), staged.link.as_str());
    assert_eq!(opened.scheme(), "bambu-connect");
    assert_eq!(opened.host_str(), Some("import-file"));
    assert_eq!(
        opened.query_pairs().collect::<Vec<_>>(),
        [
            ("path".into(), staged.path.to_str().unwrap().into()),
            ("name".into(), NAME.into()),
            ("version".into(), "1.0.0".into())
        ]
    );
    staged.assert_unchanged();
}

#[tokio::test]
async fn opener_failure_is_reported_and_only_a_new_invocation_tries_again() {
    let staged = Fixture::new();
    let action = staged.action(MAIN);
    let mut opener = Opener {
        fail: true,
        ..Opener::default()
    };
    assert_eq!(opener.from_main(&action, &staged).await, Err(OPEN_FAILED));
    assert_eq!(opener.calls, [staged.link.clone()]);
    staged.assert_unchanged();

    opener.fail = false;
    assert_eq!(opener.from_main(&action, &staged).await, Ok(()));
    assert_eq!(opener.calls, [staged.link.clone(), staged.link.clone()]);
    staged.assert_unchanged();
}

#[tokio::test]
async fn ineligible_windows_webviews_and_origins_never_reach_the_fake_opener() {
    let staged = Fixture::new();
    let action = staged.action(MAIN);
    let mut opener = Opener::default();
    for (window, webview, current) in [
        ("main", "other", staged.current.clone()),
        ("other", "main", staged.current.clone()),
        ("main", "main", Url::parse(OTHER).unwrap()),
        (
            "main",
            "main",
            Url::parse("http://localhost:46687/production").unwrap(),
        ),
        (
            "main",
            "main",
            Url::parse("https://127.0.0.1:46687/production").unwrap(),
        ),
        (
            "main",
            "main",
            Url::parse("http://user@127.0.0.1:46687/production").unwrap(),
        ),
    ] {
        assert_eq!(
            opener
                .invoke(&action, window, webview, &current, &staged.link)
                .await,
            Err(DENIED),
            "{window}/{webview}: {current}"
        );
    }
    let relaunched = staged.action(OTHER);
    assert_eq!(opener.from_main(&relaunched, &staged).await, Err(DENIED));
    assert!(opener.calls.is_empty());
    assert_eq!(
        opener
            .invoke(
                &relaunched,
                "main",
                "main",
                &Url::parse(OTHER).unwrap(),
                &staged.link
            )
            .await,
        Ok(())
    );
    assert_eq!(opener.calls, [staged.link.clone()]);
    staged.assert_unchanged();
}

#[tokio::test]
async fn unavailable_or_unowned_staging_paths_never_reach_the_fake_opener() {
    let staged = Fixture::new();
    let action = staged.action(MAIN);
    let mut opener = Opener::default();
    let outside = staged.data.join(staged.path.file_name().unwrap());
    fs::write(&outside, PRINT).unwrap();
    let invalid_name = staged.path.with_extension("stl");
    fs::write(&invalid_name, PRINT).unwrap();
    let directory = staged.path.with_file_name(format!("8-{DIGEST}.gcode"));
    fs::create_dir(&directory).unwrap();
    let nonexistent = staged.path.with_file_name(format!("9-{DIGEST}.gcode"));
    let relative = Path::new(ROOT).join(staged.path.file_name().unwrap());
    for path in [&outside, &invalid_name, &directory, &nonexistent, &relative] {
        let link = import_link(path);
        assert_eq!(
            opener
                .invoke(&action, "main", "main", &staged.current, &link)
                .await,
            Err(DENIED),
            "{path:?}"
        );
    }

    let alias = staged.data.join("hard-link.gcode");
    fs::hard_link(&staged.path, &alias).unwrap();
    assert_eq!(opener.from_main(&action, &staged).await, Err(DENIED));
    fs::remove_file(alias).unwrap();
    assert!(opener.calls.is_empty());

    assert_eq!(opener.from_main(&action, &staged).await, Ok(()));
    fs::remove_file(&staged.path).unwrap();
    assert_eq!(opener.from_main(&action, &staged).await, Err(DENIED));
    assert_eq!(opener.calls, [staged.link.clone()]);
    assert_eq!(fs::read(outside).unwrap(), PRINT);
}

#[cfg(unix)]
#[tokio::test]
async fn a_replaced_artifact_or_staging_directory_cannot_redirect_the_opener() {
    use std::os::unix::fs::symlink;

    let staged = Fixture::new();
    let action = staged.action(MAIN);
    let mut opener = Opener::default();
    let dir = staged.data.join(ROOT);
    let moved = staged.data.join("moved");
    fs::rename(&dir, &moved).unwrap();
    symlink(&moved, &dir).unwrap();
    assert_eq!(opener.from_main(&action, &staged).await, Err(DENIED));
    fs::remove_file(&dir).unwrap();
    fs::create_dir(&dir).unwrap();
    let original = moved.join(staged.path.file_name().unwrap());
    symlink(&original, &staged.path).unwrap();
    assert_eq!(opener.from_main(&action, &staged).await, Err(DENIED));
    assert!(opener.calls.is_empty());
    assert_eq!(fs::read(original).unwrap(), PRINT);
}

pub(crate) fn verify_authority(context: &mut tauri::Context<tauri::Wry>) {
    let staged = Fixture::new();
    let action = staged.action(MAIN);
    let allowed = Origin::Remote {
        url: staged.current,
    };
    let authority = context.runtime_authority_mut();
    let command = "open_bambu_connect";
    assert!(
        authority
            .resolve_access(command, "main", "main", &allowed)
            .is_none()
    );
    authority.add_capability(action.capability()).unwrap();
    assert!(
        authority
            .resolve_access(command, "main", "main", &allowed)
            .is_some()
    );
    assert!(
        authority
            .resolve_access(command, "main", "other", &allowed)
            .is_none()
    );
    for origin in [
        Origin::Local,
        Origin::Remote {
            url: Url::parse(OTHER).unwrap(),
        },
    ] {
        assert!(
            authority
                .resolve_access(command, "main", "main", &origin)
                .is_none()
        );
    }
    for command in [
        "open_sponsor",
        "sync_portable_settings",
        "plugin:shell|execute",
    ] {
        assert!(
            authority
                .resolve_access(command, "main", "main", &allowed)
                .is_none()
        );
    }
}
