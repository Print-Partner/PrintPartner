use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Assets(PathBuf);

impl Assets {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-spa-packaging-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(
            root.join("index.html"),
            "<script src=\"/assets/app.js\"></script>",
        )
        .unwrap();
        fs::write(root.join("assets/app.js"), "console.log('desktop');\n").unwrap();
        fs::write(root.join("assets/app.css"), "body { color: black; }\n").unwrap();
        Self(root)
    }

    fn run(&self, validate: bool) -> std::process::Output {
        self.run_root(&self.0, validate, None)
    }

    fn run_root(
        &self,
        root: &Path,
        validate: bool,
        boundary: Option<&Path>,
    ) -> std::process::Output {
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/precompress-spa.py");
        let mut command = Command::new("python3");
        command.arg(script).arg(root);
        if validate {
            command.arg("--validate");
        }
        if let Some(boundary) = boundary {
            command.arg("--boundary").arg(boundary);
        }
        command.output().unwrap()
    }

    fn prepare(&self) {
        let output = self.run(false);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout)
                .unwrap()
                .replace("\r\n", "\n"),
            "{\"assets\": 3, \"encoding\": \"gzip\"}\n"
        );
    }

    fn rejected(&self, reason: &str) {
        let output = self.run(true);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Assets {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn ticket_t_11_packaging_gzip_roundtrip_is_deterministic() {
    let assets = Assets::new();
    fs::write(assets.0.join("assets/app.css"), "body{}\n".repeat(200_000)).unwrap();
    assets.prepare();
    let gzip = fs::read(assets.0.join("assets/app.js.gz")).unwrap();
    assert_eq!(&gzip[..10], &[31, 139, 8, 0, 0, 0, 0, 0, 2, 255]);
    let output = Command::new("python3").arg("-c")
        .arg("import gzip,sys;sys.stdout.buffer.write(gzip.decompress(open(sys.argv[1],'rb').read()))")
        .arg(assets.0.join("assets/app.js.gz")).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"console.log('desktop');\n");
    assets.prepare();
    assert_eq!(fs::read(assets.0.join("assets/app.js.gz")).unwrap(), gzip);
    assert!(assets.run(true).status.success());
    let mut large = fs::read(assets.0.join("assets/app.css")).unwrap();
    *large.last_mut().unwrap() = b'x';
    fs::write(assets.0.join("assets/app.css"), large).unwrap();
    assets.rejected("Gzip content mismatch: assets/app.css");
}

#[test]
fn ticket_t_11_packaging_missing_variant_is_rejected() {
    let assets = Assets::new();
    assets.prepare();
    fs::remove_file(assets.0.join("assets/app.js.gz")).unwrap();
    assets.rejected("Missing gzip variant: assets/app.js");
}

#[test]
fn ticket_t_11_packaging_changed_identity_is_rejected() {
    let assets = Assets::new();
    assets.prepare();
    fs::write(assets.0.join("assets/app.js"), "different release").unwrap();
    assets.rejected("Gzip content mismatch: assets/app.js");
    let output = assets.run(false);
    assert!(
        !output.status.success(),
        "Preparation must refuse a stale variant"
    );
}

#[test]
fn ticket_t_11_packaging_replaces_foreign_gzip_with_canonical_bytes() {
    let assets = Assets::new();
    assets.prepare();
    let expected = fs::read(assets.0.join("assets/app.js.gz")).unwrap();
    let output = Command::new("python3")
        .arg("-c")
        .arg("import gzip,pathlib,sys;p=pathlib.Path(sys.argv[1]);p.with_name(p.name+'.gz').write_bytes(gzip.compress(p.read_bytes(),compresslevel=1,mtime=123))")
        .arg(assets.0.join("assets/app.js"))
        .output().unwrap();
    assert!(output.status.success());
    assets.prepare();
    assert_eq!(
        fs::read(assets.0.join("assets/app.js.gz")).unwrap(),
        expected
    );
}

#[cfg(unix)]
#[test]
fn ticket_t_11_packaging_symlinked_root_cannot_write_outside() {
    let assets = Assets::new();
    let outside = Assets::new();
    let alias = assets.0.join("linked");
    std::os::unix::fs::symlink(&outside.0, &alias).unwrap();
    let output = assets.run_root(&alias, false, None);
    assert!(
        !output.status.success(),
        "Must reject root alias before writing gzip"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("SPA asset symlink: linked"));
    assert!(!outside.0.join("index.html.gz").exists());
}

#[cfg(unix)]
#[test]
fn ticket_t_11_packaging_rejects_ancestor_links_below_trusted_boundary() {
    let assets = Assets::new();
    let outside = Assets::new();
    fs::write(outside.0.join("assets/index.html"), "<html>outside</html>").unwrap();
    let alias = assets.0.join("linked");
    std::os::unix::fs::symlink(&outside.0, &alias).unwrap();
    let output = assets.run_root(&alias.join("assets"), false, Some(&assets.0));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("SPA asset symlink: linked"));
    assert!(!outside.0.join("assets/index.html.gz").exists());
}

#[test]
fn ticket_t_11_packaging_corrupt_and_orphan_variants_are_rejected() {
    let assets = Assets::new();
    assets.prepare();
    fs::write(assets.0.join("assets/app.js.gz"), b"bad gzip").unwrap();
    assets.rejected("Invalid gzip variant: assets/app.js");
    fs::remove_file(assets.0.join("assets/app.js.gz")).unwrap();
    assets.prepare();
    fs::remove_file(assets.0.join("assets/app.js")).unwrap();
    assets.rejected("Orphan gzip variant: assets/app.js.gz");
}

#[test]
fn ticket_t_11_packaging_missing_entrypoint_is_rejected() {
    let assets = Assets::new();
    fs::remove_file(assets.0.join("index.html")).unwrap();
    assets.rejected("Missing SPA entrypoint: index.html");
}

#[test]
fn ticket_t_11_packaging_manifest_measures_both_representations() {
    let assets = Assets::new();
    let web = assets.0.join("web");
    let frontend = web.join("apps/web/dist");
    fs::create_dir_all(&frontend).unwrap();
    fs::write(frontend.join("index.html"), "<html>desktop</html>").unwrap();
    fs::write(
        frontend.join("desktop-build.json"),
        r#"{"mode":"desktop","service_worker":false,"version":"1.0.0"}"#,
    )
    .unwrap();
    for file in [
        "apps/server/dist/current/desktop-resolution.js",
        "packages/contracts/dist/current/index.js",
        "packages/domain/dist/current/index.js",
        "node_modules/runtime/index.js",
    ] {
        let path = web.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "export {};").unwrap();
    }
    for file in [
        "package.json",
        "package-lock.json",
        "apps/server/package.json",
        "packages/contracts/package.json",
        "packages/domain/package.json",
    ] {
        fs::write(web.join(file), r#"{"version":"1.0.0"}"#).unwrap();
    }
    let manifest = assets.0.join("manifest.json");
    let node = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    let node = String::from_utf8(node.stdout).unwrap();
    let build = || {
        Command::new("python3")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../scripts/build-desktop-manifest.py"),
            )
            .arg("--web")
            .arg(&web)
            .arg("--node")
            .arg(node.trim())
            .arg("--output")
            .arg(&manifest)
            .arg("--commit")
            .arg("a".repeat(40))
            .output()
            .unwrap()
    };
    let output = build();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let measured: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let frontend_inventory = measured["frontend"].as_object().unwrap();
    assert_eq!(frontend_inventory.len(), 3);
    for file in ["index.html", "index.html.gz", "desktop-build.json"] {
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(fs::read(frontend.join(file)).unwrap()));
        assert_eq!(frontend_inventory[file], digest);
    }
    let before = fs::read(&manifest).unwrap();
    fs::write(frontend.join("index.html.gz"), b"corrupt").unwrap();
    let output = build();
    assert!(
        !output.status.success(),
        "Manifest must refuse corrupt gzip"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid gzip variant: index.html"));
    assert_eq!(fs::read(manifest).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn ticket_t_11_packaging_preserves_asset_read_permissions() {
    use std::os::unix::fs::PermissionsExt;
    for expected in [0o644, 0o640] {
        let assets = Assets::new();
        fs::set_permissions(
            assets.0.join("assets/app.js"),
            fs::Permissions::from_mode(expected),
        )
        .unwrap();
        assets.prepare();
        let mode = fs::metadata(assets.0.join("assets/app.js.gz"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, expected);
        fs::set_permissions(
            assets.0.join("assets/app.js.gz"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assets.prepare();
        assert_eq!(
            fs::metadata(assets.0.join("assets/app.js.gz"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            expected,
        );
    }
}

#[cfg(unix)]
#[test]
fn ticket_t_11_packaging_symlink_is_rejected_without_outside_write() {
    let assets = Assets::new();
    let outside = Assets::new();
    std::os::unix::fs::symlink(outside.0.join("assets"), assets.0.join("linked")).unwrap();
    let output = assets.run(false);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("SPA asset symlink: linked"));
    assert!(!outside.0.join("assets/app.js.gz").exists());
}
