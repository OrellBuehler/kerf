//! System font discovery for text overlays. `fontdb` scans the OS's known font
//! directories and parses faces itself (including Linux fontconfig directory
//! lists, via a pure-Rust config parser) — no fontconfig/CoreText/DirectWrite
//! linking, so this works in the `--no-default-features` build too.

use std::path::PathBuf;
use std::sync::OnceLock;

use fontdb::{Database, Family, Query, Source, Weight};

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
    let mut names: Vec<String> =
        db().faces().filter_map(|f| f.families.first().map(|(name, _)| name.clone())).collect();
    names.sort();
    names.dedup();
    names
}

/// Resolve `family` to a font file on disk, preferring a bold face when `bold`
/// is set. Returns the path and whether the matched face actually satisfies
/// the bold request, so the caller knows whether a thickening fallback is
/// still needed. `None` if the family isn't installed.
pub fn resolve_font_file(family: &str, bold: bool) -> Option<(PathBuf, bool)> {
    let weight = if bold { Weight::BOLD } else { Weight::NORMAL };
    let query = Query { families: &[Family::Name(family)], weight, ..Query::default() };
    let face = db().face(db().query(&query)?)?;
    let path = match &face.source {
        Source::File(p) | Source::SharedFile(p, _) => p.clone(),
        Source::Binary(_) => return None,
    };
    Some((path, face.weight >= Weight::BOLD))
}
