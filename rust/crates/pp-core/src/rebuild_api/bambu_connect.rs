use super::slicer_inbox::publish_locked;
use pp_api::exports::bambu_connect::{PrintFile, Stage, StageFailure};
use pp_os::{Dir, status_of};
use pp_services::native_slicers::HandoffFailure;
use sha2::{Digest, Sha256};
use std::{
    io,
    path::PathBuf,
    time::{Duration, SystemTime},
};

const ROOT: &str = "bambu-connect-handoff";
const RETENTION: Duration = Duration::from_hours(24);
const MAX_FILES: usize = 16;

pub(super) struct LocalFiles(pub(super) PathBuf);

impl Stage for LocalFiles {
    fn stage(&self, print: PrintFile) -> Result<PathBuf, StageFailure> {
        self.stage_file(print.build().get(), print.extension(), print.bytes())
    }
}

impl LocalFiles {
    fn stage_file(
        &self,
        build: u64,
        extension: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, StageFailure> {
        let path = self.0.join(ROOT);
        if !path.is_absolute() || path.to_str().is_none() {
            return Err(StageFailure::Unavailable);
        }
        let data = Dir::open(&self.0).map_err(|_| StageFailure::Unavailable)?;
        match data.create_dir(ROOT) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(StageFailure::Unavailable),
        }
        data.sync().map_err(|_| StageFailure::Unavailable)?;
        let dir = data
            .open_dir_nofollow(ROOT)
            .map_err(|_| StageFailure::Unavailable)?;
        if dir
            .posix_access()
            .map_err(|_| StageFailure::Unavailable)?
            .is_some_and(|access| !access.owned_by_current_user() || access.mode() & 0o777 != 0o700)
        {
            return Err(StageFailure::Unavailable);
        }
        dir.try_lock().map_err(|_| StageFailure::Unavailable)?;
        let digest = hex::encode(Sha256::digest(bytes));
        let filename = format!("{build}-{digest}{extension}");
        prune(&dir, &filename, SystemTime::now())?;
        let already_staged =
            publish_locked(&dir, &filename, bytes).map_err(|error| match error {
                HandoffFailure::ArtifactConflict => StageFailure::Conflict,
                _ => StageFailure::Unavailable,
            })?;
        if already_staged {
            dir.open_write_nofollow(&filename)
                .and_then(|file| {
                    file.set_modified(SystemTime::now())
                        .and_then(|()| file.sync_all())
                })
                .map_err(|_| StageFailure::Unavailable)?;
        }
        Ok(path.join(filename))
    }
}

/// Uses the same private directory lock as publication. Only regular files
/// with names from this stager are eligible for cleanup.
fn prune(dir: &Dir, keep: &str, now: SystemTime) -> Result<(), StageFailure> {
    let mut count = 0;
    let mut already_staged = false;
    for entry in dir.entries().map_err(|_| StageFailure::Unavailable)? {
        let entry = entry.map_err(|_| StageFailure::Unavailable)?;
        let Some(name) = entry.to_str() else { continue };
        let (name_body, pending) = match name
            .strip_prefix('.')
            .and_then(|n| n.strip_suffix(".pending"))
        {
            Some(body) => (body, true),
            None => (name, false),
        };
        if !staged_name(name_body) {
            continue;
        }
        let status = dir
            .status_nofollow(name)
            .map_err(|_| StageFailure::Unavailable)?;
        if name == keep {
            already_staged = true;
        }
        if !status.is_file() || status.links() != 1 {
            count += 1;
            continue;
        }
        let file = dir
            .open_read_nofollow(name)
            .map_err(|_| StageFailure::Unavailable)?;
        if status_of(&file)
            .map_err(|_| StageFailure::Unavailable)?
            .identity()
            != status.identity()
        {
            return Err(StageFailure::Conflict);
        }
        let modified = file
            .metadata()
            .and_then(|meta| meta.modified())
            .map_err(|_| StageFailure::Unavailable)?;
        if pending
            || (name != keep
                && now
                    .duration_since(modified)
                    .is_ok_and(|age| age >= RETENTION))
        {
            drop(file);
            dir.remove_file(name)
                .map_err(|_| StageFailure::Unavailable)?;
        } else {
            count += 1;
        }
    }
    if count >= MAX_FILES && !already_staged {
        return Err(StageFailure::Full);
    }
    Ok(())
}

fn staged_name(name: &str) -> bool {
    let Some((build, rest)) = name.split_once('-') else {
        return false;
    };
    let Some((hash, ext)) = rest.split_at_checked(64) else {
        return false;
    };
    !build.is_empty()
        && build.bytes().all(|byte| byte.is_ascii_digit())
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        && matches!(ext, ".gcode.3mf" | ".3mf" | ".gcode" | ".gco")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn replay_after_restart_keeps_the_same_file_and_refuses_different_bytes_at_its_path() {
        let data = tempfile::tempdir().unwrap();
        let local = || LocalFiles(data.path().into());
        let path = local().stage_file(1, ".gcode.3mf", b"sliced").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"sliced");
        assert_eq!(
            local().stage_file(1, ".gcode.3mf", b"sliced").unwrap(),
            path
        );
        let other = local().stage_file(2, ".gcode.3mf", b"sliced").unwrap();
        assert_ne!(other, path);
        fs::write(&path, b"other!").unwrap();
        assert!(matches!(
            local().stage_file(1, ".gcode.3mf", b"sliced"),
            Err(StageFailure::Conflict)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"other!");
    }

    #[test]
    fn cap_keeps_recent_files_and_prunes_only_old_staging_files() {
        let data = tempfile::tempdir().unwrap();
        let local = LocalFiles(data.path().into());
        let mut paths = Vec::new();
        for build in 1..=MAX_FILES as u64 {
            paths.push(local.stage_file(build, ".gcode", b"fake").unwrap());
        }
        assert!(matches!(
            local.stage_file(42, ".3mf", b"fake"),
            Err(StageFailure::Full)
        ));
        assert_eq!(local.stage_file(1, ".gcode", b"fake").unwrap(), paths[0]);
        let old = SystemTime::now() - RETENTION - Duration::from_secs(5);
        let unrelated = data.path().join(ROOT).join("other.gcode");
        fs::write(&unrelated, b"other").unwrap();
        for path in [&paths[0], &unrelated] {
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let pending = data.path().join(ROOT).join(format!(
            ".{}.pending",
            paths[1].file_name().unwrap().to_str().unwrap()
        ));
        fs::write(&pending, b"interrupted").unwrap();
        local.stage_file(42, ".3mf", b"fake").unwrap();
        assert!(!paths[0].exists());
        assert!(!pending.exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"other");
        assert!(paths[1..].iter().all(|path| path.exists()));
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_a_redirected_staging_root_or_artifact() {
        let data = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = data.path().join(ROOT);
        let local = LocalFiles(data.path().into());
        std::os::unix::fs::symlink(outside.path(), &root).unwrap();
        assert!(matches!(
            local.stage_file(1, ".gcode", b"sliced"),
            Err(StageFailure::Unavailable)
        ));
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
        fs::remove_file(root).unwrap();
        let path = local.stage_file(1, ".gcode", b"sliced").unwrap();
        fs::remove_file(&path).unwrap();
        let target = outside.path().join("file.gcode");
        fs::write(&target, b"sliced").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(matches!(
            local.stage_file(1, ".gcode", b"sliced"),
            Err(StageFailure::Conflict)
        ));
        assert_eq!(fs::read(target).unwrap(), b"sliced");
    }
}
