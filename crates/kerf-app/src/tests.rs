use super::{
    bring_forward, check_output_path, file_appender, file_layer, filmstrip_payload, id, ids, launch_project, log_panic, opt_id,
    panic_summary, playback_message, playback_session, project_arg, require_generation, require_json_path,
    require_local_output_path, reveal_once, truncate_log, LaunchProject, LaunchSlot, LogBudget, PathSource, Playbacks, StopEpoch,
    FRONTEND_LOG_PER_SEC, PLAYBACK_HEADER_BYTES, REVEAL_FAILSAFE,
};
use base64::Engine as _;
use kerf_core::{Filmstrip, FilmstripSheet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing_subscriber::prelude::*;

/// The bytes `parsePlaybackMessage` (frontend/src/lib/frame-transport.test.ts) is held to: the
/// same vector is spelled there, so the two ends cannot drift apart.
#[test]
fn a_playback_message_is_the_time_then_the_jpeg() {
    let jpeg = [0xFF, 0xD8, 1, 2, 0xFF, 0xD9];
    let message = playback_message(1.5, &jpeg);
    assert_eq!(
        message,
        [0, 0, 0, 0, 0, 0, 0xF8, 0x3F, 0xFF, 0xD8, 1, 2, 0xFF, 0xD9],
        "1.5 as a little-endian f64, then the frame untouched"
    );
    assert_eq!(message.len(), PLAYBACK_HEADER_BYTES + jpeg.len());
    assert_eq!(f64::from_le_bytes(message[..8].try_into().unwrap()), 1.5);
    let empty = playback_message(0.0, &[]);
    assert_eq!(empty, [0u8; PLAYBACK_HEADER_BYTES]);
}

/// A fresh directory under the system temp dir, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("kerf-app-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// Every logfile in the directory, concatenated.
    fn logs(&self) -> String {
        let mut text = String::new();
        for entry in std::fs::read_dir(&self.0).unwrap() {
            text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
        }
        text
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn truncate_log_keeps_short_and_cuts_on_a_char_boundary() {
    assert_eq!(truncate_log("short", 10), "short");
    let cut = truncate_log("aéééé", 4);
    assert!(cut.starts_with("aé…"), "{cut}");
    assert!(cut.contains("truncated"));
}

#[test]
fn log_budget_caps_a_second_and_reports_the_drops_next_window() {
    let mut b = LogBudget::default();
    for _ in 0..FRONTEND_LOG_PER_SEC {
        assert_eq!(b.admit(5_000), (true, 0));
    }
    assert_eq!(b.admit(5_500), (false, 0));
    assert_eq!(b.admit(5_900), (false, 0));
    assert_eq!(b.admit(6_100), (true, 2));
}

#[test]
fn accepts_absolute_local_paths() {
    assert!(require_local_output_path("/home/user/out.mp4").is_ok());
    assert!(require_local_output_path("C:\\Users\\me\\out.mp4").is_ok());
    assert!(require_local_output_path("c:/Users/me/out.mp4").is_ok());
    assert!(
        require_local_output_path("\\\\?\\C:\\Users\\me\\out.mp4").is_ok(),
        "the verbatim spelling of a local drive is local"
    );
}

const NETWORK_PATHS: [&str; 8] = [
    "\\\\server\\share\\out.mp4",
    "//server/share/out.mp4",
    "\\/server/share/out.mp4",
    "/\\server\\share\\out.mp4",
    "\\\\?\\UNC\\server\\share\\out.mp4",
    "//?/UNC/server/share/out.mp4",
    "\\\\.\\pipe\\kerf",
    "\\\\localhost\\c$\\out.mp4",
];

#[test]
fn an_agent_may_not_name_a_network_share_on_windows() {
    for bad in NETWORK_PATHS {
        let why = check_output_path(bad, PathSource::Agent, true).expect_err(bad);
        assert!(why.contains("network"), "{bad}: {why}");
    }
}

#[test]
fn a_double_slash_is_an_ordinary_absolute_path_off_windows() {
    for ok in ["//tmp/out.mp4", "///tmp/out.mp4", "//home/user/out.mp4"] {
        assert!(check_output_path(ok, PathSource::Agent, false).is_ok(), "{ok}");
        assert!(check_output_path(ok, PathSource::User, false).is_ok(), "{ok}");
    }
    // The backslash forms are Windows spellings and no absolute path anywhere else.
    for bad in ["\\\\server\\share\\out.mp4", "\\/server/share/out.mp4", "\\\\.\\pipe\\kerf"] {
        assert!(check_output_path(bad, PathSource::Agent, false).is_err(), "{bad}");
        assert!(check_output_path(bad, PathSource::User, false).is_err(), "{bad}");
    }
}

#[test]
fn a_share_picked_in_the_save_dialog_is_the_users_intent_on_windows() {
    for ok in [
        "\\\\nas\\share\\out.mp4",
        "//nas/share/out.mp4",
        "\\\\?\\UNC\\nas\\share\\out.mp4",
        "C:\\Users\\me\\out.mp4",
    ] {
        assert!(check_output_path(ok, PathSource::User, true).is_ok(), "{ok}");
    }
    // The device namespace is never an output, and the rest of the rules still hold.
    for bad in [
        "\\\\.\\pipe\\kerf",
        "\\\\?\\GLOBALROOT\\Device\\x",
        "https://nas/share/out.mp4",
        "out.mp4",
        "pipe:1",
    ] {
        assert!(check_output_path(bad, PathSource::User, true).is_err(), "{bad}");
    }
}

#[test]
fn rejects_relative_paths() {
    assert!(require_local_output_path("out.mp4").is_err());
    assert!(require_local_output_path("renders/out.mp4").is_err());
    assert!(require_local_output_path("Users\\me\\out.mp4").is_err());
}

#[test]
fn rejects_protocol_urls() {
    for bad in [
        "rtmp://example.com/live",
        "https://attacker.example/upload",
        "http://127.0.0.1:8080/x",
        "udp://239.0.0.1:1234",
        "file:///etc/passwd",
    ] {
        assert!(require_local_output_path(bad).is_err(), "{bad} should be rejected");
    }
}

#[test]
fn rejects_bare_ffmpeg_protocol_prefixes() {
    for bad in ["pipe:1", "pipe:", "concat:a.mp4|b.mp4", "async:tcp://host:1234"] {
        assert!(require_local_output_path(bad).is_err(), "{bad} should be rejected");
    }
}

#[test]
fn windows_drive_letter_is_not_a_protocol() {
    // A lone drive letter followed by a path separator must never be
    // mistaken for a scheme — this is the whole reason the check isn't
    // just "contains a colon".
    assert!(require_local_output_path("D:\\video\\clip.mov").is_ok());
}

#[test]
fn a_second_launch_forwards_only_a_kerf_path() {
    assert_eq!(project_arg(&argv(&["kerf"]), "/home/u"), None);
    assert_eq!(project_arg(&argv(&["kerf", "--flag", "notes.txt"]), "/home/u"), None);
    assert_eq!(
        project_arg(&argv(&["kerf", "/data/cut.kerf"]), "/home/u").as_deref(),
        Some("/data/cut.kerf")
    );
    assert_eq!(
        project_arg(&argv(&["kerf", "CUT.KERF"]), "/home/u").map(|p| p.replace('\\', "/")),
        Some("/home/u/CUT.KERF".to_string())
    );
    // argv[0] is the binary, never a project.
    assert_eq!(project_arg(&argv(&["/opt/x.kerf"]), "/home/u"), None);
}

#[test]
fn a_filmstrip_reaches_the_webview_as_geometry_plus_a_data_url_per_sheet() {
    let first: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xD9];
    let second: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xD9, 0x00];
    let strip = Filmstrip {
        interval: 0.5,
        frame_width: 170,
        frame_height: 96,
        frames: 3,
        columns: 2,
        sheets: vec![
            FilmstripSheet {
                first_frame: 0,
                count: 2,
                width: 340,
                height: 96,
                jpeg: first.clone().into(),
            },
            FilmstripSheet {
                first_frame: 2,
                count: 1,
                width: 340,
                height: 96,
                jpeg: second.clone().into(),
            },
        ],
    };
    let json = serde_json::to_value(filmstrip_payload(&strip)).unwrap();

    // The geometry is core's own serialization, unchanged...
    let mut core = serde_json::to_value(&strip).unwrap();
    for (sheet, core_sheet) in json["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .zip(core["sheets"].as_array_mut().unwrap())
    {
        // ...and each sheet is core's sheet plus exactly one key, `data_url`.
        let mut sheet = sheet.clone();
        let url = sheet.as_object_mut().unwrap().remove("data_url").unwrap();
        assert_eq!(&sheet, core_sheet);
        assert!(url.as_str().unwrap().starts_with("data:image/jpeg;base64,"), "{url}");
    }
    for key in ["interval", "frame_width", "frame_height", "frames", "columns"] {
        assert_eq!(json[key], core[key], "{key}");
    }
    assert_eq!(json.as_object().unwrap().len(), 6, "geometry plus the sheets, nothing else");

    // ...and the URL is the sheet's own bytes, base64.
    let decode = |sheet: &serde_json::Value| {
        let url = sheet["data_url"].as_str().unwrap();
        base64::engine::general_purpose::STANDARD
            .decode(url.strip_prefix("data:image/jpeg;base64,").unwrap())
            .unwrap()
    };
    assert_eq!(decode(&json["sheets"][0]), first);
    assert_eq!(decode(&json["sheets"][1]), second);
    assert_eq!(json["sheets"][0]["data_url"], "data:image/jpeg;base64,/9j/2Q==");
    assert_eq!(json["sheets"][0]["first_frame"], 0);
    assert_eq!(json["sheets"][1]["first_frame"], 2);
}

#[test]
fn text_file_commands_only_take_json() {
    assert!(require_json_path("/t/dark.kerf-theme.json").is_ok());
    assert!(require_json_path("C:\\t\\THEME.JSON").is_ok());
    assert!(require_json_path("/home/u/.bashrc").is_err());
    assert!(require_json_path("/t/theme.json.exe").is_err());
}

#[test]
fn template_files_are_only_read_as_kerftemplate() {
    use super::require_template_path;
    assert!(require_template_path("/t/card.kerftemplate").is_ok());
    assert!(require_template_path("C:\\t\\CARD.KERFTEMPLATE").is_ok());
    assert!(require_template_path("/home/u/.bashrc").is_err());
    assert!(require_template_path("/t/card.kerftemplate.zip").is_err());
    assert!(require_template_path("/t/card").is_err());
}

/// The ids command takes text straight from the webview: a title is at most 128 KiB, and a document
/// over that is refused before anything is parsed.
#[test]
fn the_ids_of_a_title_are_listed_for_a_title_that_could_be_one() {
    let ok =
        super::svg_element_ids(r#"<svg xmlns="http://www.w3.org/2000/svg"><text id="a"/><g id="b"/></svg>"#.to_string()).unwrap();
    assert_eq!(
        ok.iter().map(|e| (e.id.as_str(), e.tag.as_str())).collect::<Vec<_>>(),
        [("a", "text"), ("b", "g")]
    );
    let big = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg"><text id="a"/><!--{}--></svg>"#,
        "x".repeat(200 * 1024)
    );
    let e = super::svg_element_ids(big).err().expect("over the size of a title");
    assert!(e.contains("larger than 128 KiB"), "{e}");
    assert!(super::svg_element_ids("not svg".to_string()).is_err());
}

/// A picture for an image control is read bounded, and the engine judges it: a file that is not a
/// picture, a relative path, a directory and an oversized file are all refused with a reason.
#[test]
fn a_picture_for_a_template_is_read_bounded_and_judged_by_the_engine() {
    use super::template_image_data_url;
    let limit = kerf_core::template::MAX_IMAGE_BYTES as u64;
    let dir = Scratch::new("template-image");
    let png = dir.0.join("logo.png");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAACp8Z5+AAAAEklEQVR4nGP4z8DwHxkzkC4AADxAH+HggXe0AAAAAElFTkSuQmCC")
        .unwrap();
    std::fs::write(&png, &bytes).unwrap();
    let url = template_image_data_url(png.to_str().unwrap()).unwrap();
    assert!(url.starts_with("data:image/png;base64,"), "{url}");

    let refused = |path: &Path| template_image_data_url(path.to_str().unwrap()).unwrap_err().to_string();
    assert!(template_image_data_url("logo.png")
        .unwrap_err()
        .to_string()
        .contains("absolute"));
    assert!(refused(&dir.0).contains("not a regular file"));
    assert!(refused(&dir.0.join("missing.png")).contains("cannot read"));
    let text = dir.0.join("notes.txt");
    std::fs::write(&text, "not a picture").unwrap();
    assert!(refused(&text).contains("PNG, JPEG, GIF or WebP"));
    // The bound is the engine's own: a file one byte over what fits in a title is refused, whatever
    // it is, and without being read whole (a huge sparse one answers at once).
    let big = dir.0.join("big.png");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(limit + 1).unwrap();
    let why = refused(&big);
    assert!(why.contains("is 95 KiB") && why.contains("at most 94 KiB"), "{why}");
    let huge = dir.0.join("huge.png");
    std::fs::File::create(&huge).unwrap().set_len(8 << 30).unwrap();
    assert!(refused(&huge).contains("too large"));
    // A picture that is within the bound but not one the engine takes is the engine's refusal.
    let mut padded = bytes;
    padded.resize(limit as usize, 0);
    assert!(template_image_data_url(write(&dir.0, "padded.png", &padded).to_str().unwrap()).is_ok());
    padded.push(0);
    assert!(refused(&write(&dir.0, "over.png", &padded)).contains("too large"));
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn the_first_launch_opens_the_kerf_it_was_given() {
    let there = |_: &Path| true;
    let nowhere = |_: &Path| false;

    assert_eq!(launch_project(&argv(&["kerf"]), "/home/u", there), LaunchProject::Nothing);
    assert_eq!(
        launch_project(&argv(&["kerf", "--flag", "notes.txt"]), "/home/u", there),
        LaunchProject::Nothing
    );
    // An absolute path stands as it is, whatever the working directory is.
    assert_eq!(
        launch_project(&argv(&["kerf", "/data/cut.kerf"]), "/home/u", there),
        LaunchProject::Open("/data/cut.kerf".to_string())
    );
    // A relative one is relative to the directory the app was started from.
    let LaunchProject::Open(relative) = launch_project(&argv(&["kerf", "cut.kerf"]), "/home/u", there) else {
        panic!("a relative .kerf should open");
    };
    assert_eq!(relative.replace('\\', "/"), "/home/u/cut.kerf");
    // Other arguments around it do not matter; the first .kerf wins.
    assert_eq!(
        launch_project(&argv(&["kerf", "--x", "/a.kerf", "/b.kerf"]), "/", there),
        LaunchProject::Open("/a.kerf".to_string())
    );
    // argv[0] is the binary, never a project.
    assert_eq!(
        launch_project(&argv(&["/opt/x.kerf"]), "/home/u", there),
        LaunchProject::Nothing
    );
    // No working directory (it was deleted under us) leaves a relative path as typed.
    assert_eq!(
        launch_project(&argv(&["kerf", "cut.kerf"]), "", there),
        LaunchProject::Open("cut.kerf".to_string())
    );
    // A path that is not there is reported, not opened: opening it would create it.
    assert_eq!(
        launch_project(&argv(&["kerf", "/data/typo.kerf"]), "/home/u", nowhere),
        LaunchProject::Missing("/data/typo.kerf".to_string())
    );
}

#[test]
fn a_launch_is_judged_against_the_real_filesystem_too() {
    let scratch = Scratch::new("launch");
    let real = scratch.0.join("real.kerf");
    std::fs::write(&real, b"").unwrap();
    let dir = scratch.0.display().to_string();
    let is_file = |p: &Path| p.is_file();

    assert_eq!(
        launch_project(&argv(&["kerf", "real.kerf"]), &dir, is_file),
        LaunchProject::Open(real.display().to_string())
    );
    assert!(matches!(
        launch_project(&argv(&["kerf", "gone.kerf"]), &dir, is_file),
        LaunchProject::Missing(_)
    ));
    // A directory that happens to be named like a project is not one.
    std::fs::create_dir(scratch.0.join("folder.kerf")).unwrap();
    assert!(matches!(
        launch_project(&argv(&["kerf", "folder.kerf"]), &dir, is_file),
        LaunchProject::Missing(_)
    ));
}

#[test]
fn the_launch_request_is_handed_over_once() {
    let open = |p: &str| LaunchProject::Open(p.to_string());
    let mut slot = LaunchSlot::new(open("/data/cut.kerf"));
    assert_eq!(slot.take(), Some(open("/data/cut.kerf")));
    // A reloaded webview asking again must not reopen it over the user's edits.
    assert_eq!(slot.take(), None);
    assert_eq!(LaunchSlot::new(LaunchProject::Nothing).take(), None);
    // A missing file is a request too: the webview says so.
    let mut slot = LaunchSlot::new(LaunchProject::Missing("/x.kerf".to_string()));
    assert_eq!(slot.take(), Some(LaunchProject::Missing("/x.kerf".to_string())));
}

#[test]
fn a_second_launch_while_the_webview_boots_is_held_not_emitted() {
    let open = |p: &str| LaunchProject::Open(p.to_string());
    // The first launch carried nothing, and a second arrives before the page is
    // listening: an event now would be lost, so the slot holds it.
    let mut slot = LaunchSlot::new(LaunchProject::Nothing);
    assert_eq!(slot.offer(open("/a.kerf")), None);
    assert_eq!(slot.take(), Some(open("/a.kerf")));

    // Newest wins over what the first launch asked for, and over an earlier second.
    let mut slot = LaunchSlot::new(open("/first.kerf"));
    assert_eq!(slot.offer(open("/second.kerf")), None);
    assert_eq!(slot.offer(open("/third.kerf")), None);
    assert_eq!(slot.take(), Some(open("/third.kerf")));

    // Once the webview has asked it is listening, so a later one is an event.
    assert_eq!(slot.offer(open("/late.kerf")), Some(open("/late.kerf")));
    assert_eq!(slot.take(), None, "an event-delivered request is not also held");

    // A second launch with no project argument asks for nothing, either way.
    let mut slot = LaunchSlot::new(open("/first.kerf"));
    assert_eq!(slot.offer(LaunchProject::Nothing), None);
    assert_eq!(slot.take(), Some(open("/first.kerf")), "and does not clobber what is waiting");
    assert_eq!(slot.offer(LaunchProject::Nothing), None);
}

#[test]
fn the_project_a_setting_reopens_arrives_whenever_the_disk_answers_and_never_beats_a_named_one() {
    let open = |p: &str| LaunchProject::Open(p.to_string());
    // The disk answers before the page asks: it is held for the page's pull.
    let mut slot = LaunchSlot::new(LaunchProject::Nothing);
    assert_eq!(slot.offer_reopen("/last.kerf".to_string()), None);
    assert_eq!(slot.take(), Some(open("/last.kerf")));

    // After: a slow share. The page is listening, so it is told as an event, once.
    let mut slot = LaunchSlot::new(LaunchProject::Nothing);
    assert_eq!(slot.take(), None);
    assert_eq!(slot.offer_reopen("/last.kerf".to_string()), Some(open("/last.kerf")));

    // A project named on the command line, or by a second launch, is the one wanted.
    let mut slot = LaunchSlot::new(open("/named.kerf"));
    assert_eq!(slot.offer_reopen("/last.kerf".to_string()), None);
    assert_eq!(slot.take(), Some(open("/named.kerf")));
    let mut slot = LaunchSlot::new(LaunchProject::Nothing);
    assert_eq!(slot.take(), None);
    assert_eq!(slot.offer(open("/second.kerf")), Some(open("/second.kerf")));
    assert_eq!(slot.offer_reopen("/last.kerf".to_string()), None, "the late reopen gives way");
    let mut slot = LaunchSlot::new(LaunchProject::Nothing);
    assert_eq!(slot.offer(open("/second.kerf")), None);
    assert_eq!(slot.offer_reopen("/last.kerf".to_string()), None);
    assert_eq!(slot.take(), Some(open("/second.kerf")));
}

#[test]
fn a_missing_file_reaches_the_webview_as_a_variant_it_can_name() {
    let json = |p: LaunchProject| serde_json::to_value(p).unwrap();
    assert_eq!(
        json(LaunchProject::Open("/a.kerf".into())),
        serde_json::json!({ "open": "/a.kerf" })
    );
    assert_eq!(
        json(LaunchProject::Missing("/b.kerf".into())),
        serde_json::json!({ "missing": "/b.kerf" })
    );
}

#[test]
fn the_window_is_revealed_once_whoever_asks_first() {
    let shown = AtomicBool::new(false);
    let mut calls = 0;
    // The webview asks, then the failsafe timer fires: only the first shows it.
    assert!(reveal_once(&shown, || {
        calls += 1;
        true
    }));
    assert!(!reveal_once(&shown, || {
        calls += 1;
        true
    }));
    assert!(!reveal_once(&shown, || {
        calls += 1;
        true
    }));
    assert_eq!(calls, 1);
    assert!(shown.load(Ordering::Acquire));
}

#[test]
fn a_reveal_that_showed_nothing_is_not_counted() {
    // No window yet (or the platform refused): the claim is given back, so the
    // next caller — the failsafe's retry — still gets to try.
    let shown = AtomicBool::new(false);
    assert!(!reveal_once(&shown, || false));
    assert!(!shown.load(Ordering::Acquire));
    assert!(reveal_once(&shown, || true));
    assert!(shown.load(Ordering::Acquire));
    assert!(!reveal_once(&shown, || panic!("already shown")));
}

#[test]
fn a_second_launch_before_the_window_exists_leaves_the_failsafe_armed() {
    let shown = AtomicBool::new(false);
    // The single-instance callback can run before the config windows exist.
    assert!(!bring_forward(&shown, || false));
    assert!(
        !shown.load(Ordering::Acquire),
        "nothing was shown, so the failsafe must still fire"
    );
    assert!(bring_forward(&shown, || true));
    assert!(shown.load(Ordering::Acquire));
    // It brings the window forward every time, whatever was shown before.
    let mut forward = 0;
    assert!(bring_forward(&shown, || {
        forward += 1;
        true
    }));
    assert_eq!(forward, 1);
}

#[test]
fn the_failsafe_is_short_enough_to_notice_and_long_enough_to_boot() {
    assert!(REVEAL_FAILSAFE >= std::time::Duration::from_secs(2));
    assert!(REVEAL_FAILSAFE <= std::time::Duration::from_secs(10));
}

/// The packaged build's CSP: what the app loads, and nothing more. A packaged run logged
/// `connect-src` blocking `_app/version.json` — SvelteKit fetches it from the app's own origin
/// (`tauri://localhost`, `http://tauri.localhost`) whenever the window becomes visible or regains
/// focus — so `connect-src` has `'self'`; scripts (the limiter worklet included) stay `'self'` only.
/// Every directive is pinned, and the set of them, in the packaged policy and in the dev one. The dev
/// policy differs in three ways: `script-src` adds `'unsafe-inline'` (the inline script hot reload
/// needs), `connect-src` names the dev server (http and ws) instead of `'self'`, and there is no
/// `form-action`.
#[test]
fn the_packaged_csp_admits_what_the_app_loads_and_nothing_more() {
    let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    let security = &conf["app"]["security"];
    let policy = |csp: &serde_json::Value| -> std::collections::BTreeMap<String, Vec<String>> {
        csp.as_object()
            .expect("the CSP is a directive map")
            .iter()
            .map(|(directive, value)| {
                let sources = value.as_str().unwrap_or_else(|| panic!("{directive} is a string"));
                (directive.clone(), sources.split_whitespace().map(str::to_string).collect())
            })
            .collect()
    };
    let expect = |directives: &[(&str, &[&str])]| -> std::collections::BTreeMap<String, Vec<String>> {
        directives
            .iter()
            .map(|(d, s)| (d.to_string(), s.iter().map(|s| s.to_string()).collect()))
            .collect()
    };
    assert_eq!(
        policy(&security["csp"]),
        expect(&[
            ("default-src", &["'self'"]),
            ("script-src", &["'self'"]),
            ("style-src", &["'self'", "'unsafe-inline'"]),
            ("font-src", &["'self'", "data:"]),
            ("img-src", &["'self'", "data:"]),
            ("connect-src", &["'self'", "ipc:", "http://ipc.localhost"]),
            ("object-src", &["'none'"]),
            ("base-uri", &["'none'"]),
            ("form-action", &["'none'"]),
        ])
    );
    assert_eq!(
        policy(&security["devCsp"]),
        expect(&[
            ("default-src", &["'self'"]),
            ("script-src", &["'self'", "'unsafe-inline'"]),
            ("style-src", &["'self'", "'unsafe-inline'"]),
            ("font-src", &["'self'", "data:"]),
            ("img-src", &["'self'", "data:"]),
            (
                "connect-src",
                &["ipc:", "http://ipc.localhost", "http://localhost:1420", "ws://localhost:1420"]
            ),
            ("object-src", &["'none'"]),
            ("base-uri", &["'none'"]),
        ])
    );
}

#[test]
fn the_main_window_starts_hidden_in_every_build() {
    let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    let windows = conf["app"]["windows"].as_array().unwrap();
    let main = windows.iter().find(|w| w["label"] == "main").expect("a main window");
    assert_eq!(main["visible"], false, "the main window must be created hidden");
    // …by `popout::create_main_window`, from this very entry: Tauri must not also create
    // it, and a second window labelled `main` would be refused.
    assert_eq!(
        main["create"], false,
        "the main window is built in code (popout.rs) so it can answer window.open"
    );
    // Hidden shows the window's own backdrop in the moment before the page paints.
    assert!(main["backgroundColor"].as_str().is_some_and(|c| c.starts_with('#')));

    // The debug identity is merged over this file (build.rs); it may change who the
    // app *is* but must not bring a window of its own, or a dev build would show
    // the window the release one hides.
    let dev: serde_json::Value = serde_json::from_str(include_str!("../tauri.dev.conf.json")).unwrap();
    assert!(dev.get("app").is_none(), "tauri.dev.conf.json must not override app.windows");

    // Showing goes through the `show_main_window` command, so the webview needs no
    // window permission for it; granting one would be a needless widening.
    let caps = include_str!("../capabilities/default.json");
    assert!(
        !caps.contains("window:allow-show"),
        "show_main_window makes this permission unnecessary"
    );
}

#[test]
fn a_panic_message_and_place_are_read_from_either_payload_type() {
    let place = std::panic::Location::caller();
    let (loc, msg) = panic_summary(Some(place), &"literal");
    assert_eq!(msg, "literal");
    assert!(loc.contains("tests.rs:"), "{loc}");
    assert_eq!(
        panic_summary(None, &"formatted".to_string()),
        (String::new(), "formatted".to_string())
    );
    assert_eq!(panic_summary(None, &42_u32).1, "panic");
}

#[test]
fn a_panic_is_in_the_logfile_the_moment_it_is_logged() {
    let scratch = Scratch::new("panic");
    let subscriber = tracing_subscriber::registry().with(file_layer(file_appender(&scratch.0).unwrap()));
    tracing::subscriber::with_default(subscriber, || {
        let (location, message) = panic_summary(Some(std::panic::Location::caller()), &"boom");
        log_panic(&location, &message, &"frame 0\nframe 1");
    });
    // No guard dropped, no sleep, no flush: the file already has it. (Behind
    // `tracing_appender::non_blocking` this read races a worker thread.)
    let logs = scratch.logs();
    assert!(logs.contains("ERROR"), "{logs}");
    assert!(logs.contains("panic: boom"), "{logs}");
    assert!(logs.contains("frame 1"), "the backtrace is part of the line: {logs}");
    assert!(logs.contains("tests.rs:"), "{logs}");
}

#[test]
fn a_line_is_on_disk_the_moment_logging_returns() {
    // The property a crash relies on, checked without needing one: when a
    // `tracing` call returns, its line is already in the file — nothing is
    // queued for a worker to write later. Read straight back after each line,
    // many times over: behind `tracing_appender::non_blocking` the worker has
    // to be woken first, so the read misses it most of the time and a few
    // dozen tries make that all but certain.
    let scratch = Scratch::new("sync");
    let subscriber = tracing_subscriber::registry().with(file_layer(file_appender(&scratch.0).unwrap()));
    tracing::subscriber::with_default(subscriber, || {
        for i in 0..40 {
            tracing::info!(i, "line");
            let logs = scratch.logs();
            assert!(
                logs.contains(&format!("i={i} ")) || logs.contains(&format!("i={i}\n")),
                "line {i} is not on disk yet:\n{logs}"
            );
        }
    });
}

/// A process that logs and then dies without unwinding leaves its lines in
/// the file. The "crash" is a child process (this test binary, re-run on just
/// this test) that calls `process::exit(1)` straight after a burst of logging:
/// no destructors run, so anything held in a buffer is gone. That is what this
/// catches (a `BufWriter` around the appender, say). It does *not* reliably
/// catch a queue-backed writer — `exit` takes long enough for that worker to
/// drain — which is `a_line_is_on_disk_the_moment_logging_returns`'s job.
/// (`abort()` would model a crash more literally, but raises the OS's crash
/// reporter — apport, WER, ReportCrash — on a developer's machine.)
#[test]
fn the_last_lines_survive_a_hard_crash() {
    const CHILD_DIR: &str = "KERF_TEST_LOG_CRASH_DIR";
    const BURST: u32 = 2000;
    if let Some(dir) = std::env::var_os(CHILD_DIR) {
        tracing_subscriber::registry()
            .with(file_layer(file_appender(Path::new(&dir)).unwrap()))
            .init();
        tracing::info!("starting");
        // A burst, so a queue-backed writer is behind when the end comes.
        for i in 0..BURST {
            tracing::info!(i, "burst");
        }
        tracing::error!("last words before the crash");
        std::process::exit(1);
    }

    let scratch = Scratch::new("crash");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::the_last_lines_survive_a_hard_crash",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_DIR, &scratch.0)
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(1),
        "the child should have died at its exit(1), not finished the test: {}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    let logs = scratch.logs();
    assert!(logs.contains("starting"), "{logs}");
    assert_eq!(
        logs.lines().filter(|l| l.contains("burst")).count(),
        BURST as usize,
        "every line logged before the crash is in the file"
    );
    assert!(
        logs.contains("last words before the crash"),
        "the very last line is in the file"
    );
}

#[test]
fn a_bad_id_names_the_argument_it_came_from() {
    let good = "3f2b1c9e-5a7d-4e0a-9c1b-2d4f6a8b0c1d";
    assert!(id(good, "clip_id").is_ok());
    let why = id("not-an-id", "clip_id").unwrap_err();
    assert!(why.contains("clip_id") && why.contains("not-an-id"), "{why}");
    let why = opt_id(Some(""), "track_id").unwrap_err();
    assert!(why.contains("track_id"), "{why}");
    assert_eq!(opt_id(None, "track_id"), Ok(None));
    let list = vec![good.to_string(), good.to_string(), "oops".to_string()];
    let why = ids(&list, "clip_ids").unwrap_err();
    assert!(why.contains("clip_ids[2]"), "the entry that is wrong is named: {why}");
    assert_eq!(ids(&list[..2], "clip_ids").unwrap().len(), 2);
    let long = "x".repeat(10_000);
    assert!(
        id(&long, "asset_id").unwrap_err().len() < 300,
        "a pasted blob is not echoed whole"
    );
}

#[test]
fn a_stop_reaches_every_job_that_began_before_it_and_none_after() {
    let stop = StopEpoch::default();
    let (first, second) = (stop.begin(), stop.begin());
    assert!(!first() && !second());
    stop.stop();
    assert!(first() && second(), "one stop ends the whole batch");
    let later = stop.begin();
    assert!(!later(), "a job that began after the stop is not cancelled by it");
    stop.stop();
    assert!(later());
}

/// An agent's long job takes its stop from the same epoch as the GUI button that cancels it, and
/// from no other: pressing Stop on a voiceover must not end an export.
#[test]
fn an_agents_long_jobs_stop_with_their_own_cancel_buttons() {
    use crate::AppState;
    use kerf_core::Project;
    use std::sync::{Arc, Mutex};
    let state = AppState::new(
        Arc::new(Mutex::new(Project::open_in_memory().unwrap())),
        Arc::new(AtomicBool::new(false)),
        LaunchSlot::default(),
        Arc::new(kerf_core::ScrubFrames::new(kerf_core::ScrubConfig::default())),
    );
    type Running = Box<dyn Fn() -> bool>;
    let jobs: [(&str, Running, &StopEpoch); 9] = [
        ("voiceover", Box::new(state.begin_voiceover()), &state.voiceover_cancel),
        ("music", Box::new(state.begin_music()), &state.music_cancel),
        ("stems", Box::new(state.begin_stems()), &state.stems_cancel),
        ("smart crop", Box::new(state.begin_smart_crop()), &state.smart_crop_cancel),
        ("stabilize", Box::new(state.begin_stabilize()), &state.stabilize_cancel),
        ("export", Box::new(state.begin_export()), &state.export_cancel),
        ("analysis", Box::new(state.begin_analysis()), &state.analysis_cancel),
        ("cover", Box::new(state.begin_cover_export()), &state.cover_cancel),
        ("import", Box::new(state.begin_import()), &state.import_stop),
    ];
    for (name, running, _) in &jobs {
        assert!(!running(), "{name} starts clean");
    }
    for (pressed, (name, _, button)) in jobs.iter().enumerate() {
        button.stop();
        for (at, (other, running, _)) in jobs.iter().enumerate() {
            assert_eq!(running(), at <= pressed, "{other} after the {name} button");
        }
    }
    let later = state.begin_voiceover();
    assert!(!later(), "a Stop pressed before a run began does not end it");
    state.voiceover_cancel.stop();
    assert!(later());
}

#[test]
fn a_job_for_a_replaced_project_is_refused() {
    use kerf_core::Project;
    let mut project = Project::open_in_memory().unwrap();
    let noted = project.generation();
    assert!(require_generation(&project, noted, "importing").is_ok());
    project.replace_with(Project::open_in_memory().unwrap()).close();
    let why = require_generation(&project, noted, "importing").unwrap_err();
    assert!(why.contains("replaced") && why.contains("importing"), "{why}");
    assert!(require_generation(&project, project.generation(), "importing").is_ok());
}

/// The tick an export read is written under a lock taken for that alone: not at all when nothing was read or the
/// cut asks for nothing, once the asset is still the file that was probed, and as no edit.
#[test]
fn the_ticks_an_export_read_are_kept_under_a_lock_taken_only_for_them() {
    use super::{keep_time_bases, lock_user, store_time_bases};
    use kerf_core::{Asset, ExportOptions, Project, SourceTick, Timeline};
    use std::sync::Mutex;
    use uuid::Uuid;
    let asset: Asset = serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4(),
        "path": "/footage/old.mp4",
        "name": "old.mp4",
        "duration": 10.0,
        "streams": [{ "index": 0, "kind": "video", "codec": "h264", "width": 1920, "height": 1080, "fps": 30.0 }],
        "imported_at": "2026-01-01T00:00:00Z",
    }))
    .unwrap();
    let shared = Mutex::new(Project::open_in_memory().unwrap());
    shared.lock().unwrap().insert_asset(&asset).unwrap();
    let tick = |path: &str, time_base| SourceTick {
        asset: asset.id,
        path: path.to_string(),
        time_base,
    };
    let stored = || shared.lock().unwrap().get_asset(asset.id).unwrap().unwrap().streams[0].time_base;
    let never = || -> std::sync::MutexGuard<'_, Project> { panic!("the project is locked for nothing") };

    store_time_bases(&[], never);
    keep_time_bases(
        &Timeline::new(),
        std::slice::from_ref(&asset),
        &ExportOptions::default(),
        never,
    );
    assert_eq!(stored(), None);

    let history = shared.lock().unwrap().history().unwrap().len();
    store_time_bases(&[tick("/footage/relinked.mp4", (1, 1000))], || lock_user(&shared));
    assert_eq!(stored(), None, "a tick is for the file that was probed");
    store_time_bases(&[tick(&asset.path, (1, 90_000))], || lock_user(&shared));
    assert_eq!(stored(), Some((1, 90_000)));
    store_time_bases(&[tick(&asset.path, (1, 1000))], || lock_user(&shared));
    assert_eq!(stored(), Some((1, 90_000)), "the first one stays");
    assert_eq!(shared.lock().unwrap().history().unwrap().len(), history, "no revision");
}

/// Save As swaps in the reopened copy without moving the generation: a job that noted it before
/// (an analysis, an import, a voiceover) still lands in the project, and a swap that is another
/// project (open / new) still drops it.
#[test]
fn a_job_in_flight_still_lands_after_a_save_as() {
    use kerf_core::{AnalysisKind, AssetAnalysis, Project, TimeRange};
    let dir = std::env::temp_dir().join(format!("kerf-save-as-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut project = Project::sample().unwrap();
    let Some(asset) = project.list_assets().ok().and_then(|a| a.into_iter().next()) else {
        panic!("the sample project lists an asset");
    };

    // The job resolved its inputs under the old project, and is running with the lock released.
    let noted = project.generation();
    let changes = project.change_count();
    let copy = dir.join("copy.kerf");
    project.save_as(&copy).unwrap();
    project.replace_with_saved_copy(Project::open(&copy).unwrap()).close();

    require_generation(&project, noted, "it was being analysed").expect("the same project, in another file");
    assert!(project.change_count() > changes, "the count only goes up");
    let patch = AssetAnalysis {
        asset_id: asset.id,
        silence_segments: vec![TimeRange { start: 1.0, end: 2.0 }],
        ran: vec![AnalysisKind::Silence],
        ..AssetAnalysis::default()
    };
    project.merge_analysis(&patch).expect("what the job found lands");
    assert!(project.get_analysis(asset.id).unwrap().is_some());

    // Another project is another generation, and drops the job.
    project.replace_with(Project::open_in_memory().unwrap()).close();
    assert!(require_generation(&project, noted, "it was being analysed").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The generation check polled from a job's cancel is asked at most once per interval.
#[test]
fn the_replaced_check_is_throttled_and_sticks_once_true() {
    use std::cell::Cell;
    let asked = Cell::new(0u32);
    let moved = Cell::new(false);
    let check = crate::Throttled::new(std::time::Duration::from_millis(60), || {
        asked.set(asked.get() + 1);
        moved.get()
    });
    for _ in 0..100 {
        assert!(!check.ask());
    }
    assert_eq!(asked.get(), 1, "a hundred polls in a hurry are one question");
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(!check.ask());
    assert_eq!(asked.get(), 2, "and it is asked again after the interval");
    moved.set(true);
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(check.ask(), "the project was replaced");
    let after = asked.get();
    assert!(check.ask() && check.ask());
    assert_eq!(asked.get(), after, "once it is true it stays true, unasked");
}

/// A job stops for the user's Stop or for a replaced project, whichever comes first.
#[test]
fn a_job_stops_for_a_stop_or_for_a_replaced_project() {
    use kerf_core::Project;
    let shared = std::sync::Mutex::new(Project::open_in_memory().unwrap());
    let generation = crate::lock_user(&shared).generation();
    let stop = StopEpoch::default();
    let cancel = crate::stop_or_replaced(stop.begin(), &shared, generation);
    assert!(!cancel(), "nothing happened");
    stop.stop();
    assert!(cancel(), "the user's Stop");

    let cancel = crate::stop_or_replaced(stop.begin(), &shared, generation);
    assert!(!cancel());
    crate::lock_user(&shared)
        .replace_with(Project::open_in_memory().unwrap())
        .close();
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert!(cancel(), "the project it began on was replaced");
}

/// Kerf's own settings and logs are never somewhere to write an output.
#[test]
fn an_output_is_refused_inside_kerfs_own_directories() {
    let config = std::env::temp_dir().join(format!("kerf-config-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&config).unwrap();
    let _ = crate::APP_DIRS.set(vec![(config.clone(), "Kerf's settings".to_string())]);
    let protected = kerf_core::paths::Protected::default();
    let inside = config.join("settings.json");
    let err = crate::check_output(&inside, &protected).unwrap_err().to_string();
    assert!(err.contains("Kerf's settings"), "{err}");
    assert!(crate::check_output(&config.join("anything").join("deeper.mp4"), &protected).is_err());
    let elsewhere = std::env::temp_dir().join("kerf-out-elsewhere.mp4");
    assert!(crate::check_output(&elsewhere, &protected).is_ok());
    let _ = std::fs::remove_dir_all(&config);
}

/// A page's id for its `n`th playback, as `playback-id.ts` builds it.
fn page_id(session: u64, n: u64) -> u64 {
    (session << 32) | n
}

#[test]
fn the_newest_playback_wins_whatever_order_the_calls_arrive_in() {
    let p = Playbacks::new();
    p.begin(6);
    p.begin(5);
    assert!(!p.ended(6), "a start that arrives late does not take the pane back");
    assert!(p.ended(5), "and its own stream ends at once");
    assert!(p.ended(7), "an id that never began is not running");
}

#[test]
fn a_stop_that_arrives_before_its_start_still_ends_the_stream() {
    let p = Playbacks::new();
    p.stop(4);
    p.begin(4);
    assert!(p.ended(4), "the stream notices at its first frame");
    p.begin(5);
    assert!(!p.ended(5), "the next playback is not held back by it");
}

#[test]
fn stopping_an_old_stream_does_not_end_the_one_that_replaced_it() {
    let p = Playbacks::new();
    p.begin(1);
    p.begin(2);
    p.stop(1);
    assert!(!p.ended(2));
    p.stop(2);
    assert!(p.ended(2));
    p.finish(2);
    p.begin(3);
    assert!(!p.ended(3));
}

#[test]
fn a_reloaded_page_cannot_collide_with_the_ids_the_one_before_it_left_behind() {
    let p = Playbacks::new();
    let (old, new) = (playback_session(), playback_session());
    assert!(new > old, "each page gets a higher session than the last");
    // The first page played a lot and was reloaded mid-stream; its counter restarts at 1.
    p.begin(page_id(old, 900));
    p.stop(page_id(old, 899));
    p.begin(page_id(new, 1));
    assert!(p.ended(page_id(old, 900)), "the old page's stream is superseded");
    assert!(
        !p.ended(page_id(new, 1)),
        "the new page's first playback runs, though 1 < 900"
    );
    // A stop the old page sent last is not a stop of anything the new page plays.
    p.stop(page_id(old, 950));
    assert!(!p.ended(page_id(new, 1)));
    p.begin(page_id(new, 2));
    assert!(p.ended(page_id(new, 1)) && !p.ended(page_id(new, 2)));
}

#[test]
fn the_ids_a_page_builds_stay_exact_in_a_javascript_number() {
    // 2^21 sessions of 2^32 ids: the largest is below 2^53, where doubles stop being exact.
    let largest = page_id((1 << 21) - 1, u64::from(u32::MAX));
    assert!(largest < (1 << 53));
    assert_eq!(largest as f64 as u64, largest);
}
