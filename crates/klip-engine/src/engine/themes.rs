//! Custom themes (M10, ADR 0019): `theme.list|save|delete|import|export`. The files live
//! in `<settings dir>/themes/`: `<id>.json` (the stored form of
//! `klip_settings::theme_file`) and its picture `<id>.<png|jpg|webp>`. Every file read
//! here is hostile input: sizes are checked before reading and everything is parsed by
//! `theme_file`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use klip_control::CommandError;
use klip_settings::theme_file::{
    self, CustomTheme, MAX_EXPORT_BYTES, MAX_PICTURE_BYTES, MAX_STORED_BYTES, Picture,
};
use klip_settings::{Issue, Theme};
use serde_json::{Value, json};

use super::{Inner, lock};
use crate::events::Event;
use crate::media_worker::write_atomic;

/// Most themes listed (and kept: saving or importing past it is refused).
pub const MAX_THEMES: usize = 200;
const PICTURE_EXTS: [&str; 3] = ["png", "jpg", "webp"];

/// One theme change at a time (new ids must stay unique).
static THEMES: Mutex<()> = Mutex::new(());

/// `<settings dir>/themes`.
pub fn themes_dir(settings_path: &Path) -> PathBuf {
    settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("themes")
}

fn failed(what: &str, e: impl std::fmt::Display) -> CommandError {
    CommandError::new("failed", format!("{what}: {e}"))
}

/// Read at most `max` bytes of `path`; more is an error (checked before and while
/// reading, so a growing file can't slip past).
fn read_capped(path: &Path, max: usize, what: &str) -> Result<Vec<u8>, CommandError> {
    let too_big =
        || CommandError::bad_args(format!("{what} is larger than {} MB", max / 1_048_576));
    let f = fs::File::open(path).map_err(|e| failed(&format!("opening {what}"), e))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max as u64 {
        return Err(too_big());
    }
    let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0).min(max));
    f.take((max as u64).saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| failed(&format!("reading {what}"), e))?;
    if buf.len() > max {
        return Err(too_big());
    }
    Ok(buf)
}

fn arg_id(args: &Value) -> Result<String, CommandError> {
    match args.get("id").and_then(Value::as_str) {
        Some(id) if theme_file::valid_id(id) => Ok(id.to_string()),
        _ => Err(CommandError::bad_args("id must be a theme id")),
    }
}

fn arg_path(args: &Value, key: &str) -> Result<PathBuf, CommandError> {
    match args.get(key).and_then(Value::as_str).map(PathBuf::from) {
        Some(p) if p.is_absolute() => Ok(p),
        _ => Err(CommandError::bad_args(format!(
            "{key} must be an absolute path"
        ))),
    }
}

fn issues_json(issues: &[Issue]) -> Value {
    Value::Array(
        issues
            .iter()
            .map(|i| json!({ "path": i.path, "message": i.message }))
            .collect(),
    )
}

/// A stored theme read back: the theme and its picture file, if any.
struct Loaded {
    theme: CustomTheme,
    picture: Option<PathBuf>,
    issues: Vec<Issue>,
}

/// The stored themes' ids, sorted, at most [`MAX_THEMES`].
fn ids(dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_suffix(".json")?;
            theme_file::valid_id(id).then(|| id.to_string())
        })
        .take(MAX_THEMES.saturating_mul(4))
        .collect();
    out.sort();
    out.truncate(MAX_THEMES);
    out
}

fn load(dir: &Path, id: &str) -> Result<Loaded, CommandError> {
    let bytes = read_capped(
        &dir.join(format!("{id}.json")),
        MAX_STORED_BYTES,
        "the theme",
    )?;
    let s = theme_file::parse_stored(&bytes).map_err(|e| failed("the theme", e))?;
    let mut issues = s.issues;
    // The picture must be this theme's own and exist.
    let picture = match s.picture_file {
        Some(f) if f.split_once('.').map(|(i, _)| i) == Some(id) => {
            let p = dir.join(&f);
            if p.is_file() {
                Some(p)
            } else {
                issues.push(Issue::new("background.file", "the picture is missing"));
                None
            }
        }
        Some(_) => {
            issues.push(Issue::new("background.file", "not this theme's picture"));
            None
        }
        None => None,
    };
    let mut theme = s.theme;
    if picture.is_none() {
        theme.backdrop = None;
    }
    Ok(Loaded {
        theme,
        picture,
        issues,
    })
}

/// The UI form, with the picture's path and a version (its modification time) so a
/// replaced picture isn't served from the WebView's cache.
fn ui(id: &str, l: &Loaded) -> Value {
    let path = l.picture.as_ref().map(|p| p.to_string_lossy().into_owned());
    let mut v = l.theme.to_ui(id, path.as_deref());
    if let (Some(p), Some(bg)) = (&l.picture, v.get_mut("background"))
        && let Some(bg) = bg.as_object_mut()
    {
        let version = fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
        bg.insert("version".into(), json!(version));
    }
    v
}

/// A free id from the theme's name: `neon`, `neon-2`, … .
fn new_id(dir: &Path, name: &str) -> Result<String, CommandError> {
    let base = theme_file::slug(name);
    for n in 1u32..=999 {
        let id = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        if !dir.join(format!("{id}.json")).exists() {
            return Ok(id);
        }
    }
    Err(CommandError::new(
        "failed",
        "no free theme id for that name",
    ))
}

fn remove_pictures(dir: &Path, id: &str, keep: Option<&str>) {
    for ext in PICTURE_EXTS {
        if Some(ext) != keep {
            let _ = fs::remove_file(dir.join(format!("{id}.{ext}")));
        }
    }
}

/// Write a theme and (when given) its new picture; `keep_picture`: leave the current one.
fn store(
    dir: &Path,
    id: &str,
    theme: &mut CustomTheme,
    picture: Option<&Picture>,
    keep_picture: bool,
) -> Result<(), CommandError> {
    fs::create_dir_all(dir).map_err(|e| failed("creating the themes folder", e))?;
    let file = match picture {
        Some(p) => {
            let name = format!("{id}.{}", p.ext);
            write_atomic(&dir.join(&name), &p.bytes)
                .map_err(|e| failed("saving the picture", e))?;
            remove_pictures(dir, id, Some(p.ext));
            theme.backdrop.get_or_insert_with(Default::default);
            Some(name)
        }
        None if keep_picture && theme.backdrop.is_some() => PICTURE_EXTS
            .iter()
            .map(|e| format!("{id}.{e}"))
            .find(|n| dir.join(n).is_file()),
        None => {
            remove_pictures(dir, id, None);
            None
        }
    };
    if file.is_none() {
        theme.backdrop = None;
    }
    let json = serde_json::to_vec_pretty(&theme.to_stored(file.as_deref()))
        .map_err(|e| failed("writing the theme", e))?;
    write_atomic(&dir.join(format!("{id}.json")), &json).map_err(|e| failed("saving the theme", e))
}

impl Inner {
    fn themes_dir(&self) -> PathBuf {
        themes_dir(&self.settings_path)
    }

    fn themes_changed(&self, id: &str) {
        self.emit(&Event::ThemesChanged { id: id.to_string() });
    }

    /// `theme.list` → `{dir, themes: [ui form], issues: [{id, path, message}]}`.
    pub(super) fn theme_list(&self) -> Result<Value, CommandError> {
        let dir = self.themes_dir();
        let mut themes = Vec::new();
        let mut issues = Vec::new();
        for id in ids(&dir) {
            match load(&dir, &id) {
                Ok(l) => {
                    for i in &l.issues {
                        issues.push(json!({ "id": id, "path": i.path, "message": i.message }));
                    }
                    themes.push(ui(&id, &l));
                }
                Err(e) => issues.push(json!({ "id": id, "path": "", "message": e.message })),
            }
        }
        Ok(json!({
            "dir": dir.to_string_lossy(),
            "themes": themes,
            "issues": issues,
        }))
    }

    /// `theme.save {id?, theme, picture?}`: create (no `id`) or replace a theme from the
    /// editor. `theme` is the UI form (strict: any problem is refused); `picture`
    /// absent keeps the current picture, `null` removes it, `{file}` takes a new one.
    pub(super) fn theme_save(&self, args: &Value) -> Result<Value, CommandError> {
        let (mut theme, issues) = theme_file::from_value(args.get("theme").unwrap_or(&Value::Null));
        if let Some(i) = issues.first() {
            return Err(CommandError::bad_args(format!("theme: {i}")));
        }
        let (picture, keep) = match args.get("picture") {
            None => (None, true),
            Some(Value::Null) => (None, false),
            Some(p) => {
                let path = arg_path(p, "file")?;
                let bytes = read_capped(&path, MAX_PICTURE_BYTES, "the picture")?;
                let pic = Picture::from_bytes(bytes)
                    .map_err(|e| CommandError::bad_args(e.to_string()))?;
                (Some(pic), false)
            }
        };
        let dir = self.themes_dir();
        let _g = lock(&THEMES);
        let id = match args.get("id") {
            None | Some(Value::Null) => {
                if ids(&dir).len() >= MAX_THEMES {
                    return Err(CommandError::bad_args(format!(
                        "at most {MAX_THEMES} themes"
                    )));
                }
                new_id(&dir, &theme.name)?
            }
            Some(_) => {
                let id = arg_id(args)?;
                if !dir.join(format!("{id}.json")).is_file() {
                    return Err(CommandError::bad_args(format!("no theme {id}")));
                }
                id
            }
        };
        store(&dir, &id, &mut theme, picture.as_ref(), keep)?;
        let l = load(&dir, &id)?;
        self.themes_changed(&id);
        Ok(json!({ "id": id, "theme": ui(&id, &l) }))
    }

    /// `theme.delete {id}`; the theme in use falls back to Follow system.
    pub(super) fn theme_delete(&self, args: &Value) -> Result<Value, CommandError> {
        let id = arg_id(args)?;
        let dir = self.themes_dir();
        let deleted = {
            let _g = lock(&THEMES);
            let file = dir.join(format!("{id}.json"));
            let existed = file.is_file();
            if existed {
                fs::remove_file(&file).map_err(|e| failed("deleting the theme", e))?;
            }
            remove_pictures(&dir, &id, None);
            existed
        };
        let a = self.settings().appearance;
        if a.theme == Theme::Custom && a.custom_theme.as_deref() == Some(id.as_str()) {
            self.set_settings(&json!({ "appearance": { "theme": "system" } }))?;
        }
        if deleted {
            self.themes_changed(&id);
        }
        Ok(json!({ "deleted": deleted }))
    }

    /// `theme.import {path}` → `{id, theme, issues}` (what was repaired on the way).
    pub(super) fn theme_import(&self, args: &Value) -> Result<Value, CommandError> {
        let path = arg_path(args, "path")?;
        let bytes = read_capped(&path, MAX_EXPORT_BYTES, "the theme file")?;
        let imp =
            theme_file::parse_export(&bytes).map_err(|e| CommandError::bad_args(e.to_string()))?;
        let mut theme = imp.theme;
        let dir = self.themes_dir();
        let id = {
            let _g = lock(&THEMES);
            if ids(&dir).len() >= MAX_THEMES {
                return Err(CommandError::bad_args(format!(
                    "at most {MAX_THEMES} themes"
                )));
            }
            let id = new_id(&dir, &theme.name)?;
            store(&dir, &id, &mut theme, imp.picture.as_ref(), false)?;
            id
        };
        let l = load(&dir, &id)?;
        self.themes_changed(&id);
        Ok(json!({ "id": id, "theme": ui(&id, &l), "issues": issues_json(&imp.issues) }))
    }

    /// `theme.export {id, path}`: one self-contained `*.klip-theme` file.
    pub(super) fn theme_export(&self, args: &Value) -> Result<Value, CommandError> {
        let id = arg_id(args)?;
        let path = arg_path(args, "path")?;
        let dir = self.themes_dir();
        if !dir.join(format!("{id}.json")).is_file() {
            return Err(CommandError::bad_args(format!("no theme {id}")));
        }
        let l = load(&dir, &id)?;
        let picture = match &l.picture {
            Some(p) => Some(
                Picture::from_bytes(read_capped(p, MAX_PICTURE_BYTES, "the picture")?)
                    .map_err(|e| failed("the picture", e))?,
            ),
            None => None,
        };
        let json = serde_json::to_vec_pretty(&l.theme.to_export(picture.as_ref()))
            .map_err(|e| failed("writing the theme", e))?;
        write_atomic(&path, &json).map_err(|e| failed("saving the theme file", e))?;
        Ok(json!({ "path": path.to_string_lossy() }))
    }
}
