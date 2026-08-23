fn main() {
    // Tauri's default app manifest — the one asking for Common-Controls v6 —
    // rides in the Windows resource file, which cargo links into **bins only**.
    // The lib's own test binary therefore starts with no activation context, so
    // the loader binds comctl32 v5; and rfd (via tauri-plugin-dialog) statically
    // imports `TaskDialogIndirect`, which only v6 exports. The test exe then dies
    // with STATUS_ENTRYPOINT_NOT_FOUND before a single test runs. Whether the
    // linker pulls that object in at all shifts with unrelated dependency
    // changes, so pass the same manifest through the linker instead: that covers
    // every binary this crate links, tests included.
    let attributes =
        tauri_build::Attributes::new().windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attributes).expect("tauri-build failed");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed=windows-app-manifest.xml");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    }
}
