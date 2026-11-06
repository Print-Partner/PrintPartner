use crate::{Error, Result};
use pp_os::{Dir, FileOpen};
use std::{
    fs::File,
    io,
    path::{Component, Path, PathBuf},
};

pub(crate) struct Directory(pub(crate) Dir);

impl Directory {
    pub(crate) fn open_root(path: &Path) -> Result<Self> {
        Self::open_root_as(path, pp_os::effective_uid())
    }

    /// Opens `path` as the confinement root on behalf of `uid`. Every pinned
    /// component, from `/` to the root itself, is checked through its own
    /// handle so a replaced path cannot slip past the ownership check. On
    /// Windows, which has no POSIX owner, the walk still pins every component
    /// and refuses links and junctions, but checks no ownership: pp-os does
    /// not inspect ACLs yet.
    fn open_root_as(path: &Path, uid: Option<u32>) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Error::UnsafePath);
        }
        // `/` on Unix; the drive or share prefix and its root on Windows.
        let mut components = path.components().peekable();
        let mut anchor = PathBuf::new();
        while let Some(part @ (Component::Prefix(_) | Component::RootDir)) = components.peek() {
            anchor.push(part.as_os_str());
            components.next();
        }
        let mut root = Self(Dir::open_search(&anchor)?);
        // The anchor is already pinned. A sticky directory is safe only when a
        // subsequently checked component confines access below it.
        root.check_ancestor(uid, components.peek().is_some())?;
        while let Some(component) = components.next() {
            match component {
                Component::Normal(name) => {
                    let name = name.to_str().ok_or(Error::UnsafePath)?;
                    root = Self(root.0.open_search_nofollow(name)?);
                    root.check_ancestor(uid, components.peek().is_some())?;
                }
                _ => return Err(Error::UnsafePath),
            }
        }
        // Only the selected root needs enumeration and fsync support. Reopen
        // the pinned directory rather than resolving the configured path again.
        Ok(Self(root.0.reopen()?))
    }

    fn check_ancestor(&self, uid: Option<u32>, allow_sticky: bool) -> Result<()> {
        let (Some(uid), Some(access)) = (uid, self.0.posix_access()?) else {
            return Ok(());
        };
        if !ancestor_is_trusted(access.uid(), access.mode(), uid, allow_sticky) {
            return Err(Error::UnsafeAncestor);
        }
        // ACLs supplement mode bits on macOS; read them from the same handle.
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsFd;
            if !crate::acl::entries_are_trusted(&crate::acl::read(self.0.as_fd())?, uid) {
                return Err(Error::UnsafeAcl);
            }
        }
        Ok(())
    }

    pub(crate) fn child(&self, name: &str, create: bool) -> Result<Self> {
        if create {
            match self.0.create_dir(name) {
                Ok(()) => self.sync()?,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Self(self.0.open_dir_nofollow(name)?))
    }

    pub(crate) fn parent(&self, path: &str, create: bool) -> Result<(Self, String)> {
        let mut parent = Self(self.0.try_clone()?);
        let mut segments = path.split('/').peekable();
        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                return Ok((parent, segment.to_owned()));
            }
            parent = parent.child(segment, create)?;
        }
        Err(Error::UnsafePath)
    }

    pub(crate) fn file(&self, path: &str, create: bool) -> Result<File> {
        let (parent, name) = self.parent(path, create)?;
        let how = if create {
            FileOpen::CreateWriteOnly
        } else {
            FileOpen::Read
        };
        regular(parent.0.open_file(&name, how)?)
    }

    pub(crate) fn file_read_write(&self, path: &str) -> Result<File> {
        let (parent, name) = self.parent(path, false)?;
        regular(parent.0.open_file(&name, FileOpen::ReadWrite)?)
    }

    pub(crate) fn entries(&self) -> Result<Vec<String>> {
        let mut entries = Vec::new();
        for entry in self.0.entries()? {
            let name = entry?.into_string().map_err(|_| Error::UnsafePath)?;
            if entries.len() > crate::MAX_ENTRIES {
                return Err(Error::Limit);
            }
            entries.push(name);
        }
        Ok(entries)
    }

    pub(crate) fn remove_tree(&self, name: &str) -> Result<()> {
        match self.child(name, false) {
            Ok(child) => {
                for entry in child.entries()? {
                    child.remove_tree(&entry)?;
                }
                self.0.remove_dir(name)?;
            }
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                self.0.remove_file(name)?;
            }
        }
        Ok(())
    }

    pub(crate) fn sync(&self) -> Result<()> {
        self.0.sync().map_err(Into::into)
    }

    pub(crate) fn publish(&self, candidate: &str, revision: &str) -> Result<()> {
        self.0.rename_no_replace(candidate, revision)?;
        self.sync()
    }

    pub(crate) fn try_clone(&self) -> Result<Self> {
        Ok(Self(self.0.try_clone()?))
    }

    /// Takes this directory's exclusive lock without waiting. On Windows
    /// the lock is [`pp_os::DIR_LOCK_FILE`] inside the directory; revision
    /// keys start with a letter or digit, so listings cannot mistake it.
    pub(crate) fn try_lock(&self) -> io::Result<()> {
        self.0.try_lock()
    }

    pub(crate) fn lock(&self) -> io::Result<()> {
        self.0.lock()
    }

    pub(crate) fn unlock(&self) -> io::Result<()> {
        self.0.unlock()
    }
}

fn regular(file: File) -> Result<File> {
    if !file.metadata()?.is_file() {
        return Err(Error::UnsafePath);
    }
    Ok(file)
}

#[cfg(feature = "desktop_rebuild")]
use pp_os::Identity;

#[cfg(feature = "desktop_rebuild")]
impl Directory {
    pub(crate) fn create_child_exclusive(&self, name: &str) -> Result<Self> {
        self.0.create_dir(name)?;
        self.child(name, false)
    }

    pub(crate) fn create_child_unsynced(&self, name: &str) -> Result<(Self, bool)> {
        let created = match self.0.create_dir(name) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        };
        Ok((self.child(name, false)?, created))
    }

    pub(crate) fn file_exclusive(&self, name: &str) -> Result<File> {
        regular(self.0.open_file(name, FileOpen::CreateWriteOnly)?)
    }

    pub(crate) fn rename_no_replace(&self, from: &str, to: &str) -> Result<()> {
        self.0.rename_no_replace(from, to)?;
        Ok(())
    }

    pub(crate) fn identity(&self) -> Result<Identity> {
        Ok(self.0.status()?.identity())
    }

    /// Removes the regular file `name` only while it is still `owned`.
    pub(crate) fn remove_owned_file(&self, name: &str, owned: Identity) -> Result<()> {
        self.require_owned_file(name, owned)?;
        self.0.remove_file(name)?;
        Ok(())
    }

    pub(crate) fn require_owned_file(&self, name: &str, owned: Identity) -> Result<()> {
        let current = pp_os::status_of(&self.file(name, false)?)?;
        if !current.is_file() || current.identity() != owned {
            return Err(Error::CorruptSnapshot);
        }
        Ok(())
    }

    /// Takes the handle so it closes as soon as the entry is gone. Without
    /// POSIX delete semantics (FAT on Windows) a removed entry keeps its name
    /// until its last handle closes, which would block removing the parent.
    pub(crate) fn remove_owned_empty_directory(&self, name: &str, owned: Directory) -> Result<()> {
        self.remove_empty_directory_owned_as(name, owned.identity()?)
    }

    /// Removes the empty directory `name` only while it is still `owned`.
    pub(crate) fn remove_empty_directory_owned_as(
        &self,
        name: &str,
        owned: Identity,
    ) -> Result<()> {
        self.require_owned_directory(name, owned)?;
        self.0.remove_dir(name)?;
        Ok(())
    }

    pub(crate) fn require_owned_directory(&self, name: &str, owned: Identity) -> Result<()> {
        let current = self.child(name, false)?.0.status()?;
        if !current.is_dir() || current.identity() != owned {
            return Err(Error::CorruptSnapshot);
        }
        Ok(())
    }
}

/// A root ancestor is trusted when only `uid` or root can replace its entries:
/// it is owned by one of them and is not group/world-writable unless sticky.
/// Sticky `/tmp`-style ancestors are accepted because the next component must
/// itself pass this check, so other users cannot rename it away. The selected
/// root has no such checked child, so it must not be group/world-writable.
fn ancestor_is_trusted(owner: u32, mode: u32, uid: u32, allow_sticky: bool) -> bool {
    const GROUP_OR_OTHER_WRITE: u32 = 0o022;
    const STICKY: u32 = 0o1000;
    (owner == uid || owner == 0)
        && (mode & GROUP_OR_OTHER_WRITE == 0 || (allow_sticky && mode & STICKY != 0))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs::{self, Permissions},
        os::unix::fs::{MetadataExt, PermissionsExt},
    };

    use crate::scratch::Scratch;

    /// Removed when the scratch drops, including while a test unwinds from a
    /// panic.
    struct Tree(Scratch);
    impl Tree {
        fn new() -> Self {
            let tree = Self(Scratch::new("pp-source-root-"));
            tree.dir("", 0o755)
        }
        fn dir(self, name: &str, mode: u32) -> Self {
            let path = self.0.join(name);
            if !path.exists() {
                fs::create_dir(&path).unwrap();
            }
            fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
            self
        }
        fn open(&self, name: &str) -> Result<Directory> {
            Directory::open_root(&self.0.join(name))
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            // Deny entries can block cleanup, so strip every ACL first.
            #[cfg(target_os = "macos")]
            chmod(&["-R", "-N"], &self.0);
        }
    }

    fn euid() -> u32 {
        pp_os::effective_uid().unwrap()
    }

    #[test]
    fn world_writable_ancestor_is_rejected() {
        let tree = Tree::new().dir("shared", 0o777).dir("shared/repos", 0o755);
        assert!(matches!(
            tree.open("shared/repos"),
            Err(Error::UnsafeAncestor)
        ));
    }

    #[test]
    fn group_writable_ancestor_is_rejected() {
        let tree = Tree::new().dir("team", 0o775).dir("team/repos", 0o700);
        assert!(matches!(
            tree.open("team/repos"),
            Err(Error::UnsafeAncestor)
        ));
    }

    #[test]
    fn world_writable_root_itself_is_rejected() {
        let tree = Tree::new().dir("repos", 0o777);
        assert!(matches!(tree.open("repos"), Err(Error::UnsafeAncestor)));
    }

    #[test]
    fn foreign_owned_ancestor_is_rejected() {
        assert!(!ancestor_is_trusted(1001, 0o755, 1000, true));
        assert!(!ancestor_is_trusted(1001, 0o1777, 1000, true));
        assert!(ancestor_is_trusted(1000, 0o755, 1000, false));
        assert!(ancestor_is_trusted(0, 0o755, 1000, false));

        let tree = Tree::new().dir("repos", 0o700);
        let root = tree.0.join("repos");
        // Unprivileged tests cannot chown, so open as a different user: the
        // fixture directories then belong to someone else. Root-owned
        // fixtures stay trusted, so only the pure policy runs as root.
        if euid() == 0 {
            return;
        }
        let other = euid().wrapping_add(1);
        assert!(Directory::open_root_as(&root, Some(euid())).is_ok());
        assert!(matches!(
            Directory::open_root_as(&root, Some(other)),
            Err(Error::UnsafeAncestor)
        ));
        // When every directory above the fixture is root-owned (Linux /tmp),
        // pin the rejection to the selected root's own owner.
        let temp = tree.0.parent().unwrap();
        if temp
            .ancestors()
            .all(|path| fs::metadata(path).unwrap().uid() == 0)
        {
            // A sticky temp directory is trusted as an intermediate ancestor,
            // even though it is no longer accepted as the selected root.
            let temp_handle = Directory(Dir::open(temp).unwrap());
            assert!(temp_handle.check_ancestor(Some(other), true).is_ok());
            assert!(matches!(
                Directory::open_root_as(&tree.0, Some(other)),
                Err(Error::UnsafeAncestor)
            ));
        }
    }

    #[test]
    fn owned_root_is_accepted() {
        let tree = Tree::new().dir("repos", 0o700).dir("repos/nested", 0o755);
        assert!(tree.open("repos").is_ok());
        assert!(tree.open("repos/nested").is_ok());
        assert!(ancestor_is_trusted(1000, 0o40755, 1000, false));
    }

    #[test]
    fn sticky_shared_ancestor_is_accepted() {
        // `/tmp`-style: world-writable with the sticky bit, owned by the user
        // (or root). Entries below it must still pass the ownership check.
        let tree = Tree::new().dir("tmp", 0o1777).dir("tmp/repos", 0o700);
        assert!(tree.open("tmp/repos").is_ok());
        assert!(ancestor_is_trusted(0, 0o41777, 1000, true));
        assert!(ancestor_is_trusted(1000, 0o1770, 1000, true));
        let tree = Tree::new().dir("tmp", 0o1777).dir("tmp/repos", 0o777);
        assert!(matches!(tree.open("tmp/repos"), Err(Error::UnsafeAncestor)));
    }

    #[test]
    fn sticky_writable_root_is_rejected() {
        for mode in [0o1777, 0o1770, 0o1707] {
            let tree = Tree::new().dir("repos", mode);
            assert!(matches!(tree.open("repos"), Err(Error::UnsafeAncestor)));
        }
    }

    #[cfg(target_os = "macos")]
    fn chmod(args: &[&str], path: &std::path::Path) {
        let status = crate::test_process::command("/bin/chmod")
            .args(args)
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "chmod {args:?} {}", path.display());
    }

    #[cfg(target_os = "macos")]
    mod macos_acl {
        use super::*;

        impl Tree {
            /// Appends `entry` (chmod +a syntax) to the ACL of `name`.
            fn acl(self, name: &str, entry: &str) -> Self {
                chmod(&["+a", entry], &self.0.join(name));
                self
            }
        }

        fn me() -> String {
            let out = crate::test_process::command("/usr/bin/id")
                .arg("-un")
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        }

        fn unsafe_acl(result: Result<Directory>) -> bool {
            matches!(result, Err(Error::UnsafeAcl))
        }

        #[test]
        fn foreign_write_grant_on_0755_root_is_rejected() {
            for rights in [
                "add_file",
                "add_subdirectory",
                "delete_child",
                "delete",
                "writesecurity",
                "chown",
                "writeattr",
                "writeextattr",
            ] {
                let tree = Tree::new()
                    .dir("repos", 0o755)
                    .acl("repos", &format!("user:nobody allow {rights}"));
                assert!(unsafe_acl(tree.open("repos")), "{rights}");
            }
        }

        #[test]
        fn foreign_write_grant_on_ancestor_is_rejected() {
            let tree = Tree::new()
                .dir("parent", 0o755)
                .dir("parent/repos", 0o700)
                .acl("parent", "group:everyone allow delete_child");
            assert!(unsafe_acl(tree.open("parent/repos")));
            // Groups are foreign principals, even the user's own.
            let tree = Tree::new()
                .dir("parent", 0o755)
                .dir("parent/repos", 0o700)
                .acl("parent", "group:staff allow add_subdirectory");
            assert!(unsafe_acl(tree.open("parent/repos")));
        }

        #[test]
        fn earlier_deny_does_not_excuse_grant() {
            let tree = Tree::new()
                .dir("repos", 0o755)
                .acl("repos", "user:nobody allow add_file")
                .acl("repos", "user:nobody deny add_file");
            assert!(unsafe_acl(tree.open("repos")));
        }

        #[test]
        fn inheritable_and_inherited_grants_are_rejected() {
            let tree = Tree::new().dir("parent", 0o755).acl(
                "parent",
                "user:nobody allow add_file,directory_inherit,only_inherit",
            );
            // Stricter than the kernel: inherit-only grants still count.
            assert!(unsafe_acl(tree.open("parent")));
            let tree = tree.dir("parent/repos", 0o700);
            chmod(&["-N"], &tree.0.join("parent"));
            assert!(tree.open("parent").is_ok());
            // The child received an effective, inherited copy of the grant.
            assert!(unsafe_acl(tree.open("parent/repos")));
        }

        #[test]
        fn owned_roots_without_unsafe_grants_are_accepted() {
            let tree = Tree::new().dir("repos", 0o755).dir("repos/nested", 0o700);
            assert!(tree.open("repos").is_ok());
            assert!(tree.open("repos/nested").is_ok());
            let tree = tree
                .acl(
                    "repos",
                    &format!(
                        "user:{} allow add_file,delete_child,writesecurity,chown",
                        me()
                    ),
                )
                .acl("repos", "user:root allow add_file")
                .acl(
                    "repos",
                    "user:nobody allow list,search,readattr,readextattr,readsecurity",
                );
            assert!(tree.open("repos/nested").is_ok());
        }

        #[test]
        fn deny_only_acls_are_accepted() {
            // Like ~/Documents: everyone is denied deleting the directory.
            let tree = Tree::new()
                .dir("parent", 0o755)
                .dir("parent/repos", 0o700)
                .acl("parent", "group:everyone deny delete")
                .acl("parent/repos", "group:everyone deny add_file,delete_child");
            assert!(tree.open("parent/repos").is_ok());
        }

        #[test]
        fn sticky_intermediate_ancestors() {
            let tree = Tree::new().dir("tmp", 0o1777).dir("tmp/repos", 0o700);
            assert!(tree.open("tmp/repos").is_ok());
            let tree = tree.acl("tmp", "group:everyone deny delete");
            assert!(tree.open("tmp/repos").is_ok());
            // Stricter than the mode check: ACL grants are not sticky-bounded.
            let tree = tree.acl("tmp", "user:nobody allow add_file");
            assert!(unsafe_acl(tree.open("tmp/repos")));
        }
    }
}
