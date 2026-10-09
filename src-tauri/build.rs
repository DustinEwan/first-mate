fn main() {
    // One manifest for every exe this package links: the Common-Controls v6
    // dependency. Both consumers need it:
    //
    // - The app binary embeds comctl32 v6 bindings for rfd message dialogs.
    // - The unit-test exe links the tauri runtime (webview2 + rfd) and
    //   imports comctl32!TaskDialogIndirect; binding that against
    //   comctl32 v5 dies at load with STATUS_ENTRYPOINT_NOT_FOUND.
    //
    // `cargo:rustc-link-arg` is the only manifest hook that reaches ALL
    // targets (`-tests` skips the lib's unit-test target — upstream cargo
    // gap; plain `-arg` also hits it, as tauri_build's own static-CRT
    // args prove). tauri_build's default embedded manifest is suppressed
    // below so the linker-generated one is the single RT_MANIFEST per
    // binary — two would collide (CVT1100 duplicate resource).
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );

    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new_without_app_manifest(),
    ))
    .expect("failed to run tauri_build");
}
