use anyhow::{Context, Result, ensure};
use pp_os::{Dir, Identity};
use pp_storage::lease::StorageLease;
use std::io::{Read, Write};

pub(super) struct Snapshot {
    identity: Identity,
    pub bytes: Vec<u8>,
}

pub(super) fn read(owner: &StorageLease, name: &str) -> Result<Option<Snapshot>> {
    let file = match Dir::open(owner.data_dir())
        .and_then(|directory| directory.open_read_nofollow(name))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Desktop configuration unavailable"),
    };
    let status = pp_os::status_of(&file)?;
    ensure!(status.is_file(), "Invalid {name}");
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4096, "Invalid {name}");
    Ok(Some(Snapshot {
        identity: status.identity(),
        bytes,
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Boundary {
    Write,
    FileSync,
    Recheck,
    Rename,
    HardLink,
    DirectorySync,
}

pub(super) fn publish(
    owner: &StorageLease,
    name: &str,
    bytes: &[u8],
    previous: Option<&Snapshot>,
) -> Result<()> {
    publish_with(owner, name, bytes, previous, |_| Ok(()))
}

pub(super) fn publish_with(
    owner: &StorageLease,
    name: &str,
    bytes: &[u8],
    previous: Option<&Snapshot>,
    mut boundary: impl FnMut(Boundary) -> Result<()>,
) -> Result<()> {
    let temporary = owner.data_dir().join(format!(
        "{name}-{}.new",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let mut file = pp_os::create_new_private_file(&temporary)?;
    let result = (|| {
        boundary(Boundary::Write)?;
        file.write_all(bytes)?;
        boundary(Boundary::FileSync)?;
        file.sync_all()?;
        drop(file);
        if let Some(previous) = previous {
            boundary(Boundary::Recheck)?;
            let current = read(owner, name)?.context("Desktop configuration changed")?;
            ensure!(
                current.identity == previous.identity && current.bytes == previous.bytes,
                "Desktop configuration changed"
            );
            boundary(Boundary::Rename)?;
            pp_os::publish_rename(&temporary, &owner.data_dir().join(name))?;
        } else {
            publish_first(&temporary, &owner.data_dir().join(name))?;
            boundary(Boundary::HardLink)?;
        }
        Ok(())
    })();
    let cleanup = match std::fs::remove_file(&temporary) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    };
    result?;
    cleanup?;
    boundary(Boundary::DirectorySync)?;
    pp_os::sync_directory(owner.data_dir())?;
    Ok(())
}

/// Links the first version into place without replacing a concurrent
/// winner, leaving the temporary for the shared cleanup. The directory sync
/// that follows makes the link durable.
#[cfg(unix)]
fn publish_first(temporary: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    std::fs::hard_link(temporary, target)
}

/// Moves the first version into place write-through without replacing a
/// concurrent winner: Windows cannot flush the directory afterwards, so the
/// publication itself must be durable.
#[cfg(windows)]
fn publish_first(temporary: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    pp_os::publish_new(temporary, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leftovers(directory: &std::path::Path) -> Vec<std::ffi::OsString> {
        std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().ends_with(".new"))
            .collect()
    }

    #[test]
    fn first_publication_never_replaces_a_concurrent_winner() {
        let directory = std::env::temp_dir().join(format!(
            "pp-state-file-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        pp_os::create_private_dir_all(&directory).unwrap();
        let owner = StorageLease::acquire(&directory).unwrap();
        std::fs::write(directory.join("desktop.toml"), b"winner").unwrap();
        let error = publish(&owner, "desktop.toml", b"loser", None).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            std::fs::read(directory.join("desktop.toml")).unwrap(),
            b"winner"
        );
        assert_eq!(leftovers(&directory), Vec::<std::ffi::OsString>::new());

        let previous = read(&owner, "desktop.toml").unwrap().unwrap();
        publish(&owner, "desktop.toml", b"next", Some(&previous)).unwrap();
        assert_eq!(
            std::fs::read(directory.join("desktop.toml")).unwrap(),
            b"next"
        );
        assert_eq!(leftovers(&directory), Vec::<std::ffi::OsString>::new());

        std::fs::remove_file(directory.join("desktop.toml")).unwrap();
        publish(&owner, "desktop.toml", b"first", None).unwrap();
        assert_eq!(
            std::fs::read(directory.join("desktop.toml")).unwrap(),
            b"first"
        );
        assert_eq!(leftovers(&directory), Vec::<std::ffi::OsString>::new());
    }
}
