//! `ScrubFrames` against an `ffmpeg` that is a shell script: what it does with the processes it
//! starts, which no real binary would let a test arrange. Frames are numbered in their bytes
//! (`w<pid>-<i>` for the i-th of a window, `single-<pid>` for a one-shot), so a test can say which
//! process a picture came from and which frame of its window it was.
//!
//! The wrapper reads its mode from a file beside the clip it is asked to read (`fast` writes every
//! frame at once, `slow` one every 100 ms, `hang` writes nothing and never exits, `die` exits 1,
//! `linger` writes its frames, closes its output and never exits), appends a line to a log and
//! leaves a file named after its pid, so a test counts the processes a request started and checks
//! that every one it expects dead is. Its own binary: it points `KERF_FFMPEG` at the script for the
//! whole process. A POSIX shell and `/proc`.

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use kerf_core::{Asset, Clip, ScrubConfig, ScrubFrames, StreamInfo, StreamKind, Timeline, Track};

const WRAPPER: &str = r#"#!/bin/sh
input=""; frames=""; prev=""
for a in "$@"; do
  case "$prev" in
    -i) input=$a ;;
    -frames:v) frames=$a ;;
  esac
  prev=$a
done
# A probe of the binary itself (`-h full`, `-version`) reads no clip and has nothing to log.
[ -z "$input" ] && exit 0
dir=$(dirname "$input")
mode=$(cat "$dir/mode" 2>/dev/null || echo fast)
echo "$$ $frames $mode" >> "$dir/log"
echo $$ > "$dir/pid.$$"
if [ "$frames" = "1" ]; then
  [ "$mode" = single-fails ] && { echo 'refusing' >&2; exit 1; }
  printf '\377\330single-%s\377\331' "$$"
  exit 0
fi
delay=0
case "$mode" in
  hang) exec sleep 600 ;;
  die) echo 'refusing' >&2; exit 1 ;;
  slow) delay=0.1 ;;
esac
i=0
while [ "$i" -lt "$frames" ]; do
  printf '\377\330w%s-%s\377\331' "$$" "$i"
  [ "$delay" != 0 ] && sleep "$delay"
  i=$((i + 1))
done
case "$mode" in
  linger) exec >&-; exec sleep 600 ;;
esac
exit 0
"#;

/// This process's directory: another run of the suite in the same tree has its own.
fn root() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("scrub-fake-{}", std::process::id()))
}

/// The directories of runs that are long over.
fn sweep_old_roots() {
    let Ok(entries) = std::fs::read_dir(env!("CARGO_TARGET_TMPDIR")) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > Duration::from_secs(3600)));
        if old && entry.file_name().to_string_lossy().starts_with("scrub-fake") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let dir = root();
        sweep_old_roots();
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("ffmpeg-wrapper.sh");
        std::fs::write(&script, WRAPPER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Before any thread of the tests reads it.
        std::env::set_var("KERF_FFMPEG", &script);
    });
}

/// A 30 fps clip of a file that is only a name to the wrapper, in a directory of its own.
struct Rig {
    dir: PathBuf,
    asset: Asset,
}

impl Rig {
    fn new(name: &str) -> Self {
        install();
        let dir = root().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        std::fs::write(&path, b"not a video").unwrap();
        let asset = Asset {
            id: uuid::Uuid::new_v4(),
            path: path.to_string_lossy().into_owned(),
            name: "clip.mp4".into(),
            duration: 60.0,
            streams: vec![StreamInfo {
                index: 0,
                kind: StreamKind::Video,
                codec: "h264".into(),
                width: Some(1280),
                height: Some(720),
                fps: Some(30.0),
                sample_rate: None,
                channels: None,
                image: false,
                projection: None,
                rotation: 0,
                color_transfer: None,
                color_primaries: None,
                pix_fmt: Some("yuv420p".into()),
                color_space: None,
            }],
            imported_at: chrono::Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
        };
        Self { dir, asset }
    }

    fn mode(&self, mode: &str) {
        std::fs::write(self.dir.join("mode"), mode).unwrap();
    }

    /// The cut: one clip of the file, `edit`ed.
    fn cut(&self, edit: impl FnOnce(&mut Clip)) -> Timeline {
        let mut clip = Clip::new(self.asset.id, 0.0, 60.0, 0.0);
        edit(&mut clip);
        Timeline {
            tracks: vec![Track {
                clips: vec![clip],
                ..Track::new(StreamKind::Video, "V1".to_string())
            }],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        }
    }

    /// One line per process the wrapper has been run as: `(pid, frames asked, mode)`.
    fn runs(&self) -> Vec<(u32, u32, String)> {
        let text = std::fs::read_to_string(self.dir.join("log")).unwrap_or_default();
        text.lines()
            .filter_map(|l| {
                let mut p = l.split_whitespace();
                Some((p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.to_string()))
            })
            .collect()
    }

    fn windows(&self) -> Vec<u32> {
        self.runs().into_iter().filter(|r| r.1 > 1).map(|r| r.0).collect()
    }

    fn singles(&self) -> usize {
        self.runs().iter().filter(|r| r.1 == 1).count()
    }
}

/// The time of frame `k` of a 30 fps cut, in the middle of its slot (the grid puts it in `k`).
fn at(k: u64) -> f64 {
    (k as f64 + 0.5) / 30.0
}

fn text(jpeg: &[u8]) -> String {
    assert!(jpeg.starts_with(&[0xFF, 0xD8]) && jpeg.ends_with(&[0xFF, 0xD9]), "a JPEG");
    String::from_utf8_lossy(&jpeg[2..jpeg.len() - 2]).into_owned()
}

/// `(pid, i)` of a window frame `w<pid>-<i>`.
fn window_frame(jpeg: &[u8]) -> (u32, u32) {
    let t = text(jpeg);
    let rest = t.strip_prefix('w').unwrap_or_else(|| panic!("not a window frame: {t}"));
    let (pid, i) = rest.split_once('-').unwrap();
    (pid.parse().unwrap(), i.parse().unwrap())
}

fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat.contains(") Z"),
        Err(_) => false,
    }
}

fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn scrub() -> ScrubFrames {
    scrub_reading_ahead(8)
}

fn scrub_reading_ahead(frames: u32) -> ScrubFrames {
    ScrubFrames::new(ScrubConfig {
        window_frames: 24,
        lead_frames: 2,
        read_ahead_frames: frames,
        wait_frames: 12,
        first_frame_timeout: Duration::from_millis(500),
        frame_timeout: Duration::from_millis(500),
        request_timeout: Duration::from_secs(10),
        ..ScrubConfig::default()
    })
}

fn frame(s: &ScrubFrames, tl: &Timeline, rig: &Rig, k: u64) -> Arc<Vec<u8>> {
    s.frame(tl, std::slice::from_ref(&rig.asset), at(k), 960, 4).expect("a frame")
}

#[test]
fn a_miss_starts_one_window_and_the_next_frames_are_hits() {
    let rig = Rig::new("hits");
    let tl = rig.cut(|_| {});
    let s = scrub_reading_ahead(0);
    // Frame 100 is the third of a window that starts two frames before it.
    let first = window_frame(&frame(&s, &tl, &rig, 100));
    assert_eq!(first.1, 2, "two frames of lead");
    for (step, k) in (101..=110).enumerate() {
        let got = window_frame(&frame(&s, &tl, &rig, k));
        assert_eq!(
            got,
            (first.0, 3 + step as u32),
            "frame {k} came from the same process, in order"
        );
    }
    // And back over what the lead drew.
    assert_eq!(window_frame(&frame(&s, &tl, &rig, 98)), (first.0, 0));
    assert_eq!(rig.windows().len(), 1, "one process for twelve frames");
    assert_eq!(rig.singles(), 0);
    let stats = s.stats();
    // A frame asked for the instant before the window draws it is waited for, and counts as a miss.
    assert_eq!((stats.windows, stats.singles, stats.hits + stats.misses), (1, 0, 12));
    assert!(stats.misses >= 1);
    assert!(stats.drawn >= 13, "frames 98 to 110 were served, so they were drawn");
}

#[test]
fn a_frame_asked_again_is_the_same_picture_and_starts_nothing() {
    let rig = Rig::new("again");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let a = frame(&s, &tl, &rig, 50);
    for _ in 0..5 {
        assert_eq!(*frame(&s, &tl, &rig, 50), *a);
    }
    assert_eq!(rig.runs().len(), 1);
}

#[test]
fn a_time_inside_a_frame_is_that_frame() {
    let rig = Rig::new("grid");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let a = s
        .frame(&tl, std::slice::from_ref(&rig.asset), 100.0 / 30.0 + 0.001, 960, 4)
        .unwrap();
    let b = s
        .frame(&tl, std::slice::from_ref(&rig.asset), 101.0 / 30.0 - 0.001, 960, 4)
        .unwrap();
    assert_eq!(*a, *b, "both are in frame 100");
    assert_eq!(rig.runs().len(), 1);
}

#[test]
fn a_request_for_a_frame_the_window_is_drawing_waits_for_it_and_starts_nothing() {
    let rig = Rig::new("waits");
    rig.mode("slow");
    let tl = rig.cut(|_| {});
    let s = Arc::new(scrub());
    let started = Instant::now();
    // Frame 98 is its first frame; 104 arrives six frames, 0.6 s, later.
    let _ = frame(&s, &tl, &rig, 100);
    let late = frame(&s, &tl, &rig, 104);
    assert_eq!(window_frame(&late).1, 6);
    assert!(
        started.elapsed() >= Duration::from_millis(350),
        "it waited for the process to draw it"
    );
    assert_eq!(rig.windows().len(), 1, "and did not start another");
    assert_eq!(s.stats().windows, 1);
}

#[test]
fn a_jump_away_kills_the_window_it_leaves() {
    let rig = Rig::new("jump");
    rig.mode("slow");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let first = window_frame(&frame(&s, &tl, &rig, 100));
    assert!(alive(first.0), "still drawing");
    let second = window_frame(&frame(&s, &tl, &rig, 900));
    assert_ne!(first.0, second.0, "another process");
    wait_for("the first window to die", Duration::from_secs(5), || !alive(first.0));
    let stats = s.stats();
    assert_eq!((stats.windows, stats.killed), (2, 1));
}

#[test]
fn an_edit_that_moves_the_frame_kills_the_window_and_never_serves_the_old_picture() {
    let rig = Rig::new("edit");
    rig.mode("slow");
    let before = rig.cut(|_| {});
    let after = rig.cut(|c| c.color.brightness = 0.3);
    let s = scrub();
    let old = frame(&s, &before, &rig, 100);
    let old_window = window_frame(&old);
    // The same frame of the edited cut is not the cached one.
    let new = frame(&s, &after, &rig, 100);
    let new_window = window_frame(&new);
    assert_ne!(*new, *old, "the edited frame is not the frame cached before the edit");
    assert_ne!(new_window.0, old_window.0, "it came from a window of its own");
    wait_for("the first window to die", Duration::from_secs(5), || !alive(old_window.0));
    // Undoing the edit is the same key again: served from the cache, no process.
    let runs = rig.runs().len();
    assert_eq!(*frame(&s, &before, &rig, 100), *old);
    assert_eq!(rig.runs().len(), runs, "an undo costs nothing");
}

#[test]
fn an_edit_that_does_not_touch_a_frame_leaves_it_cached() {
    let rig = Rig::new("unrelated");
    let a = Clip::new(rig.asset.id, 0.0, 5.0, 0.0);
    let b = Clip::new(rig.asset.id, 10.0, 15.0, 5.0);
    let mut cut = Timeline {
        tracks: vec![Track {
            clips: vec![a, b],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    };
    let s = scrub();
    let cached = frame(&s, &cut, &rig, 30);
    let runs = rig.runs().len();
    cut.tracks[0].clips[1].color.contrast = 1.5;
    assert_eq!(
        *frame(&s, &cut, &rig, 30),
        *cached,
        "grading the second clip leaves the first's frames"
    );
    assert_eq!(rig.runs().len(), runs);
}

#[test]
fn stopping_kills_the_window_and_keeps_what_it_drew() {
    let rig = Rig::new("stop");
    rig.mode("slow");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let first = window_frame(&frame(&s, &tl, &rig, 100));
    assert!(alive(first.0));
    s.stop();
    wait_for("the window to die", Duration::from_secs(5), || !alive(first.0));
    let runs = rig.runs().len();
    assert_eq!(
        window_frame(&frame(&s, &tl, &rig, 100)).0,
        first.0,
        "a frame it drew is still held"
    );
    assert_eq!(rig.runs().len(), runs);
}

#[test]
fn clearing_forgets_the_frames_too() {
    let rig = Rig::new("clear");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let _ = frame(&s, &tl, &rig, 100);
    s.clear();
    assert_eq!(s.stats().cache.entries, 0);
    let _ = frame(&s, &tl, &rig, 100);
    assert_eq!(rig.windows().len(), 2, "so the frame is drawn again");
}

#[test]
fn dropping_it_kills_the_window() {
    let rig = Rig::new("drop");
    rig.mode("slow");
    let tl = rig.cut(|_| {});
    let pid = {
        let s = scrub();
        window_frame(&frame(&s, &tl, &rig, 100)).0
    };
    wait_for("the window to die", Duration::from_secs(5), || !alive(pid));
}

#[test]
fn a_window_that_draws_nothing_is_killed_and_the_frame_is_a_one_shot() {
    let rig = Rig::new("hang");
    rig.mode("hang");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let started = Instant::now();
    let got = text(&frame(&s, &tl, &rig, 100));
    assert!(got.starts_with("single-"), "{got}");
    assert!(started.elapsed() < Duration::from_secs(8), "after its timeouts, not for ever");
    for pid in rig.windows() {
        wait_for("a hung window to die", Duration::from_secs(5), || !alive(pid));
    }
    assert!(s.stats().killed >= 1);
}

#[test]
fn a_window_that_dies_leaves_the_frame_to_a_one_shot() {
    let rig = Rig::new("die");
    rig.mode("die");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let got = text(&frame(&s, &tl, &rig, 100));
    assert!(got.starts_with("single-"), "{got}");
    assert_eq!(s.stats().singles, 1);
    // And it is cached: no process for the same frame again.
    let runs = rig.runs().len();
    assert_eq!(text(&frame(&s, &tl, &rig, 100)), got);
    assert_eq!(rig.runs().len(), runs);
}

#[test]
fn a_window_that_closes_its_output_and_never_exits_is_killed() {
    let rig = Rig::new("linger");
    rig.mode("linger");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let first = window_frame(&frame(&s, &tl, &rig, 100));
    wait_for("the lingering window to be killed", Duration::from_secs(8), || {
        !alive(first.0)
    });
    // What it drew is kept.
    assert_eq!(window_frame(&frame(&s, &tl, &rig, 101)).0, first.0);
}

#[test]
fn a_frame_inside_a_fade_is_a_one_shot_and_a_window_never_spans_it() {
    let rig = Rig::new("fade");
    let tl = rig.cut(|c| c.fade_in = 1.0);
    let s = scrub();
    // Frames 0..30 are the fade: each is drawn alone, nothing is read ahead from them.
    let got = text(&frame(&s, &tl, &rig, 10));
    assert!(got.starts_with("single-"), "{got}");
    assert!(rig.windows().is_empty());
    // Past it a window starts.
    let after = window_frame(&frame(&s, &tl, &rig, 60));
    assert_eq!(after.1, 2);
}

#[test]
fn a_scrub_nearing_the_end_of_a_window_has_the_next_one_before_it_gets_there() {
    let rig = Rig::new("ahead");
    let tl = rig.cut(|_| {});
    let s = scrub();
    // The first window is frames 98..122.
    let first = window_frame(&frame(&s, &tl, &rig, 100));
    wait_for("the first window to be drawn", Duration::from_secs(5), || {
        s.stats().drawn >= 24
    });
    std::thread::sleep(Duration::from_millis(200));
    // Step on to eight frames from its end.
    for k in 101..=114 {
        let _ = frame(&s, &tl, &rig, k);
    }
    wait_for("the next window to start", Duration::from_secs(5), || {
        rig.windows().len() >= 2
    });
    wait_for("it to draw", Duration::from_secs(5), || s.stats().drawn >= 48);
    // Frame 122 is the first of the second window: already there.
    let runs = rig.runs().len();
    let second = window_frame(&frame(&s, &tl, &rig, 123));
    assert_ne!(second.0, first.0, "the second process drew it");
    assert_eq!(second.1, 1);
    assert_eq!(rig.runs().len(), runs, "and asking for it started nothing");
}

#[test]
fn a_scrub_going_back_has_the_frames_behind_it_before_it_gets_there() {
    let rig = Rig::new("back");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let _ = frame(&s, &tl, &rig, 300);
    wait_for("the first window to be drawn", Duration::from_secs(5), || {
        s.stats().drawn >= 24
    });
    std::thread::sleep(Duration::from_millis(200));
    // 299 is a hit of the window 298..322, within eight frames of its start going back: the window
    // that ends there is started.
    let _ = frame(&s, &tl, &rig, 299);
    wait_for("the window behind to start", Duration::from_secs(5), || {
        rig.windows().len() >= 2
    });
    wait_for("it to draw", Duration::from_secs(5), || s.stats().drawn >= 48);
    assert_eq!(
        window_frame(&frame(&s, &tl, &rig, 297)).1,
        23,
        "the last of the window 274..298"
    );
    // The scrub nears the start of that window and the one behind it is read ahead, which is a
    // process nobody asked for: what is asked for is held.
    let before = s.stats();
    for k in (274..297).rev() {
        let _ = frame(&s, &tl, &rig, k);
    }
    let after = s.stats();
    assert_eq!(after.misses, before.misses, "twenty-four steps back, none of them a miss");
    assert_eq!(after.hits, before.hits + 23);
}

#[test]
fn a_backward_miss_starts_a_window_that_ends_just_after_the_frame() {
    let rig = Rig::new("reverse");
    let tl = rig.cut(|_| {});
    let s = scrub();
    let _ = frame(&s, &tl, &rig, 500);
    wait_for("the window 498..522", Duration::from_secs(10), || s.stats().drawn >= 24);
    // 20 frames back, outside the window 498..522: a step back, not a jump.
    let got = window_frame(&frame(&s, &tl, &rig, 480));
    assert_eq!(got.1, 22, "the requested frame is the 23rd of the window 458..482");
    wait_for("the window 458..482", Duration::from_secs(10), || s.stats().drawn >= 48);
    let before = s.stats();
    for k in (458..482).rev() {
        let _ = frame(&s, &tl, &rig, k);
    }
    let after = s.stats();
    assert_eq!(after.misses, before.misses, "all of it is held");
    assert_eq!(after.hits, before.hits + 24);
}

#[test]
fn the_cache_is_bounded_in_entries() {
    let rig = Rig::new("bounded");
    let tl = rig.cut(|_| {});
    let s = ScrubFrames::new(ScrubConfig {
        cache_entries: 8,
        ..ScrubConfig::default()
    });
    for (n, k) in [100, 500, 900, 1300].into_iter().enumerate() {
        let _ = frame(&s, &tl, &rig, k);
        wait_for("the window to draw", Duration::from_secs(10), || {
            s.stats().drawn >= 24 * (n as u64 + 1)
        });
    }
    assert!(s.stats().cache.entries <= 8, "{:?}", s.stats().cache);
    assert!(s.stats().cache.evicted > 0);
}

#[test]
fn windows_off_is_a_cached_one_shot_per_frame() {
    let rig = Rig::new("off");
    let tl = rig.cut(|_| {});
    let s = ScrubFrames::new(ScrubConfig {
        window_frames: 0,
        ..ScrubConfig::default()
    });
    let a = frame(&s, &tl, &rig, 100);
    let b = frame(&s, &tl, &rig, 101);
    assert!(text(&a).starts_with("single-") && text(&b).starts_with("single-"));
    assert_ne!(*a, *b);
    assert_eq!(rig.singles(), 2);
    assert_eq!(*frame(&s, &tl, &rig, 100), *a, "and held");
    assert_eq!(rig.singles(), 2);
}

#[test]
fn a_one_shot_that_fails_is_the_requests_error() {
    let rig = Rig::new("single-fails");
    rig.mode("single-fails");
    let tl = rig.cut(|c| c.fade_in = 1.0);
    let s = scrub();
    let err = s.frame(&tl, std::slice::from_ref(&rig.asset), at(5), 960, 4).unwrap_err();
    assert!(err.to_string().contains("could not render timeline frame"), "{err}");
}

#[test]
fn a_missing_asset_is_the_error_a_still_gives() {
    let rig = Rig::new("missing");
    let tl = rig.cut(|_| {});
    let s = scrub();
    assert!(s.frame(&tl, &[], at(5), 960, 4).is_err());
}

#[test]
fn many_threads_asking_for_the_same_frames_get_the_same_pictures() {
    let rig = Rig::new("threads");
    let tl = rig.cut(|_| {});
    let s = Arc::new(scrub());
    let rig = Arc::new(rig);
    let tl = Arc::new(tl);
    let handles: Vec<_> = (0..4)
        .map(|n| {
            let (s, rig, tl) = (Arc::clone(&s), Arc::clone(&rig), Arc::clone(&tl));
            std::thread::spawn(move || (100..112).map(|k| (k, frame(&s, &tl, &rig, k + n % 2))).collect::<Vec<_>>())
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for (k, jpeg) in &results[0] {
        let again = frame(&s, &tl, &rig, *k);
        assert_eq!(**jpeg, *again, "frame {k} is one picture");
    }
}
