//! Custom theme files (M10, ADR 0019). Pure: parsing, repairing and writing; the engine
//! keeps the files in `<settings dir>/themes/`.
//!
//! A theme is a built-in `base` plus the colours it overrides, a corner radius, a font
//! family and an optional background picture. Two forms of the same JSON:
//! - **export** (`*.klip-theme`, what people share): the picture is embedded as a
//!   `data:image/…;base64,` URI in `background.image`;
//! - **stored** (`themes/<id>.json`): the picture sits beside it as `<id>.<ext>`, named in
//!   `background.file`.
//!
//! Every file is hostile input: the size is capped before parsing, wrong fields are
//! dropped with an [`Issue`] (like the settings file), and only a missing or newer format
//! version, a non-object or an oversized file is refused.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::Issue;

/// The format version this build writes and reads.
pub const FORMAT_VERSION: u64 = 1;
/// Largest export file (picture included).
pub const MAX_EXPORT_BYTES: usize = 12 << 20;
/// Largest stored theme JSON (no picture).
pub const MAX_STORED_BYTES: usize = 64 << 10;
/// Largest background picture (decoded).
pub const MAX_PICTURE_BYTES: usize = 8 << 20;
/// Longest theme name, in characters.
pub const MAX_NAME_CHARS: usize = 48;
/// Longest font family name, in characters.
pub const MAX_FONT_CHARS: usize = 64;
/// Longest theme id.
pub const MAX_ID_LEN: usize = 40;
/// Corner radius range, px.
pub const RADIUS_MAX: u32 = 16;
/// The radius when a theme doesn't say (today's look).
pub const RADIUS_DEFAULT: u32 = 6;
/// How much the picture is darkened (dark base) or lightened (light base), %.
pub const DIM_MAX: u32 = 90;
pub const DIM_DEFAULT: u32 = 50;
/// Opacity of the big surfaces over the picture, %.
pub const OPACITY_MIN: u32 = 60;
pub const OPACITY_DEFAULT: u32 = 85;
/// At most this many issues are reported per file (the rest are counted in one).
const MAX_ISSUES: usize = 32;

/// The colour tokens a theme may set, as CSS variable names without `--` (the UI's
/// `Tokens` in camelCase). `hover` is derived from the scheme, not set.
pub const TOKENS: [&str; 21] = [
    "gutter",
    "chrome",
    "statusbar",
    "panel",
    "panel-raised",
    "viewer",
    "input",
    "border",
    "border-strong",
    "text",
    "text-strong",
    "text-muted",
    "accent",
    "accent-strong",
    "accent-text",
    "on-accent",
    "good",
    "warn",
    "bad",
    "rec",
    "focus",
];

/// The built-in theme a custom theme starts from.
pub const BASES: [&str; 4] = ["dark", "graphite", "light", "oled"];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ThemeError {
    #[error("the file is larger than {0} MB")]
    TooLarge(usize),
    #[error("not a Klip theme file (it isn't JSON)")]
    NotJson,
    #[error("not a Klip theme file (no \"klip_theme\" version)")]
    NotTheme,
    #[error("the theme was made by a newer Klip (format {0})")]
    Newer(u64),
    #[error("the picture isn't a PNG, JPEG or WebP file")]
    NotPicture,
    #[error("the picture is larger than {} MB", MAX_PICTURE_BYTES >> 20)]
    PictureTooLarge,
}

/// A background picture: its bytes and the file extension its magic bytes say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub ext: &'static str,
    pub bytes: Vec<u8>,
}

impl Picture {
    /// Check `bytes` is a PNG, JPEG or WebP file of an accepted size.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Picture, ThemeError> {
        if bytes.len() > MAX_PICTURE_BYTES {
            return Err(ThemeError::PictureTooLarge);
        }
        let ext = picture_ext(&bytes).ok_or(ThemeError::NotPicture)?;
        Ok(Picture { ext, bytes })
    }

    pub fn mime(&self) -> &'static str {
        match self.ext {
            "png" => "image/png",
            "webp" => "image/webp",
            _ => "image/jpeg",
        }
    }

    pub fn data_uri(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.mime(),
            base64::engine::general_purpose::STANDARD.encode(&self.bytes)
        )
    }
}

/// The extension for PNG, JPEG or WebP magic bytes.
fn picture_ext(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("webp")
    } else {
        None
    }
}

/// How the picture is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backdrop {
    /// Darken (dark base) or lighten (light base) by this %, 0–[`DIM_MAX`].
    pub dim: u32,
    /// Opacity of the big surfaces over it, %, [`OPACITY_MIN`]–100.
    pub opacity: u32,
}

impl Default for Backdrop {
    fn default() -> Self {
        Backdrop {
            dim: DIM_DEFAULT,
            opacity: OPACITY_DEFAULT,
        }
    }
}

/// A custom theme without its picture bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomTheme {
    pub name: String,
    /// One of [`BASES`].
    pub base: String,
    /// Token (one of [`TOKENS`]) → `#rrggbb`.
    pub colors: BTreeMap<String, String>,
    pub radius: u32,
    /// A font family name; `None`: the default UI font.
    pub font: Option<String>,
    /// Set when the theme has a picture.
    pub backdrop: Option<Backdrop>,
}

impl Default for CustomTheme {
    fn default() -> Self {
        CustomTheme {
            name: "Custom theme".into(),
            base: "dark".into(),
            colors: BTreeMap::new(),
            radius: RADIUS_DEFAULT,
            font: None,
            backdrop: None,
        }
    }
}

/// A parsed export file.
#[derive(Debug)]
pub struct Imported {
    pub theme: CustomTheme,
    /// The embedded picture, when it was valid.
    pub picture: Option<Picture>,
    pub issues: Vec<Issue>,
}

/// A parsed stored file.
#[derive(Debug)]
pub struct Stored {
    pub theme: CustomTheme,
    /// `background.file`, checked to be a plain `<id>.<ext>` name.
    pub picture_file: Option<String>,
    pub issues: Vec<Issue>,
}

/// Parse a shared `*.klip-theme` file.
pub fn parse_export(bytes: &[u8]) -> Result<Imported, ThemeError> {
    let (obj, mut issues) = parse_object(bytes, MAX_EXPORT_BYTES)?;
    let mut theme = fields(&obj, &mut issues);
    let mut picture = None;
    if let Some(bg) = obj.get("background").filter(|v| !v.is_null()) {
        match bg.get("image").and_then(Value::as_str).map(decode_data_uri) {
            Some(Ok(p)) => {
                theme.backdrop = Some(backdrop(bg, &mut issues));
                picture = Some(p);
            }
            Some(Err(e)) => issues.push(Issue::new("background.image", format!("{e}; no picture"))),
            None => issues.push(Issue::new("background.image", "missing; no picture")),
        }
    }
    cap(&mut issues);
    Ok(Imported {
        theme,
        picture,
        issues,
    })
}

/// Parse a stored `themes/<id>.json` file.
pub fn parse_stored(bytes: &[u8]) -> Result<Stored, ThemeError> {
    let (obj, mut issues) = parse_object(bytes, MAX_STORED_BYTES)?;
    let mut theme = fields(&obj, &mut issues);
    let mut picture_file = None;
    if let Some(bg) = obj.get("background").filter(|v| !v.is_null()) {
        match bg.get("file").and_then(Value::as_str) {
            Some(f) if valid_picture_file(f) => {
                theme.backdrop = Some(backdrop(bg, &mut issues));
                picture_file = Some(f.to_string());
            }
            _ => issues.push(Issue::new(
                "background.file",
                "not a picture file name; no picture",
            )),
        }
    }
    cap(&mut issues);
    Ok(Stored {
        theme,
        picture_file,
        issues,
    })
}

/// Repair a theme the UI sends (the editor): same rules as a file, minus the picture.
/// `value` is the theme object without `klip_theme` and `background.image`/`file`.
pub fn from_value(value: &Value) -> (CustomTheme, Vec<Issue>) {
    let mut issues = Vec::new();
    let empty = Map::new();
    let obj = value.as_object().unwrap_or(&empty);
    if !value.is_object() {
        issues.push(Issue::new("", "a theme must be a JSON object"));
    }
    let mut theme = fields(obj, &mut issues);
    if let Some(bg) = obj.get("background").filter(|v| !v.is_null()) {
        theme.backdrop = Some(backdrop(bg, &mut issues));
    }
    cap(&mut issues);
    (theme, issues)
}

impl CustomTheme {
    /// The JSON both forms share (without `background`).
    fn base_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("klip_theme".into(), json!(FORMAT_VERSION));
        m.insert("name".into(), json!(self.name));
        m.insert("base".into(), json!(self.base));
        m.insert("colors".into(), json!(self.colors));
        m.insert("radius".into(), json!(self.radius));
        if let Some(f) = &self.font {
            m.insert("font".into(), json!(f));
        }
        m
    }

    /// The stored form; `picture_file` names the picture beside it.
    pub fn to_stored(&self, picture_file: Option<&str>) -> Value {
        let mut m = self.base_json();
        if let (Some(b), Some(f)) = (self.backdrop, picture_file) {
            m.insert(
                "background".into(),
                json!({ "file": f, "dim": b.dim, "opacity": b.opacity }),
            );
        }
        Value::Object(m)
    }

    /// The export form, with the picture embedded.
    pub fn to_export(&self, picture: Option<&Picture>) -> Value {
        let mut m = self.base_json();
        if let (Some(b), Some(p)) = (self.backdrop, picture) {
            m.insert(
                "background".into(),
                json!({ "image": p.data_uri(), "dim": b.dim, "opacity": b.opacity }),
            );
        }
        Value::Object(m)
    }

    /// What the UI gets (`theme.list`): the stored form's fields, the picture as a path.
    pub fn to_ui(&self, id: &str, picture_path: Option<&str>) -> Value {
        let mut m = self.base_json();
        m.remove("klip_theme");
        m.insert("id".into(), json!(id));
        m.insert(
            "background".into(),
            match (self.backdrop, picture_path) {
                (Some(b), Some(p)) => json!({ "path": p, "dim": b.dim, "opacity": b.opacity }),
                _ => Value::Null,
            },
        );
        Value::Object(m)
    }
}

/// A theme id: `[a-z0-9-]`, 1–[`MAX_ID_LEN`] long, not starting with `-`.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// An id made from a theme name (`My Theme!` → `my-theme`); `theme` when nothing is left.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let t = out.trim_matches('-');
    if t.is_empty() {
        "theme".into()
    } else {
        t.into()
    }
}

fn valid_picture_file(f: &str) -> bool {
    match f.rsplit_once('.') {
        Some((id, ext)) => valid_id(id) && matches!(ext, "png" | "jpg" | "webp"),
        None => false,
    }
}

/// `#rgb` or `#rrggbb` (any case) → `#rrggbb` lower case.
pub fn normalise_colour(s: &str) -> Option<String> {
    let hex = s.trim().strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hex = hex.to_ascii_lowercase();
    match hex.len() {
        6 => Some(format!("#{hex}")),
        3 => Some(format!(
            "#{}",
            hex.chars().flat_map(|c| [c, c]).collect::<String>()
        )),
        _ => None,
    }
}

/// A font family name: letters, digits, spaces, `.`, `_`, `-`; nothing CSS could read as
/// syntax (quotes, `;`, braces, `\`, `(`).
pub fn valid_font(s: &str) -> bool {
    let n = s.chars().count();
    (1..=MAX_FONT_CHARS).contains(&n)
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
}

fn parse_object(bytes: &[u8], max: usize) -> Result<(Map<String, Value>, Vec<Issue>), ThemeError> {
    if bytes.len() > max {
        return Err(ThemeError::TooLarge(max / 1_048_576));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|_| ThemeError::NotJson)?;
    let Value::Object(obj) = v else {
        return Err(ThemeError::NotTheme);
    };
    match obj.get("klip_theme").and_then(Value::as_u64) {
        None | Some(0) => Err(ThemeError::NotTheme),
        Some(v) if v > FORMAT_VERSION => Err(ThemeError::Newer(v)),
        Some(_) => Ok((obj, Vec::new())),
    }
}

/// The fields both forms share, repaired.
fn fields(obj: &Map<String, Value>, issues: &mut Vec<Issue>) -> CustomTheme {
    let mut t = CustomTheme::default();
    match obj.get("name").and_then(Value::as_str).map(clean_name) {
        Some(Some(n)) => t.name = n,
        _ => issues.push(Issue::new(
            "name",
            "missing or empty; using \"Custom theme\"",
        )),
    }
    match obj.get("base") {
        None => {}
        Some(Value::String(b)) if BASES.contains(&b.as_str()) => t.base = b.clone(),
        Some(_) => issues.push(Issue::new(
            "base",
            "not dark, graphite, light or oled; using dark",
        )),
    }
    match obj.get("colors") {
        None | Some(Value::Null) => {}
        Some(Value::Object(c)) => {
            for (k, v) in c.iter().take(256) {
                let path = format!("colors.{}", short_key(k));
                if !TOKENS.contains(&k.as_str()) {
                    issues.push(Issue::new(path, "unknown colour; ignored"));
                    continue;
                }
                match v.as_str().and_then(normalise_colour) {
                    Some(c) => {
                        t.colors.insert(k.clone(), c);
                    }
                    None => issues.push(Issue::new(path, "not a #rrggbb colour; using the base's")),
                }
            }
            if c.len() > 256 {
                issues.push(Issue::new(
                    "colors",
                    "too many entries; the rest are ignored",
                ));
            }
        }
        Some(_) => issues.push(Issue::new("colors", "not an object; using the base's")),
    }
    if let Some(r) = obj.get("radius") {
        match r.as_u64() {
            Some(r) if r <= u64::from(RADIUS_MAX) => {
                t.radius = u32::try_from(r).unwrap_or(RADIUS_DEFAULT)
            }
            Some(_) => {
                issues.push(Issue::new(
                    "radius",
                    format!("over {RADIUS_MAX}; using {RADIUS_MAX}"),
                ));
                t.radius = RADIUS_MAX;
            }
            None => issues.push(Issue::new(
                "radius",
                format!("not a whole number of px; using {RADIUS_DEFAULT}"),
            )),
        }
    }
    match obj.get("font") {
        None | Some(Value::Null) => {}
        Some(Value::String(f)) if valid_font(f.trim()) => t.font = Some(f.trim().to_string()),
        Some(_) => issues.push(Issue::new(
            "font",
            "not a font family name; using the default font",
        )),
    }
    t
}

fn backdrop(bg: &Value, issues: &mut Vec<Issue>) -> Backdrop {
    let mut b = Backdrop::default();
    if let Some(d) = bg.get("dim") {
        b.dim = pct(d, 0, DIM_MAX, DIM_DEFAULT, "background.dim", issues);
    }
    if let Some(o) = bg.get("opacity") {
        b.opacity = pct(
            o,
            OPACITY_MIN,
            100,
            OPACITY_DEFAULT,
            "background.opacity",
            issues,
        );
    }
    b
}

fn pct(v: &Value, min: u32, max: u32, def: u32, path: &str, issues: &mut Vec<Issue>) -> u32 {
    match v.as_u64() {
        Some(n) if (u64::from(min)..=u64::from(max)).contains(&n) => {
            u32::try_from(n).unwrap_or(def)
        }
        Some(n) => {
            let fixed = if n < u64::from(min) { min } else { max };
            issues.push(Issue::new(
                path,
                format!("outside {min}–{max}; using {fixed}"),
            ));
            fixed
        }
        None => {
            issues.push(Issue::new(path, format!("not a whole number; using {def}")));
            def
        }
    }
}

/// Trimmed, control characters removed, at most [`MAX_NAME_CHARS`]; `None` when empty.
fn clean_name(s: &str) -> Option<String> {
    let t: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn short_key(k: &str) -> String {
    k.chars().filter(|c| !c.is_control()).take(32).collect()
}

fn decode_data_uri(s: &str) -> Result<Picture, ThemeError> {
    let rest = s.strip_prefix("data:").ok_or(ThemeError::NotPicture)?;
    let (head, data) = rest.split_once(',').ok_or(ThemeError::NotPicture)?;
    if !head.ends_with(";base64") {
        return Err(ThemeError::NotPicture);
    }
    // 4 base64 characters carry 3 bytes; refuse before decoding.
    const MAX_BASE64: usize = MAX_PICTURE_BYTES / 3 * 4 + 8;
    if data.trim().len() > MAX_BASE64 {
        return Err(ThemeError::PictureTooLarge);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|_| ThemeError::NotPicture)?;
    Picture::from_bytes(bytes)
}

fn cap(issues: &mut Vec<Issue>) {
    if issues.len() > MAX_ISSUES {
        let more = issues.len().saturating_sub(MAX_ISSUES);
        issues.truncate(MAX_ISSUES);
        issues.push(Issue::new("", format!("and {more} more problems")));
    }
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod tests;
