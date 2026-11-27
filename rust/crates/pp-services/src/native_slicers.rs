//! Native slicer discovery and managed Plate handoff, separate from profile-file discovery.

use crate::discovery::SlicerKind;
use serde::Serialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Default, Serialize)]
pub struct Picker {
    pub instances: Vec<Instance>,
    pub default_instance_id: Option<String>,
}

#[derive(Serialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub available: bool,
}

/// A standalone 3MF generated from one verified managed Plate snapshot.
pub struct Artifact {
    pub plate_revision_id: i64,
    pub plate_ordinal: u32,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffFailure {
    UnknownInstance,
    NoDefault,
    Unavailable,
    ArtifactConflict,
}

#[derive(Debug, Serialize)]
pub struct Delivery {
    pub instance_id: String,
    pub filename: String,
    pub already_delivered: bool,
}

/// Delivers one immutable artifact to the chosen instance. Replays
/// must not hand the same artifact off again.
pub trait Handoff: Send + Sync {
    fn picker(&self) -> Picker;
    fn deliver(
        &self,
        instance_id: Option<&str>,
        artifact: Artifact,
    ) -> Result<Delivery, HandoffFailure>;
}

#[derive(Clone, Debug)]
pub struct NativeRoots {
    pub home_dir: PathBuf,
    pub system_applications: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSlicer {
    pub kind: SlicerKind,
    pub executable: PathBuf,
    pub version: String,
    pub config_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeDiscovery {
    Installed(NativeSlicer),
    Absent,
    Unsupported,
    InvalidBundle {
        path: PathBuf,
        reason: BundleProblem,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundleProblem {
    Metadata,
    Executable,
    Version,
}

#[cfg(target_os = "macos")]
impl NativeRoots {
    pub fn for_home(home_dir: PathBuf) -> Self {
        Self {
            home_dir,
            system_applications: PathBuf::from("/Applications"),
        }
    }
}

/// Discovers only the current platform. The caller owns home-directory resolution.
pub fn discover_native_kind(kind: &str, roots: &NativeRoots) -> io::Result<NativeDiscovery> {
    let Some(kind) = SlicerKind::from_kind(kind) else {
        return Ok(NativeDiscovery::Unsupported);
    };
    #[cfg(target_os = "macos")]
    {
        discover_macos(kind, roots)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (kind, roots);
        Ok(NativeDiscovery::Unsupported)
    }
}

/// Portable macOS layout reader for fixtures. Reads bundle metadata without launching apps.
/// Searches system Applications before user Applications and returns the first usable bundle.
pub fn discover_macos(kind: SlicerKind, roots: &NativeRoots) -> io::Result<NativeDiscovery> {
    let names: &[&str] = match kind {
        SlicerKind::Orca => &["OrcaSlicer.app"],
        SlicerKind::Bambu => &["BambuStudio.app", "Bambu Studio.app"],
    };
    let mut result = NativeDiscovery::Absent;
    for applications in [
        &roots.system_applications,
        &roots.home_dir.join("Applications"),
    ] {
        for name in names {
            let bundle = applications.join(name);
            let Some(metadata) = metadata_if_present(&bundle)? else {
                continue;
            };
            let invalid = |reason| NativeDiscovery::InvalidBundle {
                path: bundle.clone(),
                reason,
            };
            if !metadata.is_dir() {
                result = invalid(BundleProblem::Metadata);
                continue;
            }
            let executable = bundle.join("Contents/MacOS").join(kind.app_dir());
            let Some(binary) = metadata_if_present(&executable)? else {
                result = invalid(BundleProblem::Executable);
                continue;
            };
            if !binary.is_file() || !is_executable(&binary) {
                result = invalid(BundleProblem::Executable);
                continue;
            }
            let canonical_bundle = fs::canonicalize(&bundle)?;
            if !canonical_bundle.starts_with(fs::canonicalize(applications)?)
                || !fs::canonicalize(&executable)?.starts_with(&canonical_bundle)
            {
                result = invalid(BundleProblem::Executable);
                continue;
            }
            let Ok(metadata_path) = fs::canonicalize(bundle.join("Contents/Info.plist")) else {
                result = invalid(BundleProblem::Metadata);
                continue;
            };
            if !metadata_path.starts_with(&canonical_bundle) || !metadata_path.is_file() {
                result = invalid(BundleProblem::Metadata);
                continue;
            }
            let Ok(metadata) = plist::Value::from_file(metadata_path) else {
                result = invalid(BundleProblem::Metadata);
                continue;
            };
            let Some(metadata) = metadata.as_dictionary() else {
                result = invalid(BundleProblem::Metadata);
                continue;
            };
            if metadata
                .get("CFBundleExecutable")
                .and_then(plist::Value::as_string)
                != Some(kind.app_dir())
            {
                result = invalid(BundleProblem::Executable);
                continue;
            }
            let Some(version) = metadata
                .get("CFBundleShortVersionString")
                .and_then(plist::Value::as_string)
                .filter(|s| !s.trim().is_empty())
            else {
                result = invalid(BundleProblem::Version);
                continue;
            };
            let config = roots
                .home_dir
                .join("Library/Application Support")
                .join(kind.app_dir());
            let config_dir = metadata_if_present(&config)?
                .filter(fs::Metadata::is_dir)
                .map(|_| config);
            return Ok(NativeDiscovery::Installed(NativeSlicer {
                kind,
                executable,
                version: version.to_owned(),
                config_dir,
            }));
        }
    }
    Ok(result)
}

fn metadata_if_present(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}
