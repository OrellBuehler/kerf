use std::path::Path;
use std::sync::Mutex;

/// `text` at `path`, for a fixture the tests of one binary share. `std::fs::write` truncates before it writes, and a reader
/// that opens the file in between (the plan's LUT read, FFmpeg's) finds it empty: a `.cube` read that way is not a usable LUT,
/// so one renderer leaves it out and the other does not. A file that already holds `text` is left alone, else it is written
/// beside `path` and renamed over it, one test at a time.
pub fn write_fixture(path: &Path, text: &str) {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if std::fs::read_to_string(path).is_ok_and(|have| have == text) {
        return;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}
