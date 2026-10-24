// Prevent an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = kerf_app_lib::headless_main() {
        std::process::exit(code);
    }
    kerf_app_lib::run();
}
