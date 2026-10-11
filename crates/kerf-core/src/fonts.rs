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
//! binary ([`DEFAULT_FAMILY`], SIL OFL 1.1, a regular and a bold face), written once to the cache
//! directory because `drawtext` reads a path. The same file on every machine is what makes a
//! preview look like the export it came from, and what lets the plan say which pixels it means.
//! A named family that is not installed falls back to it too.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

/// The shipped font's file, written to the cache directory on first use (its name carries a
/// digest of the bytes, so a file there is the file; one of the wrong length is rewritten).
/// `None` if no directory can be written.
pub fn default_font_file(bold: bool) -> Option<PathBuf> {
    #[cfg(test)]
    if PINNED.with(std::cell::Cell::get) {
        return Some(PathBuf::from(if bold {
            "/kerf-test/fonts/Geist-Bold.ttf"
        } else {
            "/kerf-test/fonts/Geist-Regular.ttf"
        }));
    }
    static FILES: [OnceLock<Option<PathBuf>>; 2] = [OnceLock::new(), OnceLock::new()];
    FILES[usize::from(bold)]
        .get_or_init(|| {
            let (name, bytes) = if bold {
                ("Geist-Bold", BOLD)
            } else {
                ("Geist-Regular", REGULAR)
            };
            let file = format!("{name}-{:08x}.ttf", digest(bytes));
            [
                dirs::cache_dir().map(|d| d.join("kerf").join("fonts")),
                Some(std::env::temp_dir().join("kerf-fonts")),
            ]
            .into_iter()
            .flatten()
            .find_map(|dir| write_font(&dir, &file, bytes))
        })
        .clone()
}

/// FNV-1a, folded to 32 bits: a name for the file, not a security property.
fn digest(bytes: &[u8]) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h ^ (h >> 32)) as u32
}

/// `bytes` as `dir/file`, atomically (another process may be writing the same file); the path when
/// it holds exactly `bytes` afterwards.
fn write_font(dir: &Path, file: &str, bytes: &[u8]) -> Option<PathBuf> {
    let path = dir.join(file);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64) {
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

    #[test]
    fn a_truncated_copy_is_rewritten() {
        let dir = std::env::temp_dir().join(format!("kerf-fonts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.ttf"), &REGULAR[..100]).unwrap();
        let path = write_font(&dir, "a.ttf", REGULAR).expect("rewritten");
        assert_eq!(std::fs::read(path).unwrap(), REGULAR);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no temporary file is left behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(write_font(Path::new("/proc/kerf-no-such-dir"), "a.ttf", REGULAR).is_none());
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
