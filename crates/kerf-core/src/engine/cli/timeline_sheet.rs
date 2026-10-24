//! A contact sheet of the composited cut (`skim_timeline`): every moment drawn as the one-shot
//! still `preview_timeline` draws ([`run_still_as`]), at cell size and a few at a time, then tiled
//! into one JPEG by one more ffmpeg. One export-graph pass picking the moments out would decode
//! every frame of the span; a still per moment reads one frame of each clip on screen, from its
//! proxy when the caller passes the preview assets.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use uuid::Uuid;

use super::still_fx::still_layers;
use super::*;
use crate::model::Asset;

/// One cell of a sheet of the cut: its timeline time and the clips on screen there, bottom to top.
#[derive(Debug, Clone, PartialEq)]
pub struct SheetCell {
    pub time: f64,
    pub clips: Vec<SheetClip>,
}

/// A clip on screen in a [`SheetCell`], named after its asset.
#[derive(Debug, Clone, PartialEq)]
pub struct SheetClip {
    pub clip_id: Uuid,
    pub name: String,
}

/// The most stills a sheet draws at once. A still from proxies is one thread, so four are four
/// cores at most, and a sheet of sixteen is four rounds.
const MAX_SHEET_WORKERS: usize = 4;

/// JPEG `-q:v` of a cell before it is tiled: the sheet is encoded once more, at the caller's.
const CELL_QUALITY: u8 = 2;

/// How long the tiling run may take: it decodes a few dozen small JPEGs.
const TILE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The clips each moment of `times` shows, bottom to top: the layers the still at that moment
/// composites ([`still_layers`] over the rendered cut), the outgoing clip of a transition included.
pub fn timeline_sheet_cells(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, times: &[f64]) -> Vec<SheetCell> {
    let rendered = timeline.for_render();
    let fps = export_format(&rendered, assets, opts).fps;
    let name = |id: Uuid| assets.iter().find(|a| a.id == id).map(|a| a.name.clone()).unwrap_or_default();
    times
        .iter()
        .map(|&time| SheetCell {
            time,
            clips: still_layers(&rendered, assets, fps, time.max(0.0))
                .iter()
                .map(|layer| SheetClip {
                    clip_id: layer.active.clip.id,
                    name: name(layer.active.clip.asset_id),
                })
                .collect(),
        })
        .collect()
}

/// Composite the cut at each of `times` (row-major) as a `cell_width` px wide still and tile them
/// `columns` across into one JPEG (`quality` = `-q:v`). The stills are drawn a few at a time
/// ([`sheet_workers`]), niced and sharing the budget's threads, each killed past its timeout; a gap
/// is the still's black canvas. Gives up with [`Error::Cancelled`] once `cancel` is true, and the
/// first still that fails stops the rest.
#[allow(clippy::too_many_arguments)]
pub fn timeline_sheet_cancellable(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    times: &[f64],
    columns: u32,
    cell_width: u32,
    quality: u8,
    cancel: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<u8>> {
    if times.is_empty() {
        return Err(Error::Engine("a contact sheet needs at least one cell".to_string()));
    }
    let columns = columns.max(1);
    let rows = (times.len() as u32).div_ceil(columns);
    let workers = sheet_workers(times.len(), cpu::budget_threads());
    let spawn = StillSpawn {
        share: workers,
        background: true,
    };
    let out = StillOutput::JpegPipe { quality: CELL_QUALITY };
    let failed = AtomicBool::new(false);
    let stop = || failed.load(Ordering::Relaxed) || cancel();
    let next = AtomicUsize::new(0);
    let cells: Mutex<Vec<Vec<u8>>> = Mutex::new(vec![Vec::new(); times.len()]);
    let error: Mutex<Option<Error>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= times.len() || stop() {
                    break;
                }
                match run_still_as(timeline, assets, opts, times[i], cell_width, None, &out, spawn, &stop) {
                    Ok(jpeg) => cells.lock().unwrap_or_else(|e| e.into_inner())[i] = jpeg,
                    Err(e) => {
                        failed.store(true, Ordering::Relaxed);
                        if !matches!(e, Error::Cancelled) {
                            error.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert(e);
                        }
                        break;
                    }
                }
            });
        }
    });
    if cancel() {
        return Err(Error::Cancelled);
    }
    if let Some(e) = error.into_inner().unwrap_or_else(|e| e.into_inner()) {
        return Err(e);
    }
    let cells = cells.into_inner().unwrap_or_else(|e| e.into_inner());
    tile(&cells.concat(), columns, rows, quality, cancel)
}

/// How many stills a sheet of `cells` draws at once at a budget of `budget` threads.
fn sheet_workers(cells: usize, budget: usize) -> usize {
    budget.clamp(1, MAX_SHEET_WORKERS).min(cells.max(1))
}

/// The cells' JPEGs, on disk for the tiling run and removed after it.
struct SheetCells(PathBuf);

impl Drop for SheetCells {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Tile the concatenated JPEGs `cells` into a `columns`x`rows` sheet, one more small ffmpeg.
fn tile(cells: &[u8], columns: u32, rows: u32, quality: u8, cancel: &(dyn Fn() -> bool + Sync)) -> Result<Vec<u8>> {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("kerf-sheet-{}-{seq}.mjpeg", std::process::id()));
    std::fs::write(&path, cells).map_err(|e| Error::Engine(format!("could not write the sheet's cells: {e}")))?;
    let path = SheetCells(path);
    let mut args = build_tile_args(&path.0.to_string_lossy(), columns, rows, quality);
    cpu::cap_input_threads(&mut args, 1);
    cpu::single_frame_args(&mut args);
    let bin = ffmpeg_bin();
    let mut cmd = bg_command(&bin);
    cmd.args(&args);
    let output = output_bounded_cancellable(cmd, TILE_TIMEOUT, Some(cancel))
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => Error::Engine(format!(
                "could not tile the contact sheet: ffmpeg did not answer in {} s and was stopped",
                TILE_TIMEOUT.as_secs()
            )),
            _ => launch_err(&bin, e),
        })?
        .ok_or(Error::Cancelled)?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(Error::Engine(format!(
            "could not tile the contact sheet: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// Pure arg builder for [`tile`]: the concatenated cell JPEGs at `path` read as one MJPEG stream,
/// `tile`d and written as one JPEG to stdout.
fn build_tile_args(path: &str, columns: u32, rows: u32, quality: u8) -> Vec<String> {
    [
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "mjpeg",
        "-i",
        path,
        "-frames:v",
        "1",
        "-vf",
        &format!("tile={columns}x{rows}"),
        "-q:v",
        &quality.to_string(),
        "-f",
        "image2pipe",
        "-vcodec",
        "mjpeg",
        "pipe:1",
    ]
    .map(String::from)
    .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};

    #[test]
    fn a_sheet_draws_a_few_stills_at_once_and_never_more_than_it_has_cells() {
        assert_eq!(sheet_workers(16, 12), MAX_SHEET_WORKERS);
        assert_eq!(sheet_workers(16, 2), 2);
        assert_eq!(sheet_workers(16, 0), 1);
        assert_eq!(sheet_workers(1, 8), 1);
        assert_eq!(sheet_workers(0, 8), 1);
    }

    #[test]
    fn the_tile_run_reads_the_cells_as_one_mjpeg_stream_and_writes_one_jpeg() {
        assert_eq!(
            build_tile_args("/tmp/cells.mjpeg", 4, 3, 5).join(" "),
            "-hide_banner -loglevel error -f mjpeg -i /tmp/cells.mjpeg -frames:v 1 -vf tile=4x3 -q:v 5 \
             -f image2pipe -vcodec mjpeg pipe:1"
        );
    }

    fn two_clips_and_a_gap() -> (Timeline, Vec<Asset>) {
        let mut a = test_asset(vec![video_stream(320, 180, 25.0)]);
        a.name = "red".to_string();
        a.duration = 4.0;
        let mut b = test_asset(vec![video_stream(320, 180, 25.0)]);
        b.name = "blue".to_string();
        b.duration = 4.0;
        let timeline = timeline_of(vec![
            video_track(vec![make_clip(a.id, 0.0, 2.0, 0.0), make_clip(b.id, 0.0, 2.0, 4.0)]),
            video_track(vec![make_clip(b.id, 2.0, 3.0, 1.0)]),
        ]);
        (timeline, vec![a, b])
    }

    #[test]
    fn a_cell_names_the_clips_on_screen_bottom_to_top_and_none_in_a_gap() {
        let (timeline, assets) = two_clips_and_a_gap();
        let cells = timeline_sheet_cells(&timeline, &assets, &ExportOptions::default(), &[0.5, 1.5, 3.0, 4.5]);
        let names = |i: usize| cells[i].clips.iter().map(|c| c.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(0), ["red"]);
        assert_eq!(names(1), ["red", "blue"]);
        assert!(cells[2].clips.is_empty(), "{:?}", cells[2]);
        assert_eq!(names(3), ["blue"]);
        assert_eq!(cells[1].clips[0].clip_id, timeline.tracks[0].clips[0].id);
        assert_eq!(cells[1].clips[1].clip_id, timeline.tracks[1].clips[0].id);
        assert_eq!(cells.iter().map(|c| c.time).collect::<Vec<_>>(), [0.5, 1.5, 3.0, 4.5]);
    }

    #[test]
    fn a_hidden_track_or_a_disabled_clip_is_not_on_screen() {
        let (mut timeline, assets) = two_clips_and_a_gap();
        timeline.tracks[1].muted = true;
        timeline.tracks[0].clips[0].enabled = false;
        let cells = timeline_sheet_cells(&timeline, &assets, &ExportOptions::default(), &[1.5]);
        assert!(cells[0].clips.is_empty(), "{:?}", cells[0]);
    }

    /// A lossless clip of `color` at 320x180.
    fn solid(dir: &std::path::Path, color: &str) -> String {
        let path = dir.join(format!("{color}.mp4"));
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c={color}:s=320x180:r=25:d=4,format=yuv420p"))
            .args(["-c:v", "libx264", "-crf", "0", "-g", "1"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
        path.to_string_lossy().into_owned()
    }

    /// The sheet as rgb24, with its size.
    fn decode(jpeg: &[u8], dir: &std::path::Path) -> (u32, u32, Vec<u8>) {
        let path = dir.join("sheet.jpg");
        std::fs::write(&path, jpeg).unwrap();
        let probe = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&path)
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(probe.status.success(), "{}", String::from_utf8_lossy(&probe.stderr));
        // 2 cells across of 160x90.
        (320, (probe.stdout.len() / (320 * 3)) as u32, probe.stdout)
    }

    #[test]
    #[ignore = "drives the real ffmpeg"]
    fn a_sheet_of_the_cut_tiles_each_moment_and_a_gap_is_black() {
        let dir = std::env::temp_dir().join(format!("kerf-timeline-sheet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (timeline, mut assets) = two_clips_and_a_gap();
        assets[0].path = solid(&dir, "red");
        assets[1].path = solid(&dir, "blue");
        let times = [0.5, 1.5, 3.0, 4.5];
        let started = std::time::Instant::now();
        let jpeg = timeline_sheet_cancellable(&timeline, &assets, &ExportOptions::default(), &times, 2, 160, 3, &|| false)
            .expect("sheet");
        let took = started.elapsed();
        assert_eq!(&jpeg[..2], [0xFF, 0xD8], "a JPEG");
        let (w, h, rgb) = decode(&jpeg, &dir);
        assert_eq!((w, h), (320, 180), "2x2 cells of 160x90");
        // The middle of each cell: red, blue (the upper track over the red), black (the gap), blue.
        let at = |cx: u32, cy: u32| {
            let (x, y) = (cx * 160 + 80, cy * 90 + 45);
            let i = ((y * w + x) * 3) as usize;
            (rgb[i], rgb[i + 1], rgb[i + 2])
        };
        let (r, g, b) = at(0, 0);
        assert!(r > 200 && g < 50 && b < 50, "cell 1 is red: {:?}", (r, g, b));
        let (r, g, b) = at(1, 0);
        assert!(b > 200 && r < 50 && g < 50, "cell 2 is blue: {:?}", (r, g, b));
        let (r, g, b) = at(0, 1);
        assert!(r < 20 && g < 20 && b < 20, "cell 3 is the gap, black: {:?}", (r, g, b));
        let (r, g, b) = at(1, 1);
        assert!(b > 200 && r < 50 && g < 50, "cell 4 is blue: {:?}", (r, g, b));
        assert!(took < std::time::Duration::from_secs(20), "{took:?}");

        let cancelled = timeline_sheet_cancellable(&timeline, &assets, &ExportOptions::default(), &times, 2, 160, 3, &|| true);
        assert!(matches!(cancelled, Err(Error::Cancelled)), "{cancelled:?}");

        let mut missing = assets.clone();
        missing[1].path = dir.join("gone.mp4").to_string_lossy().into_owned();
        let failed = timeline_sheet_cancellable(&timeline, &missing, &ExportOptions::default(), &times, 2, 160, 3, &|| false);
        assert!(matches!(failed, Err(Error::Engine(_))), "{failed:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
