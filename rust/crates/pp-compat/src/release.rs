use crate::Bundle;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Immutable file bytes retained by release verification.
pub type AssetSnapshot = Arc<BTreeMap<String, Arc<[u8]>>>;
/// The replacement document every client path serves.
pub const REPLACEMENT_DOCUMENT: &str = "index.html";
/// The gateway copies the replacement document once per request to add its
/// style nonce, so a larger one refuses startup. The built one is under 1 KiB.
pub const MAX_REPLACEMENT_DOCUMENT_BYTES: usize = 1 << 20;

#[derive(Deserialize)]
struct DependencyInventory {
    web_path: String,
    roots: Vec<String>,
    files: BTreeMap<String, String>,
    links: BTreeMap<String, String>,
    /// Links to workspaces the desktop does not ship, such as
    /// `node_modules/@print-partner/ui` in a source checkout. Each is also in
    /// `links`; verification checks the link but does not follow it.
    #[serde(default)]
    unshipped_links: std::collections::BTreeSet<String>,
}

#[derive(Deserialize)]
pub(crate) struct ReleaseManifest {
    schema: u8,
    runtime_version: String,
    pub(crate) commit: String,
    node_sha256: String,
    frontend: BTreeMap<String, String>,
    #[serde(default)]
    replacement_frontend: BTreeMap<String, String>,
    backend: BTreeMap<String, String>,
    contracts: BTreeMap<String, String>,
    domain: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
    dependencies: DependencyInventory,
}

fn measure(path: &Path) -> Result<String> {
    let mut file = File::open(path).context("Release artifact missing")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// The manifest's name for `path` under `root`: its components joined with
/// `/` on every platform.
fn inventory_key(path: &Path, root: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)?
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn verify_files(
    root: &Path,
    expected: &BTreeMap<String, String>,
    exact: bool,
    stop: &CancellationToken,
) -> Result<()> {
    let root = file_inventory_root(root, expected, exact, stop)?;

    let entries: Vec<_> = expected.iter().collect();
    let workers = std::thread::available_parallelism().map_or(1, |count| count.get().min(8));
    std::thread::scope(|scope| {
        let handles: Vec<_> = entries
            .chunks(entries.len().div_ceil(workers))
            .map(|chunk| scope.spawn(|| verify_chunk(&root, chunk, stop)))
            .collect();
        handles.into_iter().try_for_each(|handle| {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("Release verification worker panicked"))?
        })
    })
}

fn file_inventory_root(
    root: &Path,
    expected: &BTreeMap<String, String>,
    exact: bool,
    stop: &CancellationToken,
) -> Result<std::path::PathBuf> {
    let root = root.canonicalize().context("Release root missing")?;
    ensure!(
        !expected.is_empty(),
        "Release inventory empty; rebuild the desktop manifest with just ci --only web-assets"
    );
    if exact {
        let mut actual = std::collections::BTreeSet::new();
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            ensure!(!stop.is_cancelled(), "Release verification cancelled");
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                ensure!(
                    !entry.file_type()?.is_symlink(),
                    "Release inventory contains a symlink"
                );
                if entry.file_type()?.is_dir() {
                    pending.push(entry.path());
                } else {
                    actual.insert(inventory_key(&entry.path(), &root)?);
                }
            }
        }
        ensure!(
            actual == expected.keys().cloned().collect(),
            "Release file inventory mismatch"
        );
    }

    Ok(root)
}

fn verify_chunk(
    root: &Path,
    entries: &[(&String, &String)],
    stop: &CancellationToken,
) -> Result<()> {
    for (relative, digest) in entries {
        ensure!(!stop.is_cancelled(), "Release verification cancelled");
        let path = root
            .join(relative)
            .canonicalize()
            .context("Release artifact missing")?;
        ensure!(path.starts_with(root), "Release artifact escaped root");
        ensure!(
            measure(&path)? == **digest,
            "Release artifact content mismatch"
        );
    }
    Ok(())
}

pub(crate) fn manifest() -> Result<ReleaseManifest> {
    let release: ReleaseManifest = serde_json::from_slice(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/bundle-manifest.json"
    )))?;
    ensure!(release.schema == 1, "Unsupported release inventory");
    Ok(release)
}

/// A bundle whose measured release passed [`verify_bundle`]. Only verification creates one,
/// and it is not `Clone`: each supervisor start consumes fresh evidence, so its first child
/// runs without measuring the release again.
pub struct MeasuredBundle(Bundle);

impl std::ops::Deref for MeasuredBundle {
    type Target = Bundle;

    fn deref(&self) -> &Bundle {
        &self.0
    }
}

#[cfg(test)]
impl MeasuredBundle {
    /// Test fixtures whose spawns are meant to fail. The first spawn trusts this value without
    /// measuring it; later spawns still measure it.
    pub(crate) fn unchecked(bundle: Bundle) -> Self {
        Self(bundle)
    }
}

pub fn verify_bundle(bundle: Bundle, assets: &Path) -> Result<MeasuredBundle> {
    let release = manifest()?;
    let stop = CancellationToken::new();
    verify_files(assets, &release.frontend, true, &stop)?;
    verify_backend(&bundle, &stop)?;
    Ok(MeasuredBundle(bundle))
}

/// The backend inventory's key prefix for `Bundle::community_manifests_dir`,
/// for example `data/manifests/`.
fn community_manifests_prefix(bundle: &Bundle) -> Result<(std::path::PathBuf, String)> {
    let directory = bundle
        .community_manifests_dir()
        .context("Missing backend root")?;
    let backend = bundle.entry.parent().context("Missing backend root")?;
    let mut prefix = inventory_key(&directory, backend)?;
    prefix.push('/');
    Ok((directory, prefix))
}

/// The release check for a Rust-only start, which runs no Node child. It checks
/// the embedded identity, the selected frontend (exact inventory) and the
/// community manifests Rust reads (exact inventory, when the release has any).
/// It does not measure the Node executable, the Node backend, the workspace
/// packages or `node_modules`, so they may be absent: the Docker image ships
/// none of them.
pub fn verify_rust_only_release(bundle: &Bundle, assets: &Path) -> Result<()> {
    verify_rust_only_with(&manifest()?, bundle, assets, &CancellationToken::new())
}

/// Read and verify the replacement inventory once; serving must use these bytes.
pub fn snapshot_rust_only_release(bundle: &Bundle, assets: &Path) -> Result<AssetSnapshot> {
    snapshot_rust_only_with(&manifest()?, bundle, assets, &CancellationToken::new())
}

fn snapshot_rust_only_with(
    release: &ReleaseManifest,
    bundle: &Bundle,
    assets: &Path,
    stop: &CancellationToken,
) -> Result<AssetSnapshot> {
    verify_rust_identity(release, bundle)?;
    ensure!(
        !std::fs::symlink_metadata(assets)
            .context("Release root missing")?
            .is_symlink(),
        "Release inventory contains a symlink"
    );
    for key in release.replacement_frontend.keys() {
        ensure!(
            !key.is_empty()
                && Path::new(key)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "Release artifact escaped root"
        );
    }
    let root = file_inventory_root(assets, &release.replacement_frontend, true, stop)?;
    ensure!(
        release
            .replacement_frontend
            .contains_key(REPLACEMENT_DOCUMENT),
        "Replacement UI has no index.html"
    );
    let mut files = BTreeMap::new();
    for (key, digest) in &release.replacement_frontend {
        ensure!(!stop.is_cancelled(), "Release verification cancelled");
        let path = root
            .join(key)
            .canonicalize()
            .context("Release artifact missing")?;
        ensure!(path.starts_with(&root), "Release artifact escaped root");
        let bytes = std::fs::read(path)?;
        // Checked before the digest, so even a measured document this large
        // refuses startup here, before recovery or the listener bind.
        ensure!(
            key != REPLACEMENT_DOCUMENT || bytes.len() <= MAX_REPLACEMENT_DOCUMENT_BYTES,
            "Replacement UI index.html exceeds 1 MiB"
        );
        ensure!(
            hex::encode(Sha256::digest(&bytes)) == *digest,
            "Release artifact content mismatch"
        );
        files.insert(key.clone(), Arc::<[u8]>::from(bytes));
    }
    verify_community_files(release, bundle, stop)?;
    Ok(Arc::new(files))
}

/// Whether the embedded manifest measures a replacement UI: it does when
/// `web/apps/ui/dist` was built before the manifest was written.
pub fn release_measures_replacement() -> Result<bool> {
    Ok(!manifest()?.replacement_frontend.is_empty())
}

/// Verify the legacy frontend when no replacement bundle is installed.
pub fn verify_rust_only_legacy_release(bundle: &Bundle, assets: &Path) -> Result<()> {
    let release = manifest()?;
    verify_rust_only_frontend_with(
        &release,
        bundle,
        assets,
        &release.frontend,
        &CancellationToken::new(),
    )
}

fn verify_rust_only_with(
    release: &ReleaseManifest,
    bundle: &Bundle,
    assets: &Path,
    stop: &CancellationToken,
) -> Result<()> {
    snapshot_rust_only_with(release, bundle, assets, stop).map(|_| ())
}

fn verify_rust_only_frontend_with(
    release: &ReleaseManifest,
    bundle: &Bundle,
    assets: &Path,
    frontend: &BTreeMap<String, String>,
    stop: &CancellationToken,
) -> Result<()> {
    verify_rust_identity(release, bundle)?;
    verify_files(assets, frontend, true, stop)?;
    verify_community_files(release, bundle, stop)
}

fn verify_rust_identity(release: &ReleaseManifest, bundle: &Bundle) -> Result<()> {
    ensure!(
        release.runtime_version == bundle.runtime_version && release.commit == bundle.commit,
        "Requested release differs from embedded identity"
    );
    Ok(())
}

fn verify_community_files(
    release: &ReleaseManifest,
    bundle: &Bundle,
    stop: &CancellationToken,
) -> Result<()> {
    let (directory, prefix) = community_manifests_prefix(bundle)?;
    let manifests: BTreeMap<String, String> = release
        .backend
        .iter()
        .filter_map(|(key, digest)| {
            key.strip_prefix(prefix.as_str())
                .map(|name| (name.to_owned(), digest.clone()))
        })
        .collect();
    if manifests.is_empty() {
        // Rust reads this directory when it exists, so an unmeasured one must be empty.
        ensure!(
            !directory.try_exists()? || std::fs::read_dir(&directory)?.next().is_none(),
            "Release file inventory mismatch"
        );
    } else {
        verify_files(&directory, &manifests, true, stop)?;
    }
    Ok(())
}

/// Returns early with an error once `stop` is cancelled; checked between files.
pub(crate) fn verify_backend(bundle: &Bundle, stop: &CancellationToken) -> Result<()> {
    let release = manifest()?;
    ensure!(
        release.runtime_version == bundle.runtime_version && release.commit == bundle.commit,
        "Requested release differs from embedded identity"
    );
    ensure!(
        measure(&bundle.node)? == release.node_sha256,
        "Node executable content mismatch"
    );
    ensure!(
        bundle.entry.file_name().and_then(|name| name.to_str()) == Some("desktop.js"),
        "Unexpected compatibility entry"
    );
    ensure!(
        release.backend.contains_key("desktop-resolution.js"),
        "Desktop preload is not measured"
    );
    let preload = bundle
        .entry
        .parent()
        .context("Missing backend root")?
        .join("desktop-resolution.js")
        .canonicalize()?;
    ensure!(
        preload.starts_with(package_root(bundle)?),
        "Desktop preload escaped package"
    );
    verify_files(
        bundle.entry.parent().context("Missing backend root")?,
        &release.backend,
        true,
        stop,
    )?;
    verify_files(
        &bundle.web_root.join("packages/contracts/dist/current"),
        &release.contracts,
        true,
        stop,
    )?;
    verify_files(
        &bundle.web_root.join("packages/domain/dist/current"),
        &release.domain,
        true,
        stop,
    )?;
    verify_files(&bundle.web_root, &release.metadata, false, stop)?;
    verify_dependencies(&bundle.web_root, &release.dependencies, stop)?;
    Ok(())
}

fn relative(path: &str) -> Result<&Path> {
    let path = Path::new(path);
    ensure!(
        !path.is_absolute()
            && path.components().all(|part| matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )),
        "Invalid dependency inventory path"
    );
    Ok(path)
}

fn inventory_root(web: &Path, inventory: &DependencyInventory) -> Result<std::path::PathBuf> {
    let web = web.canonicalize()?;
    let suffix = relative(&inventory.web_path)?;
    let mut root = web.clone();
    for component in suffix.components().rev() {
        if let std::path::Component::Normal(name) = component {
            ensure!(root.file_name() == Some(name), "Package layout mismatch");
            ensure!(root.pop(), "Package root unavailable");
        }
    }
    ensure!(
        root.join(suffix).canonicalize()? == web,
        "Package layout mismatch"
    );
    Ok(root)
}

pub(crate) fn package_root(bundle: &Bundle) -> Result<std::path::PathBuf> {
    inventory_root(&bundle.web_root, &manifest()?.dependencies)
}

/// A dependency link's recorded target: a relative path from the link's own
/// directory. build-desktop-manifest.py records a symbolic link's own text,
/// which must be relative, and a junction's target made relative and
/// `/`-separated, since Windows stores junction targets as absolute paths.
/// Verification requires every link to resolve where its recorded target
/// resolves, inside the package, and a symbolic link's text to equal it.
fn link_target(target: &str) -> Result<&Path> {
    let path = Path::new(target);
    ensure!(
        !target.is_empty()
            && path.components().all(|part| matches!(
                part,
                std::path::Component::Normal(_)
                    | std::path::Component::CurDir
                    | std::path::Component::ParentDir
            )),
        "Invalid dependency link target"
    );
    Ok(path)
}

fn verify_dependencies(
    web: &Path,
    inventory: &DependencyInventory,
    stop: &CancellationToken,
) -> Result<()> {
    let root = inventory_root(web, inventory)?;
    ensure!(
        !inventory.files.is_empty() && !inventory.roots.is_empty(),
        "Dependency inventory empty"
    );
    ensure!(
        inventory
            .unshipped_links
            .is_subset(&inventory.links.keys().cloned().collect()),
        "Unshipped dependency link is not inventoried"
    );
    let mut actual_files = std::collections::BTreeSet::new();
    let mut actual_links = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();
    for directory in &inventory.roots {
        let directory = root.join(relative(directory)?);
        if !directory.try_exists()? {
            continue;
        }
        // The walk canonicalizes a root, so a link here would get no link
        // record. build-desktop-manifest.py refuses it too. std reports a
        // Windows junction as a symbolic link, so this covers both.
        ensure!(
            !std::fs::symlink_metadata(&directory)?.is_symlink(),
            "Dependency root is a link"
        );
        ensure!(
            directory.canonicalize()?.starts_with(&root),
            "Dependency root escaped package"
        );
        let mut pending = vec![directory];
        while let Some(directory) = pending.pop() {
            ensure!(!stop.is_cancelled(), "Release verification cancelled");
            let directory = directory.canonicalize()?;
            ensure!(
                directory.starts_with(&root),
                "Dependency root escaped package"
            );
            if !visited.insert(directory.clone()) {
                continue;
            }
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                let path = entry.path();
                let key = inventory_key(&path, &root)?;
                let kind = entry.file_type()?;
                // std reports a Windows junction, which npm creates for
                // workspace links, as a symbolic link. Both are dependency
                // links, as in build-desktop-manifest.py; the recorded-target
                // check below is the only place they differ.
                if kind.is_symlink() {
                    actual_links.insert(key.clone());
                    if inventory.unshipped_links.contains(&key) {
                        continue;
                    }
                    let target = path.canonicalize()?;
                    ensure!(target.starts_with(&root), "Dependency link escaped package");
                    if target.is_file() {
                        actual_files.insert(inventory_key(&target, &root)?);
                    } else if target.is_dir() {
                        pending.push(target);
                    } else {
                        anyhow::bail!("Unexpected dependency link target");
                    }
                } else if kind.is_dir() {
                    pending.push(path);
                } else {
                    ensure!(kind.is_file(), "Unexpected dependency file type");
                    actual_files.insert(key);
                }
            }
        }
    }
    ensure!(
        actual_files == inventory.files.keys().cloned().collect(),
        "Dependency file inventory mismatch"
    );
    ensure!(
        actual_links == inventory.links.keys().cloned().collect(),
        "Dependency link inventory mismatch"
    );
    for (path, target) in &inventory.links {
        let link = root.join(relative(path)?);
        let recorded = link
            .parent()
            .context("Dependency link has no directory")?
            .join(link_target(target)?);
        let actual = link.canonicalize().ok();
        // As in build-desktop-manifest.py: a symbolic link records its own
        // text, and a junction, whose stored target is absolute, records its
        // target relative to the link's directory.
        ensure!(
            actual.is_some()
                && actual == recorded.canonicalize().ok()
                && (crate::platform::is_junction(&link)?
                    || std::fs::read_link(&link)? == Path::new(target)),
            "Dependency link changed"
        );
        // An unshipped link is not followed by the walk, so check here that
        // its target, like every link's, is still inside the package.
        ensure!(
            actual.is_some_and(|actual| actual.starts_with(&root)),
            "Dependency link escaped package"
        );
    }
    verify_files(&root, &inventory.files, false, stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::test_support::{symlink_dir, symlink_file};

    fn rust_only_fixture() -> (std::path::PathBuf, Bundle, ReleaseManifest) {
        let root = std::env::temp_dir().join(format!(
            "pp-rust-only-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let assets = root.join("web/apps/ui/dist");
        let bundle = Bundle {
            node: root.join("absent/node"),
            entry: root.join("web/apps/server/dist/current/desktop.js"),
            web_root: root.join("web"),
            runtime_version: "1.0.0-web".into(),
            commit: "abc".into(),
        };
        let manifests = bundle.community_manifests_dir().unwrap();
        std::fs::create_dir_all(assets.join("assets")).unwrap();
        std::fs::create_dir_all(&manifests).unwrap();
        std::fs::write(assets.join("index.html"), b"<!doctype html>").unwrap();
        std::fs::write(assets.join("assets/app.js"), b"export {};").unwrap();
        std::fs::write(manifests.join("registry-index.yaml"), b"entries: []\n").unwrap();
        let mut release: ReleaseManifest = serde_json::from_value(serde_json::json!({
            "schema": 1,
            "runtime_version": "1.0.0-web",
            "commit": "abc",
            "node_sha256": "0".repeat(64),
            "frontend": {"legacy.html": "0".repeat(64)},
            "replacement_frontend": {
                "index.html": measure(&assets.join("index.html")).unwrap(),
                "assets/app.js": measure(&assets.join("assets/app.js")).unwrap(),
            },
            "backend": {
                "desktop.js": "0".repeat(64),
                "desktop-resolution.js": "0".repeat(64),
            },
            "contracts": {"index.js": "0".repeat(64)},
            "domain": {"index.js": "0".repeat(64)},
            "metadata": {"package.json": "0".repeat(64)},
            "dependencies": {"web_path": "web", "roots": ["web/node_modules"],
                             "files": {"web/node_modules/x.js": "0".repeat(64)}, "links": {}},
        }))
        .unwrap();
        release.backend.insert(
            format!(
                "{}registry-index.yaml",
                community_manifests_prefix(&bundle).unwrap().1
            ),
            measure(&manifests.join("registry-index.yaml")).unwrap(),
        );
        (root, bundle, release)
    }

    #[test]
    fn rust_only_serves_only_the_measured_replacement_inventory() {
        let (root, bundle, mut release) = rust_only_fixture();
        let assets = root.join("web/apps/ui/dist");
        let stop = CancellationToken::new();
        verify_rust_only_with(&release, &bundle, &assets, &stop).unwrap();
        // The legacy inventory cannot authorize replacement bytes.
        std::mem::swap(&mut release.frontend, &mut release.replacement_frontend);
        assert!(verify_rust_only_with(&release, &bundle, &assets, &stop).is_err());
        release.replacement_frontend.clear();
        let error = verify_rust_only_with(&release, &bundle, &assets, &stop).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Release inventory empty; rebuild the desktop manifest with just ci --only web-assets"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A measured document over the limit, or none at all, refuses the
    /// release check itself, so startup stops before recovery and bind.
    #[test]
    fn replacement_snapshot_bounds_a_measured_document() {
        let (root, bundle, mut release) = rust_only_fixture();
        let assets = root.join("web/apps/ui/dist");
        let stop = CancellationToken::new();
        let document = assets.join(REPLACEMENT_DOCUMENT);
        let mut html = std::fs::read(&document).unwrap();
        html.resize(MAX_REPLACEMENT_DOCUMENT_BYTES, b' ');
        std::fs::write(&document, &html).unwrap();
        release
            .replacement_frontend
            .insert(REPLACEMENT_DOCUMENT.into(), measure(&document).unwrap());
        let snapshot = snapshot_rust_only_with(&release, &bundle, &assets, &stop).unwrap();
        assert_eq!(
            snapshot[REPLACEMENT_DOCUMENT].len(),
            MAX_REPLACEMENT_DOCUMENT_BYTES
        );

        html.push(b' ');
        std::fs::write(&document, &html).unwrap();
        release
            .replacement_frontend
            .insert(REPLACEMENT_DOCUMENT.into(), measure(&document).unwrap());
        let error = snapshot_rust_only_with(&release, &bundle, &assets, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Replacement UI index.html exceeds 1 MiB");

        std::fs::remove_file(&document).unwrap();
        release.replacement_frontend.remove(REPLACEMENT_DOCUMENT);
        let error = snapshot_rust_only_with(&release, &bundle, &assets, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Replacement UI has no index.html");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_inventory_refuses_a_missing_file() {
        let (root, bundle, release) = rust_only_fixture();
        let assets = root.join("web/apps/ui/dist");
        std::fs::remove_file(assets.join("assets/app.js")).unwrap();
        let error = verify_rust_only_with(&release, &bundle, &assets, &CancellationToken::new())
            .unwrap_err();
        assert_eq!(error.to_string(), "Release file inventory mismatch");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_inventory_refuses_a_symlink_to_measured_bytes() {
        let (root, bundle, release) = rust_only_fixture();
        let assets = root.join("web/apps/ui/dist");
        let target = root.join("measured.js");
        std::fs::rename(assets.join("assets/app.js"), &target).unwrap();
        symlink_file(&target, &assets.join("assets/app.js"));
        let error = verify_rust_only_with(&release, &bundle, &assets, &CancellationToken::new())
            .unwrap_err();
        assert_eq!(error.to_string(), "Release inventory contains a symlink");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_inventory_refuses_an_escaping_path() {
        let (root, bundle, mut release) = rust_only_fixture();
        let assets = root.join("web/apps/ui/dist");
        release
            .replacement_frontend
            .insert("../outside.js".into(), "0".repeat(64));
        let error = verify_rust_only_with(&release, &bundle, &assets, &CancellationToken::new())
            .unwrap_err();
        assert_eq!(error.to_string(), "Release artifact escaped root");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_inventory_refuses_a_symlinked_root() {
        let (root, bundle, release) = rust_only_fixture();
        let assets = root.join("web/apps/ui/dist");
        let alias = root.join("linked-dist");
        for target in [&assets, &root.join("missing-dist")] {
            symlink_dir(target, &alias);
            let error = verify_rust_only_with(&release, &bundle, &alias, &CancellationToken::new())
                .unwrap_err();
            assert_eq!(error.to_string(), "Release inventory contains a symlink");
            std::fs::remove_dir_all(&alias).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rust_only_release_needs_no_node_files() {
        let (root, bundle, release) = rust_only_fixture();
        let assets = root.join("web/apps/ui/dist");
        let stop = CancellationToken::new();
        // No Node executable, backend code, workspace packages or node_modules exist.
        verify_rust_only_with(&release, &bundle, &assets, &stop).unwrap();
        let refusal = |bundle: &Bundle| {
            verify_rust_only_with(&release, bundle, &assets, &stop)
                .unwrap_err()
                .to_string()
        };

        let mut other = bundle.clone();
        other.commit = "def".into();
        assert_eq!(
            refusal(&other),
            "Requested release differs from embedded identity"
        );
        std::fs::write(assets.join("assets/app.js"), b"changed").unwrap();
        assert_eq!(refusal(&bundle), "Release artifact content mismatch");
        std::fs::write(assets.join("assets/app.js"), b"export {};").unwrap();
        std::fs::write(assets.join("extra.js"), b"unmeasured").unwrap();
        assert_eq!(refusal(&bundle), "Release file inventory mismatch");
        std::fs::remove_file(assets.join("extra.js")).unwrap();

        let manifests = bundle.community_manifests_dir().unwrap();
        std::fs::write(manifests.join("registry-index.yaml"), b"changed").unwrap();
        assert_eq!(refusal(&bundle), "Release artifact content mismatch");
        std::fs::remove_dir_all(&manifests).unwrap();
        assert_eq!(refusal(&bundle), "Release root missing");

        // A release with no community manifests accepts no directory or an
        // empty one, and refuses unmeasured files there.
        let mut bare = release;
        bare.backend
            .retain(|key, _| !key.ends_with("registry-index.yaml"));
        verify_rust_only_with(&bare, &bundle, &assets, &stop).unwrap();
        std::fs::create_dir_all(&manifests).unwrap();
        verify_rust_only_with(&bare, &bundle, &assets, &stop).unwrap();
        std::fs::write(manifests.join("planted.yaml"), b"entries: []\n").unwrap();
        let error = verify_rust_only_with(&bare, &bundle, &assets, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Release file inventory mismatch");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_verification_stops_before_hashing() {
        let root = std::env::temp_dir().join(format!(
            "pp-release-cancel-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("artifact.js"), b"export {};\n").unwrap();
        let expected = BTreeMap::from([(
            "artifact.js".to_string(),
            measure(&root.join("artifact.js")).unwrap(),
        )]);
        verify_files(&root, &expected, true, &CancellationToken::new()).unwrap();
        let stop = CancellationToken::new();
        stop.cancel();
        let error = verify_files(&root, &expected, true, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Release verification cancelled");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dependency_aliases_are_measured_and_cannot_escape() {
        let root = std::env::temp_dir().join(format!(
            "pp-dependencies-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let web = root.join("Resources/desktop-runtime/web");
        let modules = web.join("node_modules");
        let framework = root.join("Frameworks/addon.node");
        std::fs::create_dir_all(&modules).unwrap();
        std::fs::create_dir_all(framework.parent().unwrap()).unwrap();
        std::fs::write(&framework, b"measured native addon").unwrap();
        let alias = modules.join("addon.node");
        let target = Path::new("../../../../Frameworks/addon.node");
        symlink_file(target, &alias);
        let inventory = DependencyInventory {
            web_path: "Resources/desktop-runtime/web".into(),
            roots: vec![
                "Resources/desktop-runtime/web/node_modules".into(),
                "Resources/desktop-runtime/web/apps/node_modules".into(),
            ],
            files: BTreeMap::from([("Frameworks/addon.node".into(), measure(&framework).unwrap())]),
            links: BTreeMap::from([(
                "Resources/desktop-runtime/web/node_modules/addon.node".into(),
                target.to_string_lossy().into_owned(),
            )]),
            unshipped_links: Default::default(),
        };
        verify_dependencies(&web, &inventory, &CancellationToken::new()).unwrap();
        let moved = root.with_extension("relocated");
        std::fs::rename(&root, &moved).unwrap();
        verify_dependencies(
            &moved.join("Resources/desktop-runtime/web"),
            &inventory,
            &CancellationToken::new(),
        )
        .unwrap();
        std::fs::rename(&moved, &root).unwrap();
        std::fs::write(&framework, b"changed signed bytes").unwrap();
        assert!(verify_dependencies(&web, &inventory, &CancellationToken::new()).is_err());
        std::fs::write(&framework, b"measured native addon").unwrap();
        std::fs::write(modules.join("unmeasured.js"), b"extra code").unwrap();
        assert!(verify_dependencies(&web, &inventory, &CancellationToken::new()).is_err());
        std::fs::remove_file(modules.join("unmeasured.js")).unwrap();
        let nearer = web.join("apps/node_modules");
        std::fs::create_dir_all(&nearer).unwrap();
        std::fs::write(nearer.join("shadow.js"), b"new search root").unwrap();
        assert!(verify_dependencies(&web, &inventory, &CancellationToken::new()).is_err());
        std::fs::remove_dir_all(&nearer).unwrap();
        // The generator refuses an absolute symbolic link, so verification
        // does too, even when it resolves to the recorded target.
        std::fs::remove_file(&alias).unwrap();
        symlink_file(&framework, &alias);
        let error = verify_dependencies(&web, &inventory, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.to_string(), "Dependency link changed");
        std::fs::remove_file(&alias).unwrap();
        symlink_file(Path::new("/usr/bin/node"), &alias);
        assert!(verify_dependencies(&web, &inventory, &CancellationToken::new()).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unshipped_workspace_links_are_checked_but_not_followed() {
        let root = std::env::temp_dir().join(format!(
            "pp-unshipped-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let web = root.join("web");
        let scope = web.join("node_modules/@print-partner");
        let library = web.join("node_modules/left-pad/index.js");
        let cache = web.join("apps/ui/node_modules/.vite/vitest/results.json");
        std::fs::create_dir_all(&scope).unwrap();
        std::fs::create_dir_all(library.parent().unwrap()).unwrap();
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&library, b"module.exports = 1;\n").unwrap();
        std::fs::write(web.join("apps/ui/package.json"), b"{}").unwrap();
        symlink_dir(Path::new("../../apps/ui"), &scope.join("ui"));
        let link = "web/node_modules/@print-partner/ui".to_string();
        let mut inventory = DependencyInventory {
            web_path: "web".into(),
            roots: vec!["web/node_modules".into()],
            files: BTreeMap::from([(
                "web/node_modules/left-pad/index.js".into(),
                measure(&library).unwrap(),
            )]),
            links: BTreeMap::from([(link.clone(), "../../apps/ui".into())]),
            unshipped_links: [link.clone()].into(),
        };
        let stop = CancellationToken::new();
        verify_dependencies(&web, &inventory, &stop).unwrap();
        std::fs::write(&cache, b"{\"results\":{}}").unwrap();
        verify_dependencies(&web, &inventory, &stop).unwrap();

        inventory.unshipped_links.clear();
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency file inventory mismatch");
        inventory.unshipped_links.insert(link.clone());

        inventory.links.clear();
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Unshipped dependency link is not inventoried"
        );
        inventory
            .links
            .insert(link.clone(), web.join("apps/ui").to_string_lossy().into());
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Invalid dependency link target");
        inventory.links.insert(link.clone(), "../../apps/ui".into());

        // The link is unchanged, but its target now resolves outside the
        // package, which the generator refuses.
        let outside = root.with_extension("outside");
        std::fs::rename(web.join("apps/ui"), &outside).unwrap();
        symlink_dir(&outside, &web.join("apps/ui"));
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency link escaped package");
        let moved = web.join("apps/ui");
        std::fs::remove_dir(&moved)
            .or_else(|_| std::fs::remove_file(&moved))
            .unwrap();
        std::fs::rename(&outside, web.join("apps/ui")).unwrap();
        verify_dependencies(&web, &inventory, &stop).unwrap();

        std::fs::create_dir_all(web.join("apps/other")).unwrap();
        let ui = scope.join("ui");
        std::fs::remove_dir(&ui)
            .or_else(|_| std::fs::remove_file(&ui))
            .unwrap();
        symlink_dir(Path::new("../../apps/other"), &ui);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency link changed");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A link at a resolver root would choose which measured directory Node
    /// resolves from without a link record, so both tools refuse it, whether
    /// it is a symbolic link or (on Windows) a junction.
    #[test]
    fn resolver_root_link_is_refused() {
        let root = std::env::temp_dir().join(format!(
            "pp-root-link-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let web = root.join("web");
        let vendor = web.join("vendor/left-pad/index.js");
        std::fs::create_dir_all(vendor.parent().unwrap()).unwrap();
        std::fs::write(&vendor, b"module.exports = 1;\n").unwrap();
        let inventory = DependencyInventory {
            web_path: "web".into(),
            roots: vec!["web/node_modules".into()],
            files: BTreeMap::from([(
                "web/vendor/left-pad/index.js".into(),
                measure(&vendor).unwrap(),
            )]),
            links: Default::default(),
            unshipped_links: Default::default(),
        };
        symlink_dir(Path::new("vendor"), &web.join("node_modules"));
        let error = verify_dependencies(&web, &inventory, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.to_string(), "Dependency root is a link");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A retargeted link whose old and new targets are both walked leaves the
    /// file inventory equal, so the link check is what rejects it. Windows
    /// runs the junction form in `junctions_are_recorded_links_and_cannot_escape`.
    #[test]
    fn retargeted_release_link_is_reported_as_changed() {
        let root = std::env::temp_dir().join(format!(
            "pp-retarget-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let web = root.join("web");
        let release = web.join("apps/server/dist");
        let mut files = BTreeMap::new();
        for version in ["v0", "v1"] {
            std::fs::create_dir_all(release.join(version)).unwrap();
            std::fs::write(release.join(version).join("server.js"), b"export {};\n").unwrap();
            files.insert(
                format!("web/apps/server/dist/{version}/server.js"),
                measure(&release.join(version).join("server.js")).unwrap(),
            );
        }
        let current = release.join("current");
        symlink_dir(Path::new("v1"), &current);
        let inventory = DependencyInventory {
            web_path: "web".into(),
            roots: vec!["web/apps/server/dist".into()],
            files,
            links: BTreeMap::from([("web/apps/server/dist/current".into(), "v1".into())]),
            unshipped_links: Default::default(),
        };
        let stop = CancellationToken::new();
        verify_dependencies(&web, &inventory, &stop).unwrap();

        std::fs::remove_dir(&current)
            .or_else(|_| std::fs::remove_file(&current))
            .unwrap();
        symlink_dir(Path::new("v0"), &current);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency link changed");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// npm's Windows workspace links are junctions. They are dependency links
    /// like symbolic links: inventoried by key, checked against a target
    /// recorded relative to the link's directory, and followed.
    #[cfg(windows)]
    #[test]
    fn junctions_are_recorded_links_and_cannot_escape() {
        use crate::logs::test_support::junction;
        let root = std::env::temp_dir().join(format!(
            "pp-junctions-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let web = root.join("web");
        let scope = web.join("node_modules/@pp");
        std::fs::create_dir_all(&scope).unwrap();
        let mut files = BTreeMap::new();
        let mut links = BTreeMap::new();
        for name in ["contracts", "domain"] {
            let package = web.join("packages").join(name);
            std::fs::create_dir_all(&package).unwrap();
            std::fs::write(package.join("index.js"), b"export {};\n").unwrap();
            files.insert(
                format!("web/packages/{name}/index.js"),
                measure(&package.join("index.js")).unwrap(),
            );
            junction(&package, &scope.join(name));
            links.insert(
                format!("web/node_modules/@pp/{name}"),
                format!("../../packages/{name}"),
            );
        }
        let release = web.join("apps/server/dist");
        for version in ["v0", "v1"] {
            std::fs::create_dir_all(release.join(version)).unwrap();
            std::fs::write(release.join(version).join("server.js"), b"export {};\n").unwrap();
            files.insert(
                format!("web/apps/server/dist/{version}/server.js"),
                measure(&release.join(version).join("server.js")).unwrap(),
            );
        }
        junction(&release.join("v1"), &release.join("current"));
        links.insert("web/apps/server/dist/current".into(), "v1".into());
        let mut inventory = DependencyInventory {
            web_path: "web".into(),
            roots: vec!["web/apps/server/dist".into(), "web/node_modules".into()],
            files,
            links,
            unshipped_links: Default::default(),
        };
        let stop = CancellationToken::new();
        verify_dependencies(&web, &inventory, &stop).unwrap();

        // The pre-GRE-1204 manifest, which walked junctions as directories.
        let walked = DependencyInventory {
            web_path: inventory.web_path.clone(),
            roots: inventory.roots.clone(),
            files: inventory.files.clone(),
            links: Default::default(),
            unshipped_links: Default::default(),
        };
        let error = verify_dependencies(&web, &walked, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency link inventory mismatch");

        // Both release directories are walked, so retargeting `current`
        // keeps the file inventory equal and the link check reports it.
        let current = release.join("current");
        std::fs::remove_dir(&current).unwrap();
        junction(&release.join("v0"), &current);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency link changed");
        std::fs::remove_dir(&current).unwrap();
        junction(&release.join("v1"), &current);
        verify_dependencies(&web, &inventory, &stop).unwrap();

        // Retargeting the only link to `contracts` also drops its files from
        // the walk, which the file check reports first.
        let link = scope.join("contracts");
        std::fs::remove_dir(&link).unwrap();
        junction(&web.join("packages/domain"), &link);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency file inventory mismatch");

        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::remove_dir(&link).unwrap();
        junction(&outside, &link);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency link escaped package");
        std::fs::remove_dir(&link).unwrap();
        junction(&web.join("packages/contracts"), &link);
        verify_dependencies(&web, &inventory, &stop).unwrap();

        let extra = scope.join("extra");
        junction(&web.join("packages/domain"), &extra);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency link inventory mismatch");
        std::fs::remove_dir(&extra).unwrap();

        // A junction at a resolver root is refused, as before GRE-1204.
        let server_modules = web.join("apps/server/node_modules");
        junction(&web.join("node_modules"), &server_modules);
        inventory.roots.push("web/apps/server/node_modules".into());
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency root is a link");
        std::fs::remove_dir(&server_modules).unwrap();
        inventory.roots.pop();

        inventory.links.insert(
            "web/node_modules/@pp/contracts".into(),
            web.join("packages/contracts")
                .to_string_lossy()
                .into_owned(),
        );
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Invalid dependency link target");
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
