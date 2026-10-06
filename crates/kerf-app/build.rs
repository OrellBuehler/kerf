/// Give debug builds their own app identity (`tauri.dev.conf.json`).
///
/// The identifier names the app's config and log directories and the
/// single-instance lock, so a dev run sharing the installed app's would read and
/// rewrite its settings and refuse to start while the real one is open. Tauri
/// resolves the config at compile time — `tauri_build` here and
/// `generate_context!` in the lib — and both honour `TAURI_CONFIG`, a JSON merge
/// patch. `tauri dev --config` sets it for the CLI; `cargo run` has no CLI, so
/// set it from here, keyed on the profile. Anything the CLI already put in it
/// wins, and release builds are untouched.
fn dev_identity() {
    println!("cargo:rerun-if-changed=tauri.dev.conf.json");
    println!("cargo:rerun-if-env-changed=TAURI_CONFIG");
    if std::env::var("PROFILE").as_deref() != Ok("debug") {
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dev: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("tauri.dev.conf.json")).expect("tauri.dev.conf.json"))
            .expect("tauri.dev.conf.json is not valid JSON");
    let mut config: serde_json::Value = match std::env::var("TAURI_CONFIG") {
        Ok(env) => serde_json::from_str(&env).expect("TAURI_CONFIG is not valid JSON"),
        Err(_) => serde_json::json!({}),
    };
    let (Some(dev), Some(config_map)) = (dev.as_object(), config.as_object_mut()) else {
        panic!("tauri.dev.conf.json and TAURI_CONFIG must be JSON objects");
    };
    for (key, value) in dev.iter().filter(|(k, _)| k.as_str() != "$schema") {
        config_map.entry(key.clone()).or_insert_with(|| value.clone());
    }
    let json = config.to_string();
    std::env::set_var("TAURI_CONFIG", &json);
    println!("cargo:rustc-env=TAURI_CONFIG={json}");
}

fn main() {
    dev_identity();
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
