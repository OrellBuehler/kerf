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

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use fontdb::{Database, Family, Query, Source, Weight};

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
    private_cache_dir(base, "kerf-fonts")
}

/// [`private_dir`] under another name (`<prefix>-<uid>`), for the other files Kerf writes for FFmpeg
/// to read (the LUT copies, `color_lut`).
pub(crate) fn private_cache_dir(base: &Path, prefix: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        let dir = base.join(format!("{prefix}-{uid}"));
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
        let meta = std::fs::symlink_metadata(&dir).ok()?;
        (meta.is_dir() && meta.uid() == uid && meta.mode() & 0o022 == 0).then_some(dir)
    }
    #[cfg(not(unix))]
    {
        let dir = base.join(prefix);
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
