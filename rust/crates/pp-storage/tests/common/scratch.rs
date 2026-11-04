use std::ops::Deref;
use std::path::Path;

/// Creates an auto-removing directory with mode `0o700`.
///
/// `WriterOwner::open` validates the directory and every ancestor: they must
/// be owned by root or the current user and have no group/world write (unless
/// sticky). tempfile defaults to `0o777` minus umask, so a permissive umask
/// like `0o002` would produce a group-writable directory that `WriterOwner`
/// refuses. Pin `0o700` so the helper works under any umask.
///
/// The directory is created under the canonical temp root, so its path has no
/// symlink ancestors (such as `/var` on macOS), which source roots reject.
fn temp_dir(prefix: &str) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .prefix(prefix)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap()
}

/// An auto-removing scratch directory for pp-storage tests.
///
/// Removed when the value drops, including while a test unwinds from a panic.
/// Deref to `Path` lets it stand in for the `PathBuf` the tests used to build
/// by hand, so call sites that pass `&scratch` or call `scratch.join(...)`
/// keep working unchanged.
pub struct Scratch {
    inner: tempfile::TempDir,
}

impl Scratch {
    pub fn new(prefix: &str) -> Self {
        Self {
            inner: temp_dir(prefix),
        }
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        self.inner.path()
    }
}

impl Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        self.inner.path()
    }
}
