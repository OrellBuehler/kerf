//! Fonts for text overlays: the system's, found by `fontdb`, and the one Kerf ships.
//!
//! `fontdb` scans the OS's known font directories and parses faces itself (including Linux
//! fontconfig directory lists, via a pure-Rust config parser) — no fontconfig/CoreText/DirectWrite
//! linking, so this works in the `--no-default-features` build too.
//!
//! **Every overlay names a font file.** An overlay with no `font` (every title and caption the
//! app makes) used to leave `fontfile=` out and let `drawtext` ask its own build for "Sans": a
//! different face on every machine, no face at all in a build without fontconfig (the Windows and
//! macOS bundles' FFmpeg), and — the reason it matters here — a face the render plan could not
//! name, so no renderer but FFmpeg could draw the text. The default is now a font shipped in the
//! binary ([`DEFAULT_FAMILY`], SIL OFL 1.1, a regular and a bold face), written to the cache
//! directory because `drawtext` reads a path. The same file on every machine is what makes a
//! preview look like the export it came from, and what lets the plan say which pixels it means.
//! A named family that is not installed falls back to it too. The file is looked at on every use
//! (a `stat`), so a cache that is cleared or a temp directory that is swept while the app runs is
//! written again rather than leaving every title unrenderable until a restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use fontdb::{Database, Family, Query, Source, Style, Weight};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The family shipped in the binary (Geist, the interface's own typeface).
pub const DEFAULT_FAMILY: &str = "Geist";

const REGULAR: &[u8] = include_bytes!("../assets/fonts/Geist-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../assets/fonts/Geist-Bold.ttf");

fn db() -> &'static Database {
    static DB: OnceLock<Database> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = Database::new();
        db.load_system_fonts();
        db
    })
}

/// The system's fonts and the shipped Geist, with Geist standing in for `serif` and `sans-serif`.
/// Built once; what a project embeds is layered on top of it ([`build_svg_db`]).
fn base_svg_db() -> Arc<Database> {
    static DB: OnceLock<Arc<Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut svg = db().clone();
        finish_svg_db(&mut svg);
        Arc::new(svg)
    })
    .clone()
}

/// Geist, and Geist for `serif` and `sans-serif` — which is also where a family nothing has falls
/// to, so text never vanishes for want of a face and a title looks the same on a machine with no Times.
fn finish_svg_db(svg: &mut Database) {
    svg.load_font_source(Source::Binary(Arc::new(REGULAR)));
    svg.load_font_source(Source::Binary(Arc::new(BOLD)));
    svg.set_serif_family(DEFAULT_FAMILY);
    svg.set_sans_serif_family(DEFAULT_FAMILY);
}

/// The fonts an animated SVG title is drawn with: the system's, the shipped Geist, and **the fonts
/// the open project embeds** ([`replace_embedded`], [`register_embedded`]) — a template's typeface
/// travels with it, so a title looks the same on a machine that has never installed it. An embedded
/// face is looked at *before* the installed ones, so a family both have is the project's. Geist
/// stands for `serif` and `sans-serif`, which is also where a family nothing has falls to.
///
/// This is the database of the **whole** registered set ([`snapshot`]); drawing a timeline's titles takes
/// [`snapshot_for`], the part of the set those titles name. With nothing embedded it is the one database
/// built for the process; with some it is built once per set, and [`FontSet::digest`] says which set it
/// was built from. `Arc` because `usvg` takes its font database that way.
pub fn svg_fontdb() -> Arc<Database> {
    snapshot().db()
}

/// [`svg_fontdb`] with `extra` fonts layered in as well, registered nowhere: a template being saved
/// or imported draws its poster with the fonts it is about to carry, whatever the open project has.
pub fn svg_fontdb_with(extra: &[EmbeddedFont]) -> Arc<Database> {
    let mut all: BTreeMap<String, EmbeddedFont> = snapshot().fonts.iter().map(|f| (f.sha256.clone(), f.clone())).collect();
    for font in extra {
        all.entry(font.sha256.clone()).or_insert_with(|| font.clone());
    }
    if all.is_empty() {
        return base_svg_db();
    }
    Arc::new(build_svg_db(all.values()))
}

// ---- fonts that travel with a project --------------------------------------------------------------

/// A font a project or a template carries rather than finds installed: the file's bytes, named by
/// their SHA-256 (the key the project's `fonts` table stores them under).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedFont {
    pub sha256: String,
    /// The family it is called in the template's manifest / the table (what a person reads; the
    /// family text uses is the one inside the font file).
    pub family: String,
    pub data: Arc<Vec<u8>>,
}

impl EmbeddedFont {
    pub fn new(family: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            sha256: sha256_hex(&data),
            family: family.into(),
            data: Arc::new(data),
        }
    }
}

/// The SHA-256 of `bytes` as lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes).iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A set of embedded fonts and the database drawing with them, **immutable once made**: a picture is
/// keyed by [`FontSet::digest`] and drawn with [`FontSet::db`], and both come from one value, so the key
/// cannot name a set other than the one the picture was drawn with, however the registry moves in between.
/// Planning a title takes one (`snapshot_for`) and carries it to the drawing.
///
/// The database is built on first use, **by the caller of [`FontSet::db`] and under no registry lock**
/// (a build scans the system's fonts and parses every embedded face: seconds, on a cold disk): the
/// registry is only ever held to swap an `Arc`, so [`replace_embedded`] and [`embedded_digest`] never wait
/// on a build.
pub struct FontSet {
    /// A digest of the set: `None` when it is empty.
    digest: Option<String>,
    /// By SHA-256.
    fonts: Vec<EmbeddedFont>,
    db: OnceLock<Arc<Database>>,
}

impl std::fmt::Debug for FontSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSet")
            .field("digest", &self.digest)
            .field("fonts", &self.fonts.len())
            .finish()
    }
}

impl FontSet {
    fn of(fonts: &BTreeMap<String, EmbeddedFont>) -> Arc<Self> {
        let digest = (!fonts.is_empty()).then(|| {
            let mut hash = Sha256::new();
            for key in fonts.keys() {
                hash.update(key.as_bytes());
            }
            hash.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect()
        });
        Arc::new(Self {
            digest,
            fonts: fonts.values().cloned().collect(),
            db: OnceLock::new(),
        })
    }

    /// The set with nothing embedded: the process's one database, whatever the registry holds.
    pub fn none() -> Arc<Self> {
        static NONE: OnceLock<Arc<FontSet>> = OnceLock::new();
        NONE.get_or_init(|| FontSet::of(&BTreeMap::new())).clone()
    }

    /// A digest of the set (32 hex digits), `None` for an empty one. **Part of every animated-title
    /// picture's cache key** when there is one, so a frame drawn with other fonts is never served; with
    /// none the keys are what they always were.
    pub fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }

    /// The fonts of the set.
    pub fn fonts(&self) -> &[EmbeddedFont] {
        &self.fonts
    }

    /// The database to draw with: the system's faces behind the set's, built once.
    pub fn db(&self) -> Arc<Database> {
        self.db_with(|fonts| {
            if fonts.is_empty() {
                base_svg_db()
            } else {
                Arc::new(build_svg_db(fonts.iter()))
            }
        })
    }

    /// [`FontSet::db`] with the build given (a test makes it observable).
    fn db_with(&self, build: impl FnOnce(&[EmbeddedFont]) -> Arc<Database>) -> Arc<Database> {
        self.db.get_or_init(|| build(&self.fonts)).clone()
    }
}

/// The embedded set. Per process; under `cfg(test)` per thread, so that tests running side by side
/// cannot see (or key their pictures by) each other's fonts. Held only to read or swap an `Arc`.
struct Registry {
    /// Everything registered; `None` is nothing.
    set: Option<Arc<FontSet>>,
    /// The parts of `set` that titles named lately ([`snapshot_for`]), so that a part is built once.
    parts: Vec<Arc<FontSet>>,
}

/// How many parts of the registered set are remembered.
const PARTS_KEPT: usize = 8;

impl Registry {
    const fn new() -> Self {
        Registry {
            set: None,
            parts: Vec::new(),
        }
    }

    fn set(&self) -> Arc<FontSet> {
        self.set.clone().unwrap_or_else(FontSet::none)
    }

    fn replace(&mut self, fonts: &BTreeMap<String, EmbeddedFont>) {
        self.set = (!fonts.is_empty()).then(|| FontSet::of(fonts));
        self.parts.clear();
    }
}

/// The system's faces behind `embedded` ones (the first of equal matches wins a `fontdb` query, so
/// putting them first makes a template's own typeface beat an installed one of the same family).
fn build_svg_db<'a>(embedded: impl Iterator<Item = &'a EmbeddedFont>) -> Database {
    let mut svg = Database::new();
    for font in embedded {
        let ids = svg.load_font_source(Source::Binary(font.data.clone()));
        if ids.is_empty() {
            tracing::warn!(family = %font.family, "an embedded font could not be read; text in it falls back");
        }
    }
    for face in db().faces() {
        svg.push_face_info(face.clone());
    }
    finish_svg_db(&mut svg);
    svg
}

#[cfg(not(test))]
fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());
    f(&mut REGISTRY.lock().unwrap_or_else(PoisonError::into_inner))
}

#[cfg(test)]
fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    thread_local! {
        static REGISTRY: std::cell::RefCell<Registry> = const { std::cell::RefCell::new(Registry::new()) };
    }
    REGISTRY.with(|r| f(&mut r.borrow_mut()))
}

/// The whole registered set, as it is now.
pub fn snapshot() -> Arc<FontSet> {
    with_registry(|r| r.set())
}

/// Whether the text of a title (`lowered`: lower-cased, see [`snapshot_for`]) names `family`. By the
/// family's name appearing in the text, wherever it is written (an attribute, a `style`, a `<style>`
/// rule, a SMIL `set` / `animate`): a title cannot be drawn in a family it does not name, and a name that
/// is only text content ("Lora" in a caption) costs one font too many, never one too few.
pub fn names_family(lowered: &str, family: &str) -> bool {
    let family = family.trim().to_lowercase();
    !family.is_empty() && lowered.contains(&family)
}

/// [`FontSet::named_by`] for the registry's whole set, as it is now.
pub fn snapshot_for<'a>(titles: impl IntoIterator<Item = &'a str>) -> Arc<FontSet> {
    snapshot().named_by(titles)
}

impl FontSet {
    /// The part of this set that `titles` (the sources of the animated titles being drawn) name: **the
    /// fonts those titles are drawn with**. A font nobody here names changes neither a picture nor its
    /// cache key, so a template's font that sits in the project (held for a proposal's review, or left by
    /// a title that was deleted) does not move how an unrelated title draws.
    ///
    /// Take it **once, where the drawing is planned**, and carry it to the drawing: the key
    /// ([`FontSet::digest`]) and the fonts ([`FontSet::db`]) then come from the same set. A part is
    /// built once however often it is asked for.
    pub fn named_by<'a>(self: &Arc<Self>, titles: impl IntoIterator<Item = &'a str>) -> Arc<FontSet> {
        if self.fonts.is_empty() {
            return self.clone();
        }
        let lowered: Vec<String> = titles.into_iter().map(str::to_lowercase).collect();
        let named: BTreeMap<String, EmbeddedFont> = self
            .fonts
            .iter()
            .filter(|f| lowered.iter().any(|t| names_family(t, &f.family)))
            .map(|f| (f.sha256.clone(), f.clone()))
            .collect();
        if named.len() == self.fonts.len() {
            return self.clone();
        }
        if named.is_empty() {
            return FontSet::none();
        }
        let part = FontSet::of(&named);
        with_registry(|r| {
            if let Some(known) = r.parts.iter().find(|p| p.digest == part.digest) {
                return known.clone();
            }
            // Remembered only while this is the registry's set: one that moved on keeps its own parts.
            if r.set.as_ref().is_some_and(|s| Arc::ptr_eq(s, self)) {
                if r.parts.len() >= PARTS_KEPT {
                    r.parts.remove(0);
                }
                r.parts.push(part.clone());
            }
            part
        })
    }
}

/// Add `fonts` to the embedded set (a font already there, by its SHA-256, is left alone). Returns
/// whether the set changed — when it did, the pictures drawn so far no longer stand for it and
/// [`embedded_digest`] has moved.
pub fn register_embedded(fonts: &[EmbeddedFont]) -> bool {
    with_registry(|r| {
        let mut all: BTreeMap<String, EmbeddedFont> = r.set().fonts.iter().map(|f| (f.sha256.clone(), f.clone())).collect();
        let before = all.len();
        for font in fonts {
            all.entry(font.sha256.clone()).or_insert_with(|| font.clone());
        }
        let changed = all.len() != before;
        if changed {
            r.replace(&all);
        }
        changed
    })
}

/// Make `fonts` the embedded set, as a project becoming the open one does: the previous project's fonts
/// are not this one's, and a title must draw the same however many projects were opened before it.
/// Returns whether the set changed.
pub fn replace_embedded(fonts: Vec<EmbeddedFont>) -> bool {
    let next: BTreeMap<String, EmbeddedFont> = fonts.into_iter().map(|f| (f.sha256.clone(), f)).collect();
    with_registry(|r| {
        let changed = !r.set().fonts.iter().map(|f| &f.sha256).eq(next.keys());
        if changed {
            r.replace(&next);
        }
        changed
    })
}

/// A digest of the whole embedded set (32 hex digits), `None` when nothing is embedded. What a picture
/// is keyed by is [`FontSet::digest`] of the set it was planned with ([`snapshot_for`]).
pub fn embedded_digest() -> Option<String> {
    with_registry(|r| r.set().digest.clone())
}

/// The embedded set, by SHA-256.
pub fn embedded_fonts() -> Vec<EmbeddedFont> {
    with_registry(|r| r.set().fonts.clone())
}

/// Whether a family called `family` is installed or embedded (or is the shipped Geist): the one the
/// title will really be drawn with, rather than the Geist a family nothing has falls to.
pub fn has_family(family: &str) -> bool {
    let family = family.trim().to_lowercase();
    svg_fontdb()
        .faces()
        .any(|face| face.families.iter().any(|(name, _)| name.to_lowercase() == family))
}

/// The families of a CSS `font-family` list, in order: split on commas, trimmed, quotes stripped.
/// (Generic families such as `sans-serif` are among them; [`is_generic_family`] tells.)
pub fn family_list(list: &str) -> impl Iterator<Item = &str> {
    list.split(',')
        .map(|family| family.trim().trim_matches(['"', '\'']).trim())
        .filter(|family| !family.is_empty())
}

/// Whether two family names are the same family (CSS compares them without regard to case).
pub fn same_family(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Families CSS names for a kind of face rather than a font: never embedded, never looked up.
pub fn is_generic_family(family: &str) -> bool {
    matches!(
        family.trim().to_ascii_lowercase().as_str(),
        "serif"
            | "sans-serif"
            | "monospace"
            | "cursive"
            | "fantasy"
            | "system-ui"
            | "ui-serif"
            | "ui-sans-serif"
            | "ui-monospace"
            | "ui-rounded"
            | "emoji"
            | "math"
            | "fangsong"
            | "inherit"
            | "initial"
            | "unset"
            | "revert"
    )
}

/// What a font file turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontCheck {
    /// The family the file calls itself (what text names).
    pub family: String,
}

/// The sfnt wrappers a single TrueType / OpenType font starts with. A collection (`ttcf`) is not
/// one: it holds several faces, and a template carries one file per font.
fn is_sfnt(data: &[u8]) -> bool {
    data.starts_with(&[0, 1, 0, 0]) || data.starts_with(b"OTTO") || data.starts_with(b"true")
}

/// Whether the font's `OS/2` table allows embedding it. `fsType` is refused when it says "restricted
/// licence embedding" (0x0002), "preview & print embedding" (0x0004: for reading a document, not for
/// carrying the font into another) or "bitmap embedding only" (0x0200); installable (0) and editable
/// (0x0008) embedding are allowed, and so is the "no subsetting" flag (0x0100) on its own, since a font is
/// carried whole. A font with no such table is taken to allow it.
pub fn embedding_allowed(data: &[u8]) -> bool {
    // Restricted licence, preview & print, bitmap only.
    const EMBEDDING_REFUSED: u16 = 0x0002 | 0x0004 | 0x0200;
    let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let Some(tables) = u16_at(4) else { return true };
    for i in 0..usize::from(tables) {
        let record = 12 + i * 16;
        if data.get(record..record + 4) == Some(b"OS/2") {
            let Some(offset) = u32_at(record + 8) else { return true };
            return u16_at(offset as usize + 8).is_none_or(|fs_type| fs_type & EMBEDDING_REFUSED == 0);
        }
    }
    true
}

/// Read `data` as the one font it must be: a TrueType / OpenType file `fontdb` can parse, not a
/// collection. `fontdb` quietly skips what it cannot read, so an empty answer is the refusal.
pub fn check_font(data: &Arc<Vec<u8>>) -> Result<FontCheck> {
    if data.starts_with(b"ttcf") {
        return Err(Error::InvalidArgument(
            "a font collection (.ttc) cannot be carried: use a single .ttf or .otf".to_string(),
        ));
    }
    if !is_sfnt(data) {
        return Err(Error::InvalidArgument("not a TrueType or OpenType font".to_string()));
    }
    let mut probe = Database::new();
    let ids = probe.load_font_source(Source::Binary(data.clone()));
    let face = ids
        .first()
        .and_then(|id| probe.face(*id))
        .ok_or_else(|| Error::InvalidArgument("the font file could not be read".to_string()))?;
    let family = face
        .families
        .first()
        .map(|(name, _)| name.clone())
        .ok_or_else(|| Error::InvalidArgument("the font has no family name".to_string()))?;
    Ok(FontCheck { family })
}

/// What looking a face up for embedding found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaceLookup {
    Found(EmbeddedFont),
    /// No installed or embedded font has the family.
    Missing,
    /// The face lives in a `.ttc` collection or is not a plain sfnt file.
    NotSingleFile,
    /// Bigger than the cap (`max` bytes), given.
    TooLarge(u64),
    /// The font's licence flags forbid embedding.
    Restricted,
}

/// The font file the family `family` is drawn with at `weight` (CSS 100..=900) and `italic`, read for
/// embedding, `max` bytes at most. The generic families and Geist (which every Kerf has) are the
/// caller's to leave out.
pub fn lookup_face(family: &str, weight: u16, italic: bool, max: u64) -> FaceLookup {
    let db = svg_fontdb();
    let query = Query {
        families: &[Family::Name(family)],
        weight: Weight(weight),
        style: if italic { Style::Italic } else { Style::Normal },
        ..Query::default()
    };
    let Some(id) = db.query(&query) else {
        return FaceLookup::Missing;
    };
    let Some(face) = db.face(id) else {
        return FaceLookup::Missing;
    };
    if !face.families.iter().any(|(name, _)| name == family) {
        return FaceLookup::Missing;
    }
    let read = db.with_face_data(id, |data, index| {
        if index != 0 || !is_sfnt(data) {
            Err(FaceLookup::NotSingleFile)
        } else if data.len() as u64 > max {
            Err(FaceLookup::TooLarge(data.len() as u64))
        } else if !embedding_allowed(data) {
            Err(FaceLookup::Restricted)
        } else {
            Ok(data.to_vec())
        }
    });
    match read {
        Some(Ok(bytes)) => FaceLookup::Found(EmbeddedFont::new(family, bytes)),
        Some(Err(why)) => why,
        None => FaceLookup::Missing,
    }
}

/// Read the machine's fonts now if they have not been, so the first animated title (or font list)
/// finds them: a macOS install has some hundreds of faces, and a cold disk takes seconds to read
/// them. Idempotent and quick once done; the app calls it from a thread at launch.
pub fn warm() {
    let _ = svg_fontdb();
}

/// Distinct family names of every font installed on this machine, sorted.
pub fn list_system_fonts() -> Vec<String> {
    let mut names: Vec<String> = db()
        .faces()
        .filter_map(|f| f.families.first().map(|(name, _)| name.clone()))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Resolve `family` to a font file on disk, preferring a bold face when `bold`
/// is set. Returns the path and whether the matched face actually satisfies
/// the bold request, so the caller knows whether a thickening fallback is
/// still needed. `None` if the family isn't installed.
pub fn resolve_font_file(family: &str, bold: bool) -> Option<(PathBuf, bool)> {
    #[cfg(test)]
    if PINNED.with(std::cell::Cell::get) && family == PINNED_FAMILY {
        return Some((PathBuf::from("/kerf-test/fonts/PinnedSans-Regular.ttf"), false));
    }
    let weight = if bold { Weight::BOLD } else { Weight::NORMAL };
    let query = Query {
        families: &[Family::Name(family)],
        weight,
        ..Query::default()
    };
    let face = db().face(db().query(&query)?)?;
    let path = match &face.source {
        Source::File(p) | Source::SharedFile(p, _) => p.clone(),
        Source::Binary(_) => return None,
    };
    Some((path, face.weight >= Weight::BOLD))
}

/// The font file an overlay is drawn with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFont {
    pub path: PathBuf,
    /// The face is a bold one: a bold request needs no same-colour border to thicken it.
    pub bold: bool,
}

/// The font file `family` (an overlay's `font`) is drawn with: the installed family it names, else
/// the shipped default. `None` only when the default could not be written to disk, which leaves
/// `drawtext` its own default and the compositor nothing to name.
///
/// The export graph and the render plan both ask this, so they cannot disagree.
pub fn resolve_text_font(family: Option<&str>, bold: bool) -> Option<TextFont> {
    if let Some((path, matched_bold)) = family.and_then(|f| resolve_font_file(f, bold)) {
        return Some(TextFont {
            path,
            bold: matched_bold,
        });
    }
    default_font_file(bold).map(|path| TextFont { path, bold })
}

/// The shipped font's file, in the cache directory (else a private per-user directory in the temp
/// one). It is checked on every call and written again if it is gone or is not the shipped bytes;
/// a failure to write is not remembered, so the next call tries again. `None` if no directory can
/// be written, which leaves `drawtext` its own default and the compositor nothing to name.
pub fn default_font_file(bold: bool) -> Option<PathBuf> {
    #[cfg(test)]
    if PINNED.with(std::cell::Cell::get) {
        return Some(PathBuf::from(if bold {
            "/kerf-test/fonts/Geist-Bold.ttf"
        } else {
            "/kerf-test/fonts/Geist-Regular.ttf"
        }));
    }
    static FILES: [Mutex<Option<OnDisk>>; 2] = [Mutex::new(None), Mutex::new(None)];
    // The name carries a digest of the bytes: worked out once, not on every call.
    static NAMES: [OnceLock<String>; 2] = [OnceLock::new(), OnceLock::new()];
    let (name, bytes) = if bold {
        ("Geist-Bold", BOLD)
    } else {
        ("Geist-Regular", REGULAR)
    };
    let file = NAMES[usize::from(bold)].get_or_init(|| format!("{name}-{:08x}.ttf", digest(bytes)));
    ensure(&FILES[usize::from(bold)], font_dirs, file, bytes)
}

/// Where the shipped fonts are written, in order of preference. Lazy: the private temp directory
/// is only made when the cache directory did not work.
fn font_dirs() -> impl Iterator<Item = PathBuf> {
    dirs::cache_dir()
        .map(|d| d.join("kerf").join("fonts"))
        .into_iter()
        .chain(std::iter::once_with(|| private_dir(&std::env::temp_dir())).flatten())
}

/// A directory under `base` that only this user can write: `base` is `/tmp` on Linux, shared by
/// everyone, and a font file there that another user could plant would be one `drawtext` parses.
/// Named for the user, created owner-only, and refused when it already exists but is someone else's
/// or open to others. Elsewhere (Windows, macOS) the temp directory is already the user's own.
fn private_dir(base: &Path) -> Option<PathBuf> {
    private_dir_named(base, "kerf-fonts")
}

/// [`private_dir`] under another name: the animated SVG frames fall back to one the same way.
pub(crate) fn private_dir_named(base: &Path, name: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        let dir = base.join(format!("{name}-{uid}"));
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
        let meta = std::fs::symlink_metadata(&dir).ok()?;
        (meta.is_dir() && meta.uid() == uid && meta.mode() & 0o022 == 0).then_some(dir)
    }
    #[cfg(not(unix))]
    {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }
}

/// A shipped font known to be on disk, and what `stat` said when its bytes were last compared.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OnDisk {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

impl OnDisk {
    fn of(path: PathBuf) -> Option<Self> {
        let meta = std::fs::metadata(&path).ok()?;
        Some(Self {
            path,
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    /// The file is still the one that was compared.
    fn unchanged(&self) -> bool {
        Self::of(self.path.clone()).as_ref() == Some(self)
    }
}

/// The path of `file` holding `bytes`: the one in `slot` while it is unchanged, else the first of
/// `dirs()` that holds or can be made to hold it. A miss leaves `slot` empty.
fn ensure<I: IntoIterator<Item = PathBuf>>(
    slot: &Mutex<Option<OnDisk>>,
    dirs: impl FnOnce() -> I,
    file: &str,
    bytes: &[u8],
) -> Option<PathBuf> {
    let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(known) = slot.as_ref().filter(|k| k.unchanged()) {
        return Some(known.path.clone());
    }
    *slot = dirs().into_iter().find_map(|dir| write_font(&dir, file, bytes));
    slot.as_ref().map(|k| k.path.clone())
}

/// FNV-1a, folded to 32 bits: a name for the file, not a security property (the content is compared
/// whole, below).
fn digest(bytes: &[u8]) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h ^ (h >> 32)) as u32
}

/// `dir/file` holding exactly `bytes`: left alone if it does, else written atomically (another
/// process may be writing the same file). The bytes are compared, not just the length: a file of the
/// right size with other content is rewritten.
fn write_font(dir: &Path, file: &str, bytes: &[u8]) -> Option<OnDisk> {
    let path = dir.join(file);
    if std::fs::read(&path).is_ok_and(|have| have == bytes) {
        return OnDisk::of(path);
    }
    std::fs::create_dir_all(dir).ok()?;
    let tmp = dir.join(format!(".{file}.{}.tmp", std::process::id()));
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    OnDisk::of(path)
}

#[cfg(test)]
thread_local! {
    static PINNED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While the default font is pinned, this family exists, at a fixed path and with no bold face:
/// the one font an argv oracle can name that has to be thickened with a border.
#[cfg(test)]
pub(crate) const PINNED_FAMILY: &str = "Kerf Pinned Sans";

/// Run `f` with the default font at a fixed path that is not written anywhere, so an argv oracle
/// does not hold this machine's cache directory (see `golden`), and [`PINNED_FAMILY`] installed.
#[cfg(test)]
pub(crate) fn with_pinned_default_font<R>(f: impl FnOnce() -> R) -> R {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            PINNED.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(PINNED.with(|c| c.replace(true)));
    f()
}

/// A copy of the shipped Geist (`bold` or regular) that calls itself `family` (as many characters as
/// "Geist" has): the names in the file are replaced in place, in both encodings, so every table keeps its
/// place. A face a test can embed that looks different from the Geist a family nothing has falls to.
#[cfg(test)]
pub(crate) fn shipped_font_renamed(bold: bool, family: &str) -> Vec<u8> {
    fn replace(data: &mut [u8], from: &[u8], to: &[u8]) {
        let mut at = 0;
        while at + from.len() <= data.len() {
            if &data[at..at + from.len()] == from {
                data[at..at + to.len()].copy_from_slice(to);
                at += from.len();
            } else {
                at += 1;
            }
        }
    }
    assert_eq!(family.len(), DEFAULT_FAMILY.len());
    let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>();
    let mut data = if bold { BOLD } else { REGULAR }.to_vec();
    replace(&mut data, DEFAULT_FAMILY.as_bytes(), family.as_bytes());
    replace(&mut data, &utf16(DEFAULT_FAMILY), &utf16(family));
    data
}

/// `font` with the `fsType` of its `OS/2` table set to `fs_type`.
#[cfg(test)]
pub(crate) fn with_fs_type(mut font: Vec<u8>, fs_type: u16) -> Vec<u8> {
    let tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
    let record = (0..tables)
        .map(|i| 12 + i * 16)
        .find(|r| &font[*r..*r + 4] == b"OS/2")
        .expect("an OS/2 table");
    let offset = u32::from_be_bytes(font[record + 8..record + 12].try_into().unwrap()) as usize;
    font[offset + 8..offset + 10].copy_from_slice(&fs_type.to_be_bytes());
    font
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_written_once_and_is_the_shipped_font() {
        let regular = default_font_file(false).expect("a writable cache or temp directory");
        let bold = default_font_file(true).expect("a writable cache or temp directory");
        assert_ne!(regular, bold);
        assert_eq!(std::fs::read(&regular).unwrap(), REGULAR);
        assert_eq!(std::fs::read(&bold).unwrap(), BOLD);
        assert_eq!(default_font_file(false), Some(regular), "the same path every time");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-fonts-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_truncated_copy_is_rewritten() {
        let dir = scratch("truncated");
        std::fs::write(dir.join("a.ttf"), &REGULAR[..100]).unwrap();
        let path = write_font(&dir, "a.ttf", REGULAR).expect("rewritten").path;
        assert_eq!(std::fs::read(path).unwrap(), REGULAR);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no temporary file is left behind"
        );
        // A directory that cannot exist on any OS: a child of a regular file.
        assert!(write_font(&dir.join("a.ttf").join("sub"), "b.ttf", REGULAR).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_of_the_right_length_with_other_content_is_rewritten() {
        let dir = scratch("planted");
        let mut planted = REGULAR.to_vec();
        planted[REGULAR.len() / 2] ^= 0xff;
        std::fs::write(dir.join("a.ttf"), &planted).unwrap();
        let path = write_font(&dir, "a.ttf", REGULAR).expect("rewritten").path;
        assert_eq!(std::fs::read(path).unwrap(), REGULAR);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_font_deleted_while_running_is_written_again() {
        let dir = scratch("deleted");
        let slot = Mutex::new(None);
        let there = || vec![dir.join("fonts")];
        let path = ensure(&slot, there, "a.ttf", REGULAR).expect("written");
        assert_eq!(
            ensure(&slot, there, "a.ttf", REGULAR),
            Some(path.clone()),
            "the same path while it is there"
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            ensure(&slot, there, "a.ttf", REGULAR),
            Some(path.clone()),
            "written again after the file went"
        );
        assert_eq!(std::fs::read(&path).unwrap(), REGULAR);
        std::fs::remove_dir_all(dir.join("fonts")).unwrap();
        assert_eq!(
            ensure(&slot, there, "a.ttf", REGULAR),
            Some(path.clone()),
            "and after the directory went"
        );
        assert_eq!(std::fs::read(&path).unwrap(), REGULAR);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_is_tried_again_and_the_next_directory_is_used() {
        let dir = scratch("failed");
        std::fs::write(dir.join("file"), b"x").unwrap();
        let blocked = dir.join("file").join("sub");
        let slot = Mutex::new(None);
        assert_eq!(ensure(&slot, || vec![blocked.clone()], "a.ttf", REGULAR), None);
        assert!(slot.lock().unwrap().is_none(), "a miss is not remembered");
        let good = dir.join("good");
        let path = ensure(&slot, || vec![blocked.clone(), good.clone()], "a.ttf", REGULAR);
        assert_eq!(path, Some(good.join("a.ttf")), "the first directory that works");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_later_directory_is_not_touched_when_the_first_works() {
        let dir = scratch("lazy");
        let slot = Mutex::new(None);
        let first = dir.join("first");
        let dirs = || {
            std::iter::once(first.clone()).chain(std::iter::once_with(|| -> PathBuf {
                panic!("the fallback directory was made though the first one worked")
            }))
        };
        assert_eq!(ensure(&slot, dirs, "a.ttf", REGULAR), Some(first.join("a.ttf")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_temp_fallback_is_this_users_and_nobody_elses() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let base = scratch("private");
        let dir = private_dir(&base).expect("a fresh private directory");
        let uid = std::fs::metadata(&dir).unwrap().uid();
        assert!(dir.ends_with(format!("kerf-fonts-{uid}")), "named for the user: {dir:?}");
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        assert_eq!(private_dir(&base), Some(dir.clone()), "and the same one again");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(private_dir(&base), None, "a directory open to others is refused");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_unnamed_or_missing_family_is_the_default_and_its_bold_is_real() {
        for family in [None, Some("Definitely Not An Installed Font XYZ123")] {
            for bold in [false, true] {
                let font = resolve_text_font(family, bold).expect("the shipped default");
                assert_eq!(Some(font.path), default_font_file(bold), "{family:?} bold={bold}");
                assert_eq!(font.bold, bold, "a bold request is met by the bold face");
            }
        }
    }

    #[test]
    fn an_installed_family_is_what_it_names() {
        let Some(family) = list_system_fonts().into_iter().next() else {
            return;
        };
        let font = resolve_text_font(Some(&family), false).expect("an installed family");
        assert_eq!(Some((font.path, font.bold)), resolve_font_file(&family, false));
    }

    fn family_of(db: &Database, name: &str, weight: u16) -> Option<fontdb::ID> {
        db.query(&Query {
            families: &[Family::Name(name)],
            weight: Weight(weight),
            ..Query::default()
        })
    }

    #[test]
    fn nothing_embedded_is_the_one_database_built_for_the_process() {
        assert_eq!(embedded_digest(), None);
        assert!(Arc::ptr_eq(&svg_fontdb(), &svg_fontdb()));
        assert!(Arc::ptr_eq(&svg_fontdb(), &base_svg_db()));
        assert!(Arc::ptr_eq(&svg_fontdb_with(&[]), &base_svg_db()));
    }

    #[test]
    fn an_embedded_family_is_drawn_with_and_the_digest_follows_the_set() {
        let zeist = EmbeddedFont::new("Zeist", shipped_font_renamed(true, "Zeist"));
        assert!(family_of(&svg_fontdb(), "Zeist", 400).is_none(), "nobody has it yet");
        assert!(!has_family("Zeist"));
        assert!(register_embedded(std::slice::from_ref(&zeist)));
        assert!(has_family("Zeist"));
        let one = embedded_digest().expect("a digest");
        assert_eq!(one.len(), 32);
        let db = svg_fontdb();
        assert!(Arc::ptr_eq(&db, &svg_fontdb()), "kept while the set does not change");
        let id = family_of(&db, "Zeist", 400).expect("embedded family found");
        assert_eq!(db.face(id).unwrap().weight, Weight::BOLD, "the bold face this font is");
        // System fonts and Geist are still there, and Geist still stands for sans-serif.
        assert!(has_family(DEFAULT_FAMILY));
        assert!(db
            .query(&Query {
                families: &[Family::SansSerif],
                ..Query::default()
            })
            .is_some());

        // Adding the same font again changes nothing; another one moves the digest and rebuilds.
        assert!(!register_embedded(std::slice::from_ref(&zeist)));
        assert_eq!(embedded_digest().as_deref(), Some(one.as_str()));
        let zeyst = EmbeddedFont::new("Zeyst", shipped_font_renamed(false, "Zeyst"));
        assert!(register_embedded(std::slice::from_ref(&zeyst)));
        let two = embedded_digest().unwrap();
        assert_ne!(two, one);
        assert!(!Arc::ptr_eq(&db, &svg_fontdb()), "rebuilt for the new set");
        assert!(has_family("Zeyst") && has_family("Zeist"));
        assert_eq!(embedded_fonts().len(), 2);

        // Replacing is not adding: the first project's fonts are not the second's.
        assert!(replace_embedded(vec![zeyst.clone()]));
        assert!(!has_family("Zeist") && has_family("Zeyst"));
        assert_ne!(embedded_digest().unwrap(), one);
        assert!(!replace_embedded(vec![zeyst]), "the same set again");
        assert!(replace_embedded(Vec::new()));
        assert_eq!(embedded_digest(), None);
        assert!(!has_family("Zeyst"));
        assert!(Arc::ptr_eq(&svg_fontdb(), &base_svg_db()), "and back to the shared one");
    }

    #[test]
    fn an_embedded_face_wins_over_an_installed_one_of_the_same_family() {
        // Geist Regular is in every database. Embed another file that matches a query just as well (the
        // same face but for a byte of padding at the end): of equal matches the first loaded wins.
        let mut other = REGULAR.to_vec();
        other.push(0);
        let impostor = EmbeddedFont::new("Geist", other.clone());
        assert!(register_embedded(std::slice::from_ref(&impostor)));
        let db = svg_fontdb();
        let id = family_of(&db, "Geist", 400).unwrap();
        match db.face_source(id).unwrap().0 {
            Source::Binary(data) => assert_eq!(data.as_ref().as_ref(), other.as_slice(), "the embedded file"),
            other => panic!("expected the embedded data, got {other:?}"),
        }
        replace_embedded(Vec::new());
        let id = family_of(&svg_fontdb(), "Geist", 400).unwrap();
        match svg_fontdb().face_source(id).unwrap().0 {
            Source::Binary(data) => assert_eq!(data.as_ref().as_ref(), REGULAR, "the shipped one again"),
            other => panic!("expected the shipped data, got {other:?}"),
        }
    }

    #[test]
    fn the_extra_fonts_of_a_poster_are_in_a_database_that_is_registered_nowhere() {
        let zeist = EmbeddedFont::new("Zeist", shipped_font_renamed(true, "Zeist"));
        let db = svg_fontdb_with(std::slice::from_ref(&zeist));
        assert!(family_of(&db, "Zeist", 400).is_some());
        assert_eq!(embedded_digest(), None);
        assert!(family_of(&svg_fontdb(), "Zeist", 400).is_none());
    }

    #[test]
    fn the_registry_is_per_thread_under_test_so_tests_cannot_see_each_other() {
        let zeist = EmbeddedFont::new("Zeist", shipped_font_renamed(true, "Zeist"));
        register_embedded(std::slice::from_ref(&zeist));
        let there = std::thread::spawn(embedded_digest).join().unwrap();
        assert_eq!(there, None);
        replace_embedded(Vec::new());
    }

    #[test]
    fn a_font_file_is_checked_for_what_it_is() {
        let good = Arc::new(shipped_font_renamed(false, "Zeist"));
        assert_eq!(check_font(&good).unwrap().family, "Zeist");
        for (bytes, why) in [
            (b"ttcf\0\0\0\0".to_vec(), "collection"),
            (b"MZ".to_vec(), "TrueType or OpenType"),
            (Vec::new(), "TrueType or OpenType"),
            (good[..40].to_vec(), "could not be read"),
        ] {
            let err = check_font(&Arc::new(bytes)).unwrap_err().to_string();
            assert!(err.contains(why), "{err}");
        }
    }

    #[test]
    fn restricted_embedding_is_read_from_the_os2_table() {
        let font = shipped_font_renamed(false, "Zeist");
        assert!(embedding_allowed(&font), "Geist is an open font");
        for refused in [0x0002u16, 0x0004, 0x0200, 0x0102, 0x0204] {
            assert!(
                !embedding_allowed(&with_fs_type(font.clone(), refused)),
                "fsType {refused:#06x} does not allow carrying the font"
            );
        }
        for allowed in [0u16, 0x0008, 0x0100, 0x0108] {
            assert!(
                embedding_allowed(&with_fs_type(font.clone(), allowed)),
                "fsType {allowed:#06x} allows it"
            );
        }
        assert!(embedding_allowed(b"short"));
    }

    #[test]
    fn a_face_is_looked_up_for_embedding_with_its_limits() {
        let font = EmbeddedFont::new("Zeist", shipped_font_renamed(true, "Zeist"));
        register_embedded(std::slice::from_ref(&font));
        assert_eq!(lookup_face("Zeist", 700, false, 8 << 20), FaceLookup::Found(font.clone()));
        assert_eq!(lookup_face("Nobody Has This", 400, false, 8 << 20), FaceLookup::Missing);
        assert_eq!(
            lookup_face("Zeist", 400, false, 10),
            FaceLookup::TooLarge(font.data.len() as u64)
        );
        assert!(is_generic_family("Sans-Serif") && is_generic_family("monospace") && !is_generic_family("Roboto"));
        replace_embedded(Vec::new());
    }

    #[test]
    fn a_build_in_progress_holds_no_registry_lock() {
        let zeist = EmbeddedFont::new("Zeist", shipped_font_renamed(true, "Zeist"));
        assert!(register_embedded(std::slice::from_ref(&zeist)));
        let set = snapshot();
        let digest = embedded_digest();
        // From inside the build the registry is read, replaced and read again: were it held across the
        // build (it was, once), this would deadlock, or under test panic on the borrow.
        let db = set.db_with(|fonts| {
            assert_eq!(embedded_digest(), digest);
            assert!(replace_embedded(Vec::new()));
            assert_eq!(embedded_digest(), None);
            assert!(register_embedded(&[EmbeddedFont::new(
                "Zeyst",
                shipped_font_renamed(false, "Zeyst")
            )]));
            Arc::new(build_svg_db(fonts.iter()))
        });
        assert!(
            family_of(&db, "Zeist", 400).is_some(),
            "the set it was taken for, not the registry's now"
        );
        assert!(family_of(&db, "Zeyst", 400).is_none());
        assert!(Arc::ptr_eq(&db, &set.db()), "and built once");
        assert_eq!(set.digest(), digest.as_deref());
        replace_embedded(Vec::new());
    }

    #[test]
    fn a_snapshot_is_one_set_for_the_key_and_for_the_drawing() {
        let zeist = EmbeddedFont::new("Zeist", shipped_font_renamed(true, "Zeist"));
        register_embedded(std::slice::from_ref(&zeist));
        let before = snapshot();
        assert_eq!(before.digest(), embedded_digest().as_deref());
        // The registry moves on; the snapshot (key and fonts together) does not.
        let zeyst = EmbeddedFont::new("Zeyst", shipped_font_renamed(false, "Zeyst"));
        assert!(register_embedded(std::slice::from_ref(&zeyst)));
        assert_ne!(before.digest(), embedded_digest().as_deref());
        assert!(family_of(&before.db(), "Zeist", 400).is_some() && family_of(&before.db(), "Zeyst", 400).is_none());
        assert_eq!(before.fonts().len(), 1);
        replace_embedded(Vec::new());
    }

    #[test]
    fn only_the_fonts_a_timeline_names_are_in_the_set_it_is_drawn_with() {
        let zeist = EmbeddedFont::new("Zeist", shipped_font_renamed(true, "Zeist"));
        let zeyst = EmbeddedFont::new("Zeyst", shipped_font_renamed(false, "Zeyst"));
        register_embedded(&[zeist, zeyst]);
        let plain = r#"<svg><text font-family="Geist">a</text></svg>"#;
        let with_zeist = r#"<svg><text style="font-family:'ZEIST', sans-serif">a</text></svg>"#;
        let with_both = r#"<svg><text font-family="Zeist">a</text><set attributeName="font-family" to="Zeyst"/></svg>"#;

        let none = snapshot_for([plain]);
        assert_eq!(
            none.digest(),
            None,
            "no title names an embedded family: the keys are what they were"
        );
        assert!(Arc::ptr_eq(&none.db(), &base_svg_db()));
        assert!(Arc::ptr_eq(&snapshot_for([]).db(), &base_svg_db()));

        let one = snapshot_for([plain, with_zeist]);
        assert_eq!(one.fonts().iter().map(|f| f.family.as_str()).collect::<Vec<_>>(), ["Zeist"]);
        assert!(family_of(&one.db(), "Zeist", 400).is_some() && family_of(&one.db(), "Zeyst", 400).is_none());
        assert!(Arc::ptr_eq(&one, &snapshot_for([with_zeist])), "the same part is built once");

        let both = snapshot_for([with_both]);
        assert_eq!(both.digest(), embedded_digest().as_deref());
        assert!(Arc::ptr_eq(&both, &snapshot()), "all of it is the registry's own set");
        assert_ne!(one.digest(), both.digest());
        replace_embedded(Vec::new());
        assert_eq!(snapshot_for([with_both]).digest(), None, "and nothing registered is nothing");
    }

    #[test]
    fn family_lists_and_names_are_read_as_css_reads_them() {
        assert_eq!(
            family_list(r#" "Barlow Condensed" , 'Open Sans',sans-serif,, "#).collect::<Vec<_>>(),
            ["Barlow Condensed", "Open Sans", "sans-serif"]
        );
        assert!(same_family(" geist ", "GEIST") && !same_family("Geist", "Geist Mono"));
        assert!(
            has_family("geist") && has_family("GEIST"),
            "installed or shipped, whatever the case"
        );
    }

    #[test]
    fn the_pin_replaces_the_path_and_nothing_else() {
        let pinned = with_pinned_default_font(|| resolve_text_font(None, true));
        assert_eq!(pinned.unwrap().path, Path::new("/kerf-test/fonts/Geist-Bold.ttf"));
        let named = with_pinned_default_font(|| resolve_text_font(Some(PINNED_FAMILY), true)).unwrap();
        assert!(!named.bold, "the pinned family has no bold face");
        assert_eq!(resolve_font_file(PINNED_FAMILY, false), None, "and only exists while pinned");
        assert_ne!(
            default_font_file(true),
            Some(PathBuf::from("/kerf-test/fonts/Geist-Bold.ttf"))
        );
    }
}
