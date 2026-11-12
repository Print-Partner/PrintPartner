//! The measured release this build embeds, as a `DesktopLaunch` for a data
//! directory. Shared by the runtime and `pp-server` launcher tests.
use pp_core::{DesktopLaunch, VerifiedBundle};
use std::path::{Path, PathBuf};

pub fn launch(data: PathBuf) -> DesktopLaunch {
    launch_from_manifest(data, &manifest_path())
}

/// The release manifest this build embeds, as the build script chose it.
pub fn manifest_path() -> PathBuf {
    std::env::var_os("PP_DESKTOP_RELEASE_MANIFEST")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../rust/bundle-manifest.json")
                .canonicalize()
                .unwrap()
        })
}

pub fn launch_from_manifest(data: PathBuf, manifest: &Path) -> DesktopLaunch {
    let inventory: serde_json::Value =
        serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    let (web, node, release) = if inventory["dependencies"]["web_path"] == "." {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .unwrap();
        (
            root.join("web"),
            std::env::var_os("PP_TEST_NODE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/usr/bin/node")),
            inventory,
        )
    } else {
        let stage = manifest.parent().unwrap().canonicalize().unwrap();
        let identity = if stage.join("release.json").is_file() {
            stage.join("release.json")
        } else {
            stage.join("Resources/desktop-runtime/release.json")
        };
        let release: serde_json::Value =
            serde_json::from_slice(&std::fs::read(identity).unwrap()).unwrap();
        (
            stage.join(release["web"].as_str().unwrap()),
            stage.join(release["node"].as_str().unwrap()),
            release,
        )
    };
    DesktopLaunch {
        data_dir: data,
        assets: web.join("apps/web/dist"),
        bundle: VerifiedBundle {
            node,
            entry: web.join("apps/server/dist/current/desktop.js"),
            web_root: web,
            runtime_version: release["runtime_version"].as_str().unwrap().into(),
            commit: release["commit"].as_str().unwrap().into(),
        },
    }
}
