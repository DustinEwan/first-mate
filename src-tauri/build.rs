fn main() {
    // Every exe this package links needs the Common-Controls v6 dependency.
    // The app binary embeds comctl32 v6 bindings for rfd message dialogs. The
    // unit-test exe links the tauri runtime (webview2 + rfd) and imports
    // comctl32!TaskDialogIndirect. comctl32 v5 has no such entry point. The
    // test exe therefore dies at load with STATUS_ENTRYPOINT_NOT_FOUND.
    //
    // `cargo:rustc-link-arg` is the only manifest flag form that reaches the
    // lib's unit-test target. `cargo:rustc-link-arg-tests` skips that target,
    // so these two lines must keep the `-arg` form.
    //
    // One binary carries one embedded RT_MANIFEST. A second manifest fails
    // the link with CVT1100 (duplicate resource).
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );

    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new_without_app_manifest(),
    ))
    .expect("failed to run tauri_build");
}
