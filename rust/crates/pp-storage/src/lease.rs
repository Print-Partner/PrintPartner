use anyhow::{Context, Result, ensure};
use pp_os::{Dir, Status};
use std::{
    collections::BTreeMap,
    ffi::CStr,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

pub struct LeaseRelease {
    /// Removal of the standalone Node compatibility reservation.
    pub marker_removed: bool,
    pub runtime_removed: bool,
    pub lock_released: bool,
    pub ownership_retained: bool,
}

pub struct StorageLease {
    compatibility: Option<CompatibilityReservation>,
    advisory_lock: Option<File>,
    admitted: Arc<AdmittedDirectory>,
    runtime_dir: PathBuf,
    release_allowed: bool,
    /// This acquisition created `printpartner.lock` rather than opening an existing one.
    lock_created: bool,
}

struct CompatibilityReservation {
    directory: Dir,
    file: File,
    held: bool,
    /// This acquisition created `.desktop-lease` rather than reusing a leftover.
    created: bool,
}

impl CompatibilityReservation {
    fn acquire(admitted: &AdmittedDirectory) -> Result<Self> {
        // Node publishes a directory at this name. A regular file prevents that
        // rename and can be reused after a crash under the kernel lock.
        let (file, created) = admitted.open_lock(c".desktop-lease").context(
            "Data directory already owned by standalone Node; stop it before starting Rust",
        )?;
        let mut held = false;
        let directory = (|| {
            #[cfg(all(test, unix))]
            tests::after_reservation_opened(&admitted.path)?;
            validate_open_lock_identity(&file, &admitted.path.join(".desktop-lease"))
                .context("Could not validate .desktop-lease compatibility reservation identity")?;
            match pp_os::try_lock(&file) {
                Ok(()) => held = true,
                Err(pp_os::TryLockError::WouldBlock) => anyhow::bail!(
                    "Data directory already owned; .desktop-lease compatibility reservation is locked"
                ),
                Err(pp_os::TryLockError::Io(error)) => {
                    return Err(error)
                        .context("Could not lock .desktop-lease compatibility reservation");
                }
            }
            validate_open_lock_identity(&file, &admitted.path.join(".desktop-lease"))
                .context("Could not validate .desktop-lease compatibility reservation identity")?;
            admitted
                .directory
                .try_clone()
                .context("Could not retain directory for .desktop-lease compatibility reservation")
        })();
        match directory {
            Ok(directory) => Ok(Self {
                directory,
                file,
                held: true,
                created,
            }),
            Err(error) => {
                if created {
                    remove_created_lease(&admitted.directory, ".desktop-lease", &file, held);
                }
                Err(error)
            }
        }
    }

    fn release(&mut self) -> std::io::Result<()> {
        if self.held {
            #[cfg(all(test, unix))]
            tests::assert_lock_held_during_reservation_release();
            let named = match self.directory.status_nofollow(".desktop-lease") {
                Ok(named) => named,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.held = false;
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            let acquired = pp_os::status_of(&self.file)?;
            if named.identity() != acquired.identity() {
                return Err(std::io::Error::other(
                    ".desktop-lease compatibility reservation was replaced",
                ));
            }
            if let Err(error) = self.directory.remove_file(".desktop-lease")
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error);
            }
            self.held = false;
        }
        Ok(())
    }
}

impl Drop for CompatibilityReservation {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Create private store directories and admit existing owner-controlled stores.
/// Existing modes and data-directory aliases are preserved. Existing lock entries
/// must be owner-controlled regular files with one link. Ancestors must be root or
/// effective-user owned and deny group/other write unless sticky. These are path
/// snapshots, not confinement. A held descriptor checks leaf identity through
/// canonicalization. Lock acquisition retains it through ownership cleanup.
/// SQLite initialization and ACL policy remain separate concerns: on Windows,
/// where `pp_os` reports no POSIX owner or mode, the owner and mode checks are
/// skipped and the store relies on the ACL it inherits from its parent.
pub fn prepare_directory(path: &Path) -> Result<PathBuf> {
    Ok(admit_directory(path)?.path)
}

pub(crate) struct AdmittedDirectory {
    path: PathBuf,
    directory: Dir,
}

pub(crate) struct LockedDirectory {
    admitted: AdmittedDirectory,
    compatibility: CompatibilityReservation,
    lock: File,
    /// This lock created `printpartner.lock` rather than opening an existing one.
    lock_created: bool,
}

/// Remove the lease file `name` that this process created, after a failed
/// acquisition. The removal runs only while this process holds the file's lock:
/// `held` says it already does, otherwise it takes the lock now and leaves the
/// file when another owner holds it. It also runs only while the name still
/// refers to the opened file. Errors are ignored; the caller reports the
/// original failure.
fn remove_created_lease(directory: &Dir, name: &str, file: &File, held: bool) {
    if !held && pp_os::try_lock(file).is_err() {
        return;
    }
    if let (Ok(named), Ok(opened)) = (directory.status_nofollow(name), pp_os::status_of(file))
        && named.identity() == opened.identity()
    {
        let _ = directory.remove_file(name);
    }
}

impl LockedDirectory {
    pub(crate) fn preflight(&self) -> Result<()> {
        self.admitted.preflight()
    }

    /// The canonical data directory this lock holds.
    pub(crate) fn path(&self) -> &Path {
        &self.admitted.path
    }

    /// The open handle of that directory.
    pub(crate) fn dir(&self) -> &Dir {
        &self.admitted.directory
    }

    /// Give up a lock that failed or refused before it became a
    /// [`StorageLease`], leaving the lease files as found: `.desktop-lease` and
    /// `printpartner.lock` are removed only if this lock created them, while
    /// the lock is still held and only when the name still refers to the file
    /// this lock opened.
    pub(crate) fn abandon(self) {
        let LockedDirectory {
            admitted,
            mut compatibility,
            lock,
            lock_created,
        } = self;
        if !compatibility.created {
            compatibility.held = false;
        }
        drop(compatibility);
        if lock_created {
            remove_created_lease(&admitted.directory, "printpartner.lock", &lock, true);
        }
    }
}

pub(crate) fn admit_directory(path: &Path) -> Result<AdmittedDirectory> {
    ensure!(!path.as_os_str().is_empty(), "Data directory path is empty");
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("admit_directory: current_dir")?
            .join(path)
    };
    validate_input_ancestors(&path)?;
    pp_os::create_private_dir_all(&path)
        .with_context(|| format!("admit_directory: create data directory {}", path.display()))?;
    let opened = Dir::open(&path).map_err(|error| {
        let kind = error.kind();
        let error = anyhow::Error::new(error).context(format!(
            "admit_directory: open data directory {}",
            path.display()
        ));
        match kind {
            std::io::ErrorKind::PermissionDenied => {
                error.context("Unsafe data directory permissions")
            }
            std::io::ErrorKind::NotADirectory => error.context("Data directory is not a directory"),
            _ => error,
        }
    })?;
    let metadata = opened
        .status()
        .context("admit_directory: fstat opened data directory")?;
    ensure!(metadata.is_dir(), "Data directory is not a directory");
    validate_directory_permissions(&metadata)?;
    #[cfg(all(test, unix))]
    tests::replace_directory_before_canonicalization(&path)?;
    let directory = path
        .canonicalize()
        .with_context(|| format!("admit_directory: canonicalize {}", path.display()))?;
    let admitted = AdmittedDirectory {
        path: directory,
        directory: opened,
    };
    admitted.validate_identity()?;
    let directory = &admitted.path;
    validate_resolved_ancestors(directory, false)?;
    match pp_os::status_nofollow(&directory.join("printpartner.lock")) {
        Ok(metadata) => validate_lock_metadata(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "admit_directory: lstat {}/printpartner.lock",
                    directory.display()
                )
            });
        }
    }
    #[cfg(all(test, unix))]
    tests::replace_directory_before_writer_preflight(directory)?;
    Ok(admitted)
}

fn validate_directory_permissions(metadata: &Status) -> Result<()> {
    if let Some(access) = metadata.posix_access() {
        ensure!(
            access.owned_by_current_user()
                && access.mode() & 0o700 == 0o700
                && access.mode() & 0o022 == 0,
            "Unsafe data directory permissions"
        );
    }
    Ok(())
}

impl AdmittedDirectory {
    pub(crate) fn lock(self) -> Result<LockedDirectory> {
        self.validate_identity()?;
        validate_resolved_ancestors(&self.path, false)?;
        // A standalone Node lease is a directory. Refuse it before creating
        // printpartner.lock, so Rust writes nothing while Node holds the lease.
        // The reservation below still refuses a lease published after this check.
        if self
            .directory
            .status_nofollow(".desktop-lease")
            .is_ok_and(|status| status.is_dir())
        {
            anyhow::bail!(
                "Data directory already owned by standalone Node; stop it before starting Rust"
            );
        }
        let path = self.path.join("printpartner.lock");
        let (lock, lock_created) = self.open_lock(c"printpartner.lock")?;
        let mut held = false;
        let compatibility = (|| {
            #[cfg(all(test, unix))]
            tests::after_lock_created(&self.path)?;
            self.validate_identity()?;
            validate_open_lock_identity(&lock, &path)?;
            match pp_os::try_lock(&lock) {
                Ok(()) => held = true,
                Err(pp_os::TryLockError::WouldBlock) => anyhow::bail!(
                    "Data directory already owned; stop the other Print Partner process before starting another instance"
                ),
                Err(pp_os::TryLockError::Io(error)) => {
                    return Err(error)
                        .context("AdmittedDirectory::lock: try_lock printpartner.lock");
                }
            }
            self.validate_identity()?;
            validate_open_lock_identity(&lock, &path)?;
            #[cfg(all(test, unix))]
            tests::before_reservation(&self.path)?;
            CompatibilityReservation::acquire(&self)
        })();
        let compatibility = match compatibility {
            Ok(compatibility) => compatibility,
            Err(error) => {
                // After WouldBlock another owner holds the lock: the removal
                // below cannot take it and leaves the file.
                if lock_created {
                    remove_created_lease(&self.directory, "printpartner.lock", &lock, held);
                }
                return Err(error);
            }
        };
        let locked = LockedDirectory {
            admitted: self,
            lock,
            compatibility,
            lock_created,
        };
        if let Err(error) = locked.admitted.validate_identity() {
            locked.abandon();
            return Err(error);
        }
        Ok(locked)
    }

    pub(crate) fn preflight(&self) -> Result<()> {
        self.preflight_schema(false).map(|_| ())
    }

    /// Runs only under the writer lock: it first removes working folders a
    /// killed start left, which no live preflight can own.
    fn preflight_schema(&self, _owned: bool) -> Result<u64> {
        self.validate_identity()?;
        crate::schema::sweep_preflight_leftovers(&self.directory)?;
        crate::schema::preflight(
            #[cfg(test)]
            &self.path.join("print-partner.db"),
            #[cfg(test)]
            _owned,
            (&self.directory, &self.path),
            |name| self.open_preflight_file(name),
            || self.validate_identity(),
        )
        .map_err(|error| {
            if error.downcast_ref::<std::io::Error>().is_some() {
                error.context(format!(
                    "AdmittedDirectory::preflight_schema: database preflight in {} (owned={_owned})",
                    self.path.display()
                ))
            } else {
                error
            }
        })
    }

    fn validate_identity(&self) -> Result<()> {
        let opened = self
            .directory
            .status()
            .context("validate_identity: fstat admitted data directory")?;
        let named = pp_os::status_nofollow(&self.path)
            .with_context(|| format!("validate_identity: lstat {}", self.path.display()))
            .context("Data directory was replaced")?;
        ensure!(
            named.is_dir() && opened.identity() == named.identity(),
            "Data directory was replaced"
        );
        validate_directory_permissions(&opened)?;
        validate_directory_permissions(&named)
    }

    fn open_preflight_file(&self, name: &str) -> std::io::Result<File> {
        let at_stage = |stage: &str, error: std::io::Error| {
            std::io::Error::new(
                error.kind(),
                format!("open_preflight_file: {stage} {name}: {error}"),
            )
        };
        let file = self
            .directory
            .open_read_nofollow(name)
            .map_err(|error| at_stage("openat O_RDONLY|O_NOFOLLOW", error))?;
        let metadata = file.metadata().map_err(|error| at_stage("fstat", error))?;
        if !metadata.is_file() {
            return Err(at_stage(
                "require regular file",
                pp_os::not_regular_file(metadata.is_dir()),
            ));
        }
        Ok(file)
    }

    /// Create with O_EXCL, then open an existing entry without O_CREAT. On macOS,
    /// racing dirfd-relative O_CREAT opens without O_EXCL can fail with ENOENT.
    /// Also reports whether this call created the file.
    fn open_lock(&self, name: &CStr) -> Result<(File, bool)> {
        let display = name.to_string_lossy();
        match self.directory.create_new_nofollow(&display) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => self
                .directory
                .open_write_nofollow(&display)
                .map(|file| (file, false))
                .with_context(|| format!("open_lock: openat O_RDWR|O_NOFOLLOW {display}")),
            result => result.map(|file| (file, true)).with_context(|| {
                format!("open_lock: openat O_RDWR|O_CREAT|O_EXCL|O_NOFOLLOW {display}")
            }),
        }
    }
}

fn validate_input_ancestors(path: &Path) -> Result<()> {
    for prefix in path.ancestors() {
        match std::fs::symlink_metadata(prefix) {
            Ok(_) => {
                // Resolve every existing prefix before mkdir, including aliases whose
                // target has unsafe parents hidden by the input spelling.
                let resolved = prefix.canonicalize().with_context(|| {
                    format!(
                        "validate_input_ancestors: canonicalize {}",
                        prefix.display()
                    )
                })?;
                validate_resolved_ancestors(&resolved, prefix != path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("validate_input_ancestors: lstat {}", prefix.display())
                });
            }
        }
    }
    Ok(())
}

fn validate_resolved_ancestors(path: &Path, include_self: bool) -> Result<()> {
    for ancestor in path.ancestors().skip(usize::from(!include_self)) {
        let metadata = pp_os::status_follow(ancestor)
            .with_context(|| format!("validate_resolved_ancestors: stat {}", ancestor.display()))?;
        ensure!(
            metadata.is_dir()
                && metadata.posix_access().is_none_or(|access| {
                    (access.owned_by_root() || access.owned_by_current_user())
                        && (access.mode() & 0o022 == 0 || access.mode() & 0o1000 != 0)
                }),
            "Unsafe data directory ancestor"
        );
    }
    Ok(())
}

fn validate_lock_metadata(metadata: &Status) -> Result<()> {
    ensure!(
        metadata.is_file()
            && metadata.links() == 1
            && metadata.posix_access().is_none_or(|access| {
                access.owned_by_current_user() && access.mode() & 0o022 == 0
            }),
        "Unsafe data directory lock"
    );
    Ok(())
}

fn validate_open_lock_identity(file: &File, path: &Path) -> Result<()> {
    let opened = pp_os::status_of(file)
        .with_context(|| format!("validate_open_lock_identity: fstat {}", path.display()))?;
    let named = pp_os::status_nofollow(path)
        .with_context(|| format!("validate_open_lock_identity: lstat {}", path.display()))?;
    validate_lock_metadata(&opened)?;
    validate_lock_metadata(&named)?;
    ensure!(
        opened.identity() == named.identity(),
        "Data directory lock was replaced"
    );
    Ok(())
}

#[cfg(all(test, unix))]
thread_local! {
    static REPLACE_DIRECTORY_BEFORE_WRITER_PREFLIGHT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static REPLACE_DIRECTORY_BEFORE_CANONICALIZATION: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

impl StorageLease {
    pub fn acquire(path: &Path) -> Result<Self> {
        Self::acquire_admitted(admit_directory(path)?)
    }

    pub(crate) fn acquire_admitted(admitted: AdmittedDirectory) -> Result<Self> {
        Self::acquire_locked(admitted.lock()?)
    }

    pub(crate) fn acquire_locked(locked: LockedDirectory) -> Result<Self> {
        let runtime_dir = std::env::temp_dir().join(format!(
            "pp-{}-{}",
            std::process::id(),
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let prepared = (|| {
            #[cfg(all(test, unix))]
            tests::before_runtime_dir(&locked.admitted.path)?;
            locked.admitted.validate_identity()?;
            pp_os::create_private_dir(&runtime_dir).with_context(|| {
                format!(
                    "StorageLease::acquire_locked: create runtime directory {}",
                    runtime_dir.display()
                )
            })
        })();
        if let Err(error) = prepared {
            locked.abandon();
            return Err(error);
        }
        #[cfg(all(test, unix))]
        tests::record_runtime_dir(&runtime_dir);
        let LockedDirectory {
            admitted,
            lock,
            compatibility,
            lock_created,
        } = locked;
        Ok(Self {
            advisory_lock: Some(lock),
            compatibility: Some(compatibility),
            admitted: Arc::new(admitted),
            runtime_dir,
            release_allowed: true,
            lock_created,
        })
    }
    pub(crate) fn preflight(&self) -> Result<u64> {
        #[cfg(all(test, unix))]
        tests::before_owned_preflight()?;
        self.admitted.preflight_schema(true)
    }
    /// Give up a lease that failed before its owner used it, leaving the lease
    /// files as found, like [`LockedDirectory::abandon`]: `.desktop-lease` and
    /// `printpartner.lock` are removed only if this acquisition created them,
    /// while the lock is still held and only when the name still refers to the
    /// file it opened. The private runtime directory it created is removed.
    pub(crate) fn abandon(mut self) {
        self.release_allowed = false;
        let _ = std::fs::remove_dir(&self.runtime_dir);
        if let Some(mut compatibility) = self.compatibility.take() {
            if !compatibility.created {
                compatibility.held = false;
            }
            drop(compatibility);
        }
        if let Some(lock) = self.advisory_lock.take()
            && self.lock_created
        {
            remove_created_lease(&self.admitted.directory, "printpartner.lock", &lock, true);
        }
    }
    pub fn data_dir(&self) -> &Path {
        &self.admitted.path
    }
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }
    pub fn clone_file(&self) -> Result<File> {
        self.advisory_lock
            .as_ref()
            .expect("active storage owner")
            .try_clone()
            .context("StorageLease::clone_file: duplicate advisory lock descriptor")
    }
    pub fn retain_on_drop(&mut self) {
        self.release_allowed = false;
    }
    pub fn allow_drop_cleanup(&mut self) {
        self.release_allowed = true;
    }
    pub fn release(mut self) -> LeaseRelease {
        self.release_allowed = false;
        self.finish_release()
    }
    pub fn retain_until_process_exit(mut self) {
        self.release_allowed = false;
        let directory = self.admitted.path.clone();
        failed_releases()
            .lock()
            .expect("failed release registry poisoned")
            .insert(
                directory,
                RetainedLease {
                    lease: self,
                    retry_allowed: false,
                },
            );
    }
    pub fn retry_failed_release(directory: &Path) -> Result<LeaseRelease> {
        Self::retry_failed_release_if_present(directory)?
            .context("No retained failed release for this directory")
    }
    /// Retry a failed release before restarting, only if closure was proved.
    /// Missing registry entries leave active leases and other processes untouched.
    pub fn retry_failed_release_if_present(directory: &Path) -> Result<Option<LeaseRelease>> {
        let directory = directory.canonicalize()?;
        let mut retained = failed_releases()
            .lock()
            .expect("failed release registry poisoned");
        let Some(entry) = retained.get(&directory) else {
            return Ok(None);
        };
        ensure!(
            entry.retry_allowed,
            "Closure was not proved; ownership retained until process exit"
        );
        let mut lease = retained
            .remove(&directory)
            .expect("retained lease exists")
            .lease;
        drop(retained);
        Ok(Some(lease.finish_release()))
    }
    fn finish_release(&mut self) -> LeaseRelease {
        let runtime_removed =
            !self.runtime_dir.exists() || std::fs::remove_dir(&self.runtime_dir).is_ok();
        let marker_removed = runtime_removed
            && self
                .compatibility
                .as_mut()
                .is_none_or(|guard| guard.release().is_ok());
        let lock_released = marker_removed
            && self
                .advisory_lock
                .as_ref()
                .is_none_or(|lock| pp_os::unlock(lock).is_ok());
        if !lock_released {
            let retained = Self {
                advisory_lock: self.advisory_lock.take(),
                compatibility: self.compatibility.take(),
                admitted: self.admitted.clone(),
                runtime_dir: self.runtime_dir.clone(),
                release_allowed: false,
                lock_created: self.lock_created,
            };
            failed_releases()
                .lock()
                .expect("failed release registry poisoned")
                .insert(
                    self.admitted.path.clone(),
                    RetainedLease {
                        lease: retained,
                        retry_allowed: true,
                    },
                );
        } else {
            drop(self.compatibility.take());
            drop(self.advisory_lock.take());
        }
        LeaseRelease {
            marker_removed,
            runtime_removed,
            lock_released,
            ownership_retained: !lock_released,
        }
    }
}

struct RetainedLease {
    lease: StorageLease,
    retry_allowed: bool,
}

fn failed_releases() -> &'static Mutex<BTreeMap<PathBuf, RetainedLease>> {
    static LEASES: OnceLock<Mutex<BTreeMap<PathBuf, RetainedLease>>> = OnceLock::new();
    LEASES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

impl Drop for StorageLease {
    fn drop(&mut self) {
        if self.release_allowed {
            self.release_allowed = false;
            self.finish_release();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        ffi::CString,
        fs::OpenOptions,
        io::Write,
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    };

    pub(super) fn replace_directory_before_canonicalization(path: &Path) -> Result<()> {
        match REPLACE_DIRECTORY_BEFORE_CANONICALIZATION.take() {
            0 => {}
            1 => {
                std::fs::rename(path, path.with_extension("retired"))?;
                std::fs::DirBuilder::new().mode(0o700).create(path)?;
            }
            2 => {
                let retired = path.with_extension("retired");
                std::fs::rename(path, &retired)?;
                let replacement = path.with_extension("replacement");
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&replacement)?;
                std::os::unix::fs::symlink(replacement, path)?;
            }
            3 => std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))?,
            _ => unreachable!(),
        }
        Ok(())
    }

    #[test]
    fn ticket_t_12_directory_replacement_during_admission_is_refused() {
        for replacement in [1, 2] {
            let path = directory();
            let retired = path.with_extension("retired");
            let marker = br#"{"pid":2147483647,"kind":"standalone"}"#;
            std::fs::write(path.join(".desktop-owner.json"), marker).unwrap();
            std::fs::write(path.join("print-partner.db"), b"never open").unwrap();
            std::fs::write(path.join("desktop.toml"), b"port = 45678\n").unwrap();
            REPLACE_DIRECTORY_BEFORE_CANONICALIZATION.set(replacement);
            let result = StorageLease::acquire(&path);
            assert!(
                result.is_err(),
                "replacement {replacement} acquired ownership of an unchecked directory"
            );
            assert_eq!(
                result.err().unwrap().to_string(),
                "Data directory was replaced"
            );
            assert_eq!(
                std::fs::read(retired.join(".desktop-owner.json")).unwrap(),
                marker
            );
            assert_eq!(
                std::fs::read(retired.join("print-partner.db")).unwrap(),
                b"never open"
            );
            assert_eq!(
                std::fs::read(retired.join("desktop.toml")).unwrap(),
                b"port = 45678\n"
            );
            assert_eq!(std::fs::read_dir(&retired).unwrap().count(), 3);
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
            std::fs::remove_dir_all(retired).unwrap();
            if replacement == 2 {
                std::fs::remove_file(&path).unwrap();
                std::fs::remove_dir_all(path.with_extension("replacement")).unwrap();
            } else {
                std::fs::remove_dir_all(path).unwrap();
            }
        }
    }

    #[test]
    fn ticket_t_12_directory_permission_change_during_admission_is_refused() {
        let path = directory();
        REPLACE_DIRECTORY_BEFORE_CANONICALIZATION.set(3);
        let result = prepare_directory(&path);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir(&path).unwrap();
        assert_eq!(
            result.unwrap_err().to_string(),
            "Unsafe data directory permissions"
        );
    }

    #[test]
    fn ticket_t_12_replaced_directory_refuses_before_writer_database_preflight() {
        let path = directory();
        let retired = path.with_extension("retired");
        std::fs::write(path.join("print-partner.db"), b"never open").unwrap();
        REPLACE_DIRECTORY_BEFORE_CANONICALIZATION.set(1);
        let result = crate::WriterOwner::open(&path, crate::Limits::default());
        assert_eq!(
            result.err().unwrap().to_string(),
            "Data directory was replaced"
        );
        assert_eq!(
            std::fs::read(retired.join("print-partner.db")).unwrap(),
            b"never open"
        );
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        std::fs::remove_dir_all(retired).unwrap();
        std::fs::remove_dir(path).unwrap();
    }

    pub(super) fn replace_directory_before_writer_preflight(path: &Path) -> Result<()> {
        let replacement = REPLACE_DIRECTORY_BEFORE_WRITER_PREFLIGHT.take();
        match replacement {
            0 => {}
            4 => std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o770))?,
            5 => std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500))?,
            _ => {
                REPLACE_DIRECTORY_BEFORE_CANONICALIZATION.set(replacement);
                replace_directory_before_canonicalization(path)?;
            }
        }
        Ok(())
    }

    #[test]
    fn ticket_t_12_writer_preflight_rejects_replaced_admitted_directory() {
        for replacement in [1, 2] {
            assert_writer_preflight_refuses(replacement, "Data directory was replaced");
        }
    }

    #[test]
    fn ticket_t_12_writer_preflight_rejects_changed_admitted_permissions() {
        for replacement in [3, 4, 5] {
            assert_writer_preflight_refuses(replacement, "Unsafe data directory permissions");
        }
    }

    fn assert_writer_preflight_refuses(replacement: u8, expected: &str) {
        let path = directory();
        let files: [(&str, &[u8]); 5] = [
            (
                ".desktop-owner.json",
                br#"{"pid":2147483647,"kind":"standalone"}"#,
            ),
            ("print-partner.db", b"never open"),
            ("print-partner.db-wal", b"copied wal"),
            ("print-partner.db-journal", b"copied journal"),
            ("desktop.toml", b"port = 45678\n"),
        ];
        for (name, bytes) in files {
            std::fs::write(path.join(name), bytes).unwrap();
        }
        crate::schema::PREFLIGHT_CALLS.set(0);
        REPLACE_DIRECTORY_BEFORE_WRITER_PREFLIGHT.set(replacement);
        let result = crate::WriterOwner::open(&path, crate::Limits::default());
        let error = result.err().map(|error| error.to_string());
        assert_eq!(crate::schema::PREFLIGHT_CALLS.get(), 0);
        let preserved = if replacement >= 3 {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path.clone()
        } else {
            path.with_extension("retired")
        };
        for (name, bytes) in files {
            assert_eq!(std::fs::read(preserved.join(name)).unwrap(), bytes);
        }
        assert_eq!(std::fs::read_dir(&preserved).unwrap().count(), files.len());
        if replacement < 3 {
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        }
        std::fs::remove_dir_all(preserved).unwrap();
        if replacement == 2 {
            std::fs::remove_file(&path).unwrap();
            std::fs::remove_dir_all(path.with_extension("replacement")).unwrap();
        } else if replacement == 1 {
            std::fs::remove_dir_all(path).unwrap();
        }
        assert_eq!(
            error.as_deref(),
            Some(expected),
            "replacement {replacement}"
        );
    }

    #[test]
    fn ticket_t_12_writer_preflight_retains_admission_into_os_lock() {
        for replacement in [1, 2, 3] {
            let path = directory();
            let admitted = admit_directory(&path).unwrap();
            crate::schema::PREFLIGHT_CALLS.set(0);
            admitted.preflight().unwrap();
            assert_eq!(crate::schema::PREFLIGHT_CALLS.get(), 1);
            REPLACE_DIRECTORY_BEFORE_WRITER_PREFLIGHT.set(replacement);
            replace_directory_before_writer_preflight(&path).unwrap();
            let result = StorageLease::acquire_admitted(admitted);
            let expected = if replacement == 3 {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
                "Unsafe data directory permissions"
            } else {
                assert_eq!(
                    std::fs::read_dir(path.with_extension("retired"))
                        .unwrap()
                        .count(),
                    0
                );
                std::fs::remove_dir(path.with_extension("retired")).unwrap();
                "Data directory was replaced"
            };
            assert_eq!(result.err().unwrap().to_string(), expected);
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
            if replacement == 2 {
                std::fs::remove_file(&path).unwrap();
                std::fs::remove_dir(path.with_extension("replacement")).unwrap();
            } else {
                std::fs::remove_dir(path).unwrap();
            }
        }
    }

    #[test]
    fn ticket_t_12_writer_preflight_rechecks_ancestor_permissions_before_lock() {
        let parent = directory();
        let path = parent.join("store");
        let admitted = admit_directory(&path).unwrap();
        let marker = br#"{"pid":2147483647,"kind":"standalone"}"#;
        std::fs::write(path.join(".desktop-owner.json"), marker).unwrap();
        std::fs::write(path.join("print-partner.db"), b"").unwrap();
        std::fs::write(path.join("desktop.toml"), b"port = 45678\n").unwrap();
        admitted.preflight().unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = StorageLease::acquire_admitted(admitted)
            .err()
            .map(|error| error.to_string());
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(error.as_deref(), Some("Unsafe data directory ancestor"));
        assert_eq!(
            std::fs::read(path.join(".desktop-owner.json")).unwrap(),
            marker
        );
        assert_eq!(std::fs::read(path.join("print-partner.db")).unwrap(), b"");
        assert_eq!(
            std::fs::read(path.join("desktop.toml")).unwrap(),
            b"port = 45678\n"
        );
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 3);
        std::fs::remove_dir_all(parent).unwrap();
    }

    fn directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pp-release-order-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .unwrap();
        path
    }

    #[test]
    fn gre_673_lock_open_and_lstat_errors_name_their_stages() {
        let path = directory();
        let admitted = admit_directory(&path).unwrap();
        std::fs::remove_dir(&path).unwrap();
        for name in [c"printpartner.lock", c".desktop-lease"] {
            let error = admitted.open_lock(name).unwrap_err();
            let diagnostic = format!("{error:?}");
            assert!(diagnostic.contains("open_lock: openat"), "{diagnostic}");
            assert!(diagnostic.contains(name.to_str().unwrap()), "{diagnostic}");
            assert_eq!(
                error
                    .downcast_ref::<std::io::Error>()
                    .unwrap()
                    .raw_os_error(),
                Some(libc::ENOENT)
            );
        }
        let path = directory();
        let file_path = path.join("printpartner.lock");
        let file = File::create(&file_path).unwrap();
        std::fs::remove_file(&file_path).unwrap();
        let error = validate_open_lock_identity(&file, &file_path).unwrap_err();
        let diagnostic = format!("{error:?}");
        assert!(
            diagnostic.contains("validate_open_lock_identity: lstat"),
            "{diagnostic}"
        );
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
        std::fs::remove_dir(path).unwrap();
    }

    #[test]
    fn gre_673_racing_first_lock_opens_share_one_file() {
        let path = directory();
        let admitted = admit_directory(&path).unwrap();
        let threads = 8;
        for trial in 0..200 {
            let name = CString::new(format!("race-{trial}")).unwrap();
            let barrier = std::sync::Barrier::new(threads);
            let opened: Vec<_> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..threads)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            admitted
                                .open_lock(&name)
                                .map(|(file, _)| file.metadata().unwrap().ino())
                                .map_err(|error| format!("{error:#}"))
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            let inode = std::fs::symlink_metadata(path.join(name.to_str().unwrap()))
                .unwrap()
                .ino();
            assert!(
                opened.iter().all(|result| result == &Ok(inode)),
                "trial {trial}: {opened:?}"
            );
        }
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn gre_636_busy_lock_prevents_all_database_preflight() {
        let path = directory();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path.join("printpartner.lock"))
            .unwrap();
        fs4::FileExt::try_lock(&lock).unwrap();
        crate::schema::PREFLIGHT_CALLS.set(0);
        let error = crate::WriterOwner::open(&path, crate::Limits::default())
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "Data directory already owned; stop the other Print Partner process before starting another instance"
        );
        assert_eq!(crate::schema::PREFLIGHT_CALLS.get(), 0);
        assert!(!path.join("print-partner.db").exists());
        drop(lock);
        let (owner, _) = crate::WriterOwner::open(&path, crate::Limits::default()).unwrap();
        assert_eq!(crate::schema::PREFLIGHT_CALLS.get(), 2);
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }

    thread_local! {
        static CHECK_LOCK_DURING_RESERVATION_RELEASE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn assert_lock_held_during_reservation_release() {
        if let Some(path) = CHECK_LOCK_DURING_RESERVATION_RELEASE.take() {
            assert!(path.join(".desktop-lease").is_file());
            let contender = File::open(path.join("printpartner.lock")).unwrap();
            assert!(
                matches!(
                    fs4::FileExt::try_lock(&contender),
                    Err(fs4::TryLockError::WouldBlock)
                ),
                "advisory lock released before compatibility reservation cleanup"
            );
        }
    }

    #[test]
    fn gre_636_failed_writer_preflight_cleans_reservation_before_unlock() {
        let path = directory();
        std::fs::create_dir(path.join("print-partner.db")).unwrap();
        CHECK_LOCK_DURING_RESERVATION_RELEASE.set(Some(path.clone()));
        let error = crate::WriterOwner::open(&path, crate::Limits::default())
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("Is a directory"), "{error:#}");
        assert!(CHECK_LOCK_DURING_RESERVATION_RELEASE.take().is_none());
        assert!(!path.join(".desktop-lease").exists());
        // GRE-1159: the lock file this call created is removed like on other refusals.
        assert!(!path.join("printpartner.lock").exists());
        std::fs::remove_dir_all(path).unwrap();
    }

    thread_local! {
        static FAIL_OWNED_PREFLIGHT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static RUNTIME_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    /// Remembers, per thread, the runtime directory the last acquired lease created.
    pub(super) fn record_runtime_dir(path: &Path) {
        RUNTIME_DIR.set(Some(path.to_path_buf()));
    }

    /// Fails `StorageLease::preflight` once when armed, after the lease is held.
    pub(super) fn before_owned_preflight() -> Result<()> {
        if FAIL_OWNED_PREFLIGHT.take() {
            anyhow::bail!("forced owned preflight failure");
        }
        Ok(())
    }

    mod preflight_refusal;

    #[test]
    fn gre_679_retained_drop_cleans_reservation_before_unlock() {
        let path = directory();
        let mut owner = StorageLease::acquire(&path).unwrap();
        let runtime = owner.runtime_dir().to_owned();
        owner.retain_on_drop();
        CHECK_LOCK_DURING_RESERVATION_RELEASE.set(Some(path.clone()));
        drop(owner);
        assert!(CHECK_LOCK_DURING_RESERVATION_RELEASE.take().is_none());
        assert!(!path.join(".desktop-lease").exists());
        fs4::FileExt::try_lock(&File::open(path.join("printpartner.lock")).unwrap()).unwrap();
        std::fs::remove_dir(runtime).unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn gre_636_shutdown_preserves_replaced_compatibility_reservation() {
        let path = directory();
        let owner = StorageLease::acquire(&path).unwrap();
        let reservation = path.join(".desktop-lease");
        let acquired = std::fs::symlink_metadata(&reservation).unwrap();
        std::fs::rename(&reservation, path.join("previous-reservation")).unwrap();
        std::fs::write(&reservation, b"replacement reservation").unwrap();
        let replacement = std::fs::symlink_metadata(&reservation).unwrap();
        assert_ne!(acquired.ino(), replacement.ino());
        let released = owner.release();
        assert!(!released.marker_removed);
        assert!(!released.lock_released);
        assert!(released.ownership_retained);
        assert!(matches!(
            fs4::FileExt::try_lock(&File::open(path.join("printpartner.lock")).unwrap()),
            Err(fs4::TryLockError::WouldBlock)
        ));
        assert_eq!(
            std::fs::read(&reservation).unwrap(),
            b"replacement reservation"
        );
        assert_eq!(
            std::fs::symlink_metadata(&reservation).unwrap().ino(),
            replacement.ino()
        );
        std::fs::remove_file(&reservation).unwrap();
        std::fs::rename(path.join("previous-reservation"), &reservation).unwrap();
        let released = StorageLease::retry_failed_release(&path).unwrap();
        assert!(released.marker_removed);
        assert!(released.lock_released);
        assert!(!released.ownership_retained);
        assert!(!reservation.exists());
        fs4::FileExt::try_lock(&File::open(path.join("printpartner.lock")).unwrap()).unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn release_unlocks_descriptors_inherited_by_an_unrelated_spawn() {
        let path = directory();
        let owner = StorageLease::acquire(&path).unwrap();
        // A concurrent fork can retain the same open file description until exec.
        let inherited = owner.clone_file().unwrap();
        assert!(owner.release().lock_released);
        fs4::FileExt::try_lock(&File::open(path.join("printpartner.lock")).unwrap()).unwrap();
        std::fs::remove_dir_all(&path).unwrap();
        assert!(inherited.metadata().is_ok());
        drop(inherited);
    }

    thread_local! {
        static LEASE_FAILURE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
        static RACED_HOLDER: std::cell::RefCell<Option<File>> = const { std::cell::RefCell::new(None) };
    }

    /// After `printpartner.lock` was opened: 1 makes the directory
    /// group-writable, so the identity check fails; 2 opens and locks the file
    /// as another owner, so `try_lock` returns WouldBlock.
    pub(super) fn after_lock_created(path: &Path) -> Result<()> {
        match LEASE_FAILURE.get() {
            1 => {
                LEASE_FAILURE.set(0);
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o770))?;
            }
            2 => {
                LEASE_FAILURE.set(0);
                let holder = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path.join("printpartner.lock"))?;
                pp_os::try_lock(&holder)
                    .map_err(|error| anyhow::anyhow!("raced holder lock: {error:?}"))?;
                RACED_HOLDER.set(Some(holder));
            }
            8 => {
                LEASE_FAILURE.set(0);
                replace_with_file(&path.join("printpartner.lock"))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// After `.desktop-lease` was opened: 5 makes it group-writable, 6 replaces
    /// it with a symlink, 7 adds a hard link, and 9 replaces it with another
    /// regular file. Each makes the reservation's identity check fail.
    pub(super) fn after_reservation_opened(path: &Path) -> Result<()> {
        let lease = path.join(".desktop-lease");
        match LEASE_FAILURE.get() {
            5 => std::fs::set_permissions(&lease, std::fs::Permissions::from_mode(0o620))?,
            6 => {
                std::fs::remove_file(&lease)?;
                std::os::unix::fs::symlink("elsewhere", &lease)?;
            }
            7 => std::fs::hard_link(&lease, path.join("extra-link"))?,
            9 => replace_with_file(&lease)?,
            _ => return Ok(()),
        }
        LEASE_FAILURE.set(0);
        Ok(())
    }

    /// Move the file at `path` to `<path>.moved`, keeping its one link, and
    /// create a different regular file at `path`.
    fn replace_with_file(path: &Path) -> std::io::Result<()> {
        std::fs::rename(path, path.with_extension("moved"))?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?
            .write_all(b"replacement")
    }

    /// 3 publishes a Node lease directory, so the reservation open fails.
    pub(super) fn before_reservation(path: &Path) -> Result<()> {
        if LEASE_FAILURE.get() == 3 {
            LEASE_FAILURE.set(0);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(path.join(".desktop-lease"))?;
            std::fs::write(path.join(".desktop-lease/owner.json"), b"{}")?;
        }
        Ok(())
    }

    /// 4 makes the directory group-writable once the lock is built, so
    /// `StorageLease::acquire_locked` fails.
    pub(super) fn before_runtime_dir(path: &Path) -> Result<()> {
        if LEASE_FAILURE.get() == 4 {
            LEASE_FAILURE.set(0);
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o770))?;
        }
        Ok(())
    }

    /// Every entry under `root` with its bytes; directories map to `None`.
    fn listing(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut entries = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
                if kind.is_dir() {
                    entries.insert(relative, None);
                    pending.push(path);
                } else if kind.is_symlink() {
                    let target = std::fs::read_link(&path).unwrap();
                    let text = format!("symlink -> {}", target.display());
                    entries.insert(relative, Some(text.into_bytes()));
                } else {
                    entries.insert(relative, Some(std::fs::read(&path).unwrap()));
                }
            }
        }
        entries
    }

    /// Runs `StorageLease::acquire` with failure `stage`, restores the
    /// directory mode, and returns the error text.
    fn failed_acquire(path: &Path, stage: u8) -> String {
        LEASE_FAILURE.set(stage);
        let result = StorageLease::acquire(path);
        LEASE_FAILURE.set(0);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        match result {
            Ok(_) => panic!("stage {stage} acquired the lease"),
            Err(error) => format!("{error:#}"),
        }
    }

    fn set_old_mtime(path: &Path) -> std::time::SystemTime {
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        modified
    }

    #[test]
    fn identity_failure_after_creating_the_lock_removes_it() {
        let path = directory();
        let before = listing(&path);
        let error = failed_acquire(&path, 1);
        assert!(
            error.contains("Unsafe data directory permissions"),
            "{error}"
        );
        assert_eq!(listing(&path), before);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn would_block_on_a_lock_file_this_call_created_leaves_it_to_its_holder() {
        let path = directory();
        let error = failed_acquire(&path, 2);
        assert!(error.contains("Data directory already owned"), "{error}");
        // The raced holder locked the file this call created; it stays.
        let mut expected = BTreeMap::new();
        expected.insert(PathBuf::from("printpartner.lock"), Some(Vec::new()));
        assert_eq!(listing(&path), expected);
        let holder = RACED_HOLDER.take().unwrap();
        let probe = File::open(path.join("printpartner.lock")).unwrap();
        assert!(matches!(
            pp_os::try_lock(&probe),
            Err(pp_os::TryLockError::WouldBlock)
        ));
        drop(holder);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn would_block_on_a_held_pre_existing_lock_leaves_every_file() {
        let path = directory();
        let owner = StorageLease::acquire(&path).unwrap();
        let modified = set_old_mtime(&path.join("printpartner.lock"));
        let before = listing(&path);
        let error = StorageLease::acquire(&path).err().unwrap();
        assert!(
            format!("{error:#}").contains("Data directory already owned"),
            "{error:#}"
        );
        assert_eq!(listing(&path), before);
        let metadata = std::fs::metadata(path.join("printpartner.lock")).unwrap();
        assert_eq!(metadata.modified().unwrap(), modified);
        drop(owner);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn node_lease_published_before_the_reservation_leaves_no_lock_file() {
        let path = directory();
        let mut expected = listing(&path);
        let error = failed_acquire(&path, 3);
        assert!(error.contains("standalone Node"), "{error}");
        // Only the Node lease the hook published remains.
        expected.insert(PathBuf::from(".desktop-lease"), None);
        expected.insert(
            PathBuf::from(".desktop-lease/owner.json"),
            Some(b"{}".to_vec()),
        );
        assert_eq!(listing(&path), expected);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn acquire_locked_failure_removes_both_lease_files_it_created() {
        let path = directory();
        let before = listing(&path);
        let error = failed_acquire(&path, 4);
        assert!(
            error.contains("Unsafe data directory permissions"),
            "{error}"
        );
        assert_eq!(listing(&path), before);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn failures_keep_pre_existing_lease_files_byte_for_byte() {
        for stage in [1, 4] {
            let path = directory();
            let mut modified = Vec::new();
            for name in ["printpartner.lock", ".desktop-lease"] {
                std::fs::write(path.join(name), b"").unwrap();
                std::fs::set_permissions(path.join(name), std::fs::Permissions::from_mode(0o600))
                    .unwrap();
                modified.push(set_old_mtime(&path.join(name)));
            }
            let before = listing(&path);
            failed_acquire(&path, stage);
            assert_eq!(listing(&path), before, "stage {stage}");
            for (name, modified) in ["printpartner.lock", ".desktop-lease"].iter().zip(modified) {
                let metadata = std::fs::metadata(path.join(name)).unwrap();
                assert_eq!(
                    metadata.modified().unwrap(),
                    modified,
                    "stage {stage} {name}"
                );
            }
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    /// A reservation that fails after this call created `.desktop-lease`
    /// removes it and the created lock, but never an entry that replaced it.
    #[test]
    fn reservation_failures_after_creating_it_leave_the_listing_as_found() {
        let entry = |bytes: &[u8]| Some(bytes.to_vec());
        let cases = [
            (5, "Unsafe data directory lock", vec![]),
            (
                6,
                "compatibility reservation identity",
                vec![(".desktop-lease", entry(b"symlink -> elsewhere"))],
            ),
            (
                7,
                "Unsafe data directory lock",
                vec![("extra-link", entry(b""))],
            ),
            (
                9,
                "Data directory lock was replaced",
                vec![
                    (".desktop-lease", entry(b"replacement")),
                    (".desktop-lease.moved", entry(b"")),
                ],
            ),
        ];
        for (stage, message, added) in cases {
            let path = directory();
            let mut expected = listing(&path);
            let error = failed_acquire(&path, stage);
            assert!(error.contains(message), "stage {stage}: {error}");
            // Only what the hook put there remains: no printpartner.lock, and
            // a replacement at `.desktop-lease` is never unlinked.
            for (name, bytes) in added {
                expected.insert(PathBuf::from(name), bytes);
            }
            assert_eq!(listing(&path), expected, "stage {stage}");
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn replaced_lock_file_is_not_unlinked() {
        let path = directory();
        let mut expected = listing(&path);
        let error = failed_acquire(&path, 8);
        assert!(
            error.contains("Data directory lock was replaced"),
            "{error}"
        );
        // The replacement stays, and so does the moved-away original: cleanup
        // removes only the name that still refers to the file it opened.
        expected.insert(
            PathBuf::from("printpartner.lock"),
            Some(b"replacement".to_vec()),
        );
        expected.insert(PathBuf::from("printpartner.moved"), Some(Vec::new()));
        assert_eq!(listing(&path), expected);
        std::fs::remove_dir_all(path).unwrap();
    }

    /// The same reservation failures with a pre-existing `.desktop-lease`
    /// leftover: it is not this call's to remove.
    #[test]
    fn reservation_failures_keep_a_pre_existing_reservation_byte_for_byte() {
        for stage in [5, 7] {
            let path = directory();
            let lease = path.join(".desktop-lease");
            std::fs::write(&lease, b"").unwrap();
            std::fs::set_permissions(&lease, std::fs::Permissions::from_mode(0o600)).unwrap();
            let modified = set_old_mtime(&lease);
            let mut expected = listing(&path);
            failed_acquire(&path, stage);
            if stage == 7 {
                expected.insert(PathBuf::from("extra-link"), Some(Vec::new()));
            }
            assert_eq!(listing(&path), expected, "stage {stage}");
            let metadata = std::fs::metadata(&lease).unwrap();
            assert_eq!(metadata.modified().unwrap(), modified, "stage {stage}");
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}
