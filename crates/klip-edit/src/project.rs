//! The project: schema v1, lenient construction from JSON, repair of bad values.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::advanced::Advanced;
use serde_json::Value;

use crate::format::Format;
use crate::text::{self, TextLayer};
use crate::timeline;

/// Version of the project schema written by this build.
pub const SCHEMA_VERSION: u32 = 1;
/// Project files bigger than this are not read.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// Most kept pieces of the clip.
pub const MAX_PIECES: usize = 256;
/// Most text layers.
pub const MAX_TEXT_LAYERS: usize = 64;
/// Longest clip the editor accepts (24 h), ms.
pub const MAX_SOURCE_MS: u64 = 24 * 60 * 60 * 1000;
/// Most unknown (advanced-mode or newer) keys kept in a project.
pub const MAX_EXTRA_KEYS: usize = 64;
/// Loudest volume slider position (2 = +6 dB).
pub const MAX_VOLUME: f32 = 2.0;

/// Something in a project file that was wrong and was repaired.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// Where, e.g. `pieces[2]` or `text[0].preset` (empty: the whole file).
    pub path: String,
    pub message: String,
}

impl Issue {
    pub(crate) fn new(path: impl Into<String>, message: impl Into<String>) -> Issue {
        Issue {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// The editor mode the project was last shown in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Normal,
    Advanced,
}

/// Playback speed of a piece. Stored as the number 0.5, 1 or 2.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub enum Speed {
    Half,
    #[default]
    Normal,
    Double,
}

impl Speed {
    pub const ALL: [Speed; 3] = [Speed::Half, Speed::Normal, Speed::Double];

    /// Playback rate (source seconds per output second).
    pub fn rate(self) -> f64 {
        match self {
            Speed::Half => 0.5,
            Speed::Normal => 1.0,
            Speed::Double => 2.0,
        }
    }

    /// The rate as `(numerator, denominator)`: source ms per `denominator` output ms.
    pub(crate) fn ratio(self) -> (u64, u64) {
        match self {
            Speed::Half => (1, 2),
            Speed::Normal => (1, 1),
            Speed::Double => (2, 1),
        }
    }
}

impl TryFrom<f64> for Speed {
    type Error = String;

    fn try_from(v: f64) -> Result<Speed, String> {
        Speed::ALL
            .into_iter()
            .find(|s| s.rate() == v)
            .ok_or_else(|| format!("speed must be 0.5, 1 or 2, not {v}"))
    }
}

impl From<Speed> for f64 {
    fn from(s: Speed) -> f64 {
        s.rate()
    }
}

/// A kept range of the source clip, played at `speed`. Pieces are in source order and
/// don't overlap; everything between them is cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Piece {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speed: Speed,
}

/// The two Normal-mode sliders (1 = as recorded, 0 = muted, up to [`MAX_VOLUME`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Volumes {
    pub game: f32,
    pub mic: f32,
}

impl Default for Volumes {
    fn default() -> Self {
        Volumes {
            game: 1.0,
            mic: 1.0,
        }
    }
}

/// Non-destructive edits of one clip. Times are ms; `pieces` and `source_ms` are source
/// time, text layers are output time (after cuts and speed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub version: u32,
    pub mode: Mode,
    /// The clip's length when the edits were made (0: not known yet).
    pub source_ms: u64,
    /// Kept ranges; empty means the whole clip at normal speed.
    pub pieces: Vec<Piece>,
    pub format: Format,
    pub text: Vec<TextLayer>,
    pub volumes: Volumes,
    /// Advanced mode's section (ADR 0018); absent on disk while empty.
    #[serde(skip_serializing_if = "Advanced::is_empty")]
    pub advanced: Advanced,
    /// Keys this build doesn't know (a newer Scout): kept as they are so switching
    /// modes never loses work; shown as the "advanced edits" badge.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for Project {
    fn default() -> Self {
        Project {
            version: SCHEMA_VERSION,
            mode: Mode::Normal,
            source_ms: 0,
            pieces: Vec::new(),
            format: Format::default(),
            text: Vec::new(),
            volumes: Volumes::default(),
            advanced: Advanced::default(),
            extra: BTreeMap::new(),
        }
    }
}

const KNOWN_KEYS: &[&str] = &[
    "version",
    "mode",
    "source_ms",
    "pieces",
    "format",
    "text",
    "volumes",
    "advanced",
];

impl Project {
    /// A fresh project for a clip of `source_ms` (whole clip, normal speed).
    pub fn new(source_ms: u64) -> Project {
        Project {
            source_ms: source_ms.min(MAX_SOURCE_MS),
            ..Project::default()
        }
    }

    /// Whether the project holds edits Normal mode can't show.
    pub fn has_advanced_edits(&self) -> bool {
        !self.advanced.is_empty() || !self.extra.is_empty()
    }

    /// The next unused text layer id.
    pub fn next_text_id(&self) -> u32 {
        self.text
            .iter()
            .map(|l| l.id)
            .max()
            .map_or(1, |m| m.saturating_add(1))
    }

    /// Build a project from any JSON value. Never fails: a non-object gives the
    /// defaults, wrong fields get their defaults (a bad list entry is dropped as a
    /// whole), unknown keys are kept in `extra`, then everything is repaired.
    pub fn from_value(value: Value) -> (Project, Vec<Issue>) {
        let mut issues = Vec::new();
        let Value::Object(mut map) = value else {
            issues.push(Issue::new("", "not a JSON object; using a new project"));
            return (Project::default(), issues);
        };
        let mut p = Project::default();
        if let Some(v) = map.remove("version") {
            p.version = field(v, "version", &mut issues);
        }
        if let Some(v) = map.remove("mode") {
            p.mode = field(v, "mode", &mut issues);
        }
        if let Some(v) = map.remove("source_ms") {
            p.source_ms = field(v, "source_ms", &mut issues);
        }
        if let Some(v) = map.remove("pieces") {
            p.pieces = list(v, "pieces", MAX_PIECES, &mut issues);
        }
        if let Some(v) = map.remove("format") {
            p.format = field(v, "format", &mut issues);
        }
        if let Some(v) = map.remove("text") {
            p.text = list(v, "text", MAX_TEXT_LAYERS, &mut issues);
        }
        if let Some(v) = map.remove("volumes") {
            p.volumes = field(v, "volumes", &mut issues);
        }
        if let Some(v) = map.remove("advanced") {
            p.advanced = Advanced::from_value(v, &mut issues);
        }
        for (i, (k, v)) in map.into_iter().enumerate() {
            if i >= MAX_EXTRA_KEYS {
                issues.push(Issue::new(
                    k.chars().take(64).collect::<String>(),
                    format!("more than {MAX_EXTRA_KEYS} unknown keys; dropped"),
                ));
                break;
            }
            if !KNOWN_KEYS.contains(&k.as_str()) {
                p.extra.insert(k, v);
            }
        }
        issues.extend(p.repair());
        (p, issues)
    }

    /// Clamp, sort and drop what can't be used, and say what changed. Idempotent.
    pub fn repair(&mut self) -> Vec<Issue> {
        let mut issues = Vec::new();
        issues.extend(self.advanced.repair());
        if self.version != SCHEMA_VERSION {
            issues.push(Issue::new(
                "version",
                if self.version > SCHEMA_VERSION {
                    format!(
                        "written by a newer Scout (schema {}); read as schema {SCHEMA_VERSION}",
                        self.version
                    )
                } else {
                    format!("schema {} upgraded to {SCHEMA_VERSION}", self.version)
                },
            ));
            self.version = SCHEMA_VERSION;
        }
        if self.source_ms > MAX_SOURCE_MS {
            issues.push(Issue::new(
                "source_ms",
                format!("longer than {MAX_SOURCE_MS} ms; clamped"),
            ));
            self.source_ms = MAX_SOURCE_MS;
        }
        self.repair_pieces(&mut issues);
        if let Some(issue) = self.format.repair() {
            issues.push(issue);
        }
        self.repair_volumes(&mut issues);
        self.repair_text(&mut issues);
        issues
    }

    fn repair_pieces(&mut self, issues: &mut Vec<Issue>) {
        let had = !self.pieces.is_empty();
        if self.pieces.len() > MAX_PIECES {
            issues.push(Issue::new(
                "pieces",
                format!("more than {MAX_PIECES} pieces; the rest were dropped"),
            ));
            self.pieces.truncate(MAX_PIECES);
        }
        let limit = if self.source_ms > 0 {
            self.source_ms
        } else {
            MAX_SOURCE_MS
        };
        let mut kept: Vec<Piece> = Vec::with_capacity(self.pieces.len());
        for (i, piece) in self.pieces.iter().enumerate() {
            let mut p = *piece;
            if p.end_ms > limit {
                p.end_ms = limit;
                issues.push(Issue::new(
                    format!("pieces[{i}].end_ms"),
                    "beyond the end of the clip; clamped",
                ));
            }
            if p.start_ms >= p.end_ms {
                issues.push(Issue::new(format!("pieces[{i}]"), "empty range; dropped"));
                continue;
            }
            kept.push(p);
        }
        kept.sort_by_key(|p| p.start_ms);
        let mut ordered: Vec<Piece> = Vec::with_capacity(kept.len());
        for p in kept {
            match ordered.last() {
                Some(prev) if p.start_ms < prev.end_ms => issues.push(Issue::new(
                    "pieces",
                    format!(
                        "piece {}–{} ms overlaps the one before it; dropped",
                        p.start_ms, p.end_ms
                    ),
                )),
                _ => ordered.push(p),
            }
        }
        if had && ordered.is_empty() {
            issues.push(Issue::new(
                "pieces",
                "no usable piece was left; the whole clip is used",
            ));
        }
        self.pieces = ordered;
    }

    fn repair_volumes(&mut self, issues: &mut Vec<Issue>) {
        for (name, v) in [
            ("volumes.game", &mut self.volumes.game),
            ("volumes.mic", &mut self.volumes.mic),
        ] {
            let fixed = if v.is_nan() {
                1.0
            } else {
                v.clamp(0.0, MAX_VOLUME)
            };
            if fixed.to_bits() != v.to_bits() {
                issues.push(Issue::new(name, format!("out of range; set to {fixed}")));
                *v = fixed;
            }
        }
    }

    fn repair_text(&mut self, issues: &mut Vec<Issue>) {
        if self.text.len() > MAX_TEXT_LAYERS {
            issues.push(Issue::new(
                "text",
                format!("more than {MAX_TEXT_LAYERS} text layers; the rest were dropped"),
            ));
            self.text.truncate(MAX_TEXT_LAYERS);
        }
        let duration = timeline::output_duration_ms(self);
        let mut kept: Vec<TextLayer> = Vec::with_capacity(self.text.len());
        for (i, layer) in self.text.iter().enumerate() {
            let mut l = layer.clone();
            let cleaned = text::clean_text(&l.text);
            if cleaned != l.text {
                issues.push(Issue::new(format!("text[{i}].text"), "cleaned"));
                l.text = cleaned;
            }
            if l.text.is_empty() {
                issues.push(Issue::new(format!("text[{i}]"), "empty text; dropped"));
                continue;
            }
            if text::preset(&l.preset).is_none() {
                issues.push(Issue::new(
                    format!("text[{i}].preset"),
                    format!("unknown style; using {}", text::DEFAULT_PRESET),
                ));
                l.preset = text::DEFAULT_PRESET.to_string();
            }
            if duration > 0 && l.end_ms > duration {
                issues.push(Issue::new(
                    format!("text[{i}].end_ms"),
                    "beyond the end of the edit; clamped",
                ));
                l.end_ms = duration;
            }
            if l.start_ms >= l.end_ms {
                issues.push(Issue::new(
                    format!("text[{i}]"),
                    "empty time range; dropped",
                ));
                continue;
            }
            if kept.iter().any(|k| k.id == l.id) {
                let id = kept
                    .iter()
                    .map(|k| k.id)
                    .max()
                    .map_or(1, |m| m.saturating_add(1));
                issues.push(Issue::new(
                    format!("text[{i}].id"),
                    format!("duplicate id {}; renumbered {id}", l.id),
                ));
                l.id = id;
            }
            kept.push(l);
        }
        self.text = kept;
    }

    /// The project as JSON (what `save` writes).
    pub fn to_value(&self) -> Result<Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

pub(crate) fn field<T: for<'de> Deserialize<'de> + Default>(
    v: Value,
    path: &str,
    issues: &mut Vec<Issue>,
) -> T {
    match serde_json::from_value(v) {
        Ok(t) => t,
        Err(e) => {
            issues.push(Issue::new(path, format!("{e}; using the default")));
            T::default()
        }
    }
}

pub(crate) fn list<T: for<'de> Deserialize<'de>>(
    v: Value,
    path: &str,
    cap: usize,
    issues: &mut Vec<Issue>,
) -> Vec<T> {
    let Value::Array(items) = v else {
        issues.push(Issue::new(path, "not a list; using the default"));
        return Vec::new();
    };
    if items.len() > cap {
        issues.push(Issue::new(
            path,
            format!("more than {cap} entries; the rest were dropped"),
        ));
    }
    items
        .into_iter()
        .take(cap)
        .enumerate()
        .filter_map(|(i, item)| match serde_json::from_value(item) {
            Ok(t) => Some(t),
            Err(e) => {
                issues.push(Issue::new(format!("{path}[{i}]"), format!("{e}; dropped")));
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn speed_round_trips_as_numbers() {
        for s in Speed::ALL {
            let v = serde_json::to_value(s).unwrap();
            assert_eq!(serde_json::from_value::<Speed>(v).unwrap(), s);
        }
        assert!(serde_json::from_value::<Speed>(json!(1.5)).is_err());
        assert!(serde_json::from_value::<Speed>(json!("2")).is_err());
    }

    #[test]
    fn default_project_round_trips_and_is_clean() {
        let p = Project::new(12_345);
        let v = p.to_value().unwrap();
        let (back, issues) = Project::from_value(v);
        assert_eq!(back, p);
        assert!(issues.is_empty());
    }

    #[test]
    fn unknown_keys_are_kept_and_flagged_as_advanced() {
        let (p, issues) = Project::from_value(json!({
            "source_ms": 5000,
            "tracks": [{"kind": "music"}],
            "keyframes": {"zoom": []}
        }));
        assert!(issues.is_empty(), "{issues:?}");
        assert!(p.has_advanced_edits());
        assert_eq!(p.extra.len(), 2);
        let v = p.to_value().unwrap();
        assert_eq!(v["tracks"][0]["kind"], "music");
    }

    #[test]
    fn bad_fields_take_defaults_and_bad_pieces_are_dropped() {
        let (p, issues) = Project::from_value(json!({
            "version": "one",
            "mode": "weird",
            "source_ms": 10_000,
            "pieces": [
                {"start_ms": 2000, "end_ms": 1000},
                {"start_ms": 500, "end_ms": 3000, "speed": 3},
                {"start_ms": 4000, "end_ms": 99_000, "speed": 2},
                {"start_ms": 1000, "end_ms": 5000},
                "junk"
            ],
            "volumes": {"game": -1, "mic": "loud"},
            "text": "none"
        }));
        assert_eq!(p.version, SCHEMA_VERSION);
        assert_eq!(p.mode, Mode::Normal);
        assert_eq!(
            p.pieces,
            vec![
                Piece {
                    start_ms: 1000,
                    end_ms: 5000,
                    speed: Speed::Normal
                },
                // 4000–10000 overlaps 1000–5000 after sorting: dropped.
            ]
        );
        assert_eq!(p.volumes, Volumes::default());
        assert_eq!(p.volumes.game, 1.0);
        assert!(p.text.is_empty());
        let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"version"));
        assert!(paths.contains(&"mode"));
        assert!(paths.contains(&"pieces[0]"));
        assert!(paths.contains(&"pieces[1]"));
        assert!(paths.contains(&"pieces[4]"));
        assert!(paths.contains(&"volumes"));
        assert!(paths.contains(&"text"));
        assert!(paths.contains(&"pieces"));
    }

    #[test]
    fn repair_is_idempotent() {
        let (mut p, _) = Project::from_value(json!({
            "source_ms": 4000,
            "pieces": [{"start_ms": 3000, "end_ms": 9000}, {"start_ms": 0, "end_ms": 1000}],
            "text": [
                {"id": 1, "preset": "nope", "text": "  hi\u{0}there  ", "start_ms": 0, "end_ms": 99_999},
                {"id": 1, "preset": "bold", "text": "two", "start_ms": 100, "end_ms": 200}
            ]
        }));
        let again = p.repair();
        assert!(again.is_empty(), "{again:?}");
        assert_eq!(p.text.len(), 2);
        assert_eq!(p.text[0].text, "hithere");
        assert_eq!(p.text[0].end_ms, 2000);
        assert_eq!(p.text[1].id, 2);
    }

    #[test]
    fn non_object_resets() {
        let (p, issues) = Project::from_value(json!([1, 2, 3]));
        assert_eq!(p, Project::default());
        assert_eq!(issues.len(), 1);
    }
}
