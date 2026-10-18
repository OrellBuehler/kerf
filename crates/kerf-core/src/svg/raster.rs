//! Drawing an animated title: resvg over the static document [`SvgDoc::at`] gives for a moment,
//! and the on-disk cache those pictures are written to for ffmpeg to read.
//!
//! **One rasteriser for every frame path.** An export, the scrubbed still, the playback stream and
//! an agent's contact sheet all come through [`render`], so a title looks the same wherever it is
//! seen. ffmpeg never draws SVG: it reads PNGs (a numbered sequence for an export or a stream, one
//! file for a still) and `overlay`s them, which is why nothing in the graph has to know what an
//! animation is.
//!
//! **The cache is content-addressed and per process.** Files live under
//! `<cache>/kerf/svg/<pid>/` (else a private directory in the temp one, like the fonts), each
//! sequence in a directory named by a digest of everything that decides its pixels. A sequence is
//! usable once its `complete` marker is there; a frame is written under a temporary name and
//! renamed, so an interrupted run leaves nothing ffmpeg could read half-written. A directory whose
//! process is gone is removed on the next start, and this process's own is emptied at its first use
//! (a pid is reused: what an earlier run with the same one left is not trusted); within a process the
//! total is held under `KERF_SVG_CACHE_MB` (1024) by dropping the least recently used sequence, never
//! one a [`Pin`] holds (an export's sequences are pinned from the moment they are prepared until
//! ffmpeg is done) — the check and the removal are one step against a new pin.
//!
//! **The fonts are not in the key** because they cannot change under it: the font database is built
//! once per process (`fonts::svg_fontdb`) and so is this directory, which is emptied when the
//! process starts. A font installed while Kerf runs is seen by neither until the next run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use resvg::usvg::{self, ImageHrefResolver, ImageKind};
use tiny_skia::{Color, Pixmap, PixmapPaint, PremultipliedColorU8, Transform};

use super::images::{self, Format};
use super::{cost, invalid, SvgDoc};
use crate::error::{Error, Result};
use crate::fonts;

/// Bumped whenever the pixels a document draws to could change (a resvg upgrade that moves
/// anti-aliasing, a change to the fit or the opacity): it is part of every cache key.
pub const RASTER_VERSION: u32 = 1;

/// The most pixels one rendered picture may have (an 8K frame is 33 million).
pub const MAX_PIXELS: u64 = 50_000_000;

/// What the cache may hold, in MiB, when `KERF_SVG_CACHE_MB` does not say.
const DEFAULT_CACHE_MB: u64 = 1024;

const MARKER: &str = "complete";

// ---- drawing ----------------------------------------------------------------------------------

fn options() -> usvg::Options<'static> {
    usvg::Options {
        // The face text gets when it names none: the shipped one, on every machine.
        font_family: fonts::DEFAULT_FAMILY.to_string(),
        fontdb: fonts::svg_fontdb(),
        resources_dir: None,
        image_href_resolver: image_resolver(),
        ..usvg::Options::default()
    }
}

/// Which images a title may draw. Only one the document carries itself is read: a path or a URL in
/// `href` is nothing, so a title can neither read a file off the disk nor reach for the network;
/// of those only the four raster kinds the sanitiser allows (no nested SVG document), and only one
/// whose header says it fits in [`images::MAX_IMAGE_PIXELS`] — the decoder allocates what the header
/// claims, so a stored document from before that limit existed is held to it here too.
pub(super) fn image_resolver() -> ImageHrefResolver<'static> {
    ImageHrefResolver {
        resolve_data: Box::new(|mime, data, _| {
            let format = Format::of_mime(mime)?;
            if let Err(why) = images::check(mime, &data) {
                tracing::warn!("an embedded image was not drawn: {why}");
                return None;
            }
            Some(match format {
                Format::Png => ImageKind::PNG(data),
                Format::Jpeg => ImageKind::JPEG(data),
                Format::Gif => ImageKind::GIF(data),
                Format::Webp => ImageKind::WEBP(data),
            })
        }),
        resolve_string: Box::new(|_, _| None),
    }
}

/// Draw the static document `svg` into a transparent `pw` x `ph` picture, fit "contain": scaled to
/// the largest size that keeps its shape inside, centred, and clipped to its own viewport (what
/// slides in from outside the `viewBox` is not seen in the margin). `opacity` multiplies every
/// channel of the premultiplied result, so the alpha is already the overlay's.
pub fn render(svg: &str, pw: u32, ph: u32, opacity: f64) -> Result<Pixmap> {
    let (pw, ph) = (pw.max(1), ph.max(1));
    if u64::from(pw) * u64::from(ph) > MAX_PIXELS {
        return Err(Error::InvalidArgument(format!(
            "an animated title of {pw} x {ph} pixels is too large to draw"
        )));
    }
    let tree = usvg::Tree::from_str(svg, &options()).map_err(|e| invalid(format!("not a drawable SVG: {e}")))?;
    let (w, h) = (tree.size().width(), tree.size().height());
    let (fw, fh) = fit_contain(w, h, pw, ph);
    let into = Transform::from_scale(fw as f32 / w, fh as f32 / h);
    // On this very tree (the document of this moment, animations spliced in) and the transform
    // resvg is given: the estimate follows every group's transform down, as resvg does.
    if cost::frame_ops(&tree, into, (f64::from(fw), f64::from(fh))) > cost::MAX_FRAME_OPS {
        return Err(invalid(cost::TOO_SLOW));
    }
    let alloc = |w, h| Pixmap::new(w, h).ok_or_else(|| Error::Engine(format!("cannot allocate a {w} x {h} picture")));
    let mut out = alloc(pw, ph)?;
    if (fw, fh) == (pw, ph) {
        resvg::render(&tree, into, &mut out.as_mut());
    } else {
        let mut inner = alloc(fw, fh)?;
        resvg::render(&tree, into, &mut inner.as_mut());
        out.draw_pixmap(
            ((pw - fw) / 2) as i32,
            ((ph - fh) / 2) as i32,
            inner.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
    apply_opacity(&mut out, opacity);
    Ok(out)
}

/// The size, in pixels, a document of `w` x `h` user units is drawn at inside a `pw` x `ph` picture
/// (fit "contain": the largest size that keeps its shape inside).
pub(super) fn fit_contain(w: f32, h: f32, pw: u32, ph: u32) -> (u32, u32) {
    let scale = (pw as f32 / w).min(ph as f32 / h);
    let fit = |v: f32, max: u32| ((v * scale).round() as u32).clamp(1, max);
    (fit(w, pw), fit(h, ph))
}

fn apply_opacity(pixmap: &mut Pixmap, opacity: f64) {
    let opacity = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 1.0 };
    if opacity >= 1.0 {
        return;
    }
    let k = opacity as f32;
    for byte in pixmap.data_mut() {
        *byte = (f32::from(*byte) * k).round() as u8;
    }
}

/// `pixmap` as PNG bytes (straight alpha, as ffmpeg's `overlay` reads it), small: for what an
/// agent is shown. A picture of the cache is [`encode_frame_png`].
pub fn encode_png(pixmap: &Pixmap) -> Result<Vec<u8>> {
    pixmap
        .encode_png()
        .map_err(|e| Error::Engine(format!("could not encode an animated title frame: {e}")))
}

/// `pixmap` as the PNG the cache keeps for ffmpeg to read: the same pixels as [`encode_png`], made
/// to be quick rather than small. A sequence is hundreds of these, drawn while Play waits, and the
/// encoder's default level was most of a picture's cost. `Fast` deflate with the `Sub` filter takes
/// a 960 x 540 picture from 13.7 ms to 1.2 ms in a release build, and a picture that is mostly
/// clear stays a few kilobytes. The
/// premultiplied pixels are un-multiplied with `tiny-skia`'s own rule (what `encode_png` does), the
/// clear and opaque ones, nearly every pixel of a title, passing through unchanged.
pub fn encode_frame_png(pixmap: &Pixmap) -> Result<Vec<u8>> {
    let failed = |e: png::EncodingError| Error::Engine(format!("could not encode an animated title frame: {e}"));
    let mut straight = pixmap.data().to_vec();
    for px in straight.as_chunks_mut::<4>().0 {
        // Clear and opaque pixels are the same either way: only the ones between need the rule.
        if matches!(px[3], 0 | 255) {
            continue;
        }
        if let Some(c) = PremultipliedColorU8::from_rgba(px[0], px[1], px[2], px[3]).map(|p| p.demultiply()) {
            px.copy_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, pixmap.width(), pixmap.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        encoder.set_filter(png::Filter::Sub);
        let mut writer = encoder.write_header().map_err(failed)?;
        writer.write_image_data(&straight).map_err(failed)?;
    }
    Ok(out)
}

const SHEET_BACKGROUND: (u8, u8, u8) = (28, 28, 28);
const CELL_BACKGROUND: (u8, u8, u8) = (46, 46, 46);
const SHEET_GAP: u32 = 6;
/// The most moments one sheet shows.
pub const MAX_SHEET_CELLS: usize = 36;
/// The widest sheet, in pixels.
const MAX_SHEET_WIDTH: u32 = 8192;
/// The most pixels a sheet may have (a 16 MP sheet is 64 MB, drawn into and then encoded).
pub const MAX_SHEET_PIXELS: u64 = 16_000_000;

/// A contact sheet of `doc` at the given moments of its own clock (`times`, document seconds): a
/// PNG of a grid of cells over dark grey, each cell the document drawn into a `frame`-shaped
/// picture `cell_w` px wide (fit "contain", so a document with another shape letterboxes inside its
/// cell) with its time written under it. For an agent to *see* an animation without rendering the
/// cut. `cell_w` is clamped to 32..=1920; at most [`MAX_SHEET_CELLS`] moments, [`MAX_SHEET_WIDTH`] px
/// across and [`MAX_SHEET_PIXELS`] in all.
pub fn contact_sheet(doc: &SvgDoc, times: &[f64], cell_w: u32, frame: (u32, u32)) -> Result<Vec<u8>> {
    if times.is_empty() || times.len() > MAX_SHEET_CELLS {
        return Err(invalid(format!("a contact sheet shows 1 to {MAX_SHEET_CELLS} moments")));
    }
    if frame.0 == 0 || frame.1 == 0 {
        return Err(invalid("the frame has no size"));
    }
    let cell_w = cell_w.clamp(32, 1920);
    let cell_h = ((f64::from(cell_w) * f64::from(frame.1) / f64::from(frame.0)).round() as u32).max(1);
    let label_h = (cell_w / 12).clamp(12, 28);
    let mut cols = match times.len() {
        1 => 1,
        2 | 4 => 2,
        _ => 3,
    };
    while cols > 1 && cols as u32 * (cell_w + SHEET_GAP) + SHEET_GAP > MAX_SHEET_WIDTH {
        cols -= 1;
    }
    let rows = times.len().div_ceil(cols);
    let width = cols as u32 * (cell_w + SHEET_GAP) + SHEET_GAP;
    let height = rows as u32 * (cell_h + label_h + SHEET_GAP) + SHEET_GAP;
    if width > MAX_SHEET_WIDTH {
        return Err(invalid("the contact sheet would be too wide"));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_SHEET_PIXELS {
        return Err(invalid(format!(
            "the contact sheet would be {} megapixels (at most {}): ask for smaller cells or fewer moments",
            pixels.div_ceil(1_000_000),
            MAX_SHEET_PIXELS / 1_000_000
        )));
    }
    let mut sheet = Pixmap::new(width, height).ok_or_else(|| Error::Engine("cannot allocate the contact sheet".into()))?;
    let (r, g, b) = SHEET_BACKGROUND;
    sheet.fill(Color::from_rgba8(r, g, b, 255));
    let mut cell = Pixmap::new(cell_w, cell_h).ok_or_else(|| Error::Engine("cannot allocate a contact sheet cell".into()))?;
    for (n, &t) in times.iter().enumerate() {
        let (col, row) = ((n % cols) as u32, (n / cols) as u32);
        let (x, y) = (
            SHEET_GAP + col * (cell_w + SHEET_GAP),
            SHEET_GAP + row * (cell_h + label_h + SHEET_GAP),
        );
        let (r, g, b) = CELL_BACKGROUND;
        cell.fill(Color::from_rgba8(r, g, b, 255));
        let picture = render(&doc.at(t), cell_w, cell_h, 1.0)?;
        cell.draw_pixmap(0, 0, picture.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        sheet.draw_pixmap(
            x as i32,
            y as i32,
            cell.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        // The label is a tiny SVG of its own, so it is set in the same face as the titles.
        let label = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {cell_w} {label_h}\"><text x=\"2\" y=\"{base}\" font-size=\"{size}\" fill=\"#d0d0d0\">{t:.2}s</text></svg>",
            base = f64::from(label_h) * 0.78,
            size = f64::from(label_h) * 0.7,
        );
        if let Ok(label) = render(&label, cell_w, label_h, 1.0) {
            sheet.draw_pixmap(
                x as i32,
                (y + cell_h) as i32,
                label.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
    }
    encode_png(&sheet)
}

// ---- where the pictures live --------------------------------------------------------------------

#[cfg(test)]
thread_local! {
    static PINNED: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with the cache directory at a fixed path that is never created or written, so an argv
/// oracle does not hold this machine's cache directory (see `golden`). Per thread.
#[cfg(test)]
pub(crate) fn with_pinned_dir<R>(dir: &str, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<PathBuf>);
    impl Drop for Reset {
        fn drop(&mut self) {
            PINNED.with(|p| *p.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(PINNED.with(|p| p.borrow_mut().replace(PathBuf::from(dir))));
    f()
}

/// This process's directory: `<cache>/kerf/svg/<pid>`, else `kerf-svg-<uid>/<pid>` in the temp
/// directory (owner-only, as the fonts'). Made on first use and, with it, the directories of
/// processes that are gone removed. Not the place a path is trusted to *exist*: the cache may be
/// cleared while the app runs, so whoever writes makes its own parents.
pub fn root() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = PINNED.with(|p| p.borrow().clone()) {
        return dir;
    }
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(resolve_root).clone()
}

fn resolve_root() -> PathBuf {
    let pid = std::process::id();
    let bases = dirs::cache_dir()
        .map(|d| d.join("kerf").join("svg"))
        .into_iter()
        .chain(std::iter::once_with(|| fonts::private_dir_named(&std::env::temp_dir(), "kerf-svg")).flatten());
    for base in bases {
        let dir = base.join(pid.to_string());
        if claim_dir(&dir) {
            prune_stale(&base, pid);
            return dir;
        }
    }
    unique_private_dir()
}

/// Make `dir` an empty directory that is ours. A pid is reused (after a reboot, or by the next
/// process in a crash loop), so whatever a process of the same number left there is not ours and not
/// trusted: a `complete` marker in it vouches for a run that may have been cut off.
fn claim_dir(dir: &Path) -> bool {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).is_ok()
}

/// A directory of this process alone in the temp one, for when neither the cache directory nor the
/// per-user one can be made: a name nobody can guess, made with a plain `mkdir` (which fails rather
/// than follow a link or reuse a directory that is already there) and mode 0700.
fn unique_private_dir() -> PathBuf {
    let pid = std::process::id();
    let mut dir = PathBuf::new();
    for attempt in 0..8u32 {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        dir = std::env::temp_dir().join(format!("kerf-svg-{pid}-{nanos:x}-{attempt}"));
        if create_private(&dir) {
            break;
        }
    }
    dir
}

fn create_private(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(dir).is_ok()
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(dir).is_ok()
    }
}

/// Remove the sibling directories of `base` named for a process that is gone.
fn prune_stale(base: &Path, own: u32) {
    let Ok(entries) = std::fs::read_dir(base) else { return };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let stale = pid != own
            && if cfg!(unix) {
                !process_alive(pid)
            } else {
                // No cheap liveness check: a directory nothing has written to for a day is stale.
                entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age > std::time::Duration::from_secs(24 * 3600))
            };
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    // SAFETY: signal 0 only checks that the process exists and may be signalled; it sends nothing.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// The cache's budget, in bytes (`KERF_SVG_CACHE_MB`).
pub fn cache_cap() -> u64 {
    std::env::var("KERF_SVG_CACHE_MB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_CACHE_MB)
        .saturating_mul(1 << 20)
}

// ---- pins ---------------------------------------------------------------------------------------

fn pins() -> &'static Mutex<HashMap<PathBuf, usize>> {
    static PINS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
    PINS.get_or_init(Mutex::default)
}

/// A claim on a sequence directory or a still file: while it is held the cache will not remove it.
/// Held by whoever is preparing or reading it (an export, a stream), dropped when ffmpeg is done.
#[derive(Debug)]
#[must_use = "a dropped pin lets the cache remove what it protected"]
pub struct Pin(PathBuf);

impl Pin {
    pub fn new(path: &Path) -> Self {
        *pins()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(path.to_path_buf())
            .or_insert(0) += 1;
        Pin(path.to_path_buf())
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        let mut pins = pins().lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = pins.get_mut(&self.0) {
            *n -= 1;
            if *n == 0 {
                pins.remove(&self.0);
            }
        }
    }
}

#[cfg(test)]
fn is_pinned(path: &Path) -> bool {
    pins().lock().unwrap_or_else(PoisonError::into_inner).contains_key(path)
}

// ---- the files ----------------------------------------------------------------------------------

/// The file name of frame `j`; ffmpeg reads a sequence as `%06d.png`.
pub fn frame_name(j: u64) -> String {
    format!("{j:06}.png")
}

/// Frame `j` of the sequence in `dir`.
pub fn frame_path(dir: &Path, j: u64) -> PathBuf {
    dir.join(frame_name(j))
}

/// Whether the sequence in `dir` was finished.
pub fn is_complete(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

/// Mark the sequence in `dir` finished. The directory must be there: a marker written into one the
/// cache has since lost would make an empty sequence look drawn, so it is an error and the caller
/// draws again.
pub fn mark_complete(dir: &Path) -> Result<()> {
    write_atomic_in(&dir.join(MARKER), b"ok", false)
}

/// Whether every frame of `range` is in `dir`.
pub fn has_frames(dir: &Path, range: std::ops::Range<u64>) -> bool {
    range.into_iter().all(|j| frame_path(dir, j).is_file())
}

/// Count `path` (a sequence directory or a still) as used just now, for the least-recently-used
/// order.
pub fn touch(path: &Path) {
    let now = SystemTime::now();
    let target = if path.is_dir() {
        path.join(MARKER)
    } else {
        path.to_path_buf()
    };
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&target) {
        let _ = file.set_modified(now);
    } else if path.is_dir() {
        // A sequence that is only partly drawn has no marker: its directory stands in for it
        // (`last_used` reads the same). Windows cannot open a directory to stamp it, and goes by
        // the time its last file was added.
        #[cfg(unix)]
        if let Ok(file) = std::fs::File::open(path) {
            let _ = file.set_modified(now);
        }
    }
}

/// `bytes` at `path`, written whole or not at all: under a name of its own, then renamed. Makes the
/// parent directories (the cache may have been cleared since the last call).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_in(path, bytes, true)
}

/// [`write_atomic`], with `make_parents` saying whether a missing directory is made (a frame) or an
/// error (the marker).
fn write_atomic_in(path: &Path, bytes: &[u8], make_parents: bool) -> Result<()> {
    static N: AtomicU64 = AtomicU64::new(0);
    if make_parents {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("frame");
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// `to` as another name for `from`: a hard link, else (a file system without them) a copy, put in
/// place by a rename so that a reader never finds `to` half there. Both give a file of its own
/// that survives the removal of the other.
pub fn link_or_copy(from: &Path, to: &Path) -> Result<()> {
    static N: AtomicU64 = AtomicU64::new(0);
    let name = to.file_name().and_then(|n| n.to_str()).unwrap_or("frame");
    let tmp = to.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&tmp);
    let made = std::fs::hard_link(from, &tmp)
        .or_else(|_| std::fs::copy(from, &tmp).map(|_| ()))
        .and_then(|()| std::fs::rename(&tmp, to));
    if let Err(e) = made {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

// ---- the budget ---------------------------------------------------------------------------------

fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        return std::fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum())
            .unwrap_or(0);
    }
    // A hard link is the same bytes under another name: each of n names counts 1/n of the file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.len() / meta.nlink().max(1)
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

fn last_used(path: &Path) -> SystemTime {
    let stamp = if path.is_dir() {
        path.join(MARKER)
    } else {
        path.to_path_buf()
    };
    std::fs::metadata(&stamp)
        .or_else(|_| std::fs::metadata(path))
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Remove the least recently used entries of `dir` (sequence directories and `still-*.png` files)
/// until what is left fits in `cap` bytes, sparing every pinned entry and anything `also_pinned`
/// says is in use. Returns the bytes left.
pub fn trim_to(dir: &Path, cap: u64, also_pinned: &dyn Fn(&Path) -> bool) -> u64 {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let mut entries: Vec<(PathBuf, u64, SystemTime)> = read
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            if name.starts_with(TOMBSTONE) {
                return None;
            }
            let is_still = name.starts_with("still-") && name.ends_with(".png");
            (is_still || e.path().is_dir()).then(|| {
                let path = e.path();
                let (size, used) = (size_of(&path), last_used(&path));
                (path, size, used)
            })
        })
        .collect();
    let mut total: u64 = entries.iter().map(|(_, size, _)| size).sum();
    entries.sort_by_key(|(_, _, used)| *used);
    for (path, size, _) in entries {
        if total <= cap {
            break;
        }
        if evict(&path, also_pinned) {
            total = total.saturating_sub(size);
        }
    }
    total
}

/// What an entry being removed is renamed to first (and skipped by [`trim_to`]).
const TOMBSTONE: &str = ".gone-";

/// Remove `path` unless a [`Pin`] (or `also_pinned`) holds it. The pin check and taking the entry
/// out of the namespace happen under the pins' lock, so a [`Pin::new`] cannot slip between them: a
/// preparer either pinned first and the entry stays, or pins after and finds it gone (and draws it
/// again) — never a "complete" sequence whose frames vanish under ffmpeg.
///
/// An entry that will not be renamed (a handle open on it on Windows) is **left alone**, whole and
/// still finished, and tried again at the next trim: the only other way to empty it is to delete its
/// files, which has to happen where nobody can pin it — under the lock, and so with every pin and
/// trim waiting on a directory's worth of unlinks — or after the lock, where a preparer that pins it
/// meanwhile finds a marker and a first and last picture over frames that are being deleted.
fn evict(path: &Path, also_pinned: &dyn Fn(&Path) -> bool) -> bool {
    evict_by(path, also_pinned, &|from, to| std::fs::rename(from, to))
}

/// [`evict`], with the rename given (a test makes it fail, as a handle open on Windows does).
fn evict_by(path: &Path, also_pinned: &dyn Fn(&Path) -> bool, rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>) -> bool {
    static N: AtomicU64 = AtomicU64::new(0);
    let tomb = path.with_file_name(format!(
        "{TOMBSTONE}{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let held = pins().lock().unwrap_or_else(PoisonError::into_inner);
        if held.contains_key(path) || also_pinned(path) {
            return false;
        }
        if rename(path, &tomb).is_err() {
            return false;
        }
    }
    // Nothing can reach the tombstone: it has a name nobody asks for.
    let gone = if tomb.is_dir() {
        std::fs::remove_dir_all(&tomb)
    } else {
        std::fs::remove_file(&tomb)
    };
    gone.is_ok()
}

/// Hold this process's directory to [`cache_cap`], sparing every pinned entry.
pub fn trim() {
    #[cfg(test)]
    if PINNED.with(|p| p.borrow().is_some()) {
        return;
    }
    trim_to(&root(), cache_cap(), &|_| false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svg::SvgDoc;

    fn svg(inner: &str) -> String {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 50\">{inner}</svg>")
    }

    fn pixel(p: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let c = p.pixel(x, y).expect("inside the picture");
        [c.red(), c.green(), c.blue(), c.alpha()]
    }

    #[test]
    fn a_document_is_fit_contain_and_centred_in_its_rectangle() {
        // 2:1 into a square: the document fills the width and the middle half of the height.
        let p = render(&svg("<rect width=\"100\" height=\"50\" fill=\"#ff0000\"/>"), 200, 200, 1.0).unwrap();
        assert_eq!((p.width(), p.height()), (200, 200));
        assert_eq!(pixel(&p, 100, 100), [255, 0, 0, 255]);
        assert_eq!(pixel(&p, 100, 20)[3], 0, "the bar above is empty");
        assert_eq!(pixel(&p, 100, 180)[3], 0, "and the bar below");
        assert_eq!(pixel(&p, 2, 100), [255, 0, 0, 255], "the width is filled");
        assert_eq!(pixel(&p, 100, 49)[3], 0, "the top bar ends at y = 50");
        assert_eq!(pixel(&p, 100, 51), [255, 0, 0, 255], "and the document starts there");
    }

    #[test]
    fn what_is_outside_the_viewbox_is_not_drawn_in_the_margin() {
        let p = render(
            &svg("<rect x=\"-500\" y=\"-500\" width=\"2000\" height=\"2000\" fill=\"#00ff00\"/>"),
            200,
            200,
            1.0,
        )
        .unwrap();
        assert_eq!(pixel(&p, 100, 100), [0, 255, 0, 255]);
        assert_eq!(
            pixel(&p, 100, 10)[3],
            0,
            "the viewport is clipped to the document, not the rectangle"
        );
        assert_eq!(pixel(&p, 100, 190)[3], 0);
    }

    #[test]
    fn opacity_multiplies_every_channel() {
        let doc = svg("<rect width=\"100\" height=\"50\" fill=\"#ff0000\"/>");
        let half = render(&doc, 100, 50, 0.5).unwrap();
        let [r, g, b, a] = pixel(&half, 50, 25);
        assert!((127..=128).contains(&a), "alpha {a}");
        assert!(
            (127..=128).contains(&r) && g == 0 && b == 0,
            "premultiplied, so red scales too: {r} {g} {b}"
        );
        assert_eq!(pixel(&render(&doc, 100, 50, 0.0).unwrap(), 50, 25), [0, 0, 0, 0]);
        assert_eq!(
            pixel(&render(&doc, 100, 50, 7.0).unwrap(), 50, 25),
            [255, 0, 0, 255],
            "clamped"
        );
        assert_eq!(pixel(&render(&doc, 100, 50, f64::NAN).unwrap(), 50, 25), [255, 0, 0, 255]);
    }

    #[test]
    fn text_is_drawn_in_the_shipped_face_even_for_a_family_nobody_has() {
        let named = render(
            &svg("<text x=\"4\" y=\"36\" font-size=\"40\" font-family=\"No Such Family 9\" fill=\"#fff\">Hi</text>"),
            400,
            200,
            1.0,
        )
        .unwrap();
        let plain = render(
            &svg("<text x=\"4\" y=\"36\" font-size=\"40\" fill=\"#fff\">Hi</text>"),
            400,
            200,
            1.0,
        )
        .unwrap();
        let ink = |p: &Pixmap| p.pixels().iter().filter(|c| c.alpha() > 0).count();
        assert!(ink(&named) > 200, "text appeared: {}", ink(&named));
        assert!(ink(&plain) > 200);
        assert_eq!(named.data(), plain.data(), "an unknown family and none are the same face");
    }

    #[test]
    fn an_image_that_is_not_in_the_document_is_not_read() {
        let path = std::env::temp_dir().join(format!("kerf-svg-secret-{}.png", std::process::id()));
        let red = Pixmap::new(8, 8).map(|mut p| {
            p.fill(Color::from_rgba8(255, 0, 0, 255));
            p.encode_png().unwrap()
        });
        std::fs::write(&path, red.unwrap()).unwrap();
        for href in [
            path.to_string_lossy().into_owned(),
            format!("file://{}", path.display()),
            "http://127.0.0.1:9/x.png".into(),
        ] {
            let p = render(
                &svg(&format!("<image href=\"{href}\" width=\"100\" height=\"50\"/>")),
                100,
                50,
                1.0,
            )
            .unwrap();
            assert!(p.pixels().iter().all(|c| c.alpha() == 0), "{href}");
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_image_the_document_carries_is_drawn() {
        // A 4 x 4 red PNG, base64: a logo in a lower third is this, larger.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAACp8Z5+AAAAEklEQVR4nGP4z8DwHxkzkC4AADxAH+HggXe0AAAAAElFTkSuQmCC";
        let src = svg(&format!(
            "<image href=\"data:image/png;base64,{png}\" x=\"0\" y=\"0\" width=\"50\" height=\"50\"/>"
        ));
        // Through the sanitiser, as a stored title goes.
        let doc = SvgDoc::parse(&src).unwrap();
        assert!(doc.sanitized().contains("data:image/png"), "the sanitiser keeps it");
        let p = render(&doc.at(0.0), 100, 50, 1.0).unwrap();
        let [r, g, b, a] = pixel(&p, 20, 25);
        assert!(r > 240 && g < 15 && b < 15 && a > 240, "{r} {g} {b} {a}");
        assert_eq!(pixel(&p, 80, 25)[3], 0, "and only where it was put");
    }

    #[test]
    fn a_nested_svg_in_a_data_url_is_not_drawn() {
        let nested = "data:image/svg+xml;utf8,&lt;svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 4 4'&gt;&lt;rect width='4' height='4' fill='red'/&gt;&lt;/svg&gt;";
        let p = render(
            &svg(&format!("<image href=\"{nested}\" width=\"100\" height=\"50\"/>")),
            100,
            50,
            1.0,
        )
        .unwrap();
        assert!(p.pixels().iter().all(|c| c.alpha() == 0));
    }

    #[test]
    fn an_image_whose_header_claims_more_than_a_title_may_draw_is_not_decoded() {
        // 60,000 x 60,000 pixels is 14 GB of RGBA: the decoder would ask for all of it and abort the
        // process. The resolver reads the header first and draws nothing, whatever the document says.
        let huge = images::tests::data_url("image/png", &images::tests::png_claiming(60_000, 60_000));
        let p = render(
            &svg(&format!("<image href=\"{huge}\" width=\"100\" height=\"50\"/>")),
            100,
            50,
            1.0,
        )
        .unwrap();
        assert!(p.pixels().iter().all(|c| c.alpha() == 0));
        // The same for the other formats, and for bytes that are not the image the mime type says.
        for mime in ["image/jpeg", "image/gif", "image/webp", "image/png"] {
            let href = images::tests::data_url(mime, b"RIFF....WEBPVP8L garbage");
            let p = render(
                &svg(&format!("<image href=\"{href}\" width=\"100\" height=\"50\"/>")),
                100,
                50,
                1.0,
            )
            .unwrap();
            assert!(p.pixels().iter().all(|c| c.alpha() == 0), "{mime}");
        }
    }

    #[test]
    fn a_filter_too_slow_to_draw_is_an_error_at_the_size_it_is_drawn_at() {
        let doc = svg("<filter id=\"f\" x=\"0\" y=\"0\" width=\"1\" height=\"1\"><feMorphology radius=\"1\"/></filter><rect width=\"100\" height=\"50\" filter=\"url(#f)\"/>");
        // Small, a radius of 1 unit is a few pixels: fine. At 4K it is a 100 px window.
        assert!(render(&doc, 100, 50, 1.0).is_ok());
        let e = render(&doc, 3840, 1920, 1.0).unwrap_err().to_string();
        assert!(e.contains("feMorphology"), "{e}");
    }

    fn svg_with(attrs: &str, inner: &str) -> String {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\" {attrs}>{inner}</svg>")
    }

    const MORPH: &str = "<filter id=\"f\" x=\"0\" y=\"0\" width=\"1\" height=\"1\"><feMorphology radius=\"1\"/></filter>";

    #[test]
    fn a_documents_own_scale_is_part_of_what_a_filter_costs() {
        // A radius of 1 unit in a 96 x 54 document drawn at 1920 x 1080 is 20 px, whether the scale
        // is the caller's (viewBox alone, or width and height alone) or usvg's own (a viewBox in a
        // 1920 x 1080 document is a transform on the root group).
        let rect = "<rect width=\"96\" height=\"54\" filter=\"url(#f)\"/>";
        for attrs in [
            "viewBox=\"0 0 96 54\"",
            "width=\"96\" height=\"54\"",
            "width=\"1920\" height=\"1080\" viewBox=\"0 0 96 54\"",
        ] {
            let doc = svg_with(attrs, &format!("{MORPH}{rect}"));
            let e = render(&doc, 1920, 1080, 1.0).unwrap_err().to_string();
            assert!(e.contains("feMorphology"), "{attrs}: {e}");
            assert!(SvgDoc::parse(&doc).is_err(), "{attrs}: refused when the document is read");
            assert!(render(&doc, 96, 54, 1.0).is_ok(), "{attrs}: and fine at its own size");
        }
    }

    #[test]
    fn a_group_scaled_up_round_a_tiny_filtered_rect_is_refused() {
        // Nothing in the rect, the filter or the radius is large; the group's scale(1000) makes the
        // region as large as resvg will take (five canvases a side) and the radius 19 px.
        let doc = svg(
            "<g transform=\"scale(1000)\"><filter id=\"f\" x=\"-100\" y=\"-100\" width=\"300\" height=\"300\"><feMorphology radius=\"0.001\"/></filter><rect width=\"0.01\" height=\"0.01\" filter=\"url(#f)\"/></g>",
        );
        assert!(render(&doc, 1920, 1080, 1.0).is_err());
        assert!(SvgDoc::parse(&doc).is_err());
        // The same without the scale is a speck.
        assert!(render(&doc.replace("scale(1000)", "scale(1)"), 1920, 1080, 1.0).is_ok());
    }

    #[test]
    fn a_title_that_grows_into_a_heavy_filter_is_refused_at_the_frame_where_it_does() {
        // The stored document is cheap (its group has no transform), so reading it passes; the
        // animation scales the filtered group to 40x, region and radius with it.
        let doc = SvgDoc::parse(&svg(
            "<filter id=\"f\" x=\"0\" y=\"0\" width=\"1\" height=\"1\"><feMorphology radius=\"0.05\"/></filter><g filter=\"url(#f)\"><rect width=\"10\" height=\"5\"/><animateTransform attributeName=\"transform\" type=\"scale\" from=\"1\" to=\"40\" dur=\"2s\" fill=\"freeze\"/></g>",
        ))
        .unwrap();
        assert!(render(&doc.at(0.0), 1920, 960, 1.0).is_ok());
        let e = render(&doc.at(2.0), 1920, 960, 1.0).unwrap_err().to_string();
        assert!(e.contains("feMorphology"), "{e}");
    }

    #[test]
    fn a_picture_too_large_to_hold_is_refused() {
        assert!(render(&svg(""), 10_000, 10_000, 1.0).is_err());
        assert!(render("<not svg", 10, 10, 1.0).is_err());
    }

    #[test]
    fn png_bytes_decode_to_the_same_picture() {
        let p = render(
            &svg("<rect width=\"50\" height=\"50\" fill=\"#3366ff\" fill-opacity=\"0.5\"/>"),
            100,
            50,
            1.0,
        )
        .unwrap();
        let back = Pixmap::decode_png(&encode_png(&p).unwrap()).unwrap();
        assert_eq!((back.width(), back.height()), (100, 50));
        for (x, y) in [(10, 10), (60, 10), (99, 49)] {
            let (a, b) = (pixel(&p, x, y), pixel(&back, x, y));
            for c in 0..4 {
                assert!(a[c].abs_diff(b[c]) <= 2, "{x},{y}: {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn the_quick_png_holds_the_pixels_the_small_one_does() {
        // Clear, opaque and every partial alpha, with gradients, edges and a soft shadow: the
        // un-multiplying must be the library's own for each of them.
        let p = render(
            &svg(
                "<defs><linearGradient id=\"g\"><stop offset=\"0\" stop-color=\"#ff3355\" stop-opacity=\"0.1\"/><stop offset=\"1\" stop-color=\"#33ccff\"/></linearGradient>\
                 <filter id=\"b\"><feGaussianBlur stdDeviation=\"3\"/></filter></defs>\
                 <rect x=\"10\" y=\"10\" width=\"90\" height=\"60\" rx=\"12\" fill=\"url(#g)\" filter=\"url(#b)\"/>\
                 <circle cx=\"140\" cy=\"60\" r=\"30\" fill=\"#ffffff\" fill-opacity=\"0.37\"/>",
            ),
            192,
            108,
            0.8,
        )
        .unwrap();
        let alphas: std::collections::BTreeSet<u8> = p.pixels().iter().map(|c| c.alpha()).collect();
        assert!(alphas.contains(&0) && alphas.len() > 20, "{} distinct alphas", alphas.len());
        let (quick, small) = (encode_frame_png(&p).unwrap(), encode_png(&p).unwrap());
        let (quick, small) = (Pixmap::decode_png(&quick).unwrap(), Pixmap::decode_png(&small).unwrap());
        assert_eq!((quick.width(), quick.height()), (192, 108));
        assert_eq!(quick.data(), small.data());
    }

    #[test]
    fn a_contact_sheet_shows_each_moment_on_dark_grey() {
        let doc = SvgDoc::parse(&svg(
            "<rect width=\"100\" height=\"50\" fill=\"#ff0000\"><animate attributeName=\"opacity\" from=\"0\" to=\"1\" dur=\"2s\" fill=\"freeze\"/></rect>",
        ))
        .unwrap();
        let png = contact_sheet(&doc, &[0.0, 1.0, 2.0, 3.0], 160, (1920, 1080)).unwrap();
        let sheet = Pixmap::decode_png(&png).unwrap();
        // Two columns, two rows of 160 x 90 cells with a 6 px gap and a label under each.
        assert_eq!(sheet.width(), 2 * (160 + 6) + 6);
        assert!(sheet.height() > 2 * (90 + 6));
        assert_eq!(pixel(&sheet, 1, 1), [28, 28, 28, 255], "the sheet is dark grey");
        let cell = |n: u32| {
            let (col, row) = (n % 2, n / 2);
            pixel(&sheet, 6 + col * 166 + 80, 6 + row * (90 + 6 + 13) + 45)
        };
        // The document is 2:1 in a 16:9 cell, so its middle is red where it has faded in.
        assert_eq!(cell(0), [46, 46, 46, 255], "at 0 s it is invisible: the cell shows");
        assert!(cell(1)[0] > cell(0)[0] + 40, "at 1 s it is half there: {:?}", cell(1));
        assert_eq!(cell(2), [255, 0, 0, 255], "done at 2 s");
        assert_eq!(cell(3), [255, 0, 0, 255]);
        assert!(contact_sheet(&doc, &[], 160, (1920, 1080)).is_err());
        assert!(contact_sheet(&doc, &[0.0; 37], 160, (1920, 1080)).is_err());
        assert!(contact_sheet(&doc, &[0.0], 160, (0, 1080)).is_err());
    }

    #[test]
    fn a_contact_sheet_is_held_to_a_pixel_budget() {
        let doc = SvgDoc::parse(&svg("<rect width=\"100\" height=\"50\" fill=\"#ff0000\"/>")).unwrap();
        // 36 cells of 1920 x 1080 would be a 100-megapixel picture.
        let e = contact_sheet(&doc, &[0.0; 36], 1920, (1920, 1080)).unwrap_err().to_string();
        assert!(e.contains("megapixels") && e.contains("smaller cells"), "{e}");
        // Six at a size an agent can read are fine.
        assert!(contact_sheet(&doc, &[0.0; 6], 480, (1920, 1080)).is_ok());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-svg-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sequence(root: &Path, key: &str, bytes: usize, age_secs: u64) -> PathBuf {
        let dir = root.join(key);
        write_atomic(&frame_path(&dir, 0), &vec![7u8; bytes]).unwrap();
        mark_complete(&dir).unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(age_secs);
        std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join(MARKER))
            .unwrap()
            .set_modified(old)
            .unwrap();
        dir
    }

    #[test]
    fn the_least_recently_used_sequence_goes_first_and_a_pinned_one_never() {
        let root = scratch("lru");
        let oldest = sequence(&root, "aa", 1000, 300);
        let middle = sequence(&root, "bb", 1000, 200);
        let newest = sequence(&root, "cc", 1000, 100);
        let still = root.join("still-dd.png");
        write_atomic(&still, &[1u8; 500]).unwrap();
        let kept = trim_to(&root, 2600, &|_| false);
        assert!(!oldest.exists() && middle.exists() && newest.exists() && still.exists());
        assert!((2500..=2600).contains(&kept), "{kept}");

        // The oldest is pinned: the next one in line goes instead.
        let oldest = sequence(&root, "aa", 1000, 300);
        let pin = Pin::new(&oldest);
        trim_to(&root, 2600, &|_| false);
        assert!(oldest.exists() && !middle.exists() && newest.exists());
        drop(pin);
        assert!(!is_pinned(&oldest));

        // Touching a sequence makes it the newest.
        touch(&oldest);
        let extra = sequence(&root, "ee", 1000, 50);
        trim_to(&root, 2600, &|_| false);
        assert!(oldest.exists() && extra.exists() && !newest.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_marker_is_never_written_into_a_directory_that_is_gone() {
        let root = scratch("marker");
        let dir = root.join("cc");
        write_atomic(&frame_path(&dir, 0), &[1u8; 10]).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        // The cache lost it while it was being drawn: finishing it is an error, not an empty sequence
        // that reads as complete.
        assert!(mark_complete(&dir).is_err());
        assert!(!dir.exists() && !is_complete(&dir));
        write_atomic(&frame_path(&dir, 0), &[1u8; 10]).unwrap();
        mark_complete(&dir).unwrap();
        assert!(is_complete(&dir) && has_frames(&dir, 0..1) && !has_frames(&dir, 0..2));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_evicted_sequence_is_gone_whole_and_leaves_no_trace_and_a_pinned_one_is_not_touched() {
        let root = scratch("evict");
        let kept = sequence(&root, "aa", 1000, 300);
        let dropped = sequence(&root, "bb", 1000, 200);
        let pin = Pin::new(&kept);
        let left = trim_to(&root, 0, &|_| false);
        assert!(kept.exists() && is_complete(&kept) && has_frames(&kept, 0..1));
        assert!(!dropped.exists());
        assert!((900..=1100).contains(&left), "{left}");
        let names: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1, "no tombstone is left behind: {names:?}");
        // Once the pin is gone it is the next to go; a pin taken after finds nothing and has to draw.
        drop(pin);
        trim_to(&root, 0, &|_| false);
        assert!(!kept.exists());
        let late = Pin::new(&kept);
        assert!(!is_complete(&kept), "a pin taken after the eviction sees an empty place");
        drop(late);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_entry_that_will_not_be_renamed_is_left_whole_not_emptied_behind_a_pin() {
        let root = scratch("no-rename");
        let stuck = sequence(&root, "aa", 1000, 300);
        let fails = |_: &Path, _: &Path| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(!evict_by(&stuck, &|_| false, &fails));
        // Nothing of it was deleted, marker included: a preparer that pins it now finds what it
        // found before, finished, and ffmpeg reads frames that are all there.
        assert!(is_complete(&stuck) && has_frames(&stuck, 0..1));
        let names: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1, "and no tombstone: {names:?}");
        // The next trim, with a file system that renames, takes it.
        assert!(evict(&stuck, &|_| false));
        assert!(!stuck.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_pin_taken_while_the_cache_trims_is_never_lost_to_it() {
        // The check and the removal are one step against Pin::new: hammer both and require that
        // whenever a pin was held across the trim, the sequence it covered was still whole.
        let root = scratch("race");
        let stop = std::sync::atomic::AtomicBool::new(false);
        let broken = std::sync::atomic::AtomicU64::new(0);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    trim_to(&root, 0, &|_| false);
                }
            });
            for t in 0..2 {
                let (root, stop, broken) = (&root, &stop, &broken);
                scope.spawn(move || {
                    let dir = root.join(format!("seq{t}"));
                    for _ in 0..300 {
                        let pin = Pin::new(&dir);
                        if !is_complete(&dir) {
                            let _ = write_atomic(&frame_path(&dir, 0), &[1u8; 64]);
                            let _ = mark_complete(&dir);
                        }
                        // While the pin is held a finished sequence has its frame.
                        if is_complete(&dir) && !frame_path(&dir, 0).is_file() {
                            broken.fetch_add(1, Ordering::Relaxed);
                        }
                        drop(pin);
                    }
                    stop.store(true, Ordering::Relaxed);
                });
            }
        });
        assert_eq!(broken.load(Ordering::Relaxed), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_directory_left_by_an_earlier_process_with_this_pid_is_emptied() {
        let base = scratch("claim");
        let dir = base.join(std::process::id().to_string());
        write_atomic(&frame_path(&dir.join("old"), 0), b"stale").unwrap();
        mark_complete(&dir.join("old")).unwrap();
        assert!(claim_dir(&dir));
        assert!(dir.is_dir() && std::fs::read_dir(&dir).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn the_last_resort_directory_is_private_unique_and_never_a_name_somebody_made_first() {
        use std::os::unix::fs::PermissionsExt;
        let a = unique_private_dir();
        let b = unique_private_dir();
        assert_ne!(a, b);
        for dir in [&a, &b] {
            let mode = std::fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{dir:?}");
        }
        // A directory that is already there is refused (a plain mkdir), not reused.
        assert!(!create_private(&a));
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn hard_links_are_one_file_in_the_budget() {
        let root = scratch("links");
        let dir = root.join("ff");
        write_atomic(&frame_path(&dir, 0), &[3u8; 4000]).unwrap();
        for j in 1..4 {
            link_or_copy(&frame_path(&dir, 0), &frame_path(&dir, j)).unwrap();
        }
        mark_complete(&dir).unwrap();
        let size = size_of(&dir);
        if cfg!(unix) {
            assert!(size < 4100, "four names for 4000 bytes counted as {size}");
        }
        assert_eq!(std::fs::read(frame_path(&dir, 3)).unwrap(), vec![3u8; 4000]);
        std::fs::remove_file(frame_path(&dir, 0)).unwrap();
        assert_eq!(
            std::fs::read(frame_path(&dir, 3)).unwrap().len(),
            4000,
            "a link outlives the name it came from"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_frame_is_written_whole_or_not_at_all() {
        let root = scratch("atomic");
        let path = root.join("deep").join("000000.png");
        write_atomic(&path, b"abc").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no temporary file is left"
        );
        assert!(
            write_atomic(&path.join("child"), b"x").is_err(),
            "a child of a file cannot be made"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn only_directories_of_processes_that_are_gone_are_pruned() {
        let base = scratch("prune");
        let ours = std::process::id();
        // Far above any pid_max: never a live process.
        let gone = 4_000_000_000u32;
        for pid in [ours, gone] {
            std::fs::create_dir_all(base.join(pid.to_string()).join("key")).unwrap();
        }
        std::fs::create_dir_all(base.join("not-a-pid")).unwrap();
        // Our parent is certainly alive and is not ours.
        let parent = std::os::unix::process::parent_id();
        std::fs::create_dir_all(base.join(parent.to_string())).unwrap();
        prune_stale(&base, ours);
        assert!(base.join(ours.to_string()).exists());
        assert!(!base.join(gone.to_string()).exists());
        assert!(base.join(parent.to_string()).exists(), "a live process's directory stays");
        assert!(
            base.join("not-a-pid").exists(),
            "and anything that is not a pid is left alone"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn the_cache_cap_reads_the_environment() {
        // Not set here (a test must not set process-wide variables): the default is a gibibyte.
        if std::env::var_os("KERF_SVG_CACHE_MB").is_none() {
            assert_eq!(cache_cap(), 1 << 30);
        }
    }

    #[test]
    fn a_pinned_directory_is_a_pure_path() {
        with_pinned_dir("/kerf-test/svg", || {
            assert_eq!(root(), PathBuf::from("/kerf-test/svg"));
            trim();
            with_pinned_dir("/kerf-test/other", || assert_eq!(root(), PathBuf::from("/kerf-test/other")));
            assert_eq!(root(), PathBuf::from("/kerf-test/svg"), "the outer pin comes back");
        });
        assert!(!PINNED.with(|p| p.borrow().is_some()));
    }
}
