use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString},
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
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
}

struct CompatibilityReservation {
    directory: File,
    file: File,
    held: bool,
}

impl CompatibilityReservation {
    fn acquire(admitted: &AdmittedDirectory) -> Result<Self> {
        // Node publishes a directory at this name. A regular file prevents that
        // rename and can be reused after a crash under the kernel lock.
        let file = admitted.open_lock(c".desktop-lease").context(
            "Data directory already owned by standalone Node; stop it before starting Rust",
        )?;
        validate_open_lock_identity(&file, &admitted.path.join(".desktop-lease"))
            .context("Could not validate .desktop-lease compatibility reservation identity")?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(fs4::TryLockError::WouldBlock) => anyhow::bail!(
                "Data directory already owned; .desktop-lease compatibility reservation is locked"
            ),
            Err(fs4::TryLockError::Error(error)) => {
                return Err(error)
                    .context("Could not lock .desktop-lease compatibility reservation");
            }
        }
        validate_open_lock_identity(&file, &admitted.path.join(".desktop-lease"))
            .context("Could not validate .desktop-lease compatibility reservation identity")?;
        Ok(Self {
            directory: admitted.directory.try_clone().context(
                "Could not retain directory for .desktop-lease compatibility reservation",
            )?,
            file,
            held: true,
        })
    }

    fn release(&mut self) -> std::io::Result<()> {
        if self.held {
            #[cfg(test)]
            tests::assert_lock_held_during_reservation_release();
            let mut named = std::mem::MaybeUninit::<libc::stat>::uninit();
            let inspected = unsafe {
                libc::fstatat(
                    self.directory.as_raw_fd(),
                    c".desktop-lease".as_ptr(),
                    named.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if inspected < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    self.held = false;
                    return Ok(());
                }
                return Err(error);
            }
            let named = unsafe { named.assume_init() };
            let mut acquired = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstat(self.file.as_raw_fd(), acquired.as_mut_ptr()) } < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let acquired = unsafe { acquired.assume_init() };
            if named.st_dev != acquired.st_dev || named.st_ino != acquired.st_ino {
                return Err(std::io::Error::other(
                    ".desktop-lease compatibility reservation was replaced",
                ));
            }
            let removed = unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), c".desktop-lease".as_ptr(), 0)
            };
            if removed < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error);
                }
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

/// Create private Unix store directories and admit existing owner-controlled stores.
/// Existing modes and data-directory aliases are preserved. Existing lock entries
/// must be owner-controlled regular files with one link. Ancestors must be root or
/// effective-user owned and deny group/other write unless sticky. These are path
/// snapshots, not confinement. A held descriptor checks leaf identity through
/// canonicalization. Lock acquisition retains it through ownership cleanup.
/// SQLite initialization and ACL policy remain separate concerns.
pub fn prepare_directory(path: &Path) -> Result<PathBuf> {
    Ok(admit_directory(path)?.path)
}

pub(crate) struct AdmittedDirectory {
    path: PathBuf,
    directory: File,
}

pub(crate) struct LockedDirectory {
    admitted: AdmittedDirectory,
    compatibility: CompatibilityReservation,
    lock: File,
}

impl LockedDirectory {
    pub(crate) fn preflight(&self) -> Result<()> {
        self.admitted.preflight()
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
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .with_context(|| format!("admit_directory: create data directory {}", path.display()))?;
    let opened = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|error| {
            let errno = error.raw_os_error();
            let error = anyhow::Error::new(error).context(format!(
                "admit_directory: open data directory {}",
                path.display()
            ));
            match errno {
                Some(libc::EACCES | libc::EPERM) => {
                    error.context("Unsafe data directory permissions")
                }
                Some(libc::ENOTDIR) => error.context("Data directory is not a directory"),
                _ => error,
            }
        })?;
    let metadata = opened
        .metadata()
        .context("admit_directory: fstat opened data directory")?;
    ensure!(metadata.is_dir(), "Data directory is not a directory");
    validate_directory_permissions(&metadata)?;
    #[cfg(test)]
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
    match std::fs::symlink_metadata(directory.join("printpartner.lock")) {
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
    #[cfg(test)]
    tests::replace_directory_before_writer_preflight(directory)?;
    Ok(admitted)
}

fn validate_directory_permissions(metadata: &std::fs::Metadata) -> Result<()> {
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o700 == 0o700
            && metadata.mode() & 0o022 == 0,
        "Unsafe data directory permissions"
    );
    Ok(())
}

impl AdmittedDirectory {
    pub(crate) fn lock(self) -> Result<LockedDirectory> {
        self.validate_identity()?;
        validate_resolved_ancestors(&self.path, false)?;
        let path = self.path.join("printpartner.lock");
        let lock = self.open_lock(c"printpartner.lock")?;
        self.validate_identity()?;
        validate_open_lock_identity(&lock, &path)?;
        match fs4::FileExt::try_lock(&lock) {
            Ok(()) => {}
            Err(fs4::TryLockError::WouldBlock) => anyhow::bail!(
                "Data directory already owned; stop the other Print Partner process before starting another instance"
            ),
            Err(fs4::TryLockError::Error(error)) => {
                return Err(error).context("AdmittedDirectory::lock: try_lock printpartner.lock");
            }
        }
        self.validate_identity()?;
        validate_open_lock_identity(&lock, &path)?;
        let compatibility = CompatibilityReservation::acquire(&self)?;
        self.validate_identity()?;
        Ok(LockedDirectory {
            admitted: self,
            lock,
            compatibility,
        })
    }

    pub(crate) fn preflight(&self) -> Result<()> {
        self.preflight_schema(false).map(|_| ())
    }

    fn preflight_schema(&self, _owned: bool) -> Result<u64> {
        self.validate_identity()?;
        crate::schema::preflight(
            #[cfg(test)]
            &self.path.join("print-partner.db"),
            #[cfg(test)]
            _owned,
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
            .metadata()
            .context("validate_identity: fstat admitted data directory")?;
        let named = std::fs::symlink_metadata(&self.path)
            .with_context(|| format!("validate_identity: lstat {}", self.path.display()))
            .context("Data directory was replaced")?;
        ensure!(
            named.is_dir() && opened.dev() == named.dev() && opened.ino() == named.ino(),
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
        let name = CString::new(name).map_err(|error| at_stage("encode name", error.into()))?;
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_NOCTTY,
            )
        };
        if fd < 0 {
            return Err(at_stage(
                "openat O_RDONLY|O_NOFOLLOW",
                std::io::Error::last_os_error(),
            ));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|error| at_stage("fstat", error))?;
        if !metadata.is_file() {
            return Err(at_stage(
                "require regular file",
                std::io::Error::from_raw_os_error(if metadata.is_dir() {
                    libc::EISDIR
                } else {
                    libc::EINVAL
                }),
            ));
        }
        Ok(file)
    }

    /// Create with O_EXCL, then open an existing entry without O_CREAT. On macOS,
    /// racing dirfd-relative O_CREAT opens without O_EXCL can fail with ENOENT.
    fn open_lock(&self, name: &CStr) -> Result<File> {
        let flags = libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        let open = |flags| {
            let fd =
                unsafe { libc::openat(self.directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(unsafe { File::from_raw_fd(fd) })
        };
        let display = name.to_string_lossy();
        match open(flags | libc::O_CREAT | libc::O_EXCL) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => open(flags)
                .with_context(|| format!("open_lock: openat O_RDWR|O_NOFOLLOW {display}")),
            result => result.with_context(|| {
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
    let euid = unsafe { libc::geteuid() };
    for ancestor in path.ancestors().skip(usize::from(!include_self)) {
        let metadata = std::fs::metadata(ancestor)
            .with_context(|| format!("validate_resolved_ancestors: stat {}", ancestor.display()))?;
        ensure!(
            metadata.is_dir()
                && (metadata.uid() == 0 || metadata.uid() == euid)
                && (metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0),
            "Unsafe data directory ancestor"
        );
    }
    Ok(())
}

fn validate_lock_metadata(metadata: &std::fs::Metadata) -> Result<()> {
    ensure!(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0,
        "Unsafe data directory lock"
    );
    Ok(())
}

fn validate_open_lock_identity(file: &File, path: &Path) -> Result<()> {
    let opened = file
        .metadata()
        .with_context(|| format!("validate_open_lock_identity: fstat {}", path.display()))?;
    let named = std::fs::symlink_metadata(path)
        .with_context(|| format!("validate_open_lock_identity: lstat {}", path.display()))?;
    validate_lock_metadata(&opened)?;
    validate_lock_metadata(&named)?;
    ensure!(
        opened.dev() == named.dev() && opened.ino() == named.ino(),
        "Data directory lock was replaced"
    );
    Ok(())
}

#[cfg(test)]
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
        let LockedDirectory {
            admitted,
            lock,
            compatibility,
        } = locked;
        admitted.validate_identity()?;
        let runtime_dir = std::env::temp_dir().join(format!(
            "pp-{}-{}",
            std::process::id(),
            hex::encode(rand::random::<[u8; 8]>())
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&runtime_dir)
            .with_context(|| {
                format!(
                    "StorageLease::acquire_locked: create runtime directory {}",
                    runtime_dir.display()
                )
            })?;
        Ok(Self {
            advisory_lock: Some(lock),
            compatibility: Some(compatibility),
            admitted: Arc::new(admitted),
            runtime_dir,
            release_allowed: true,
        })
    }
    pub(crate) fn preflight(&self) -> Result<u64> {
        self.admitted.preflight_schema(true)
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
                .is_none_or(|lock| fs4::FileExt::unlock(lock).is_ok());
        if !lock_released {
            let retained = Self {
                advisory_lock: self.advisory_lock.take(),
                compatibility: self.compatibility.take(),
                admitted: self.admitted.clone(),
                runtime_dir: self.runtime_dir.clone(),
                release_allowed: false,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
                                .map(|file| file.metadata().unwrap().ino())
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
        let contender = File::open(path.join("printpartner.lock")).unwrap();
        fs4::FileExt::try_lock(&contender).unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }

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
}
