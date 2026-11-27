use pp_os::{Dir, FileOpen, status_of};
use pp_services::native_slicers::{Artifact, Delivery, Handoff, HandoffFailure, Instance, Picker};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{self, Read, Write},
    path::PathBuf,
};

const MAX_CONFIG: u64 = 16 * 1024;
const MAX_INSTANCES: usize = 32;
const ROOT: &str = "slicer-handoff";

pub(super) struct LocalInbox {
    data_directory: PathBuf,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    default: Option<String>,
    instances: Vec<ConfiguredInstance>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfiguredInstance {
    id: String,
    name: String,
}

impl LocalInbox {
    pub(super) fn new(data_directory: PathBuf) -> Self {
        Self { data_directory }
    }

    fn configured(&self) -> Option<(Dir, Configuration)> {
        let root = Dir::open(&self.data_directory)
            .ok()?
            .open_dir_nofollow(ROOT)
            .ok()?;
        let file = root.open_file("instances.json", FileOpen::Read).ok()?;
        if !status_of(&file).ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > MAX_CONFIG {
            return None;
        }
        let config: Configuration = serde_json::from_slice(&bytes).ok()?;
        let mut ids = HashSet::new();
        if config.instances.len() > MAX_INSTANCES
            || !config.instances.iter().all(|instance| {
                valid_id(&instance.id)
                    && ids.insert(instance.id.as_str())
                    && !instance.name.trim().is_empty()
                    && instance.name.len() <= 80
                    && !instance.name.chars().any(char::is_control)
            })
            || config
                .default
                .as_deref()
                .is_some_and(|id| !ids.contains(id))
        {
            return None;
        }
        Some((root, config))
    }
}

fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !matches!(id, "con" | "prn" | "aux" | "nul")
        && !["com", "lpt"].iter().any(|prefix| {
            id.strip_prefix(prefix)
                .is_some_and(|tail| tail.len() == 1 && tail.as_bytes()[0].is_ascii_digit())
        })
}

impl Handoff for LocalInbox {
    fn picker(&self) -> Picker {
        let Some((root, config)) = self.configured() else {
            return Picker::default();
        };
        let instances: Vec<_> = config
            .instances
            .into_iter()
            .map(|entry| Instance {
                available: inbox(&root, &entry.id).is_ok(),
                id: entry.id,
                name: entry.name,
            })
            .collect();
        let default_instance_id = config.default.filter(|id| {
            instances
                .iter()
                .any(|entry| entry.id == *id && entry.available)
        });
        Picker {
            instances,
            default_instance_id,
        }
    }

    fn deliver(
        &self,
        instance_id: Option<&str>,
        artifact: Artifact,
    ) -> Result<Delivery, HandoffFailure> {
        use HandoffFailure::*;
        let (root, config) = self.configured().ok_or(Unavailable)?;
        let id = instance_id.or(config.default.as_deref()).ok_or(NoDefault)?;
        if !config.instances.iter().any(|entry| entry.id == id) {
            return Err(UnknownInstance);
        }
        let dir = inbox(&root, id).map_err(|_| Unavailable)?;
        let filename = artifact_filename(&artifact);
        let already_delivered = publish(&dir, &filename, &artifact.bytes)?;
        Ok(Delivery {
            instance_id: id.into(),
            filename,
            already_delivered,
        })
    }
}

fn artifact_filename(artifact: &Artifact) -> String {
    let digest = hex::encode(Sha256::digest(&artifact.bytes));
    format!(
        "revision-{}-plate-{}-{digest}.3mf",
        artifact.plate_revision_id, artifact.plate_ordinal
    )
}

fn inbox(root: &Dir, id: &str) -> io::Result<Dir> {
    let dir = root.open_dir_nofollow(id)?;
    if dir
        .posix_access()?
        .is_some_and(|access| !access.owned_by_current_user() || access.mode() & 0o777 != 0o700)
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(dir)
}

fn publish(dir: &Dir, filename: &str, bytes: &[u8]) -> Result<bool, HandoffFailure> {
    dir.try_lock().map_err(|_| HandoffFailure::Unavailable)?;
    publish_locked(dir, filename, bytes)
}

pub(super) fn publish_locked(
    dir: &Dir,
    filename: &str,
    bytes: &[u8],
) -> Result<bool, HandoffFailure> {
    use HandoffFailure::{ArtifactConflict, Unavailable};
    match dir.status_nofollow(filename) {
        Ok(status) if status.is_file() && status.links() == 1 => {
            let file = dir
                .open_file(filename, FileOpen::Read)
                .map_err(|_| Unavailable)?;
            if status_of(&file).map_err(|_| Unavailable)?.identity() != status.identity()
                || !exact_bytes(file, bytes).map_err(|_| Unavailable)?
            {
                return Err(ArtifactConflict);
            }
            dir.sync().map_err(|_| Unavailable)?;
            return Ok(true);
        }
        Ok(_) => return Err(ArtifactConflict),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(Unavailable),
    }
    let pending = format!(".{filename}.pending");
    let staged = (|| -> io::Result<()> {
        match dir.remove_file(&pending) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut file = dir.open_file(&pending, FileOpen::CreateWriteOnly)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        dir.rename_no_replace(&pending, filename)?;
        dir.sync()
    })();
    if let Err(error) = staged {
        let _ = dir.remove_file(&pending);
        return Err(if error.kind() == io::ErrorKind::AlreadyExists {
            ArtifactConflict
        } else {
            Unavailable
        });
    }
    Ok(false)
}

fn exact_bytes(mut file: std::fs::File, bytes: &[u8]) -> io::Result<bool> {
    if file.metadata()?.len() != bytes.len() as u64 {
        return Ok(false);
    }
    let mut buffer = [0; 8192];
    for expected in bytes.chunks(buffer.len()) {
        let actual = &mut buffer[..expected.len()];
        file.read_exact(actual)?;
        if actual != expected {
            return Ok(false);
        }
    }
    Ok(file.read(&mut buffer[..1])? == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn artifact(bytes: &[u8]) -> Artifact {
        Artifact {
            plate_revision_id: 21,
            plate_ordinal: 2,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn picker_does_not_fall_back_and_delivery_survives_restart() {
        let data = tempfile::tempdir().unwrap();
        let root = data.path().join(ROOT);
        fs::create_dir(&root).unwrap();
        let dir = Dir::open(&root).unwrap();
        dir.create_dir("orca").unwrap();
        let config = root.join("instances.json");
        fs::write(
            &config,
            r#"{
            "default":"bambu",
            "instances":[{"id":"orca","name":"OrcaSlicer"},{"id":"bambu","name":"Bambu Studio"}]
        }"#,
        )
        .unwrap();
        let adapter = LocalInbox::new(data.path().into());
        let picker = adapter.picker();
        assert!(picker.default_instance_id.is_none());
        assert!(picker.instances[0].available);
        assert!(!picker.instances[1].available);
        assert_eq!(
            adapter.deliver(None, artifact(b"exact bytes")).unwrap_err(),
            HandoffFailure::Unavailable
        );
        assert_eq!(
            adapter
                .deliver(Some("../orca"), artifact(b"exact bytes"))
                .unwrap_err(),
            HandoffFailure::UnknownInstance
        );
        assert_eq!(fs::read_dir(root.join("orca")).unwrap().count(), 0);

        fs::write(
            &config,
            r#"{"default":"orca","instances":[{"id":"orca","name":"OrcaSlicer"}]}"#,
        )
        .unwrap();
        let inbox = root.join("orca");
        let pending = inbox.join(format!(
            ".{}.pending",
            artifact_filename(&artifact(b"exact bytes"))
        ));
        fs::write(&pending, b"interrupted write").unwrap();
        assert_eq!(
            adapter.picker().default_instance_id.as_deref(),
            Some("orca")
        );
        let sent = adapter.deliver(None, artifact(b"exact bytes")).unwrap();
        assert!(!sent.already_delivered);
        let destination = inbox.join(&sent.filename);
        assert_eq!(fs::read(&destination).unwrap(), b"exact bytes");
        assert!(!pending.exists());
        let before = Dir::open(&inbox)
            .unwrap()
            .status_nofollow(&sent.filename)
            .unwrap();
        drop(adapter);

        let restarted = LocalInbox::new(data.path().into());
        assert!(
            restarted
                .deliver(None, artifact(b"exact bytes"))
                .unwrap()
                .already_delivered
        );
        let after = Dir::open(&inbox)
            .unwrap()
            .status_nofollow(&sent.filename)
            .unwrap();
        assert_eq!(before.identity(), after.identity());
        assert_eq!(fs::read(&destination).unwrap(), b"exact bytes");

        // Restore can reuse the same database revision and ordinal without
        // replacing an inbox. New bytes must get a different retained file.
        let restored = restarted.deliver(None, artifact(b"new bytes")).unwrap();
        assert!(!restored.already_delivered);
        assert_ne!(restored.filename, sent.filename);
        assert_eq!(
            fs::read(inbox.join(restored.filename)).unwrap(),
            b"new bytes"
        );
        fs::write(&destination, b"tampered file").unwrap();
        assert_eq!(
            restarted
                .deliver(None, artifact(b"exact bytes"))
                .unwrap_err(),
            HandoffFailure::ArtifactConflict
        );
        assert_eq!(fs::read(destination).unwrap(), b"tampered file");
    }
}
