//! Advanced mode's part of the project (ADR 0018, schema v2): the `advanced` object
//! beside the Normal-mode fields. Tracks of placed clips (the main clip, another
//! library clip, a file, an image) with keyframed transform, opacity and volume and a
//! speed ramp; transitions between clips of a track; colour; markers; music ducking.
//! Normal mode never touches it, only keeps it (`Project::has_advanced_edits`).
//!
//! Keyframes and ramps are evaluated with the same integer-ms math as the front end
//! (`apps/klip-app/src/lib/keyframes.ts`), checked by `fixtures/advanced/*.json`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::project::{Issue, MAX_SOURCE_MS, field, list};

pub const MAX_TRACKS: usize = 16;
pub const MAX_CLIPS: usize = 256;
pub const MAX_KEYFRAMES: usize = 512;
pub const MAX_SPEED_POINTS: usize = 64;
pub const MAX_TRANSITIONS: usize = 256;
pub const MAX_MARKERS: usize = 1024;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_PATH_CHARS: usize = 1024;
/// Speed ramp rates, percent of realtime.
pub const RATE_PCT: std::ops::RangeInclusive<u32> = 25..=400;
pub const ZOOM: std::ops::RangeInclusive<f32> = 0.1..=8.0;
/// Longest a transition may take, ms.
pub const MAX_TRANSITION_MS: u64 = 10_000;

/// How a keyframe reaches the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ease {
    #[default]
    Linear,
    Hold,
}

/// A number at a time (ms into the clip's output).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keyframe {
    pub at_ms: u64,
    pub value: f32,
    pub ease: Ease,
}

/// Where a picture sits: `zoom` 1 = as is, `x`/`y` offsets as a fraction of the frame
/// (0 = centred, −1 … 1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transform {
    pub zoom: f32,
    pub x: f32,
    pub y: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Transform {
            zoom: 1.0,
            x: 0.0,
            y: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransformKey {
    pub at_ms: u64,
    pub value: Transform,
    pub ease: Ease,
}

/// From this output time on the clip plays at `rate_pct` % (until the next point).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeedPoint {
    pub at_ms: u64,
    pub rate_pct: u32,
}

impl Default for SpeedPoint {
    fn default() -> Self {
        SpeedPoint {
            at_ms: 0,
            rate_pct: 100,
        }
    }
}

/// What a placed clip shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    /// The clip the project belongs to.
    #[default]
    Main,
    /// Another clip of the library (a montage).
    Library { key: String },
    /// A media file (music, a sound effect).
    File { path: String },
    /// A picture (a sticker).
    Image { path: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    #[default]
    Video,
    Text,
    Audio,
    Music,
}

/// A clip on a track: source `in_ms..out_ms` placed at `at_ms` on the timeline, with
/// keyframes in the clip's own output time.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlacedClip {
    pub id: u32,
    pub source: Source,
    pub in_ms: u64,
    pub out_ms: u64,
    pub at_ms: u64,
    pub transform: Vec<TransformKey>,
    pub opacity: Vec<Keyframe>,
    pub volume: Vec<Keyframe>,
    pub speed: Vec<SpeedPoint>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    pub id: u32,
    pub kind: TrackKind,
    pub name: String,
    pub muted: bool,
    pub clips: Vec<PlacedClip>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransitionKind {
    #[default]
    Cut,
    Crossfade,
    /// Through black.
    Dip,
}

/// Between clip `after_clip` of `track` and the next one on that track.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transition {
    pub track: u32,
    pub after_clip: u32,
    pub kind: TransitionKind,
    pub ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Colour {
    /// −1 … 1, 0 = as recorded.
    pub brightness: f32,
    /// 0 … 2, 1 = as recorded.
    pub contrast: f32,
    /// 0 … 2, 1 = as recorded.
    pub saturation: f32,
}

impl Default for Colour {
    fn default() -> Self {
        Colour {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkerKind {
    Bookmark,
    Event,
    #[default]
    User,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Marker {
    /// Output time.
    pub at_ms: u64,
    pub title: String,
    pub kind: MarkerKind,
}

/// Music under voice.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ducking {
    pub enabled: bool,
    /// How far the music drops while someone speaks, dB (−40 … 0).
    pub duck_db: f32,
    pub attack_ms: u32,
    pub release_ms: u32,
}

impl Default for Ducking {
    fn default() -> Self {
        Ducking {
            enabled: true,
            duck_db: -12.0,
            attack_ms: 100,
            release_ms: 600,
        }
    }
}

/// The whole Advanced-mode section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Advanced {
    pub tracks: Vec<Track>,
    pub transitions: Vec<Transition>,
    pub colour: Colour,
    pub markers: Vec<Marker>,
    pub ducking: Ducking,
}

fn finite(v: f32, default: f32) -> f32 {
    if v.is_finite() { v } else { default }
}

fn clamp(v: f32, lo: f32, hi: f32, default: f32) -> f32 {
    finite(v, default).clamp(lo, hi)
}

fn cut_text(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Sort by time, keep one key per time (the last), cap the count.
fn tidy_keys<T: Copy>(keys: &mut Vec<T>, at: impl Fn(&T) -> u64, cap: usize) -> bool {
    let before = keys.len();
    keys.sort_by_key(&at);
    let mut out: Vec<T> = Vec::with_capacity(keys.len().min(cap));
    for k in keys.iter() {
        if out.last().is_some_and(|l| at(l) == at(k)) {
            out.pop();
        }
        out.push(*k);
    }
    out.truncate(cap);
    let changed = out.len() != before;
    *keys = out;
    changed
}

impl Advanced {
    /// Nothing an Advanced-mode user did.
    pub fn is_empty(&self) -> bool {
        self.tracks.iter().all(|t| t.clips.is_empty())
            && self.transitions.is_empty()
            && self.colour == Colour::default()
            && self.markers.iter().all(|m| m.kind != MarkerKind::User)
    }

    /// Lenient parse: a bad list entry is dropped, a bad field takes its default.
    pub(crate) fn from_value(v: Value, issues: &mut Vec<Issue>) -> Advanced {
        let Value::Object(mut map) = v else {
            issues.push(Issue::new("advanced", "not an object; using the default"));
            return Advanced::default();
        };
        let mut a = Advanced::default();
        if let Some(v) = map.remove("tracks") {
            let raw: Vec<Value> = list(v, "advanced.tracks", MAX_TRACKS, issues);
            for (i, t) in raw.into_iter().enumerate() {
                a.tracks.push(track_from_value(t, i, issues));
            }
        }
        if let Some(v) = map.remove("transitions") {
            a.transitions = list(v, "advanced.transitions", MAX_TRANSITIONS, issues);
        }
        if let Some(v) = map.remove("colour") {
            a.colour = field(v, "advanced.colour", issues);
        }
        if let Some(v) = map.remove("markers") {
            a.markers = list(v, "advanced.markers", MAX_MARKERS, issues);
        }
        if let Some(v) = map.remove("ducking") {
            a.ducking = field(v, "advanced.ducking", issues);
        }
        a
    }

    /// Clamp, sort and drop what can't be used; say what changed. Idempotent.
    pub fn repair(&mut self) -> Vec<Issue> {
        let mut issues = Vec::new();
        let mut note = |path: String, what: &str| issues.push(Issue::new(path, what));
        self.tracks.truncate(MAX_TRACKS);
        let mut seen_tracks = std::collections::HashSet::new();
        for (ti, track) in self.tracks.iter_mut().enumerate() {
            let tp = format!("advanced.tracks[{ti}]");
            if !seen_tracks.insert(track.id) {
                track.id = (ti as u32).saturating_add(1_000);
                note(format!("{tp}.id"), "duplicate id; renumbered");
            }
            track.name = cut_text(&track.name, MAX_TITLE_CHARS);
            let n = track.clips.len();
            track.clips.truncate(MAX_CLIPS);
            if track.clips.len() != n {
                note(
                    format!("{tp}.clips"),
                    "too many clips; the rest were dropped",
                );
            }
            let mut seen_clips = std::collections::HashSet::new();
            let mut kept = Vec::with_capacity(track.clips.len());
            for (ci, mut c) in std::mem::take(&mut track.clips).into_iter().enumerate() {
                let cp = format!("{tp}.clips[{ci}]");
                if c.in_ms >= c.out_ms || c.out_ms > MAX_SOURCE_MS || c.at_ms > MAX_SOURCE_MS {
                    note(cp, "empty or out-of-range clip; dropped");
                    continue;
                }
                if !seen_clips.insert(c.id) {
                    c.id = (ci as u32).saturating_add(1_000);
                    note(format!("{cp}.id"), "duplicate id; renumbered");
                }
                match &mut c.source {
                    Source::Library { key } => *key = cut_text(key, MAX_PATH_CHARS),
                    Source::File { path } | Source::Image { path } => {
                        *path = cut_text(path, MAX_PATH_CHARS)
                    }
                    Source::Main => {}
                }
                if tidy_keys(&mut c.transform, |k| k.at_ms, MAX_KEYFRAMES) {
                    note(
                        format!("{cp}.transform"),
                        "keyframes sorted, duplicates or extras dropped",
                    );
                }
                for k in &mut c.transform {
                    k.value.zoom = clamp(k.value.zoom, *ZOOM.start(), *ZOOM.end(), 1.0);
                    k.value.x = clamp(k.value.x, -1.0, 1.0, 0.0);
                    k.value.y = clamp(k.value.y, -1.0, 1.0, 0.0);
                }
                if tidy_keys(&mut c.opacity, |k| k.at_ms, MAX_KEYFRAMES) {
                    note(
                        format!("{cp}.opacity"),
                        "keyframes sorted, duplicates or extras dropped",
                    );
                }
                for k in &mut c.opacity {
                    k.value = clamp(k.value, 0.0, 1.0, 1.0);
                }
                if tidy_keys(&mut c.volume, |k| k.at_ms, MAX_KEYFRAMES) {
                    note(
                        format!("{cp}.volume"),
                        "keyframes sorted, duplicates or extras dropped",
                    );
                }
                for k in &mut c.volume {
                    k.value = clamp(k.value, 0.0, crate::project::MAX_VOLUME, 1.0);
                }
                if tidy_keys(&mut c.speed, |p| p.at_ms, MAX_SPEED_POINTS) {
                    note(
                        format!("{cp}.speed"),
                        "points sorted, duplicates or extras dropped",
                    );
                }
                for p in &mut c.speed {
                    p.rate_pct = p.rate_pct.clamp(*RATE_PCT.start(), *RATE_PCT.end());
                }
                kept.push(c);
            }
            track.clips = kept;
        }
        let n = self.transitions.len();
        self.transitions.retain(|t| t.ms <= MAX_TRANSITION_MS);
        if self.transitions.len() != n {
            note(
                "advanced.transitions".into(),
                "over-long transitions dropped",
            );
        }
        self.transitions.truncate(MAX_TRANSITIONS);
        let c = &mut self.colour;
        let fixed = Colour {
            brightness: clamp(c.brightness, -1.0, 1.0, 0.0),
            contrast: clamp(c.contrast, 0.0, 2.0, 1.0),
            saturation: clamp(c.saturation, 0.0, 2.0, 1.0),
        };
        if fixed != *c {
            note("advanced.colour".into(), "clamped");
            *c = fixed;
        }
        let n = self.markers.len();
        self.markers.retain(|m| m.at_ms <= MAX_SOURCE_MS);
        self.markers.truncate(MAX_MARKERS);
        for m in &mut self.markers {
            m.title = cut_text(&m.title, MAX_TITLE_CHARS);
        }
        self.markers.sort_by_key(|m| m.at_ms);
        if self.markers.len() != n {
            note(
                "advanced.markers".into(),
                "out-of-range or extra markers dropped",
            );
        }
        let d = &mut self.ducking;
        let fixed = Ducking {
            enabled: d.enabled,
            duck_db: clamp(d.duck_db, -40.0, 0.0, -12.0),
            attack_ms: d.attack_ms.min(5_000),
            release_ms: d.release_ms.min(10_000),
        };
        if fixed != *d {
            note("advanced.ducking".into(), "clamped");
            *d = fixed;
        }
        issues
    }
}

fn track_from_value(v: Value, i: usize, issues: &mut Vec<Issue>) -> Track {
    let path = format!("advanced.tracks[{i}]");
    let Value::Object(mut map) = v else {
        issues.push(Issue::new(path, "not an object; using the default"));
        return Track::default();
    };
    let mut t = Track::default();
    if let Some(v) = map.remove("id") {
        t.id = field(v, &format!("{path}.id"), issues);
    }
    if let Some(v) = map.remove("kind") {
        t.kind = field(v, &format!("{path}.kind"), issues);
    }
    if let Some(v) = map.remove("name") {
        t.name = field(v, &format!("{path}.name"), issues);
    }
    if let Some(v) = map.remove("muted") {
        t.muted = field(v, &format!("{path}.muted"), issues);
    }
    if let Some(v) = map.remove("clips") {
        t.clips = list(v, &format!("{path}.clips"), MAX_CLIPS, issues);
    }
    t
}

/// Keyframe evaluation: before the first key its value, after the last key its value,
/// between two keys `Hold` keeps the earlier value and `Linear` interpolates in f64
/// (identical in the front end).
pub mod keyframes {
    use super::{Ease, Keyframe, Transform, TransformKey};

    fn lerp(a: f32, b: f32, ta: u64, tb: u64, t: u64) -> f32 {
        if tb <= ta {
            return b;
        }
        let f = (t.saturating_sub(ta)) as f64 / (tb.saturating_sub(ta)) as f64;
        (f64::from(a) + (f64::from(b) - f64::from(a)) * f) as f32
    }

    fn around<T: Copy>(keys: &[T], at: impl Fn(&T) -> u64, t: u64) -> Option<(T, Option<T>)> {
        let first = *keys.first()?;
        if t <= at(&first) {
            return Some((first, None));
        }
        let i = keys.iter().rposition(|k| at(k) <= t)?;
        Some((*keys.get(i)?, keys.get(i.saturating_add(1)).copied()))
    }

    /// The value at output time `t`; `None` without keys.
    pub fn value_at(keys: &[Keyframe], t: u64) -> Option<f32> {
        let (a, b) = around(keys, |k| k.at_ms, t)?;
        Some(match (a.ease, b) {
            (Ease::Linear, Some(b)) => lerp(a.value, b.value, a.at_ms, b.at_ms, t),
            _ => a.value,
        })
    }

    /// The transform at output time `t`; `None` without keys.
    pub fn transform_at(keys: &[TransformKey], t: u64) -> Option<Transform> {
        let (a, b) = around(keys, |k| k.at_ms, t)?;
        Some(match (a.ease, b) {
            (Ease::Linear, Some(b)) => Transform {
                zoom: lerp(a.value.zoom, b.value.zoom, a.at_ms, b.at_ms, t),
                x: lerp(a.value.x, b.value.x, a.at_ms, b.at_ms, t),
                y: lerp(a.value.y, b.value.y, a.at_ms, b.at_ms, t),
            },
            _ => a.value,
        })
    }
}

/// Speed ramps: piecewise-constant rates (percent) from each point to the next; the
/// first point is at 0 (100 % if absent). Integer math, floor per segment.
pub mod ramp {
    use super::SpeedPoint;

    fn segments(points: &[SpeedPoint]) -> Vec<SpeedPoint> {
        let mut out: Vec<SpeedPoint> = Vec::with_capacity(points.len().saturating_add(1));
        if points.first().is_none_or(|p| p.at_ms > 0) {
            out.push(SpeedPoint::default());
        }
        for p in points {
            if out.last().is_some_and(|l| l.at_ms >= p.at_ms) {
                out.pop();
            }
            out.push(*p);
        }
        out
    }

    /// Source ms consumed by `out_ms` of output.
    pub fn source_offset(points: &[SpeedPoint], out_ms: u64) -> u64 {
        let segs = segments(points);
        let mut src: u64 = 0;
        for (i, p) in segs.iter().enumerate() {
            if out_ms <= p.at_ms {
                break;
            }
            let end = segs
                .get(i.saturating_add(1))
                .map_or(out_ms, |n| n.at_ms.min(out_ms));
            let len = end.saturating_sub(p.at_ms);
            src = src.saturating_add(
                len.saturating_mul(u64::from(p.rate_pct))
                    .checked_div(100)
                    .unwrap_or(0),
            );
        }
        src
    }

    /// Output ms it takes to play `src_len` source ms (the clip's output length).
    pub fn output_length(points: &[SpeedPoint], src_len: u64) -> u64 {
        let segs = segments(points);
        let mut src: u64 = 0;
        let mut out: u64 = 0;
        for (i, p) in segs.iter().enumerate() {
            let rate = u64::from(p.rate_pct).max(1);
            let Some(next) = segs.get(i.saturating_add(1)) else {
                let left = src_len.saturating_sub(src);
                return out.saturating_add(
                    left.saturating_mul(100)
                        .saturating_add(rate.saturating_sub(1))
                        .checked_div(rate)
                        .unwrap_or(0),
                );
            };
            let len = next.at_ms.saturating_sub(p.at_ms);
            let consumed = len.saturating_mul(rate).checked_div(100).unwrap_or(0);
            if src.saturating_add(consumed) >= src_len {
                let left = src_len.saturating_sub(src);
                return out.saturating_add(
                    left.saturating_mul(100)
                        .saturating_add(rate.saturating_sub(1))
                        .checked_div(rate)
                        .unwrap_or(0),
                );
            }
            src = src.saturating_add(consumed);
            out = next.at_ms;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::keyframes::{transform_at, value_at};
    use super::ramp::{output_length, source_offset};
    use super::*;

    fn key(at_ms: u64, value: f32, ease: Ease) -> Keyframe {
        Keyframe { at_ms, value, ease }
    }

    #[test]
    fn keyframes_interpolate_and_hold() {
        let keys = [
            key(1000, 1.0, Ease::Linear),
            key(2000, 3.0, Ease::Hold),
            key(3000, 0.0, Ease::Linear),
        ];
        assert_eq!(value_at(&keys, 0), Some(1.0));
        assert_eq!(value_at(&keys, 1500), Some(2.0));
        assert_eq!(value_at(&keys, 2000), Some(3.0));
        assert_eq!(value_at(&keys, 2999), Some(3.0));
        assert_eq!(value_at(&keys, 3000), Some(0.0));
        assert_eq!(value_at(&keys, 9000), Some(0.0));
        assert_eq!(value_at(&[], 5), None);
        let t = [
            TransformKey {
                at_ms: 0,
                value: Transform::default(),
                ease: Ease::Linear,
            },
            TransformKey {
                at_ms: 1000,
                value: Transform {
                    zoom: 2.0,
                    x: 1.0,
                    y: -1.0,
                },
                ease: Ease::Linear,
            },
        ];
        assert_eq!(
            transform_at(&t, 500),
            Some(Transform {
                zoom: 1.5,
                x: 0.5,
                y: -0.5
            })
        );
    }

    #[test]
    fn ramps_are_piecewise_constant() {
        let pts = [
            SpeedPoint {
                at_ms: 1000,
                rate_pct: 200,
            },
            SpeedPoint {
                at_ms: 3000,
                rate_pct: 50,
            },
        ];
        assert_eq!(source_offset(&pts, 0), 0);
        assert_eq!(source_offset(&pts, 500), 500);
        assert_eq!(source_offset(&pts, 1500), 2000);
        assert_eq!(source_offset(&pts, 3000), 5000);
        assert_eq!(source_offset(&pts, 4000), 5500);
        assert_eq!(output_length(&pts, 6000), 5000);
        assert_eq!(output_length(&pts, 500), 500);
        assert_eq!(output_length(&[], 1234), 1234);
        assert_eq!(
            output_length(
                &[SpeedPoint {
                    at_ms: 0,
                    rate_pct: 400
                }],
                1001
            ),
            251
        );
    }

    #[test]
    fn repair_clamps_sorts_and_drops() {
        let v: Value = serde_json::json!({
            "tracks": [
                {"id": 1, "kind": "video", "name": "A\u{0}", "clips": [
                    {"id": 1, "source": {"kind": "main"}, "in_ms": 0, "out_ms": 5000, "at_ms": 0,
                     "transform": [{"at_ms": 500, "value": {"zoom": 99, "x": 5, "y": -5}}, {"at_ms": 0, "value": {"zoom": 1}}, {"at_ms": 0, "value": {"zoom": 2}}],
                     "speed": [{"at_ms": 0, "rate_pct": 9999}]},
                    {"id": 1, "source": {"kind": "library", "key": "b.mp4"}, "in_ms": 5, "out_ms": 5, "at_ms": 0},
                    "junk"
                ]},
                {"id": 1, "kind": "music", "clips": []},
                7
            ],
            "colour": {"brightness": 9, "contrast": -1, "saturation": 7},
            "markers": [{"at_ms": 3000, "title": "later"}, {"at_ms": 1000, "title": "first", "kind": "event"}],
            "ducking": {"duck_db": -99}
        });
        let mut issues = Vec::new();
        let mut a = Advanced::from_value(v, &mut issues);
        issues.extend(a.repair());
        assert_eq!(
            a.tracks.len(),
            3,
            "a bad track becomes a default one, a bad clip is dropped"
        );
        assert_eq!(a.tracks[0].clips.len(), 1);
        assert_eq!(a.tracks[0].name, "A");
        let c = &a.tracks[0].clips[0];
        assert_eq!(c.transform.len(), 2);
        assert_eq!(c.transform[0].at_ms, 0);
        assert_eq!(
            c.transform[0].value.zoom, 2.0,
            "the last key at a time wins"
        );
        assert_eq!(
            c.transform[1].value,
            Transform {
                zoom: 8.0,
                x: 1.0,
                y: -1.0
            }
        );
        assert_eq!(c.speed[0].rate_pct, 400);
        assert_ne!(a.tracks[1].id, a.tracks[0].id, "renumbered");
        assert_eq!(
            a.colour,
            Colour {
                brightness: 1.0,
                contrast: 0.0,
                saturation: 2.0
            }
        );
        assert_eq!(a.markers[0].title, "first");
        assert_eq!(a.ducking.duck_db, -40.0);
        assert!(!issues.is_empty());
        let again = a.repair();
        assert!(again.is_empty(), "{again:?}");
        assert!(!a.is_empty());
        assert!(Advanced::default().is_empty());
    }
}
