//! The user's template library: a directory of `<id>.kerftemplate` files.
//!
//! `dirs::data_dir()/kerf/templates` (`KERF_TEMPLATES_DIR` overrides, read on every call). **Library
//! writes are app-level, not timeline edits** — they are not undone, not staged and not part of any
//! project; applying a template is the edit (`Project::apply_template_parsed`), and it copies what it
//! needs, so a project never reaches back here.
//!
//! Every write is atomic (a temp file beside the target, fsync, rename), and a file that is damaged
//! costs the list one entry, not the list: it comes back in [`TemplateList::problems`].
//!
//! The library is bounded: [`MAX_TEMPLATES`] templates and [`MAX_BYTES`] in all. A file is a library
//! entry only under its exact name, `<id>.kerftemplate` (lower case, as every path here is made), so what
//! the list shows is what `get`, `delete` and `export` can reach.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::archive::Template;
use super::model::{TemplateControl, TemplateManifest};
use super::save::{build_template, TemplateSaveRequest, TemplateSource};
use super::validate::{check_id, invalid};
use super::EXTENSION;
use crate::error::{Error, Result};

/// Most templates the library holds. A grid of five hundred is more than anyone browses, and every
/// listing reads one manifest per file.
pub const MAX_TEMPLATES: usize = 500;
/// Most bytes the library holds, all files together (a template is at most 32 MiB; this is sixty-odd of
/// the biggest, a hundred times as many of the usual).
pub const MAX_BYTES: u64 = 2 << 30;

/// The temp files a write leaves beside its target while it runs: `.kerf-tmp-<random>.tmp`.
const TEMP_PREFIX: &str = ".kerf-tmp-";
const TEMP_SUFFIX: &str = ".tmp";
/// A temp file older than this is the remains of a write that never finished (a crash, a power cut): the
/// listing removes it. A write takes seconds.
const STALE_TEMP: Duration = Duration::from_secs(10 * 60);

/// The library directory: `KERF_TEMPLATES_DIR` when set and not empty, else `<data dir>/kerf/templates`.
/// `None` only on a platform with no data directory and no override.
pub fn library_dir() -> Option<PathBuf> {
    dir_from(std::env::var_os("KERF_TEMPLATES_DIR"), dirs::data_dir())
}

/// [`library_dir`] for a given override and data directory.
fn dir_from(over: Option<std::ffi::OsString>, data: Option<PathBuf>) -> Option<PathBuf> {
    match over.filter(|v| !v.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => data.map(|d| d.join("kerf").join("templates")),
    }
}

/// A template in the library, as the grid shows it. The poster is read separately
/// ([`Library::poster`]): a list should not carry a picture per row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub controls: Vec<TemplateControl>,
    /// The families of the fonts it carries.
    pub fonts: Vec<String>,
    pub has_poster: bool,
    /// The size of the file.
    pub size_bytes: u64,
}

impl TemplateInfo {
    fn of(manifest: &TemplateManifest, has_poster: bool, size_bytes: u64) -> Self {
        Self {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            description: manifest.description.clone(),
            duration: manifest.duration,
            width: manifest.width,
            height: manifest.height,
            controls: manifest.controls.clone(),
            fonts: manifest.fonts.iter().map(|f| f.family.clone()).collect(),
            has_poster,
            size_bytes,
        }
    }
}

/// A file in the library that could not be read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateProblem {
    /// The file name.
    pub file: String,
    pub error: String,
}

/// The library: the templates that read, by name, and the files that did not.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateList {
    pub templates: Vec<TemplateInfo>,
    pub problems: Vec<TemplateProblem>,
}

/// A template saved into the library, with what could not be carried over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateSaved {
    pub info: TemplateInfo,
    pub warnings: Vec<String>,
}

/// What importing a file did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    /// A new entry, with the id it came with.
    Added,
    /// The library had this very template (same id, same content): nothing was written.
    AlreadyPresent,
    /// The library has another template with this id, so this one was kept under a new id rather than
    /// replacing it.
    AddedAsCopy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportOutcome {
    pub info: TemplateInfo,
    pub status: ImportStatus,
    pub warnings: Vec<String>,
}

/// A directory of templates.
#[derive(Debug, Clone)]
pub struct Library {
    dir: PathBuf,
}

impl Library {
    /// The library in `dir`.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The user's library ([`library_dir`]).
    pub fn default_location() -> Result<Self> {
        library_dir().map(Self::at).ok_or_else(|| {
            Error::Other("this platform has no data directory to keep templates in (set KERF_TEMPLATES_DIR)".into())
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the template `id` lives. The id is checked first: it becomes a file name.
    pub fn path_of(&self, id: &str) -> Result<PathBuf> {
        check_id(id)?;
        Ok(self.dir.join(format!("{id}.{EXTENSION}")))
    }

    fn missing(id: &str) -> Error {
        invalid(format!("there is no template with the id {id} in the library"))
    }

    /// Every template that reads, by name, and every file that does not. Only the manifest of each is
    /// read. A directory that does not exist yet is an empty library. Temp files of a write that never
    /// finished are removed on the way.
    pub fn list(&self) -> Result<TemplateList> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TemplateList::default()),
            Err(e) => return Err(e.into()),
        };
        let mut list = TemplateList::default();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            if is_temp_name(&file) {
                remove_if_stale(&path);
                continue;
            }
            // Exactly the extension `path_of` writes: `get`, `delete` and `export` reach what is listed.
            let is_template = path.extension().is_some_and(|e| e == EXTENSION);
            if file.starts_with('.') || !is_template || !path.is_file() {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            let read = Template::read_manifest(&path).and_then(|(manifest, has_poster, size)| {
                if manifest.id == stem {
                    Ok(TemplateInfo::of(&manifest, has_poster, size))
                } else {
                    Err(invalid(format!(
                        "the file is named for “{stem}” but the template inside is {}: import it instead",
                        manifest.id
                    )))
                }
            });
            match read {
                Ok(info) => list.templates.push(info),
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "a template in the library could not be read");
                    list.problems.push(TemplateProblem {
                        file,
                        error: e.to_string(),
                    });
                }
            }
        }
        list.templates.sort_by_cached_key(|t| (t.name.to_lowercase(), t.id.clone()));
        list.problems.sort_by(|a, b| a.file.cmp(&b.file));
        Ok(list)
    }

    /// The template `id`, read whole (not yet sanitised: [`Template::prepare`] does that on its way in).
    pub fn get(&self, id: &str) -> Result<Template> {
        let path = self.path_of(id)?;
        if !path.is_file() {
            return Err(Self::missing(id));
        }
        let template = Template::read_file(&path)?;
        if template.manifest.id != id {
            return Err(invalid(format!(
                "the file for {id} holds the template {}",
                template.manifest.id
            )));
        }
        Ok(template)
    }

    /// The poster of the template `id` (a PNG), `None` when it has none.
    pub fn poster(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path_of(id)?;
        if !path.is_file() {
            return Err(Self::missing(id));
        }
        Template::read_poster(&path)
    }

    /// Write `template` into the library under its own id, replacing a file of that id. Atomic.
    /// Returns the size written. A library at its limit ([`MAX_TEMPLATES`] templates, [`MAX_BYTES`] bytes)
    /// takes nothing more: delete something first. (Replacing an entry never grows the count.)
    pub fn add(&self, template: &Template) -> Result<u64> {
        let path = self.path_of(&template.manifest.id)?;
        let bytes = template.to_bytes()?;
        self.check_room(&path, bytes.len() as u64)?;
        write_atomic(&path, &bytes)?;
        Ok(bytes.len() as u64)
    }

    /// Whether `incoming` bytes fit as the file `path` (which may replace one of that name).
    fn check_room(&self, path: &Path, incoming: u64) -> Result<()> {
        self.check_room_within(path, incoming, MAX_TEMPLATES, MAX_BYTES)
    }

    fn check_room_within(&self, path: &Path, incoming: u64, max_templates: usize, max_bytes: u64) -> Result<()> {
        let replaced = std::fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len());
        let (mut count, mut total) = (0usize, 0u64);
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || !entry.path().extension().is_some_and(|e| e == EXTENSION) {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        count += 1;
                        total = total.saturating_add(meta.len());
                    }
                }
            }
        }
        if replaced.is_none() && count >= max_templates {
            return Err(invalid(format!(
                "the template library holds {count} templates, which is as many as it keeps ({max_templates}): delete some before adding more"
            )));
        }
        if total.saturating_sub(replaced.unwrap_or(0)).saturating_add(incoming) > max_bytes {
            return Err(invalid(format!(
                "the template library is {} MiB, and this would take it past the {} MiB it keeps: delete some templates first",
                total >> 20,
                max_bytes >> 20
            )));
        }
        Ok(())
    }

    /// Make a template from `source` (a title on the timeline, taken with `Project::template_source`)
    /// and keep it under a new id. The fonts are read and the poster drawn here, so call it without the
    /// project lock.
    pub fn save(&self, source: &TemplateSource, request: TemplateSaveRequest) -> Result<TemplateSaved> {
        let built = build_template(source, request, Uuid::new_v4())?;
        let size = self.add(&built.template)?;
        Ok(TemplateSaved {
            info: TemplateInfo::of(&built.template.manifest, built.template.poster.is_some(), size),
            warnings: built.warnings,
        })
    }

    /// Bring the template file at `path` into the library. The file is read with the archive limits, its
    /// title sanitised and its controls checked against it (a file that fails is refused whole), a poster
    /// drawn if it had none, and the result written as a fresh file — the library never holds the bytes
    /// of the original.
    ///
    /// An id the library already has is not overwritten: the same content is [`ImportStatus::AlreadyPresent`]
    /// (nothing is written), other content is kept as a new template with a new id
    /// ([`ImportStatus::AddedAsCopy`]).
    pub fn import(&self, path: &Path) -> Result<ImportOutcome> {
        let mut template = Template::read_file(path)?;
        let checked = template.validate()?;
        let mut warnings = checked.warnings.clone();
        if template.poster.is_none() {
            match template.render_poster(&checked.doc) {
                Ok(poster) => template.poster = Some(poster),
                Err(e) => warnings.push(format!("no poster could be drawn: {e}")),
            }
        }
        let existing = self.path_of(&template.manifest.id)?;
        let mut status = ImportStatus::Added;
        if existing.is_file() {
            // A file that does not read is replaced: importing is how a damaged entry gets repaired.
            if let Ok(mut have) = Template::read_file(&existing) {
                if have.validate().is_ok() && have.digest() == template.digest() {
                    return self.already_there(&existing, &have, warnings);
                }
                // Other content under the same id: kept as a copy, under an id **made from the content**,
                // so importing the same file again finds the copy it made rather than making another.
                template.manifest.id = copy_id(&template.digest());
                status = ImportStatus::AddedAsCopy;
                let copy = self.path_of(&template.manifest.id)?;
                if let Ok(mut have) = Template::read_file(&copy) {
                    if have.validate().is_ok() && have.digest() == template.digest() {
                        return self.already_there(&copy, &have, warnings);
                    }
                }
            }
        }
        let size = self.add(&template)?;
        Ok(ImportOutcome {
            info: TemplateInfo::of(&template.manifest, template.poster.is_some(), size),
            status,
            warnings,
        })
    }

    /// The outcome of an import the library already holds, from the file `at` that holds it.
    fn already_there(&self, at: &Path, have: &Template, warnings: Vec<String>) -> Result<ImportOutcome> {
        let size = std::fs::metadata(at)?.len();
        Ok(ImportOutcome {
            info: TemplateInfo::of(&have.manifest, have.poster.is_some(), size),
            status: ImportStatus::AlreadyPresent,
            warnings,
        })
    }

    /// Write the library's file for `id` to `dest`, atomically: a directory gets `<name>.kerftemplate`
    /// in it, a path with no extension gets `.kerftemplate`, any other path is used as it is. The file is
    /// read first, so a damaged entry is not exported. Returns where it was written.
    pub fn export(&self, id: &str, dest: &Path) -> Result<PathBuf> {
        let path = self.path_of(id)?;
        if !path.is_file() {
            return Err(Self::missing(id));
        }
        let template = Template::read_file(&path)?;
        let bytes = std::fs::read(&path)?;
        let target = if dest.is_dir() {
            dest.join(template.file_name())
        } else if dest.extension().is_none() {
            dest.with_extension(EXTENSION)
        } else {
            dest.to_path_buf()
        };
        write_atomic(&target, &bytes)?;
        Ok(target)
    }

    /// Remove the template `id` from the library. Titles already made from it are untouched.
    pub fn delete(&self, id: &str) -> Result<()> {
        let path = self.path_of(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Self::missing(id)),
            Err(e) => Err(e.into()),
        }
    }
}

/// The id a copy of an imported template is kept under: a name-based UUID (version 8) of the template's
/// content digest, so the same content always gets the same id.
fn copy_id(digest: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"kerf template copy");
    hash.update(digest.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.finalize()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).hyphenated().to_string()
}

fn is_temp_name(file: &str) -> bool {
    file.starts_with(TEMP_PREFIX) && file.ends_with(TEMP_SUFFIX)
}

/// Remove `path` (a temp file of ours by its name) if it is a regular file nobody has written to for
/// [`STALE_TEMP`]. A younger one may be a write in progress.
fn remove_if_stale(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return };
    let age = meta
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok());
    if meta.is_file() && age.is_some_and(|age| age > STALE_TEMP) {
        let _ = std::fs::remove_file(path);
    }
}

/// `bytes` at `path`, whole or not at all: written to a hidden temp file beside it (a name no one can
/// guess, opened only if it is new, so a file or link put there beforehand is never written through),
/// fsynced, renamed over it (the directory is made if it is not there). The temp file is removed if
/// anything fails; one a crash leaves is swept by [`Library::list`].
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{TEMP_PREFIX}{}{TEMP_SUFFIX}", Uuid::new_v4().simple()));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::archive::tests::{template, zip_of};

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-templates-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn library() -> (Library, PathBuf) {
        let dir = scratch();
        (Library::at(dir.join("lib")), dir)
    }

    fn file_of(dir: &Path, name: &str, t: &Template) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, t.to_bytes().unwrap()).unwrap();
        path
    }

    #[test]
    fn the_directory_is_the_override_when_set_else_the_data_directory() {
        let data = Some(PathBuf::from("/home/u/.local/share"));
        assert_eq!(
            dir_from(Some("/tmp/mine".into()), data.clone()),
            Some(PathBuf::from("/tmp/mine"))
        );
        assert_eq!(
            dir_from(None, data.clone()),
            Some(PathBuf::from("/home/u/.local/share").join("kerf").join("templates"))
        );
        assert_eq!(
            dir_from(Some("".into()), data),
            dir_from(None, Some(PathBuf::from("/home/u/.local/share")))
        );
        assert_eq!(dir_from(None, None), None);
        assert_eq!(dir_from(Some("/x".into()), None), Some(PathBuf::from("/x")));
        assert_eq!(
            library_dir(),
            dir_from(std::env::var_os("KERF_TEMPLATES_DIR"), dirs::data_dir())
        );
    }

    #[test]
    fn a_library_that_does_not_exist_yet_is_empty() {
        let (lib, dir) = library();
        let list = lib.list().unwrap();
        assert!(list.templates.is_empty() && list.problems.is_empty());
        assert!(lib
            .get(&Uuid::new_v4().to_string())
            .unwrap_err()
            .to_string()
            .contains("no template"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn add_list_get_poster_export_and_delete() {
        let (lib, dir) = library();
        let mut b = template();
        b.manifest.name = "bravo".into();
        let mut a = template();
        a.manifest.name = "Alpha".into();
        a.poster = Some(crate::svg::raster::encode_png(&tiny_skia::Pixmap::new(2, 2).unwrap()).unwrap());
        lib.add(&b).unwrap();
        let size = lib.add(&a).unwrap();
        let list = lib.list().unwrap();
        let names: Vec<&str> = list.templates.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "bravo"], "sorted by name, case-blind");
        assert!(list.problems.is_empty());
        assert_eq!(list.templates[0].size_bytes, size);
        assert!(list.templates[0].has_poster && !list.templates[1].has_poster);
        assert_eq!(list.templates[0].controls, a.manifest.controls);
        assert_eq!(lib.get(&a.manifest.id).unwrap().digest(), a.digest());
        assert_eq!(lib.poster(&a.manifest.id).unwrap(), a.poster);
        assert_eq!(lib.poster(&b.manifest.id).unwrap(), None);

        // Export to a directory, to a bare path and to a named path; the bytes are the library's.
        let out = scratch();
        let in_dir = lib.export(&a.manifest.id, &out).unwrap();
        assert_eq!(in_dir, out.join("Alpha.kerftemplate"));
        let bare = lib.export(&a.manifest.id, &out.join("copy")).unwrap();
        assert_eq!(bare, out.join("copy.kerftemplate"));
        let named = lib.export(&a.manifest.id, &out.join("x.zip")).unwrap();
        assert_eq!(named, out.join("x.zip"));
        let library_bytes = std::fs::read(lib.path_of(&a.manifest.id).unwrap()).unwrap();
        for path in [&in_dir, &bare, &named] {
            assert_eq!(std::fs::read(path).unwrap(), library_bytes);
        }
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 3, "no temp files left");

        lib.delete(&a.manifest.id).unwrap();
        assert_eq!(lib.list().unwrap().templates.len(), 1);
        assert!(lib.delete(&a.manifest.id).unwrap_err().to_string().contains("no template"));
        assert!(lib.export(&a.manifest.id, &out).is_err());
        assert!(lib.poster(&a.manifest.id).is_err());
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn an_id_that_is_not_a_uuid_never_becomes_a_path() {
        let (lib, dir) = library();
        for id in ["../x", "a/b", "", "..", "C:\\x", "ABCDEF00-0000-0000-0000-000000000000"] {
            assert!(lib.path_of(id).is_err(), "{id:?}");
            assert!(lib.get(id).is_err());
            assert!(lib.delete(id).is_err());
            assert!(lib.export(id, &dir).is_err());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_damaged_file_costs_the_list_one_entry_and_is_reported() {
        let (lib, dir) = library();
        let good = template();
        lib.add(&good).unwrap();
        let broken = Uuid::new_v4().to_string();
        std::fs::write(lib.dir().join(format!("{broken}.kerftemplate")), b"this is not a zip").unwrap();
        let mut stranger = template();
        stranger.manifest.name = "renamed on disk".into();
        std::fs::write(lib.dir().join("mine.kerftemplate"), stranger.to_bytes().unwrap()).unwrap();
        std::fs::write(lib.dir().join("notes.txt"), b"ignored").unwrap();
        std::fs::write(lib.dir().join(".hidden.kerftemplate"), b"ignored").unwrap();
        let list = lib.list().unwrap();
        assert_eq!(list.templates.len(), 1);
        assert_eq!(list.templates[0].id, good.manifest.id);
        let files: Vec<&str> = list.problems.iter().map(|p| p.file.as_str()).collect();
        assert_eq!(files, [format!("{broken}.kerftemplate").as_str(), "mine.kerftemplate"]);
        assert!(list.problems[0].error.contains("not a Kerf template"));
        assert!(list.problems[1].error.contains("import it instead"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_validates_writes_a_fresh_file_and_draws_a_missing_poster() {
        let (lib, dir) = library();
        let t = template();
        let path = file_of(&dir, "card.kerftemplate", &t);
        let out = lib.import(&path).unwrap();
        assert_eq!(out.status, ImportStatus::Added);
        assert_eq!(out.info.id, t.manifest.id);
        assert!(out.info.has_poster, "a poster was drawn for the file that had none");
        let kept = lib.get(&t.manifest.id).unwrap();
        assert_eq!(kept.digest(), t.digest());
        // Importing the same file again writes nothing.
        let before = std::fs::read(lib.path_of(&t.manifest.id).unwrap()).unwrap();
        let again = lib.import(&path).unwrap();
        assert_eq!(again.status, ImportStatus::AlreadyPresent);
        assert_eq!(std::fs::read(lib.path_of(&t.manifest.id).unwrap()).unwrap(), before);
        assert_eq!(lib.list().unwrap().templates.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_same_id_with_other_content_is_kept_as_a_copy_not_overwritten() {
        let (lib, dir) = library();
        let t = template();
        lib.import(&file_of(&dir, "a.kerftemplate", &t)).unwrap();
        let mut edited = t.clone();
        edited.manifest.description = "edited elsewhere".into();
        let out = lib.import(&file_of(&dir, "b.kerftemplate", &edited)).unwrap();
        assert_eq!(out.status, ImportStatus::AddedAsCopy);
        assert_ne!(out.info.id, t.manifest.id);
        assert_eq!(out.info.description, "edited elsewhere");
        assert_eq!(
            lib.get(&t.manifest.id).unwrap().manifest.description,
            "A card",
            "the original stands"
        );
        assert_eq!(lib.list().unwrap().templates.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_fails_validation_is_refused_and_nothing_is_written() {
        let (lib, dir) = library();
        let mut bad = template();
        bad.manifest.controls[0].targets[0].element = "not-there".into();
        let path = file_of(&dir, "bad.kerftemplate", &bad);
        let err = lib.import(&path).unwrap_err().to_string();
        assert!(err.contains("not-there"), "{err}");
        assert!(!lib.dir().exists() || lib.list().unwrap().templates.is_empty());
        let garbage = dir.join("garbage.kerftemplate");
        std::fs::write(&garbage, zip_of(&[("readme.txt", b"hi")])).unwrap();
        assert!(lib.import(&garbage).is_err());
        assert!(lib.import(&dir.join("missing.kerftemplate")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_damaged_entry_is_repaired_by_importing_the_good_file() {
        let (lib, dir) = library();
        let t = template();
        std::fs::create_dir_all(lib.dir()).unwrap();
        std::fs::write(lib.path_of(&t.manifest.id).unwrap(), b"broken").unwrap();
        let out = lib.import(&file_of(&dir, "good.kerftemplate", &t)).unwrap();
        assert_eq!(out.status, ImportStatus::Added);
        assert_eq!(out.info.id, t.manifest.id);
        assert!(lib.list().unwrap().problems.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_save_goes_to_the_library_under_a_new_id() {
        let (lib, dir) = library();
        let source = TemplateSource {
            svg: crate::template::archive::tests::SVG.to_string(),
            name: "Title".into(),
            duration: 4.0,
        };
        let request = TemplateSaveRequest {
            name: "Saved".into(),
            description: String::new(),
            controls: template().manifest.controls,
        };
        let saved = lib.save(&source, request).unwrap();
        assert_eq!(saved.info.name, "Saved");
        assert!(saved.info.has_poster);
        assert_eq!(lib.list().unwrap().templates, vec![saved.info]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn importing_the_same_other_content_again_finds_the_copy_it_made() {
        let (lib, dir) = library();
        let t = template();
        lib.import(&file_of(&dir, "a.kerftemplate", &t)).unwrap();
        let mut edited = t.clone();
        edited.manifest.description = "edited elsewhere".into();
        let path = file_of(&dir, "b.kerftemplate", &edited);
        let first = lib.import(&path).unwrap();
        assert_eq!(first.status, ImportStatus::AddedAsCopy);
        let second = lib.import(&path).unwrap();
        assert_eq!(second.status, ImportStatus::AlreadyPresent, "not a third template");
        assert_eq!(second.info.id, first.info.id);
        assert_eq!(second.info.description, "edited elsewhere");
        assert_eq!(lib.list().unwrap().templates.len(), 2);
        // The same file from somewhere else (another name, same content) is the same copy.
        let elsewhere = file_of(&dir, "c.kerftemplate", &edited);
        assert_eq!(lib.import(&elsewhere).unwrap().info.id, first.info.id);
        // Other content under that id is another copy, with an id of its own.
        let mut other = t;
        other.manifest.description = "edited again".into();
        let third = lib.import(&file_of(&dir, "d.kerftemplate", &other)).unwrap();
        assert_eq!(third.status, ImportStatus::AddedAsCopy);
        assert_ne!(third.info.id, first.info.id);
        assert_eq!(
            lib.import(&file_of(&dir, "e.kerftemplate", &other)).unwrap().status,
            ImportStatus::AlreadyPresent
        );
        assert_eq!(lib.list().unwrap().templates.len(), 3);
        // The id is a lower-case UUID like any other, so the library can use it as a file name.
        assert!(lib.get(&first.info.id).is_ok());
        assert_eq!(copy_id("x"), copy_id("x"));
        assert_ne!(copy_id("x"), copy_id("y"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_library_at_its_limit_takes_nothing_more_but_replaces() {
        let (lib, dir) = library();
        std::fs::create_dir_all(lib.dir()).unwrap();
        for _ in 0..MAX_TEMPLATES {
            std::fs::write(lib.path_of(&Uuid::new_v4().to_string()).unwrap(), b"x").unwrap();
        }
        let t = template();
        let err = lib.add(&t).unwrap_err().to_string();
        assert!(err.contains("500 templates") && err.contains("delete some"), "{err}");
        assert!(!lib.path_of(&t.manifest.id).unwrap().exists(), "nothing was written");
        let held = lib.list().unwrap();
        assert_eq!(held.problems.len(), MAX_TEMPLATES, "(the stand-ins are not templates)");
        // A file imported or saved is turned away the same way.
        let path = file_of(&dir, "in.kerftemplate", &t);
        assert!(lib.import(&path).unwrap_err().to_string().contains("delete some"));
        // Replacing an entry that is there does not grow the library.
        let one = held.problems[0].file.trim_end_matches(".kerftemplate").to_string();
        let mut again = t;
        again.manifest.id = one;
        assert!(lib.add(&again).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_library_is_held_to_its_size_as_well() {
        let (lib, dir) = library();
        std::fs::create_dir_all(lib.dir()).unwrap();
        let a = lib.path_of(&Uuid::new_v4().to_string()).unwrap();
        std::fs::write(&a, vec![0u8; 6000]).unwrap();
        std::fs::write(lib.dir().join(".kerf-tmp-x.tmp"), vec![0u8; 100_000]).unwrap();
        let there = |incoming| lib.check_room_within(&lib.path_of(&Uuid::new_v4().to_string()).unwrap(), incoming, 10, 10_000);
        assert!(there(4000).is_ok(), "exactly full is full, not over; a temp file is no entry");
        let err = there(4001).unwrap_err().to_string();
        assert!(err.contains("past the") && err.contains("delete some"), "{err}");
        // Replacing the big one with a small one is room.
        assert!(lib.check_room_within(&a, 9000, 10, 10_000).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_the_exact_extension_is_an_entry_so_what_is_listed_can_be_reached() {
        let (lib, dir) = library();
        let t = template();
        lib.add(&t).unwrap();
        let shouting = Uuid::new_v4().to_string();
        let mut other = template();
        other.manifest.id = shouting.clone();
        std::fs::write(lib.dir().join(format!("{shouting}.KERFTEMPLATE")), other.to_bytes().unwrap()).unwrap();
        let list = lib.list().unwrap();
        assert_eq!(list.templates.len(), 1, "{list:?}");
        assert!(list.problems.is_empty());
        for info in &list.templates {
            assert!(lib.get(&info.id).is_ok() && lib.poster(&info.id).is_ok());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stale_temp_file_is_swept_when_listing_and_a_young_or_foreign_one_is_not() {
        let (lib, dir) = library();
        lib.add(&template()).unwrap();
        let old = lib.dir().join(format!("{TEMP_PREFIX}old{TEMP_SUFFIX}"));
        let young = lib.dir().join(format!("{TEMP_PREFIX}young{TEMP_SUFFIX}"));
        let foreign = lib.dir().join(".notes.tmp");
        for path in [&old, &young, &foreign] {
            std::fs::write(path, b"half a template").unwrap();
        }
        let aged = std::fs::File::options().write(true).open(&old).unwrap();
        aged.set_modified(SystemTime::now() - 2 * STALE_TEMP).unwrap();
        drop(aged);
        let list = lib.list().unwrap();
        assert_eq!(list.templates.len(), 1);
        assert!(list.problems.is_empty(), "temp files are not entries");
        assert!(!old.exists(), "the stale one is gone");
        assert!(young.exists() && foreign.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_write_never_writes_through_a_planted_file_and_leaves_nothing_when_it_fails() {
        let dir = scratch();
        let target = dir.join("out.kerftemplate");
        write_atomic(&target, b"one").unwrap();
        write_atomic(&target, b"two").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp file is left");
        // A target that cannot be replaced (a directory) fails the rename: the temp file goes.
        let blocked = dir.join("blocked.kerftemplate");
        std::fs::create_dir_all(blocked.join("inside")).unwrap();
        assert!(write_atomic(&blocked, b"x").is_err());
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !is_temp_name(n)), "{names:?}");
        // Files planted beforehand under the old, predictable kind of temp name are left alone.
        let planted = dir.join(format!(".out.kerftemplate.{}.0.tmp", std::process::id()));
        std::fs::write(&planted, b"planted").unwrap();
        write_atomic(&target, b"three").unwrap();
        assert_eq!(std::fs::read(&planted).unwrap(), b"planted");
        assert_eq!(std::fs::read(&target).unwrap(), b"three");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn importing_a_file_that_carries_a_font_it_may_not_is_refused_and_writes_nothing() {
        use crate::fonts::EmbeddedFont;
        use crate::template::model::ManifestFont;
        let (lib, dir) = library();
        let mut t = template();
        t.manifest.fonts = vec![ManifestFont {
            file: "fonts/Zeist.ttf".into(),
            family: "Zeist".into(),
        }];
        // "Preview & print" embedding: the font may be read in a document, not carried into another.
        t.fonts = vec![EmbeddedFont::new(
            "Zeist",
            crate::fonts::with_fs_type(crate::fonts::shipped_font_renamed(true, "Zeist"), 0x0004),
        )];
        let path = file_of(&dir, "fonted.kerftemplate", &t);
        let err = lib.import(&path).unwrap_err().to_string();
        assert!(err.contains("does not allow embedding"), "{err}");
        assert!(!lib.dir().exists() || lib.list().unwrap().templates.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
