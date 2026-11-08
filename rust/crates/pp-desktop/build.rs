fn main() -> tauri_build::Result<()> {
    // tauri-build's static VC runtime writes a stub msvcrt.lib to OUT_DIR and
    // adds OUT_DIR to the link search path. Cargo hands that path to every
    // doctest link in the workspace, where the stub hides the real CRT.
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new().static_vc_runtime(false));
    #[cfg(feature = "desktop_rebuild")]
    let attributes = attributes.app_manifest(
        tauri_build::AppManifest::new().commands(&["open_sponsor", "sync_portable_settings"]),
    );
    tauri_build::try_build(attributes)
}
