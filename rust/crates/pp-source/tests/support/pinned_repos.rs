use std::{fs, path::Path};

/// Try to move `repos` under `root` aside to `aside` while an open Source
/// holds a handle below it. Windows refuses to rename a directory while a
/// handle below it is open, so the open Source keeps its path from being
/// replaced at all. Unix renames it and leaves an empty `repos` in its place.
/// Returns the name the Source's files now live under.
pub fn move_repos_aside(root: &Path, aside: &'static str) -> &'static str {
    let renamed = fs::rename(root.join("repos"), root.join(aside));
    if cfg!(windows) {
        assert_eq!(
            renamed.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        return "repos";
    }
    renamed.unwrap();
    fs::create_dir(root.join("repos")).unwrap();
    aside
}
