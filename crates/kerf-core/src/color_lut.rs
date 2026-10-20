//! `.cube` 3D LUTs: parsed and validated here, and handed to FFmpeg's `lut3d` as a file Kerf wrote.
//!
//! **FFmpeg never reads the user's file.** Its `.cube` reader is a subset with quirks (a
//! `DOMAIN_MIN` before `LUT_3D_SIZE` is ignored, a non-zero domain minimum is ignored, a 1D shaper
//! or an unknown keyword is a parse error, and a file with bare CR line ends is one line), so the
//! file is parsed and checked here, strictly, and the table is written again — `LUT_3D_SIZE` and the
//! rows, nothing else — to `<cache>/kerf/luts/<digest>.cube` (else a private per-user directory in
//! the temp one), which is what the graph names. The intensity is baked into that copy:
//! `k * lut + (1 - k) * identity` at every grid point is exact under tetrahedral interpolation (the
//! identity is affine, and an affine table interpolates to itself), so a LUT at 40% *is* 40% of
//! the way from the picture to the looked-up picture.
//!
//! Accepted: `TITLE`, `LUT_3D_SIZE` 2 to [`MAX_LUT_SIZE`], `DOMAIN_MIN 0 0 0` / `DOMAIN_MAX 1 1 1`
//! and `LUT_3D_INPUT_RANGE 0 1` (any other domain is refused: resampling would not be exact), `#`
//! comments, and exactly `size³` rows of three finite numbers, red changing fastest. The file is
//! read only up to [`MAX_CUBE_BYTES`] and the table is allocated once the size is known, so a
//! hostile file cannot make the parser allocate more than the largest LUT it accepts.
//!
//! **A missing or broken file at render time is skipped** (with a warning in the log), as a missing
//! font falls back to the default: the preview keeps playing, and the Inspector says the file is
//! gone ([`inspect_lut`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{ClipLut, Grade};

/// The largest `LUT_3D_SIZE` accepted (65³ rows, 3.3 MB of table).
pub const MAX_LUT_SIZE: usize = 65;
/// The most of a `.cube` file that is read.
pub const MAX_CUBE_BYTES: u64 = 32 * 1024 * 1024;

/// A 3D table, `size³` entries, red fastest.
#[derive(Debug, Clone, PartialEq)]
pub struct Lut3d {
    pub title: String,
    pub size: usize,
    pub data: Vec<[f32; 3]>,
}

/// What the Inspector shows about a LUT file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LutInfo {
    pub path: String,
    pub size: usize,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

fn bad(line: usize, msg: impl std::fmt::Display) -> Error {
    Error::InvalidArgument(format!("not a usable .cube LUT (line {line}): {msg}"))
}

fn numbers(rest: &str, line: usize) -> Result<Vec<f32>> {
    rest.split_whitespace()
        .map(|t| {
            t.parse::<f32>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| bad(line, "a value is not a finite number"))
        })
        .collect()
}

impl Lut3d {
    pub fn identity(size: usize) -> Self {
        let n = (size - 1) as f32;
        let mut data = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push([r as f32 / n, g as f32 / n, b as f32 / n]);
                }
            }
        }
        Self {
            title: String::new(),
            size,
            data,
        }
    }

    /// Parse a `.cube` holding one 3D table (see the module docs for what is accepted).
    pub fn parse_cube(text: &str) -> Result<Self> {
        let mut title = String::new();
        let mut size = 0usize;
        let mut data: Vec<[f32; 3]> = Vec::new();
        for (n, raw) in text.split(['\n', '\r']).enumerate() {
            let line = n + 1;
            let trimmed = raw.trim();
            if let Some(rest) = trimmed.strip_prefix("TITLE") {
                if !data.is_empty() {
                    return Err(bad(line, "TITLE after the table"));
                }
                title = rest
                    .trim()
                    .trim_matches('"')
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect();
                continue;
            }
            let content = trimmed.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let first = content.split_whitespace().next().unwrap_or("");
            if first.starts_with(|c: char| c.is_ascii_alphabetic()) {
                if !data.is_empty() {
                    return Err(bad(line, "a keyword after the table"));
                }
                let rest = &content[first.len()..];
                match first {
                    "LUT_3D_SIZE" => {
                        let v: usize = rest
                            .trim()
                            .parse()
                            .map_err(|_| bad(line, "LUT_3D_SIZE is not a whole number"))?;
                        if !(2..=MAX_LUT_SIZE).contains(&v) {
                            return Err(bad(line, format!("LUT_3D_SIZE {v} is outside 2 to {MAX_LUT_SIZE}")));
                        }
                        size = v;
                    }
                    "DOMAIN_MIN" | "DOMAIN_MAX" => {
                        let want = if first == "DOMAIN_MIN" { 0.0 } else { 1.0 };
                        let v = numbers(rest, line)?;
                        if v.len() != 3 || v.iter().any(|&x| x != want) {
                            return Err(bad(line, format!("{first} must be {want} {want} {want}")));
                        }
                    }
                    "LUT_3D_INPUT_RANGE" => {
                        if numbers(rest, line)? != [0.0, 1.0] {
                            return Err(bad(line, "LUT_3D_INPUT_RANGE must be 0 1"));
                        }
                    }
                    "LUT_1D_SIZE" | "LUT_1D_INPUT_RANGE" => {
                        return Err(bad(line, "a 1D table or shaper is not supported, only a 3D LUT"));
                    }
                    _ => return Err(bad(line, "an unsupported keyword")),
                }
                continue;
            }
            if size == 0 {
                return Err(bad(line, "table data before LUT_3D_SIZE"));
            }
            let total = size * size * size;
            if data.is_empty() {
                data.reserve_exact(total);
            }
            if data.len() == total {
                return Err(bad(line, format!("more than the {total} rows of a {size}³ table")));
            }
            let v = numbers(content, line)?;
            let [r, g, b] = v[..] else {
                return Err(bad(line, "a row needs three numbers"));
            };
            data.push([r, g, b]);
        }
        if size == 0 {
            return Err(Error::InvalidArgument("not a usable .cube LUT: no LUT_3D_SIZE".to_string()));
        }
        let total = size * size * size;
        if data.len() != total {
            return Err(Error::InvalidArgument(format!(
                "not a usable .cube LUT: {} rows where a {size}³ table has {total}",
                data.len()
            )));
        }
        Ok(Self { title, size, data })
    }

    /// Read and parse a `.cube` file, refusing one larger than [`MAX_CUBE_BYTES`].
    pub fn read(path: &Path) -> Result<Self> {
        use std::io::Read;
        let file =
            std::fs::File::open(path).map_err(|e| Error::InvalidArgument(format!("cannot open LUT {}: {e}", path.display())))?;
        let mut bytes = Vec::new();
        file.take(MAX_CUBE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::InvalidArgument(format!("cannot read LUT {}: {e}", path.display())))?;
        if bytes.len() as u64 > MAX_CUBE_BYTES {
            return Err(Error::InvalidArgument(format!(
                "LUT {} is larger than {} MB",
                path.display(),
                MAX_CUBE_BYTES >> 20
            )));
        }
        Self::parse_cube(&String::from_utf8_lossy(&bytes))
    }

    pub fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[r + g * self.size + b * self.size * self.size]
    }

    /// Tetrahedral interpolation of `rgb` in `0..=1`.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        let coord = |v: f32| v.clamp(0.0, 1.0) * (n - 1) as f32;
        let (x, y, z) = (coord(rgb[0]), coord(rgb[1]), coord(rgb[2]));
        let (r0, g0, b0) = ((x as usize).min(n - 2), (y as usize).min(n - 2), (z as usize).min(n - 2));
        let (fr, fg, fb) = (x - r0 as f32, y - g0 as f32, z - b0 as f32);
        let c = |dr: usize, dg: usize, db: usize| self.at(r0 + dr, g0 + dg, b0 + db);
        let (c000, c111) = (c(0, 0, 0), c(1, 1, 1));
        let (ca, cb, w) = if fr > fg {
            if fg > fb {
                (c(1, 0, 0), c(1, 1, 0), [1.0 - fr, fr - fg, fg - fb, fb])
            } else if fr > fb {
                (c(1, 0, 0), c(1, 0, 1), [1.0 - fr, fr - fb, fb - fg, fg])
            } else {
                (c(0, 0, 1), c(1, 0, 1), [1.0 - fb, fb - fr, fr - fg, fg])
            }
        } else if fb > fg {
            (c(0, 0, 1), c(0, 1, 1), [1.0 - fb, fb - fg, fg - fr, fr])
        } else if fb > fr {
            (c(0, 1, 0), c(0, 1, 1), [1.0 - fg, fg - fb, fb - fr, fr])
        } else {
            (c(0, 1, 0), c(1, 1, 0), [1.0 - fg, fg - fr, fr - fb, fb])
        };
        let mut o = [0.0; 3];
        for (k, v) in o.iter_mut().enumerate() {
            *v = w[0] * c000[k] + w[1] * ca[k] + w[2] * cb[k] + w[3] * c111[k];
        }
        o
    }

    /// The table `intensity` of the way from the identity to this one: the values the `.cube` copy holds (and
    /// that `lut3d` reads back, each number the shortest text of its `f32`), red fastest.
    pub fn at_intensity(&self, intensity: f64) -> Vec<[f32; 3]> {
        let n = (self.size - 1) as f64;
        let k = intensity.clamp(0.0, 1.0);
        self.data
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let grid = [
                    (i % self.size) as f64 / n,
                    (i / self.size % self.size) as f64 / n,
                    (i / (self.size * self.size)) as f64 / n,
                ];
                let mix = |c: usize| {
                    if k == 1.0 {
                        v[c]
                    } else {
                        (k * f64::from(v[c]) + (1.0 - k) * grid[c]) as f32
                    }
                };
                [mix(0), mix(1), mix(2)]
            })
            .collect()
    }

    /// The table `intensity` of the way from the identity to this one, as the `.cube` text FFmpeg
    /// reads: the size and the rows, each number the shortest text of its `f32`.
    pub fn to_cube(&self, intensity: f64) -> String {
        let mut out = String::with_capacity(self.data.len() * 30 + 20);
        out.push_str(&format!("LUT_3D_SIZE {}\n", self.size));
        for v in self.at_intensity(intensity) {
            out.push_str(&format!("{} {} {}\n", v[0], v[1], v[2]));
        }
        out
    }
}

/// What a `.cube` file holds, or why it cannot be used.
pub fn inspect_lut(path: &str) -> Result<LutInfo> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(Error::InvalidArgument(format!("the LUT path must be absolute: {path}")));
    }
    let lut = Lut3d::read(p)?;
    Ok(LutInfo {
        path: path.to_string(),
        size: lut.size,
        title: lut.title,
    })
}

/// Check that a grade's LUT (if any) is a usable file, and write its copy for the graph now, so the
/// first frame drawn with it does not parse it. Reads the disk: call it before taking the project.
pub fn check_grade(grade: &Grade) -> Result<()> {
    if let Some(lut) = &grade.lut {
        inspect_lut(&lut.path)?;
        let _ = lut_file(lut);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Source {
    path: String,
    len: u64,
    modified: Option<SystemTime>,
    hundredths: u32,
}

fn resolved() -> &'static Mutex<HashMap<Source, PathBuf>> {
    static MAP: std::sync::OnceLock<Mutex<HashMap<Source, PathBuf>>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The file the graph's `lut3d` reads for `lut`: the table at its intensity, written by Kerf (see the
/// module docs). Looked at on every call (a `stat` of the source and of the copy), re-parsed only when
/// the source changed or the copy went. `None` when the source is missing or not a usable LUT, or no
/// directory can be written: the graph leaves the LUT out.
pub(crate) fn lut_file(lut: &ClipLut) -> Option<PathBuf> {
    static PRUNED: std::sync::Once = std::sync::Once::new();
    PRUNED.call_once(|| lut_dirs().for_each(|d| prune_dir(&d, SystemTime::now())));
    let lut = lut.normalized();
    let meta = std::fs::metadata(&lut.path).ok()?;
    let key = Source {
        path: lut.path.clone(),
        len: meta.len(),
        modified: meta.modified().ok(),
        hundredths: (lut.intensity * 100.0).round() as u32,
    };
    let mut map = resolved().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(known) = map.get(&key).filter(|p| p.is_file()) {
        return Some(known.clone());
    }
    let table = match Lut3d::read(Path::new(&lut.path)) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("LUT left out of the graph: {e}");
            return None;
        }
    };
    let text = table.to_cube(lut.intensity);
    let file = format!("{:016x}.cube", digest(text.as_bytes()));
    let path = lut_dirs().find_map(|dir| write_once(&dir, &file, text.as_bytes()));
    if path.is_none() {
        tracing::warn!("LUT left out of the graph: no directory to write {file} to");
    }
    if map.len() >= 256 {
        map.clear();
    }
    if let Some(p) = &path {
        map.insert(key, p.clone());
    }
    path
}

/// A LUT as the graph's `lut3d` reads it: the table at its intensity, `size³` rows, red fastest — the
/// numbers of the copy [`lut_file`] writes, for a renderer that does the interpolation itself.
#[derive(Debug, Clone, PartialEq)]
pub struct BakedLut {
    pub size: usize,
    pub data: Vec<[f32; 3]>,
}

/// The tables read by [`baked_lut`], the newest last: kept so a renderer asking for every frame of a clip does
/// not parse (and mix) a 3 MB table each time. Keyed like the copies: the file's path, length and
/// modification time, and the intensity in hundredths.
fn baked_cache() -> &'static Mutex<Vec<BakedEntry>> {
    static CACHE: std::sync::OnceLock<Mutex<Vec<BakedEntry>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// A table [`baked_lut`] read, with what it was read from.
type BakedEntry = (Source, std::sync::Arc<BakedLut>);

/// How many baked tables are kept.
const BAKED_KEPT: usize = 4;

/// The table the graph's `lut3d` reads for `lut` ([`lut_file`]'s copy, as numbers), `None` when the graph leaves
/// the LUT out: the file is missing or not a usable LUT. Looked at on every call (one `stat`), re-read only when
/// the file or the intensity changed.
pub fn baked_lut(lut: &ClipLut) -> Option<std::sync::Arc<BakedLut>> {
    let lut = lut.normalized();
    // The graph leaves the LUT out when its copy cannot be made (no directory to write it to) as well as when the
    // file is missing or broken: the same answer here, so that no frame is drawn with a LUT FFmpeg's would not have.
    lut_file(&lut)?;
    let meta = std::fs::metadata(&lut.path).ok()?;
    let key = Source {
        path: lut.path.clone(),
        len: meta.len(),
        modified: meta.modified().ok(),
        hundredths: (lut.intensity * 100.0).round() as u32,
    };
    let mut cache = baked_cache().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(at) = cache.iter().position(|(k, _)| *k == key) {
        let hit = cache.remove(at);
        let table = hit.1.clone();
        cache.push(hit);
        return Some(table);
    }
    let table = match Lut3d::read(Path::new(&lut.path)) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("LUT left out of the graph: {e}");
            return None;
        }
    };
    let baked = std::sync::Arc::new(BakedLut {
        size: table.size,
        data: table.at_intensity(lut.intensity),
    });
    if cache.len() >= BAKED_KEPT {
        cache.remove(0);
    }
    cache.push((key, baked.clone()));
    Some(baked)
}

fn lut_dirs() -> impl Iterator<Item = PathBuf> {
    dirs::cache_dir()
        .map(|d| d.join("kerf").join("luts"))
        .into_iter()
        .chain(std::iter::once_with(|| crate::fonts::private_dir_named(&std::env::temp_dir(), "kerf-luts")).flatten())
}

/// A copy unused for this long is deleted on the first LUT lookup of a run.
const CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);
/// What the copies may take together; past it the least recently used go first.
const CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// The copies in `dir` to delete: older than [`CACHE_MAX_AGE`] (a temp file left by a crash
/// included), then the least recently used until the rest fit in [`CACHE_MAX_BYTES`].
fn prune_plan(mut files: Vec<(PathBuf, u64, SystemTime)>, now: SystemTime) -> Vec<PathBuf> {
    files.sort_by_key(|f| std::cmp::Reverse(f.2));
    let mut kept = 0u64;
    let mut out = Vec::new();
    for (path, len, used) in files {
        let old = now.duration_since(used).is_ok_and(|age| age > CACHE_MAX_AGE);
        if old || kept + len > CACHE_MAX_BYTES {
            out.push(path);
        } else {
            kept += len;
        }
    }
    out
}

fn prune_dir(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let files = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".cube") || name.ends_with(".tmp")
        })
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some((e.path(), meta.len(), meta.modified().ok()?))
        })
        .collect();
    for path in prune_plan(files, now) {
        let _ = std::fs::remove_file(path);
    }
}

/// FNV-1a: a name for the file (the content is compared whole before it is trusted).
fn digest(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `dir/file` holding exactly `bytes`: left alone if it does, else written atomically.
fn write_once(dir: &Path, file: &str, bytes: &[u8]) -> Option<PathBuf> {
    let path = dir.join(file);
    if std::fs::read(&path).is_ok_and(|have| have == bytes) {
        // Marked as used, so the pruning of a later run keeps it.
        let _ = std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(SystemTime::now()));
        return Some(path);
    }
    std::fs::create_dir_all(dir).ok()?;
    let tmp = dir.join(format!(".{file}.{}.tmp", std::process::id()));
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `.cube` of `size` whose every entry is `f(r, g, b)` on the grid.
    pub(crate) fn cube_text(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
        let n = (size - 1) as f32;
        let mut s = format!("TITLE \"test\"\n# a comment\nLUT_3D_SIZE {size}\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n");
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let [x, y, z] = f([r as f32 / n, g as f32 / n, b as f32 / n]);
                    s.push_str(&format!("{x} {y} {z}\n"));
                }
            }
        }
        s
    }

    #[test]
    fn a_cube_parses_and_round_trips_through_the_copy() {
        let text = cube_text(5, |[r, g, b]| [1.0 - r, g * 0.5, b]);
        let lut = Lut3d::parse_cube(&text).unwrap();
        assert_eq!((lut.size, lut.title.as_str()), (5, "test"));
        assert_eq!(lut.at(4, 0, 0), [0.0, 0.0, 0.0]);
        assert_eq!(Lut3d::parse_cube(&lut.to_cube(1.0)).unwrap().data, lut.data);
        let half = Lut3d::parse_cube(&lut.to_cube(0.5)).unwrap();
        let o = half.apply([0.2, 0.6, 0.3]);
        let want = [0.5 * 0.8 + 0.5 * 0.2, 0.5 * 0.3 + 0.5 * 0.6, 0.3];
        for c in 0..3 {
            assert!((o[c] - want[c]).abs() < 1e-6, "{o:?} {want:?}");
        }
    }

    #[test]
    fn cr_line_ends_and_trailing_comments_are_read() {
        let text = cube_text(2, |c| c).replace('\n', "\r");
        assert_eq!(Lut3d::parse_cube(&text).unwrap().data, Lut3d::identity(2).data);
        let text = cube_text(2, |c| c).replace("1 1 1\n", "1 1 1 # white\n");
        assert!(Lut3d::parse_cube(&text).is_ok());
    }

    #[test]
    fn tetrahedral_is_exact_for_an_affine_table() {
        let lut = Lut3d::parse_cube(&cube_text(3, |[r, g, b]| [0.2 + 0.5 * g, r, 0.1 * r + 0.9 * b])).unwrap();
        let o = lut.apply([0.33, 0.71, 0.12]);
        let want = [0.2 + 0.5 * 0.71, 0.33, 0.1 * 0.33 + 0.9 * 0.12];
        for c in 0..3 {
            assert!((o[c] - want[c]).abs() < 1e-6);
        }
    }

    #[test]
    fn what_ffmpeg_would_misread_is_refused() {
        let ok = cube_text(2, |c| c);
        let cases = [
            ("shaper", format!("LUT_1D_SIZE 4\n{ok}")),
            ("domain", ok.replace("DOMAIN_MIN 0 0 0", "DOMAIN_MIN -0.1 0 0")),
            ("range", ok.replace("DOMAIN_MAX 1 1 1", "LUT_3D_INPUT_RANGE 0 2")),
            ("keyword", ok.replace("# a comment", "LUT_IN_VIDEO_RANGE")),
            ("short", ok.replace("1 1 1\n", "")),
            ("long", format!("{ok}0 0 0\n")),
            ("nan", ok.replace("1 1 1\n", "1 nan 1\n")),
            ("inf", ok.replace("1 1 1\n", "1 inf 1\n")),
            ("two", ok.replace("1 1 1\n", "1 1\n")),
            ("four", ok.replace("1 1 1\n", "1 1 1 1\n")),
            ("size", ok.replace("LUT_3D_SIZE 2", "LUT_3D_SIZE 66")),
            ("size1", ok.replace("LUT_3D_SIZE 2", "LUT_3D_SIZE 1")),
            ("nosize", ok.replace("LUT_3D_SIZE 2", "")),
            ("late", format!("{ok}LUT_3D_SIZE 2\n")),
            ("empty", String::new()),
        ];
        for (what, text) in cases {
            assert!(Lut3d::parse_cube(&text).is_err(), "{what} should be refused");
        }
    }

    #[test]
    fn an_error_names_the_line_and_not_the_file_content() {
        let ok = cube_text(2, |c| c);
        for text in [
            ok.replace("# a comment", "SECRET_hunter2 1"),
            ok.replace("1 1 1\n", "1 hunter2 1\n"),
            format!("{ok}hunter2 0 0\n"),
        ] {
            let e = Lut3d::parse_cube(&text).unwrap_err().to_string();
            assert!(e.contains("(line ") && !e.contains("hunter2"), "{e}");
        }
    }

    #[test]
    fn the_cache_drops_old_copies_then_the_least_recently_used() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1 << 32);
        let ago = |days: u64| now - std::time::Duration::from_secs(days * 24 * 3600);
        let mb = 1024 * 1024;
        let files = vec![
            (PathBuf::from("old"), mb, ago(31)),
            (PathBuf::from("new"), 200 * mb, ago(0)),
            (PathBuf::from("mid"), 50 * mb, ago(2)),
            (PathBuf::from("lru"), 10 * mb, ago(3)),
        ];
        assert_eq!(prune_plan(files, now), [PathBuf::from("lru"), PathBuf::from("old")]);
        assert!(prune_plan(vec![(PathBuf::from("a"), mb, ago(1))], now).is_empty());
    }

    #[test]
    fn a_huge_size_allocates_nothing_before_it_is_refused() {
        assert!(Lut3d::parse_cube("LUT_3D_SIZE 100000\n0 0 0\n").is_err());
        assert!(Lut3d::parse_cube("LUT_3D_SIZE 18446744073709551615\n").is_err());
    }

    #[test]
    fn the_copy_is_written_once_and_follows_the_intensity() {
        let dir = std::env::temp_dir().join(format!("kerf-lut-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("look.cube");
        std::fs::write(&src, cube_text(3, |[r, g, b]| [g, b, r])).unwrap();
        let lut = ClipLut {
            path: src.to_string_lossy().into_owned(),
            intensity: 1.0,
        };
        let a = lut_file(&lut).expect("written");
        assert_eq!(lut_file(&lut), Some(a.clone()), "the same file again");
        let copy = Lut3d::parse_cube(&std::fs::read_to_string(&a).unwrap()).unwrap();
        assert_eq!(copy.data, Lut3d::read(&src).unwrap().data);
        let half = lut_file(&ClipLut {
            intensity: 0.5,
            ..lut.clone()
        })
        .unwrap();
        assert_ne!(a, half);
        std::fs::remove_file(&a).unwrap();
        assert_eq!(lut_file(&lut), Some(a.clone()), "written again after it went");
        assert!(a.is_file());
        std::fs::remove_file(&src).unwrap();
        assert_eq!(lut_file(&lut), None, "a missing source is left out");
        assert!(inspect_lut(&lut.path).is_err());
        assert!(inspect_lut("relative.cube").is_err());
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&half);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_baked_table_is_what_the_copy_holds_and_follows_the_file() {
        let dir = std::env::temp_dir().join(format!("kerf-lut-baked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("baked.cube");
        std::fs::write(&src, cube_text(4, |[r, g, b]| [g * 0.9, b, 1.0 - r])).unwrap();
        let lut = ClipLut {
            path: src.to_string_lossy().into_owned(),
            intensity: 0.4,
        };
        // The numbers the graph's `lut3d` reads: the copy, parsed back (each the shortest text of its `f32`).
        let copy = Lut3d::parse_cube(&std::fs::read_to_string(lut_file(&lut).unwrap()).unwrap()).unwrap();
        let baked = baked_lut(&lut).expect("a usable file");
        assert_eq!((baked.size, &baked.data), (copy.size, &copy.data));
        // Asked again it is the same table, not a new parse; another intensity is another table.
        assert!(std::sync::Arc::ptr_eq(&baked, &baked_lut(&lut).unwrap()));
        let full = baked_lut(&ClipLut {
            intensity: 1.0,
            ..lut.clone()
        })
        .unwrap();
        assert_eq!(full.data, Lut3d::read(&src).unwrap().data);
        assert_ne!(full.data, baked.data);
        // The file changing is a new table (the length is part of the key).
        std::fs::write(&src, cube_text(3, |c| c)).unwrap();
        assert_eq!(baked_lut(&lut).unwrap().size, 3);
        // A file the graph leaves out is none.
        std::fs::remove_file(&src).unwrap();
        assert_eq!(baked_lut(&lut), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
