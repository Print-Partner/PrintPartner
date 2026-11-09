use crate::Bundle;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};
use tokio_util::sync::CancellationToken;

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

/// A junction whose path after the last `node_modules` segment (in any letter
/// case, since Windows paths are case-insensitive) is exactly `name` or
/// `@scope/name` is a module root: npm creates workspace links such as
/// `node_modules/@print-partner/contracts` as junctions on Windows. It must
/// resolve to the package its path names: the `package.json` `name` at its
/// target equals that suffix, and a target without a readable `package.json`
/// fails. A junction at a `node_modules` segment itself, or at a bare
/// `@scope`, names no module and fails. Other junctions are only walked, with
/// their canonical target inside the root: build-output junctions such as
/// `apps/server/dist/current` and `packages/contracts/dist/current`, which
/// `web/scripts/atomic-package-build.mjs` creates, whether they sit outside
/// `node_modules` or deeper inside a package, where Node resolves only
/// subpaths and never a package name.
///
/// `key` is the logical path from the inventory root, the path Node resolves
/// through, not the canonical target. A walk root that is itself a junction is
/// checked the same way, so a `node_modules` root junction fails as naming no
/// module. The walk visits a directory once outside `node_modules` and once
/// inside it, so reaching it first through a non-module junction cannot skip
/// the check.
fn verify_junction_package(junction: &Path, key: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct Package {
        name: String,
    }
    let parts: Vec<&str> = key.split('/').collect();
    let Some(segment) = parts
        .iter()
        .rposition(|part| part.eq_ignore_ascii_case("node_modules"))
    else {
        return Ok(());
    };
    let module = match &parts[segment + 1..] {
        [] => None,
        [scope] if scope.starts_with('@') => None,
        module @ [_] => Some(module),
        module @ [scope, _] if scope.starts_with('@') => Some(module),
        _ => return Ok(()),
    }
    .with_context(|| format!("Dependency junction {key} names no module"))?
    .join("/");
    let package: Package = std::fs::read(junction.join("package.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .with_context(|| format!("Dependency junction {key} has no readable package.json"))?;
    ensure!(
        package.name == module,
        "Dependency junction {key} targets package {}",
        package.name
    );
    Ok(())
}

fn verify_files(
    root: &Path,
    expected: &BTreeMap<String, String>,
    exact: bool,
    stop: &CancellationToken,
) -> Result<()> {
    let root = root.canonicalize().context("Release root missing")?;
    ensure!(!expected.is_empty(), "Release inventory empty");
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
/// the embedded identity, the served frontend (exact inventory) and the
/// community manifests Rust reads (exact inventory, when the release has any).
/// It does not measure the Node executable, the Node backend, the workspace
/// packages or `node_modules`, so they may be absent: the Docker image ships
/// none of them.
pub fn verify_rust_only_release(bundle: &Bundle, assets: &Path) -> Result<()> {
    verify_rust_only_with(&manifest()?, bundle, assets, &CancellationToken::new())
}

fn verify_rust_only_with(
    release: &ReleaseManifest,
    bundle: &Bundle,
    assets: &Path,
    stop: &CancellationToken,
) -> Result<()> {
    ensure!(
        release.runtime_version == bundle.runtime_version && release.commit == bundle.commit,
        "Requested release differs from embedded identity"
    );
    verify_files(assets, &release.frontend, true, stop)?;
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
    for root_key in &inventory.roots {
        let directory = root.join(relative(root_key)?);
        if !directory.try_exists()? {
            continue;
        }
        ensure!(
            directory.canonicalize()?.starts_with(&root),
            "Dependency root escaped package"
        );
        if std::fs::symlink_metadata(&directory)?.is_symlink()
            && crate::platform::is_junction(&directory)?
        {
            verify_junction_package(&directory, root_key)?;
        }
        let mut pending = vec![(directory, root_key.clone())];
        while let Some((directory, logical)) = pending.pop() {
            ensure!(!stop.is_cancelled(), "Release verification cancelled");
            let directory = directory.canonicalize()?;
            ensure!(
                directory.starts_with(&root),
                "Dependency root escaped package"
            );
            let in_modules = logical
                .split('/')
                .any(|part| part.eq_ignore_ascii_case("node_modules"));
            if !visited.insert((directory.clone(), in_modules)) {
                continue;
            }
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                let path = entry.path();
                let key = inventory_key(&path, &root)?;
                let logical = format!("{logical}/{}", entry.file_name().to_string_lossy());
                let kind = entry.file_type()?;
                let junction = kind.is_symlink() && crate::platform::is_junction(&path)?;
                if kind.is_symlink() && !junction {
                    actual_links.insert(key.clone());
                    if inventory.unshipped_links.contains(&key) {
                        continue;
                    }
                    let target = path.canonicalize()?;
                    ensure!(target.starts_with(&root), "Dependency link escaped package");
                    if target.is_file() {
                        actual_files.insert(inventory_key(&target, &root)?);
                    } else if target.is_dir() {
                        pending.push((target, logical));
                    } else {
                        anyhow::bail!("Unexpected dependency link target");
                    }
                } else if junction {
                    verify_junction_package(&path, &logical)?;
                    pending.push((path, logical));
                } else if kind.is_dir() {
                    pending.push((path, logical));
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
        ensure!(
            std::fs::read_link(root.join(relative(path)?))? == Path::new(target),
            "Dependency link changed"
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
        let assets = root.join("web/apps/web/dist");
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
            "frontend": {
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
    fn rust_only_release_needs_no_node_files() {
        let (root, bundle, release) = rust_only_fixture();
        let assets = root.join("web/apps/web/dist");
        let stop = CancellationToken::new();
        // No Node executable, backend code, workspace packages or node_modules exist.
        verify_rust_only_with(&release, &bundle, &assets, &stop).unwrap();

        let mut other = bundle.clone();
        other.commit = "def".into();
        let error = verify_rust_only_with(&release, &other, &assets, &stop).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Requested release differs from embedded identity"
        );

        std::fs::write(assets.join("assets/app.js"), b"changed").unwrap();
        let error = verify_rust_only_with(&release, &bundle, &assets, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Release artifact content mismatch");
        std::fs::write(assets.join("assets/app.js"), b"export {};").unwrap();

        std::fs::write(assets.join("extra.js"), b"unmeasured").unwrap();
        let error = verify_rust_only_with(&release, &bundle, &assets, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Release file inventory mismatch");
        std::fs::remove_file(assets.join("extra.js")).unwrap();

        let manifests = bundle.community_manifests_dir().unwrap();
        std::fs::write(manifests.join("registry-index.yaml"), b"changed").unwrap();
        let error = verify_rust_only_with(&release, &bundle, &assets, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Release artifact content mismatch");
        std::fs::remove_dir_all(&manifests).unwrap();
        let error = verify_rust_only_with(&release, &bundle, &assets, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Release root missing");

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
        inventory.links.insert(link.clone(), "../../apps/ui".into());

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

    #[cfg(windows)]
    #[test]
    fn junctions_are_walked_as_directories_and_cannot_escape() {
        use crate::logs::test_support::junction;
        let root = std::env::temp_dir().join(format!(
            "pp-junctions-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let web = root.join("web");
        let scope = web.join("node_modules/@pp");
        std::fs::create_dir_all(&scope).unwrap();
        let mut files = BTreeMap::new();
        for name in ["contracts", "domain"] {
            let package = web.join("packages").join(name);
            std::fs::create_dir_all(&package).unwrap();
            std::fs::write(
                package.join("package.json"),
                format!(r#"{{"name":"@pp/{name}"}}"#),
            )
            .unwrap();
            std::fs::write(package.join("index.js"), b"export {};\n").unwrap();
            for file in ["package.json", "index.js"] {
                files.insert(
                    format!("web/packages/{name}/{file}"),
                    measure(&package.join(file)).unwrap(),
                );
            }
            junction(&package, &scope.join(name));
        }
        let build = web.join("packages/contracts/dist");
        std::fs::create_dir_all(build.join("v1")).unwrap();
        std::fs::write(build.join("v1/index.js"), b"export {};\n").unwrap();
        files.insert(
            "web/packages/contracts/dist/v1/index.js".into(),
            measure(&build.join("v1/index.js")).unwrap(),
        );
        junction(&build.join("v1"), &build.join("current"));
        let release = web.join("apps/server/dist");
        std::fs::create_dir_all(release.join("v1")).unwrap();
        std::fs::write(release.join("v1/server.js"), b"export {};\n").unwrap();
        files.insert(
            "web/apps/server/dist/v1/server.js".into(),
            measure(&release.join("v1/server.js")).unwrap(),
        );
        junction(&release.join("v1"), &release.join("current"));
        let inventory = DependencyInventory {
            web_path: "web".into(),
            roots: vec!["web/apps/server/dist".into(), "web/node_modules".into()],
            files,
            links: Default::default(),
            unshipped_links: Default::default(),
        };
        let stop = CancellationToken::new();
        verify_dependencies(&web, &inventory, &stop).unwrap();

        let decoy = web.join("decoy/@pp");
        std::fs::create_dir_all(&decoy).unwrap();
        junction(&web.join("packages/domain"), &decoy.join("contracts"));
        let server_modules = web.join("apps/server/node_modules");
        let decoyed = DependencyInventory {
            web_path: "web".into(),
            roots: vec![
                "web/apps/server/dist".into(),
                "web/node_modules".into(),
                "web/apps/server/node_modules".into(),
            ],
            files: inventory.files.clone(),
            links: Default::default(),
            unshipped_links: Default::default(),
        };
        junction(&web.join("decoy"), &server_modules);
        let error = verify_dependencies(&web, &decoyed, &stop).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Dependency junction web/apps/server/node_modules names no module"
        );
        std::fs::remove_dir(&server_modules).unwrap();
        std::fs::create_dir(&server_modules).unwrap();
        crate::logs::test_support::symlink_dir(&decoy, &server_modules.join("@pp"));
        let error = verify_dependencies(&web, &decoyed, &stop).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Dependency junction web/apps/server/node_modules/@pp/contracts targets package @pp/domain"
        );
        std::fs::remove_dir_all(&server_modules).unwrap();

        let link = scope.join("contracts");
        std::fs::remove_dir(&link).unwrap();
        junction(&web.join("packages/domain"), &link);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Dependency junction web/node_modules/@pp/contracts targets package @pp/domain"
        );
        std::fs::rename(web.join("node_modules"), web.join("Node_Modules")).unwrap();
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Dependency junction web/node_modules/@pp/contracts targets package @pp/domain"
        );
        std::fs::rename(web.join("Node_Modules"), web.join("node_modules")).unwrap();

        let bare = web.join("packages/bare");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::remove_dir(&link).unwrap();
        junction(&bare, &link);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Dependency junction web/node_modules/@pp/contracts has no readable package.json"
        );

        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("package.json"), br#"{"name":"@pp/contracts"}"#).unwrap();
        std::fs::remove_dir(&link).unwrap();
        junction(&outside, &link);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(error.to_string(), "Dependency root escaped package");
        std::fs::remove_dir(&link).unwrap();

        let unnamed = release.join("Node_Modules");
        junction(&release.join("v1"), &unnamed);
        let error = verify_dependencies(&web, &inventory, &stop).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Dependency junction web/apps/server/dist/Node_Modules names no module"
        );
        std::fs::remove_dir(&unnamed).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
