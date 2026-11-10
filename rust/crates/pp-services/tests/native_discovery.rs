//! GRE-456 (T-44): profile and bundle discovery, and native app versions and config dirs.
use pp_services::discovery::SlicerKind;
use pp_services::native_slicers::{NativeDiscovery, NativeRoots, discover_macos};
use std::{fs, path::Path};

fn bundle(root: &Path, name: &str, executable: &str, version: &str) {
    let contents = root.join(name).join("Contents");
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    let binary = contents.join("MacOS").join(executable);
    fs::write(&binary, b"fixture, never executed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(binary, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(contents.join("Info.plist"), format!(
        "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>{executable}</string><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"
    )).unwrap();
}

#[test]
fn ticket_t_44_discovery_native_apps_return_versions_and_real_config_dirs() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home with spaces; $(touch unexpected)");
    let apps = temp.path().join("Applications");
    let roots = NativeRoots {
        home_dir: home.clone(),
        system_applications: apps.clone(),
    };
    for (kind, app, executable, version, config) in [
        (
            SlicerKind::Orca,
            "OrcaSlicer.app",
            "OrcaSlicer",
            "2.3.1",
            "OrcaSlicer",
        ),
        (
            SlicerKind::Bambu,
            "BambuStudio.app",
            "BambuStudio",
            "02.03.01.51",
            "BambuStudio",
        ),
    ] {
        bundle(&apps, app, executable, version);
        let config_dir = home.join("Library/Application Support").join(config);
        fs::create_dir_all(&config_dir).unwrap();
        let NativeDiscovery::Installed(found) = discover_macos(kind, &roots).unwrap() else {
            panic!("expected installed app")
        };
        assert_eq!(found.kind, kind);
        assert_eq!(
            found.executable,
            apps.join(app).join("Contents/MacOS").join(executable)
        );
        assert_eq!(found.version, version);
        assert_eq!(found.config_dir, Some(config_dir));
    }
}

fn roots(temp: &tempfile::TempDir) -> NativeRoots {
    NativeRoots {
        home_dir: temp.path().join("home"),
        system_applications: temp.path().join("Applications"),
    }
}

#[test]
fn ticket_t_44_discovery_profile_dirs_do_not_imply_native_install() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    fs::create_dir_all(
        roots
            .home_dir
            .join("Library/Application Support/OrcaSlicer/user"),
    )
    .unwrap();
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::Absent
    );
    assert_eq!(
        discover_macos(SlicerKind::Bambu, &roots).unwrap(),
        NativeDiscovery::Absent
    );
}

#[test]
fn ticket_t_44_discovery_user_bundle_and_missing_config_are_honest() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    bundle(
        &roots.home_dir.join("Applications"),
        "Bambu Studio.app",
        "BambuStudio",
        "2.1.0",
    );
    fs::create_dir_all(roots.home_dir.join(".config/BambuStudio/user")).unwrap();
    let NativeDiscovery::Installed(found) = discover_macos(SlicerKind::Bambu, &roots).unwrap()
    else {
        panic!("expected user app")
    };
    assert_eq!(
        found.executable,
        roots
            .home_dir
            .join("Applications/Bambu Studio.app/Contents/MacOS/BambuStudio")
    );
    assert_eq!(found.version, "2.1.0");
    assert_eq!(found.config_dir, None);
}

#[test]
fn ticket_t_44_discovery_invalid_system_bundle_does_not_hide_user_install() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    fs::create_dir_all(roots.system_applications.join("OrcaSlicer.app")).unwrap();
    bundle(
        &roots.home_dir.join("Applications"),
        "OrcaSlicer.app",
        "OrcaSlicer",
        "2.0.0",
    );
    let NativeDiscovery::Installed(found) = discover_macos(SlicerKind::Orca, &roots).unwrap()
    else {
        panic!("expected fallback")
    };
    assert_eq!(found.version, "2.0.0");
    bundle(
        &roots.system_applications,
        "OrcaSlicer.app",
        "OrcaSlicer",
        "2.3.1",
    );
    let NativeDiscovery::Installed(found) = discover_macos(SlicerKind::Orca, &roots).unwrap()
    else {
        panic!("expected system app")
    };
    assert_eq!(found.version, "2.3.1");
}

#[test]
fn ticket_t_44_discovery_binary_plist_is_supported() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    bundle(
        &roots.system_applications,
        "OrcaSlicer.app",
        "OrcaSlicer",
        "2.3.1",
    );
    let path = roots
        .system_applications
        .join("OrcaSlicer.app/Contents/Info.plist");
    let value = plist::Value::from_file(&path).unwrap();
    value.to_file_binary(path).unwrap();
    let NativeDiscovery::Installed(found) = discover_macos(SlicerKind::Orca, &roots).unwrap()
    else {
        panic!("expected binary plist")
    };
    assert_eq!(found.version, "2.3.1");
}

#[test]
fn ticket_t_44_discovery_invalid_metadata_versions_and_executable_are_explicit() {
    use pp_services::native_slicers::BundleProblem;
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    let app = roots.system_applications.join("OrcaSlicer.app");
    bundle(
        &roots.system_applications,
        "OrcaSlicer.app",
        "OrcaSlicer",
        " ",
    );
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::InvalidBundle {
            path: app.clone(),
            reason: BundleProblem::Version
        }
    );
    fs::write(app.join("Contents/Info.plist"), b"not a plist").unwrap();
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::InvalidBundle {
            path: app.clone(),
            reason: BundleProblem::Metadata
        }
    );
    bundle(
        &roots.system_applications,
        "OrcaSlicer.app",
        "unexpected",
        "2.3.1",
    );
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::InvalidBundle {
            path: app.clone(),
            reason: BundleProblem::Executable
        }
    );
    fs::remove_file(app.join("Contents/MacOS/OrcaSlicer")).unwrap();
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::InvalidBundle {
            path: app,
            reason: BundleProblem::Executable
        }
    );
}

#[test]
fn ticket_t_44_discovery_prusa_and_non_mac_platform_remain_unsupported() {
    use pp_services::native_slicers::discover_native_kind;
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    bundle(
        &roots.system_applications,
        "PrusaSlicer.app",
        "PrusaSlicer",
        "2.9.0",
    );
    assert_eq!(
        discover_native_kind("prusa", &roots).unwrap(),
        NativeDiscovery::Unsupported
    );
    #[cfg(not(target_os = "macos"))]
    for kind in ["orca", "bambu"] {
        bundle(
            &roots.system_applications,
            "OrcaSlicer.app",
            "OrcaSlicer",
            "2.3.1",
        );
        assert_eq!(
            discover_native_kind(kind, &roots).unwrap(),
            NativeDiscovery::Unsupported
        );
    }
    #[cfg(target_os = "macos")]
    {
        bundle(
            &roots.system_applications,
            "OrcaSlicer.app",
            "OrcaSlicer",
            "2.3.1",
        );
        assert!(matches!(
            discover_native_kind("orca", &roots).unwrap(),
            NativeDiscovery::Installed(_)
        ));
    }
}

#[cfg(unix)]
#[test]
fn ticket_t_44_discovery_rejects_non_executable_and_escaping_symlinks() {
    use pp_services::native_slicers::BundleProblem;
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    bundle(
        &roots.system_applications,
        "OrcaSlicer.app",
        "OrcaSlicer",
        "2.3.1",
    );
    let app = roots.system_applications.join("OrcaSlicer.app");
    let binary = app.join("Contents/MacOS/OrcaSlicer");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::InvalidBundle {
            path: app.clone(),
            reason: BundleProblem::Executable
        }
    );
    let outside = temp.path().join("outside");
    fs::write(&outside, b"never run").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(&binary).unwrap();
    symlink(outside, binary).unwrap();
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::InvalidBundle {
            path: app,
            reason: BundleProblem::Executable
        }
    );
}

#[cfg(unix)]
#[test]
fn ticket_t_44_discovery_bundle_symlink_cannot_escape_applications_root() {
    use pp_services::native_slicers::BundleProblem;
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    let outside = temp.path().join("outside");
    bundle(&outside, "OrcaSlicer.app", "OrcaSlicer", "2.3.1");
    fs::create_dir_all(&roots.system_applications).unwrap();
    let path = roots.system_applications.join("OrcaSlicer.app");
    symlink(outside.join("OrcaSlicer.app"), &path).unwrap();
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::InvalidBundle {
            path,
            reason: BundleProblem::Executable
        }
    );
}

#[cfg(unix)]
#[test]
fn ticket_t_44_discovery_metadata_symlink_cannot_escape_bundle() {
    use pp_services::native_slicers::BundleProblem;
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    bundle(
        &roots.system_applications,
        "OrcaSlicer.app",
        "OrcaSlicer",
        "external-version",
    );
    let app = roots.system_applications.join("OrcaSlicer.app");
    let metadata = app.join("Contents/Info.plist");
    let outside = roots
        .system_applications
        .join("OrcaSlicer.app-external.plist");
    fs::rename(&metadata, &outside).unwrap();
    symlink(outside, metadata).unwrap();
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::InvalidBundle {
            path: app,
            reason: BundleProblem::Metadata
        }
    );
}

#[cfg(unix)]
#[test]
fn ticket_t_44_discovery_contained_metadata_symlink_is_supported() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    bundle(
        &roots.system_applications,
        "BambuStudio.app",
        "BambuStudio",
        "2.3.1",
    );
    let app = roots.system_applications.join("BambuStudio.app");
    let metadata = app.join("Contents/Info.plist");
    let target = app.join("Contents/Version.plist");
    fs::rename(&metadata, &target).unwrap();
    symlink("Version.plist", metadata).unwrap();
    let NativeDiscovery::Installed(found) = discover_macos(SlicerKind::Bambu, &roots).unwrap()
    else {
        panic!("expected contained metadata")
    };
    assert_eq!(found.version, "2.3.1");
    assert_eq!(found.executable, app.join("Contents/MacOS/BambuStudio"));
}

#[test]
fn ticket_t_44_discovery_metadata_directory_is_invalid() {
    use pp_services::native_slicers::BundleProblem;
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(&temp);
    bundle(
        &roots.system_applications,
        "OrcaSlicer.app",
        "OrcaSlicer",
        "2.3.1",
    );
    let app = roots.system_applications.join("OrcaSlicer.app");
    let metadata = app.join("Contents/Info.plist");
    fs::remove_file(&metadata).unwrap();
    fs::create_dir(&metadata).unwrap();
    assert_eq!(
        discover_macos(SlicerKind::Orca, &roots).unwrap(),
        NativeDiscovery::InvalidBundle {
            path: app,
            reason: BundleProblem::Metadata
        }
    );
}
