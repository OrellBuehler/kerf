//! The user's template library: a directory of `<id>.kerftemplate` files.
//!
//! `dirs::data_dir()/kerf/templates` (`KERF_TEMPLATES_DIR` overrides, read on every call). **Library
//! writes are app-level, not timeline edits** — they are not undone, not staged and not part of any
//! project; applying a template is the edit (`Project::apply_template_parsed`), and it copies what it
//! needs, so a project never reaches back here.
//!
//! Every write is atomic (a temp file beside the target, fsync, rename), and a file that is damaged
//! costs the list one entry, not the list: it comes back in [`TemplateList::problems`].

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::archive::Template;
use super::model::{TemplateControl, TemplateManifest};
use super::save::{build_template, TemplateSaveRequest, TemplateSource};
use super::validate::{check_id, invalid};
use super::EXTENSION;
use crate::error::{Error, Result};

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
    /// read. A directory that does not exist yet is an empty library.
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
            let is_template = path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION));
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
    /// Returns the size written.
    pub fn add(&self, template: &Template) -> Result<u64> {
        let path = self.path_of(&template.manifest.id)?;
        let bytes = template.to_bytes()?;
        write_atomic(&path, &bytes)?;
        Ok(bytes.len() as u64)
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
                    let size = std::fs::metadata(&existing)?.len();
                    return Ok(ImportOutcome {
                        info: TemplateInfo::of(&have.manifest, have.poster.is_some(), size),
                        status: ImportStatus::AlreadyPresent,
                        warnings,
                    });
                }
                template.manifest.id = Uuid::new_v4().hyphenated().to_string();
                status = ImportStatus::AddedAsCopy;
            }
        }
        let size = self.add(&template)?;
        Ok(ImportOutcome {
            info: TemplateInfo::of(&template.manifest, template.poster.is_some(), size),
            status,
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

/// `bytes` at `path`, whole or not at all: written to a hidden temp file beside it, fsynced, renamed
/// over it (the directory is made if it is not there). The temp file is removed if anything fails.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("template");
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
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
}
