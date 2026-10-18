//! The `.kerftemplate` zip.
//!
//! ```text
//! manifest.json   TemplateManifest
//! title.svg       the sanitised SMIL SVG
//! fonts/*.ttf|otf the fonts the SVG names
//! poster.png      the title at half its duration (cosmetic)
//! ```
//!
//! **A template file is untrusted input** (it is emailed around). It is read into memory **by known
//! names only** — `manifest.json`, `title.svg`, the font files the manifest lists, `poster.png` — and is
//! never extracted: no entry name becomes a path. Before anything is read the entry count, every name
//! (no separator tricks, no duplicates) and the sum of the declared sizes are checked, and an archive
//! whose entries share bytes is refused; each entry read is capped and must be exactly as long as its
//! header said. Anything in the file that is not a known name is ignored.

use std::io::{Cursor, Read, Seek};
use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

use super::model::{TemplateManifest, FORMAT};
use super::validate::{check_version, invalid, validate_controls, validate_manifest};
use super::{EXTENSION, MAX_ENTRIES, MAX_FONT_BYTES, MAX_MANIFEST_BYTES, MAX_POSTER_BYTES, MAX_TOTAL_BYTES, POSTER_SIZE};
use crate::error::{Error, Result};
use crate::fonts::{self, EmbeddedFont};
use crate::svg::{raster, SvgDoc};

const MANIFEST: &str = "manifest.json";
const TITLE: &str = "title.svg";
const POSTER: &str = "poster.png";

/// The widest a poster may be before it is taken for a decompression bomb for the grid.
const MAX_POSTER_SIDE: usize = 2048;

/// A template in memory: its manifest, the sanitised title, the fonts (in the manifest's order) and
/// the poster.
#[derive(Debug, Clone)]
pub struct Template {
    pub manifest: TemplateManifest,
    pub svg: String,
    /// The fonts, one per `manifest.fonts` entry and in that order.
    pub fonts: Vec<EmbeddedFont>,
    pub poster: Option<Vec<u8>>,
}

/// What [`Template::validate`] found.
#[derive(Debug)]
pub struct Checked {
    /// The title, read (the sanitiser and the SMIL reader have been through it).
    pub doc: SvgDoc,
    /// What the sanitiser could not honour.
    pub warnings: Vec<String>,
}

fn not_a_template(why: impl std::fmt::Display) -> Error {
    invalid(format!("not a Kerf template file: {why}"))
}

/// An entry name that is a plain relative path: no `..` or `.` part, no leading `/`, no backslash,
/// drive letter or NUL. (Nothing is extracted, so this is a second wall, not the first.)
fn entry_name_ok(name: &str) -> bool {
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    !trimmed.is_empty()
        && name.len() <= 200
        && !name.starts_with('/')
        && !name.contains(['\\', '\0', ':'])
        && trimmed.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

struct Opened<R: Read + Seek> {
    zip: ZipArchive<R>,
}

impl<R: Read + Seek> Opened<R> {
    /// The central directory and nothing else: names, count and sizes are checked here.
    fn open(reader: R) -> Result<Self> {
        let mut zip = ZipArchive::new(reader).map_err(not_a_template)?;
        if zip.len() > MAX_ENTRIES {
            return Err(invalid(format!(
                "the template has {} entries; at most {MAX_ENTRIES} are allowed",
                zip.len()
            )));
        }
        let mut names = std::collections::HashSet::new();
        let mut total = 0u64;
        for i in 0..zip.len() {
            let entry = zip.by_index_raw(i).map_err(not_a_template)?;
            let name = entry.name().map_err(not_a_template)?.into_owned();
            if !entry_name_ok(&name) {
                return Err(invalid(format!("the template has an entry with an unsafe name ({name:?})")));
            }
            if entry.encrypted() {
                return Err(invalid("the template has an encrypted entry"));
            }
            if !names.insert(name.clone()) {
                return Err(invalid(format!("the template lists “{name}” twice")));
            }
            total = total.saturating_add(entry.size());
            if total > MAX_TOTAL_BYTES {
                return Err(invalid(format!(
                    "the template is more than {} MiB uncompressed",
                    MAX_TOTAL_BYTES >> 20
                )));
            }
        }
        if zip.has_overlapping_entries().map_err(not_a_template)? {
            return Err(invalid("the template's entries overlap each other (a malformed archive)"));
        }
        Ok(Self { zip })
    }

    /// The bytes of the entry `name`, or `None` when the archive has no such entry. At most `max`
    /// bytes, and exactly as many as the header declared.
    fn entry(&mut self, name: &str, max: u64) -> Result<Option<Vec<u8>>> {
        let Some(index) = self.zip.index_for_name(name) else {
            return Ok(None);
        };
        let file = self.zip.by_index(index).map_err(not_a_template)?;
        if !file.is_file() {
            return Err(invalid(format!("“{name}” in the template is not a file")));
        }
        let declared = file.size();
        if declared > max {
            return Err(invalid(format!(
                "“{name}” in the template is {declared} bytes; at most {max} are allowed"
            )));
        }
        let mut bytes = Vec::with_capacity(declared as usize);
        file.take(max + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| invalid(format!("“{name}” in the template could not be read: {e}")))?;
        if bytes.len() as u64 != declared {
            return Err(invalid(format!("“{name}” in the template is not the size its header says")));
        }
        Ok(Some(bytes))
    }

    fn required(&mut self, name: &str, max: u64) -> Result<Vec<u8>> {
        self.entry(name, max)?
            .ok_or_else(|| invalid(format!("the template has no {name}")))
    }

    /// `manifest.json`, with the format and the version looked at before the rest is parsed so that a
    /// file from a newer Kerf is told apart from one that is broken.
    fn manifest(&mut self) -> Result<TemplateManifest> {
        let bytes = self.required(MANIFEST, MAX_MANIFEST_BYTES)?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| invalid(format!("the template's manifest is not valid JSON: {e}")))?;
        if value.get("format").and_then(|f| f.as_str()) != Some(FORMAT) {
            return Err(invalid("not a Kerf template (its manifest has no kerf-template format)"));
        }
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| invalid("the template's manifest has no version"))?;
        check_version(u32::try_from(version).unwrap_or(u32::MAX))?;
        let manifest: TemplateManifest =
            serde_json::from_value(value).map_err(|e| invalid(format!("the template's manifest is not valid: {e}")))?;
        validate_manifest(&manifest)?;
        Ok(manifest)
    }
}

/// A poster worth showing: a PNG that is not absurdly big. Anything else is no poster.
fn poster_ok(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && imagesize::blob_size(bytes)
            .is_ok_and(|s| (1..=MAX_POSTER_SIDE).contains(&s.width) && (1..=MAX_POSTER_SIDE).contains(&s.height))
}

impl Template {
    /// Read a template from `bytes`. See the module notes for what is checked; the title is **not**
    /// sanitised here (that is [`Template::validate`]'s job and costs seconds for a heavy one).
    pub fn read(bytes: &[u8]) -> Result<Template> {
        Self::read_from(Cursor::new(bytes))
    }

    /// [`Template::read`] from a file. A file far past the size a template can have is refused unread.
    pub fn read_file(path: &Path) -> Result<Template> {
        Self::read_from(Self::open_file(path)?)
    }

    fn open_file(path: &Path) -> Result<std::fs::File> {
        let file = std::fs::File::open(path)?;
        // Stored entries make the file as big as its parts, plus a header each.
        if file.metadata()?.len() > MAX_TOTAL_BYTES + (1 << 20) {
            return Err(invalid(format!(
                "the file is more than {} MiB: not a template",
                MAX_TOTAL_BYTES >> 20
            )));
        }
        Ok(file)
    }

    pub fn read_from<R: Read + Seek>(reader: R) -> Result<Template> {
        let mut opened = Opened::open(reader)?;
        let mut manifest = opened.manifest()?;
        let svg_bytes = opened.required(TITLE, crate::svg::MAX_BYTES as u64)?;
        let svg = String::from_utf8(svg_bytes).map_err(|_| invalid("the template's title.svg is not UTF-8 text"))?;
        let mut fonts = Vec::with_capacity(manifest.fonts.len());
        for entry in &mut manifest.fonts {
            let data = Arc::new(opened.required(&entry.file, MAX_FONT_BYTES)?);
            let check = fonts::check_font(&data).map_err(|e| invalid(format!("the font “{}”: {e}", entry.file)))?;
            // `save` leaves such a font out; a file that carries one anyway (made elsewhere, or by hand) is
            // refused whole, as one with a font that cannot be read is: dropping it would hand back a
            // template that draws differently from the one that was sent.
            if !fonts::embedding_allowed(&data) {
                return Err(invalid(format!(
                    "the font “{}” does not allow embedding (its licence is restricted), so a template cannot carry it",
                    entry.file
                )));
            }
            // What a person reads is the family the file really is.
            entry.family = check.family;
            fonts.push(EmbeddedFont {
                sha256: fonts::sha256_hex(&data),
                family: entry.family.clone(),
                data,
            });
        }
        let poster = opened.entry(POSTER, MAX_POSTER_BYTES).ok().flatten().filter(|p| poster_ok(p));
        Ok(Template {
            manifest,
            svg,
            fonts,
            poster,
        })
    }

    /// Only the manifest, and whether there is a poster: what the library list needs, without reading
    /// the title or the fonts.
    pub fn read_manifest(path: &Path) -> Result<(TemplateManifest, bool, u64)> {
        let file = Self::open_file(path)?;
        let size = file.metadata()?.len();
        let mut opened = Opened::open(file)?;
        let manifest = opened.manifest()?;
        let has_poster = opened.zip.index_for_name(POSTER).is_some();
        Ok((manifest, has_poster, size))
    }

    /// The poster alone, from a file.
    pub fn read_poster(path: &Path) -> Result<Option<Vec<u8>>> {
        let mut opened = Opened::open(Self::open_file(path)?)?;
        Ok(opened.entry(POSTER, MAX_POSTER_BYTES)?.filter(|p| poster_ok(p)))
    }

    /// Check this template as a whole and bring it to the form it is stored in: the manifest field by
    /// field, the title through the sanitiser (`self.svg` becomes the sanitised text) and read, the
    /// controls against that title, and the fonts against the manifest. The sanitiser takes up to
    /// a second and a half on a heavy title: not for under the project lock.
    pub fn validate(&mut self) -> Result<Checked> {
        validate_manifest(&self.manifest)?;
        if self.fonts.len() != self.manifest.fonts.len() {
            return Err(invalid("the template's fonts do not match its manifest"));
        }
        let doc = SvgDoc::parse(&self.svg)?;
        self.svg = doc.sanitized().to_string();
        validate_controls(&self.manifest.controls, &self.svg)?;
        let warnings = doc.warnings().to_vec();
        Ok(Checked { doc, warnings })
    }

    /// The title drawn at half the template's duration, as a PNG, with this template's own fonts
    /// (`doc` is [`Template::validate`]'s answer). At most [`POSTER_SIZE`] pixels on the long side.
    pub fn render_poster(&self, doc: &SvgDoc) -> Result<Vec<u8>> {
        let m = &self.manifest;
        let scale = f64::from(POSTER_SIZE) / f64::from(m.width.max(m.height));
        let side = |v: u32| ((f64::from(v) * scale).round() as u32).clamp(1, POSTER_SIZE);
        let svg = doc.at(m.duration / 2.0);
        let pixmap = raster::render_with(&svg, side(m.width), side(m.height), 1.0, fonts::svg_fontdb_with(&self.fonts))?;
        raster::encode_png(&pixmap)
    }

    /// A digest of what the template *is* — manifest, title and fonts, not the poster — so two files
    /// of the same template can be told from two that merely share an id.
    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        let part = |hash: &mut Sha256, bytes: &[u8]| {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        };
        part(&mut hash, &serde_json::to_vec(&self.manifest).unwrap_or_default());
        part(&mut hash, self.svg.as_bytes());
        for font in &self.fonts {
            part(&mut hash, font.sha256.as_bytes());
        }
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The `.kerftemplate` file: byte for byte the same for the same template (fixed timestamps, a fixed
    /// order), so a library file compares equal to its re-export.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        validate_manifest(&self.manifest)?;
        if self.fonts.len() != self.manifest.fonts.len() {
            return Err(invalid("the template's fonts do not match its manifest"));
        }
        let manifest = serde_json::to_vec_pretty(&self.manifest)?;
        let fixed = || {
            SimpleFileOptions::default()
                .last_modified_time(DateTime::default())
                .unix_permissions(0o644)
        };
        let deflated = fixed().compression_method(CompressionMethod::Deflated);
        let stored = fixed().compression_method(CompressionMethod::Stored);
        let total = manifest.len() as u64
            + self.svg.len() as u64
            + self.fonts.iter().map(|f| f.data.len() as u64).sum::<u64>()
            + self.poster.as_ref().map_or(0, |p| p.len() as u64);
        if total > MAX_TOTAL_BYTES {
            return Err(invalid(format!(
                "the template would be more than {} MiB",
                MAX_TOTAL_BYTES >> 20
            )));
        }
        let failed = |e: zip::result::ZipError| Error::Other(format!("could not write the template: {e}"));
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let mut add = |name: &str, options: SimpleFileOptions, bytes: &[u8]| -> Result<()> {
            use std::io::Write;
            zip.start_file(name, options).map_err(failed)?;
            zip.write_all(bytes)?;
            Ok(())
        };
        add(MANIFEST, deflated, &manifest)?;
        add(TITLE, deflated, self.svg.as_bytes())?;
        for (entry, font) in self.manifest.fonts.iter().zip(&self.fonts) {
            add(&entry.file, deflated, &font.data)?;
        }
        if let Some(poster) = &self.poster {
            add(POSTER, stored, poster)?;
        }
        Ok(zip.finish().map_err(failed)?.into_inner())
    }

    /// The name this template is exported under: its name made safe for a file system, then the
    /// extension. A name Windows reserves for a device (`con`, `nul`, `com1` ... also with an extension
    /// after them) gets an underscore in front, so the file can be made on every system.
    pub fn file_name(&self) -> String {
        let name: String = self
            .manifest
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = name.trim().trim_matches('.').trim();
        let name = if name.is_empty() { "template" } else { name };
        if windows_reserved(name) {
            format!("_{name}.{EXTENSION}")
        } else {
            format!("{name}.{EXTENSION}")
        }
    }
}

/// Whether Windows takes `name` for a device: `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`,
/// whatever the case and whatever follows the first dot (`con.txt` is the console too).
fn windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || stem
            .strip_prefix("com")
            .or_else(|| stem.strip_prefix("lpt"))
            .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;
    use crate::template::model::{ManifestFont, TemplateControl, TemplateKind, TemplateTarget, VERSION};

    pub(crate) const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100">
<rect id="bg" width="200" height="100" fill="#102030"/>
<text id="tag" x="10" y="60" font-size="24" font-family="Geist" fill="#ffffff"><animate attributeName="opacity" from="0" to="1" dur="1s" fill="freeze"/>Hello</text>
</svg>"##;

    pub(crate) fn manifest() -> TemplateManifest {
        TemplateManifest {
            format: FORMAT.to_string(),
            version: VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            name: "Kitefun end card".to_string(),
            description: "A card".to_string(),
            duration: 4.0,
            width: 200,
            height: 100,
            controls: vec![
                TemplateControl {
                    id: "tagline".into(),
                    label: "Tagline".into(),
                    kind: TemplateKind::Text,
                    targets: vec![TemplateTarget {
                        element: "tag".into(),
                        attribute: None,
                    }],
                    min: None,
                    max: None,
                },
                TemplateControl {
                    id: "brand".into(),
                    label: "Brand colour".into(),
                    kind: TemplateKind::Color,
                    targets: vec![TemplateTarget {
                        element: "bg".into(),
                        attribute: Some("fill".into()),
                    }],
                    min: None,
                    max: None,
                },
            ],
            fonts: Vec::new(),
        }
    }

    pub(crate) fn template() -> Template {
        Template {
            manifest: manifest(),
            svg: SVG.to_string(),
            fonts: Vec::new(),
            poster: None,
        }
    }

    /// A zip with the given entries, deflated, in order.
    pub(crate) fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, bytes) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn good_entries(m: &TemplateManifest) -> Vec<(String, Vec<u8>)> {
        vec![
            (MANIFEST.to_string(), serde_json::to_vec(m).unwrap()),
            (TITLE.to_string(), SVG.as_bytes().to_vec()),
        ]
    }

    fn zip_with(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
        let borrowed: Vec<(&str, &[u8])> = entries.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
        zip_of(&borrowed)
    }

    fn err(bytes: &[u8]) -> String {
        Template::read(bytes).unwrap_err().to_string()
    }

    #[test]
    fn a_template_round_trips_and_the_file_is_the_same_bytes_every_time() {
        let mut t = template();
        t.poster = Some(raster::encode_png(&tiny_skia::Pixmap::new(4, 4).unwrap()).unwrap());
        let bytes = t.to_bytes().unwrap();
        assert_eq!(bytes, t.to_bytes().unwrap(), "deterministic");
        let back = Template::read(&bytes).unwrap();
        assert_eq!(back.manifest, t.manifest);
        assert_eq!(back.svg, t.svg);
        assert_eq!(back.poster, t.poster);
        assert_eq!(back.digest(), t.digest());
        assert_eq!(back.to_bytes().unwrap(), bytes, "and a read-and-write changes nothing");
    }

    #[test]
    fn fonts_round_trip_and_the_family_is_the_one_in_the_file() {
        let data = crate::fonts::shipped_font_renamed(true, "Zeist");
        let mut t = template();
        t.manifest.fonts = vec![ManifestFont {
            file: "fonts/Zeist-Bold.ttf".into(),
            family: "A wrong label".into(),
        }];
        t.fonts = vec![EmbeddedFont::new("A wrong label", data.clone())];
        let back = Template::read(&t.to_bytes().unwrap()).unwrap();
        assert_eq!(back.manifest.fonts[0].family, "Zeist");
        assert_eq!(back.fonts[0].family, "Zeist");
        assert_eq!(*back.fonts[0].data, data);
        assert_eq!(back.fonts[0].sha256, crate::fonts::sha256_hex(&data));
    }

    #[test]
    fn a_font_that_does_not_allow_embedding_is_refused_when_a_file_is_read() {
        let with_font = |fs_type: u16| {
            let mut t = template();
            t.manifest.fonts = vec![ManifestFont {
                file: "fonts/Zeist.ttf".into(),
                family: "Zeist".into(),
            }];
            t.fonts = vec![EmbeddedFont::new(
                "Zeist",
                crate::fonts::with_fs_type(crate::fonts::shipped_font_renamed(true, "Zeist"), fs_type),
            )];
            t.to_bytes().unwrap()
        };
        for refused in [0x0002u16, 0x0004, 0x0200] {
            let e = err(&with_font(refused));
            assert!(
                e.contains("fonts/Zeist.ttf") && e.contains("does not allow embedding"),
                "{refused:#06x}: {e}"
            );
        }
        for allowed in [0u16, 0x0008] {
            assert!(Template::read(&with_font(allowed)).is_ok(), "{allowed:#06x}");
        }
    }

    #[test]
    fn what_is_not_a_zip_or_not_a_template_is_refused_with_a_reason() {
        assert!(err(b"hello").contains("not a Kerf template file"));
        assert!(err(&[]).contains("not a Kerf template file"));
        let m = manifest();
        let mut entries = good_entries(&m);
        entries.remove(0);
        assert!(err(&zip_with(&entries)).contains("no manifest.json"));
        let mut entries = good_entries(&m);
        entries.remove(1);
        assert!(err(&zip_with(&entries)).contains("no title.svg"));
        let mut entries = good_entries(&m);
        entries[0].1 = b"{ nope".to_vec();
        assert!(err(&zip_with(&entries)).contains("not valid JSON"));
        let mut entries = good_entries(&m);
        entries[0].1 = serde_json::to_vec(&serde_json::json!({"format": "other", "version": 1})).unwrap();
        assert!(err(&zip_with(&entries)).contains("not a Kerf template"));
        let mut entries = good_entries(&m);
        entries[1].1 = vec![0xff, 0xfe, 0xfd];
        assert!(err(&zip_with(&entries)).contains("UTF-8"));
    }

    #[test]
    fn another_version_is_refused_by_name_and_a_newer_one_says_so() {
        let mut value = serde_json::to_value(manifest()).unwrap();
        value["version"] = serde_json::json!(2);
        let mut entries = good_entries(&manifest());
        entries[0].1 = serde_json::to_vec(&value).unwrap();
        let message = err(&zip_with(&entries));
        assert!(message.contains("version 2") && message.contains("newer Kerf"), "{message}");
        // Unknown fields of a newer version do not get that far: the version is read first.
        value["controls"] = serde_json::json!("not a list");
        entries[0].1 = serde_json::to_vec(&value).unwrap();
        assert!(err(&zip_with(&entries)).contains("newer Kerf"));
        value["version"] = serde_json::json!(0);
        entries[0].1 = serde_json::to_vec(&value).unwrap();
        assert!(err(&zip_with(&entries)).contains("not supported"));
        value["version"] = serde_json::json!("1");
        entries[0].1 = serde_json::to_vec(&value).unwrap();
        assert!(err(&zip_with(&entries)).contains("no version"));
    }

    #[test]
    fn a_bad_manifest_is_refused_whole() {
        let mut m = manifest();
        m.controls[0].id = "a b".into();
        assert!(err(&zip_with(&good_entries(&m))).contains("not a control id"));
        let mut m = manifest();
        m.duration = -1.0;
        assert!(err(&zip_with(&good_entries(&m))).contains("duration"));
    }

    #[test]
    fn too_many_entries_are_refused() {
        let m = manifest();
        let mut entries = good_entries(&m);
        for i in 0..MAX_ENTRIES {
            entries.push((format!("extra/{i}.txt"), vec![b'x']));
        }
        assert!(err(&zip_with(&entries)).contains("entries"));
        // Exactly at the limit is fine.
        let mut entries = good_entries(&m);
        for i in 0..MAX_ENTRIES - 2 {
            entries.push((format!("extra/{i}.txt"), vec![b'x']));
        }
        assert!(Template::read(&zip_with(&entries)).is_ok());
    }

    #[test]
    fn sizes_are_capped_before_anything_is_decompressed() {
        let m = manifest();
        // 33 MiB of zeros deflates to a few dozen KiB: the declared sizes are what is summed.
        let mut entries = good_entries(&m);
        for i in 0..33 {
            entries.push((format!("pad/{i}.bin"), vec![0u8; 1 << 20]));
        }
        let bytes = zip_with(&entries);
        assert!(bytes.len() < 200_000, "a bomb: {} bytes", bytes.len());
        assert!(err(&bytes).contains("MiB uncompressed"));
        // One entry over its own cap: the manifest, a font, the title.
        let mut entries = good_entries(&m);
        entries[0].1 = vec![b' '; MAX_MANIFEST_BYTES as usize + 1];
        assert!(err(&zip_with(&entries)).contains("manifest.json"));
        let mut entries = good_entries(&m);
        entries[1].1 = vec![b' '; crate::svg::MAX_BYTES + 1];
        assert!(err(&zip_with(&entries)).contains("title.svg"));
        let mut with_font = manifest();
        with_font.fonts = vec![ManifestFont {
            file: "fonts/Big.ttf".into(),
            family: "Big".into(),
        }];
        let mut entries = good_entries(&with_font);
        entries.push(("fonts/Big.ttf".into(), vec![0u8; MAX_FONT_BYTES as usize + 1]));
        assert!(err(&zip_with(&entries)).contains("fonts/Big.ttf"));
    }

    #[test]
    fn an_entry_longer_than_its_header_says_is_refused() {
        let bytes = zip_of(&[(MANIFEST, &serde_json::to_vec(&manifest()).unwrap()), (TITLE, SVG.as_bytes())]);
        // Lower the declared uncompressed size of the first central-directory record.
        let mut lying = bytes;
        let at = lying.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        let size = u32::from_le_bytes(lying[at + 24..at + 28].try_into().unwrap());
        lying[at + 24..at + 28].copy_from_slice(&(size / 2).to_le_bytes());
        assert!(Template::read(&lying).is_err());
    }

    #[test]
    fn unsafe_and_repeated_names_are_refused_and_unknown_entries_are_ignored() {
        let m = manifest();
        for name in ["../evil.txt", "/abs.txt", "a/../../b", "a\\b", "C:/x", "./x", "a//b"] {
            let mut entries = good_entries(&m);
            entries.push((name.to_string(), b"x".to_vec()));
            assert!(err(&zip_with(&entries)).contains("unsafe name"), "{name}");
        }
        let mut entries = good_entries(&m);
        entries.push(("notes/readme.txt".to_string(), b"hello".to_vec()));
        entries.push(("fonts/unlisted.ttf".to_string(), b"not even a font".to_vec()));
        let t = Template::read(&zip_with(&entries)).unwrap();
        assert!(t.fonts.is_empty(), "an unlisted font is never read");
        assert!(entry_name_ok("fonts/a b.ttf") && entry_name_ok("dir/"));
    }

    #[test]
    fn a_listed_font_must_be_there_and_be_a_font() {
        let mut m = manifest();
        m.fonts = vec![ManifestFont {
            file: "fonts/A.ttf".into(),
            family: "A".into(),
        }];
        assert!(err(&zip_with(&good_entries(&m))).contains("no fonts/A.ttf"));
        let mut entries = good_entries(&m);
        entries.push(("fonts/A.ttf".into(), b"MZ this is not a font".to_vec()));
        assert!(err(&zip_with(&entries)).contains("not a TrueType or OpenType font"));
        let mut entries = good_entries(&m);
        entries.push(("fonts/A.ttf".into(), b"ttcf\0\0\0\0".to_vec()));
        assert!(err(&zip_with(&entries)).contains("collection"));
        let mut truncated = crate::fonts::shipped_font_renamed(false, "Zeist");
        truncated.truncate(64);
        let mut entries = good_entries(&m);
        entries.push(("fonts/A.ttf".into(), truncated));
        assert!(err(&zip_with(&entries)).contains("fonts/A.ttf"));
    }

    #[test]
    fn a_poster_that_is_not_a_small_png_is_no_poster() {
        let mut entries = good_entries(&manifest());
        entries.push((POSTER.to_string(), b"not a png".to_vec()));
        assert!(Template::read(&zip_with(&entries)).unwrap().poster.is_none());
        let big = crate::svg::images::tests::png_claiming(5000, 5000);
        entries[2].1 = big;
        assert!(Template::read(&zip_with(&entries)).unwrap().poster.is_none());
    }

    #[test]
    fn validate_sanitises_the_title_and_checks_the_controls_against_it() {
        let mut t = template();
        t.svg = SVG.replace("<rect", "<script>alert(1)</script><rect onclick=\"x()\"");
        let checked = t.validate().unwrap();
        assert!(!t.svg.contains("script") && !t.svg.contains("onclick"));
        assert!(!checked.warnings.is_empty() || !t.svg.contains("script"));
        let mut missing = template();
        missing.manifest.controls[0].targets[0].element = "nowhere".into();
        assert!(missing.validate().unwrap_err().to_string().contains("nowhere"));
        let mut no_size = template();
        no_size.svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><text id="tag">x</text></svg>"#.into();
        assert!(no_size.validate().is_err());
    }

    #[test]
    fn the_file_name_is_the_name_made_safe() {
        let mut t = template();
        t.manifest.name = "../Kite/fun: end*card".into();
        assert_eq!(t.file_name(), "_Kite_fun_ end_card.kerftemplate");
        t.manifest.name = "...".into();
        assert_eq!(t.file_name(), "template.kerftemplate");
    }

    #[test]
    fn a_name_windows_reserves_for_a_device_is_not_the_file_name() {
        let mut t = template();
        for (name, file) in [
            ("con", "_con.kerftemplate"),
            ("NUL", "_NUL.kerftemplate"),
            ("Prn", "_Prn.kerftemplate"),
            ("aux", "_aux.kerftemplate"),
            ("COM1", "_COM1.kerftemplate"),
            ("lpt9", "_lpt9.kerftemplate"),
            ("con.txt", "_con.txt.kerftemplate"),
            (" Com3 .x", "_Com3 .x.kerftemplate"),
            ("COM0", "COM0.kerftemplate"),
            ("COM10", "COM10.kerftemplate"),
            ("console", "console.kerftemplate"),
            ("my con", "my con.kerftemplate"),
            ("Nullify", "Nullify.kerftemplate"),
        ] {
            t.manifest.name = name.into();
            assert_eq!(t.file_name(), file, "{name:?}");
        }
    }
}
