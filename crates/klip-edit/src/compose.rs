//! From a project in either mode to what the renderer draws (ADR 0018): a
//! [`Composition`] of layers on the output timeline. Normal mode projects into it
//! (each output segment of the main clip is a video layer at a constant speed; text
//! and the two volumes ride along), Advanced mode's tracks add theirs; transitions
//! become overlaps and opacity ramps. Pure, integer ms; the renderer (`klip-export`)
//! and the preview's canvas (`src/lib/keyframes.ts`) read the same numbers.

use serde::{Deserialize, Serialize};

use crate::advanced::keyframes::{transform_at, value_at};
use crate::advanced::{
    Colour, Ducking, Ease, FROM_BELOW, Keyframe, Marker, MarkerKind, Source, SpeedPoint, TrackKind,
    Transform, TransformKey, TransitionKind, ramp,
};
use crate::format::Rect;
use crate::plan::{MAX_MARKERS, SourceInfo};
use crate::project::{MAX_SOURCE_MS, Project, Speed, Volumes};
use crate::text::TextLayer;
use crate::timeline;

/// Most layers a composition holds (tracks × clips, bounded by the caps anyway).
pub const MAX_LAYERS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerKind {
    Video,
    Image,
    Audio,
    Music,
    /// The whole frame black, at the layer's opacity: a dip to black on a picture track
    /// (no file; `source` and the source range mean nothing).
    Black,
}

/// One placed clip on the output timeline, `at_ms..end_ms`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: u32,
    /// 0 = the main clip as Normal mode cut it; else the Advanced track's id.
    pub track: u32,
    pub kind: LayerKind,
    pub source: Source,
    pub src_in_ms: u64,
    pub src_out_ms: u64,
    pub at_ms: u64,
    pub end_ms: u64,
    /// Draw order: higher on top.
    pub z: u32,
    /// No sound (a muted track).
    pub muted: bool,
    /// No picture (a hidden track); its sound still plays unless muted.
    #[serde(default)]
    pub hidden: bool,
    pub transform: Vec<TransformKey>,
    pub opacity: Vec<Keyframe>,
    pub volume: Vec<Keyframe>,
    pub speed: Vec<SpeedPoint>,
}

impl Layer {
    /// Whether the layer is on screen (or audible) at output time `out_ms`.
    pub fn active(&self, out_ms: u64) -> bool {
        out_ms >= self.at_ms && out_ms < self.end_ms
    }

    /// The source time shown at output time `out_ms`, or `None` outside the layer.
    pub fn source_ms(&self, out_ms: u64) -> Option<u64> {
        if !self.active(out_ms) {
            return None;
        }
        let src = self.src_in_ms.saturating_add(ramp::source_offset(
            &self.speed,
            out_ms.saturating_sub(self.at_ms),
        ));
        Some(src.min(self.src_out_ms.saturating_sub(1)))
    }

    /// The first output time that shows source time `src_ms`, or `None` when the layer
    /// doesn't play it (the inverse of [`Self::source_ms`]).
    pub fn output_ms(&self, src_ms: u64) -> Option<u64> {
        if src_ms < self.src_in_ms || src_ms >= self.src_out_ms || self.end_ms <= self.at_ms {
            return None;
        }
        let want = src_ms.saturating_sub(self.src_in_ms);
        // `source_offset` never decreases: the first local time reaching `want`.
        let (mut lo, mut hi) = (0u64, self.end_ms.saturating_sub(self.at_ms));
        while lo < hi {
            let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
            if ramp::source_offset(&self.speed, mid) >= want {
                hi = mid;
            } else {
                lo = mid.saturating_add(1);
            }
        }
        let out = self.at_ms.saturating_add(lo);
        self.active(out).then_some(out)
    }

    /// Layer-local time for the keyframes.
    fn local(&self, out_ms: u64) -> u64 {
        out_ms.saturating_sub(self.at_ms)
    }

    pub fn transform_at(&self, out_ms: u64) -> Transform {
        transform_at(&self.transform, self.local(out_ms)).unwrap_or_default()
    }

    /// 0..=1.
    pub fn opacity_at(&self, out_ms: u64) -> f32 {
        value_at(&self.opacity, self.local(out_ms))
            .unwrap_or(1.0)
            .clamp(0.0, 1.0)
    }

    /// 0..=2 (1 = as recorded); 0 when muted.
    pub fn volume_at(&self, out_ms: u64) -> f32 {
        if self.muted {
            return 0.0;
        }
        value_at(&self.volume, self.local(out_ms))
            .unwrap_or(1.0)
            .clamp(0.0, crate::project::MAX_VOLUME)
    }
}

/// The whole picture and sound of an edit.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Composition {
    pub duration_ms: u64,
    pub layers: Vec<Layer>,
    /// Normal mode's text cues (output time).
    pub texts: Vec<TextLayer>,
    /// The main clip's Game/Mic mix (Normal mode's sliders).
    pub main_volumes: Volumes,
    pub colour: Colour,
    /// The main clip as probed when the composition was made.
    pub source: SourceInfo,
    /// The part of the main clip's frame that is kept (Normal mode's format), source
    /// pixels: the canvas is this, scaled to the export size.
    pub crop: Rect,
    /// Output-time markers: the clip's bookmarks that survive the edit, then the
    /// Advanced-mode markers.
    pub markers: Vec<Marker>,
    pub ducking: Ducking,
}

impl Composition {
    /// Layers in draw order at `out_ms` (bottom first).
    pub fn visible_at(&self, out_ms: u64) -> Vec<&Layer> {
        let mut v: Vec<&Layer> = self
            .layers
            .iter()
            .filter(|l| {
                matches!(
                    l.kind,
                    LayerKind::Video | LayerKind::Image | LayerKind::Black
                ) && !l.hidden
                    && l.active(out_ms)
            })
            .collect();
        v.sort_by_key(|l| (l.z, l.id));
        v
    }

    /// Audible layers at `out_ms` (the main clip's own audio is `main_volumes`).
    pub fn audible_at(&self, out_ms: u64) -> Vec<&Layer> {
        self.layers
            .iter()
            .filter(|l| {
                matches!(
                    l.kind,
                    LayerKind::Audio | LayerKind::Music | LayerKind::Video
                ) && l.active(out_ms)
                    && !l.muted
            })
            .collect()
    }

    /// Anything beyond the main clip's own segments.
    pub fn has_advanced_layers(&self) -> bool {
        self.layers.iter().any(|l| l.track != 0)
    }
}

fn rate_pct(speed: Speed) -> u32 {
    match speed {
        Speed::Half => 50,
        Speed::Normal => 100,
        Speed::Double => 200,
    }
}

fn kind_of(track: TrackKind, source: &Source) -> LayerKind {
    match (track, source) {
        (_, Source::Image { .. }) => LayerKind::Image,
        (TrackKind::Music, _) => LayerKind::Music,
        (TrackKind::Audio, _) => LayerKind::Audio,
        (TrackKind::Video | TrackKind::Text, _) => LayerKind::Video,
    }
}

fn fade(from_ms: u64, to_ms: u64, from: f32, to: f32) -> Vec<Keyframe> {
    vec![
        Keyframe {
            at_ms: from_ms,
            value: from,
            ease: Ease::Linear,
        },
        Keyframe {
            at_ms: to_ms,
            value: to,
            ease: Ease::Linear,
        },
    ]
}

/// A dip's black over `from..until`: it fades in up to `black_at` (where the clip before
/// ends), stays black to `back_at` (where the next one starts) and fades out by `until`.
fn black_layer(
    id: u32,
    track: u32,
    z: u32,
    (muted, hidden): (bool, bool),
    (from, black_at, back_at, until): (u64, u64, u64, u64),
) -> Layer {
    let key = |t: u64, value: f32| Keyframe {
        at_ms: t.saturating_sub(from),
        value,
        ease: Ease::Linear,
    };
    let mut opacity = vec![key(from, 0.0), key(black_at, 1.0)];
    if back_at > black_at {
        opacity.push(key(back_at, 1.0));
    }
    opacity.push(key(until, 0.0));
    Layer {
        id,
        track,
        kind: LayerKind::Black,
        source: Source::Main,
        src_in_ms: 0,
        src_out_ms: 0,
        at_ms: from,
        end_ms: until,
        z,
        muted,
        hidden,
        transform: Vec::new(),
        opacity,
        volume: Vec::new(),
        speed: Vec::new(),
    }
}

/// A clip's chapters: source ms and, for an event, its title.
fn chapters(info: &SourceInfo) -> Vec<(u64, Option<&str>)> {
    let title_at = |ms: u64| {
        info.events
            .iter()
            .find(|e| e.at_ms == ms && !e.title.is_empty())
            .map(|e| e.title.as_str())
    };
    let mut out: Vec<(u64, Option<&str>)> = info
        .bookmarks_ms
        .iter()
        .take(MAX_MARKERS)
        .map(|&ms| (ms, title_at(ms)))
        .collect();
    for e in info.events.iter().take(MAX_MARKERS) {
        if !info.bookmarks_ms.contains(&e.at_ms) && !e.title.is_empty() {
            out.push((e.at_ms, Some(e.title.as_str())));
        }
    }
    out
}

fn chapter_marker(at_ms: u64, title: Option<&str>) -> Marker {
    match title {
        Some(t) => Marker {
            at_ms,
            title: t.to_string(),
            kind: MarkerKind::Event,
        },
        None => Marker {
            at_ms,
            title: String::new(),
            kind: MarkerKind::Bookmark,
        },
    }
}

/// Build the composition of `project` over `source` (the main clip as probed).
pub fn compose(project: &Project, source: &SourceInfo) -> Composition {
    compose_with(project, source, &[])
}

/// [`compose`] for a montage: `clips` are the project's other library clips as probed,
/// by key. Every clip's bookmarks and events become markers where the clip plays them.
pub fn compose_with(
    project: &Project,
    source: &SourceInfo,
    clips: &[(String, SourceInfo)],
) -> Composition {
    let mut p = project.clone();
    if source.duration_ms > 0 && source.duration_ms <= MAX_SOURCE_MS {
        p.source_ms = source.duration_ms;
    }
    p.repair();
    let segments = timeline::segments(&p);
    let mut layers: Vec<Layer> = Vec::new();
    let mut next_id: u32 = 1;
    // V1 arranged on the Advanced timeline replaces Normal mode's cut (M9.8.9).
    let arranged = p.advanced.tracks.iter().any(|t| t.main);
    // Track 0: the main clip as Normal mode cut it.
    for seg in segments.iter().filter(|_| !arranged) {
        layers.push(Layer {
            id: next_id,
            track: 0,
            kind: LayerKind::Video,
            source: Source::Main,
            src_in_ms: seg.src_start_ms,
            src_out_ms: seg.src_end_ms,
            at_ms: seg.out_start_ms,
            end_ms: seg.out_end_ms,
            z: 0,
            muted: false,
            hidden: false,
            transform: Vec::new(),
            opacity: Vec::new(),
            volume: Vec::new(),
            speed: vec![SpeedPoint {
                at_ms: 0,
                rate_pct: rate_pct(seg.speed),
            }],
        });
        next_id = next_id.saturating_add(1);
    }
    let mut duration = if arranged {
        0
    } else {
        segments.last().map_or(0, |s| s.out_end_ms)
    };
    // Advanced tracks above it, in order (an arranged main track at the bottom).
    for (ti, track) in p.advanced.tracks.iter().enumerate() {
        let z = if track.main {
            0
        } else {
            u32::try_from(ti).unwrap_or(u32::MAX).saturating_add(1)
        };
        // The previous clip of this track: its layer index and its project id.
        let mut previous: Option<(usize, u32)> = None;
        for clip in &track.clips {
            if layers.len() >= MAX_LAYERS {
                break;
            }
            let src_len = clip.out_ms.saturating_sub(clip.in_ms);
            let mut at = clip.at_ms.min(MAX_SOURCE_MS);
            let mut opacity = clip.opacity.clone();
            // A dip on a picture track: where the black starts, where the clip before
            // ends, and the dip's length.
            let mut dip: Option<(u64, u64, u64)> = None;
            // A transition after the previous clip of this track: overlap by its
            // length (crossfade) or dip at the join. The first clip may crossfade in
            // from whatever is below it (`FROM_BELOW`).
            let after = previous.map_or(FROM_BELOW, |(_, prev_id)| prev_id);
            let transition = p
                .advanced
                .transitions
                .iter()
                .find(|t| t.track == track.id && t.after_clip == after);
            if let Some(tr) = transition {
                let ms = tr.ms.min(src_len);
                match tr.kind {
                    TransitionKind::Cut => {}
                    TransitionKind::Crossfade => {
                        at = at.saturating_sub(ms);
                        if opacity.is_empty() {
                            opacity = fade(0, ms, 0.0, 1.0);
                        }
                    }
                    TransitionKind::Dip => {
                        if let Some((prev_i, _)) = previous {
                            if matches!(track.kind, TrackKind::Video | TrackKind::Text) {
                                // Through black whatever is below (FilmCraft's dip): a
                                // black layer over the join, the clips left as they are.
                                if let Some(prev) = layers.get(prev_i)
                                    && ms > 0
                                {
                                    let len = prev.end_ms.saturating_sub(prev.at_ms);
                                    dip = Some((
                                        prev.end_ms.saturating_sub(ms.min(len)),
                                        prev.end_ms,
                                        ms,
                                    ));
                                }
                            } else {
                                if let Some(prev) = layers.get_mut(prev_i)
                                    && prev.opacity.is_empty()
                                {
                                    let len = prev.end_ms.saturating_sub(prev.at_ms);
                                    prev.opacity = fade(len.saturating_sub(ms), len, 1.0, 0.0);
                                }
                                if opacity.is_empty() {
                                    opacity = fade(0, ms, 0.0, 1.0);
                                }
                            }
                        }
                    }
                }
            }
            let out_len = ramp::output_length(&clip.speed, src_len);
            let end = at
                .saturating_add(out_len)
                .min(MAX_SOURCE_MS.saturating_mul(2));
            if end <= at {
                continue;
            }
            layers.push(Layer {
                id: next_id,
                track: track.id.max(1),
                kind: kind_of(track.kind, &clip.source),
                source: clip.source.clone(),
                src_in_ms: clip.in_ms,
                src_out_ms: clip.out_ms,
                at_ms: at,
                end_ms: end,
                z,
                muted: track.muted,
                hidden: track.hidden,
                transform: clip.transform.clone(),
                opacity,
                volume: clip.volume.clone(),
                speed: clip.speed.clone(),
            });
            previous = Some((layers.len().saturating_sub(1), clip.id));
            next_id = next_id.saturating_add(1);
            duration = duration.max(end);
            if let Some((from, black_at, ms)) = dip
                && layers.len() < MAX_LAYERS
            {
                // Same z, a later id: over both clips, under the tracks above.
                layers.push(black_layer(
                    next_id,
                    track.id.max(1),
                    z,
                    (track.muted, track.hidden),
                    (from, black_at, at, at.saturating_add(ms.min(out_len))),
                ));
                next_id = next_id.saturating_add(1);
            }
        }
    }
    let main_chapters = chapters(source);
    // (Arranged, the main clip's chapters come with its layers below.)
    let mut markers: Vec<Marker> = main_chapters
        .iter()
        .filter(|_| !arranged)
        .filter_map(|&(src, title)| {
            timeline::output_time_in(&segments, src).map(|at| chapter_marker(at, title))
        })
        .collect();
    // Chapters of the clips placed on Advanced tracks, where they play.
    for l in layers
        .iter()
        .filter(|l| l.track != 0 && l.kind != LayerKind::Black)
    {
        let info = match &l.source {
            Source::Main => source,
            Source::Library { key } => match clips.iter().find(|(k, _)| k == key) {
                Some((_, info)) => info,
                None => continue,
            },
            Source::File { .. } | Source::Image { .. } => continue,
        };
        let list = if l.source == Source::Main {
            main_chapters.clone()
        } else {
            chapters(info)
        };
        for (src, title) in list {
            if let Some(at) = l.output_ms(src) {
                markers.push(chapter_marker(at, title));
            }
        }
    }
    markers.extend(
        p.advanced
            .markers
            .iter()
            .filter(|m| m.at_ms < duration)
            .cloned(),
    );
    let rank = |k: MarkerKind| match k {
        MarkerKind::Bookmark => 0u8,
        MarkerKind::Event => 1,
        MarkerKind::User => 2,
    };
    markers
        .sort_by(|a, b| (a.at_ms, rank(a.kind), &a.title).cmp(&(b.at_ms, rank(b.kind), &b.title)));
    // A clip placed twice over the same time gives the same marker twice.
    markers.dedup();
    markers.truncate(MAX_MARKERS);
    let mut n: usize = 0;
    for m in &mut markers {
        if m.kind == MarkerKind::Bookmark {
            n = n.saturating_add(1);
            if m.title.is_empty() {
                m.title = format!("Bookmark {n}");
            }
        } else if m.title.is_empty() {
            m.title = "Marker".into();
        }
    }
    Composition {
        duration_ms: duration,
        layers,
        texts: p
            .text
            .iter()
            .filter(|t| t.start_ms < t.end_ms && t.start_ms < duration)
            .cloned()
            .collect(),
        main_volumes: p.volumes,
        colour: p.advanced.colour,
        markers,
        ducking: p.advanced.ducking,
        source: source.clone(),
        crop: p.format.crop_rect(source.width, source.height),
    }
}

/// Where a layer's picture lands on the output canvas at one output time, in output
/// pixels. The renderer sets the compositor pad from it; the preview's canvas draws
/// the same rectangle (`src/lib/keyframes.ts`), so both round the same way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// `floor(v + 0.5)`: the same on every platform and in JavaScript (`Math.round`
/// disagrees with Rust's `round` on negative halves).
fn round_half_up(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// `w x h` scaled to fit `max_w x max_h` keeping the aspect ratio (up only when
/// `upscale`); even sides, at least 2.
fn scale_into(w: u32, h: u32, max_w: u32, max_h: u32, upscale: bool) -> (u32, u32) {
    let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
    let mut scale = (f64::from(max_w.max(2)) / w).min(f64::from(max_h.max(2)) / h);
    if !upscale {
        scale = scale.min(1.0);
    }
    let even = |v: f64| {
        let px = round_half_up(v).clamp(2.0, f64::from(u32::MAX)) as u32;
        (px & !1).max(2)
    };
    (even(w * scale), even(h * scale))
}

/// `w x h` scaled down (never up) to fit `max_w x max_h`, keeping the aspect ratio;
/// even sides, at least 2.
pub fn fit_in(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    scale_into(w, h, max_w, max_h, false)
}

/// `w x h` scaled up or down to fit `max_w x max_h`, keeping the aspect ratio; even
/// sides, at least 2.
pub fn fit_contain(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    scale_into(w, h, max_w, max_h, true)
}

/// A layer's picture size at zoom 1, in canvas pixels. The canvas is the main clip's
/// crop scaled to the export size, so the main clip fills it; another video fits inside
/// it (scaled up or down, keeping its aspect); a picture keeps its size relative to the
/// main clip (`crop_w` source pixels across = the canvas width), shrunk to fit.
/// `native` is the picture's own size (a video's whole frame).
pub fn fitted(
    kind: LayerKind,
    main: bool,
    native: (u32, u32),
    crop_w: u32,
    canvas: (u32, u32),
) -> (u32, u32) {
    let (out_w, out_h) = (canvas.0.max(2), canvas.1.max(2));
    if main {
        return (out_w, out_h);
    }
    match kind {
        LayerKind::Image => {
            let s = f64::from(out_w) / f64::from(crop_w.max(1));
            let px = |v: u32| {
                round_half_up(f64::from(v.max(1)) * s).clamp(1.0, f64::from(u32::MAX)) as u32
            };
            fit_in(px(native.0), px(native.1), out_w, out_h)
        }
        _ => fit_contain(native.0, native.1, out_w, out_h),
    }
}

/// What of a `src_w x src_h` picture placed at `p` lands on an `out_w x out_h` canvas:
/// the source rectangle (whole pixels, at least 1) and where it goes, or `None` when
/// nothing shows. Drawing only this part keeps a big zoom from scaling the whole
/// picture to many times the canvas.
pub fn visible(
    p: Placement,
    src_w: u32,
    src_h: u32,
    out_w: u32,
    out_h: u32,
) -> Option<(Rect, Rect)> {
    if p.width == 0 || p.height == 0 || src_w == 0 || src_h == 0 {
        return None;
    }
    let axis = |pos: i32, len: u32, src: u32, out: u32| -> Option<(u32, u32, u32, u32)> {
        let start = i64::from(pos);
        let end = start.saturating_add(i64::from(len));
        let d0 = start.max(0);
        let d1 = end.min(i64::from(out));
        if d1 <= d0 {
            return None;
        }
        let to_src = |d: i64| {
            let v = (d.saturating_sub(start)) as f64 * f64::from(src) / f64::from(len);
            round_half_up(v).clamp(0.0, f64::from(src)) as u32
        };
        let mut s0 = to_src(d0);
        let mut s1 = to_src(d1);
        if s1 <= s0 {
            s1 = s0.saturating_add(1).min(src);
            s0 = s1.saturating_sub(1);
        }
        let d0 = u32::try_from(d0).ok()?;
        let d1 = u32::try_from(d1).ok()?;
        Some((s0, s1.saturating_sub(s0), d0, d1.saturating_sub(d0)))
    };
    let (sx, sw, dx, dw) = axis(p.x, p.width, src_w, out_w)?;
    let (sy, sh, dy, dh) = axis(p.y, p.height, src_h, out_h)?;
    Some((
        Rect {
            x: sx,
            y: sy,
            width: sw,
            height: sh,
        },
        Rect {
            x: dx,
            y: dy,
            width: dw,
            height: dh,
        },
    ))
}

/// The placement of a picture whose fitted (zoom 1) size is `fit_w x fit_h` on a
/// `out_w x out_h` canvas under `t`: `zoom` scales about the centre, `x`/`y` shift by
/// that fraction of the canvas.
pub fn place(t: Transform, fit_w: u32, fit_h: u32, out_w: u32, out_h: u32) -> Placement {
    let zoom = if t.zoom.is_finite() {
        f64::from(t.zoom).clamp(
            *crate::advanced::ZOOM.start() as f64,
            *crate::advanced::ZOOM.end() as f64,
        )
    } else {
        1.0
    };
    let shift = |v: f32| {
        if v.is_finite() {
            f64::from(v).clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    let w = round_half_up(f64::from(fit_w) * zoom).max(1.0);
    let h = round_half_up(f64::from(fit_h) * zoom).max(1.0);
    let x = round_half_up((f64::from(out_w) - w) / 2.0 + shift(t.x) * f64::from(out_w));
    let y = round_half_up((f64::from(out_h) - h) / 2.0 + shift(t.y) * f64::from(out_h));
    let px = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    Placement {
        x: px(x),
        y: px(y),
        width: w.min(f64::from(u32::MAX)) as u32,
        height: h.min(f64::from(u32::MAX)) as u32,
    }
}

/// One constant-rate span of a layer: what one seek of its source plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Piece {
    pub out_start_ms: u64,
    pub out_end_ms: u64,
    pub src_start_ms: u64,
    pub src_end_ms: u64,
    pub rate_pct: u32,
}

impl Layer {
    /// The layer cut into constant-rate pieces at its speed points, none longer than
    /// `max_ms` of output (so a renderer's buffers stay bounded). The source range of
    /// each piece follows [`Layer::source_ms`] exactly.
    pub fn pieces(&self, max_ms: u64) -> Vec<Piece> {
        let max_ms = max_ms.max(1);
        let len = self.end_ms.saturating_sub(self.at_ms);
        if len == 0 {
            return Vec::new();
        }
        let mut cuts: Vec<u64> = vec![0];
        for p in &self.speed {
            if p.at_ms > 0 && p.at_ms < len {
                cuts.push(p.at_ms);
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        cuts.push(len);
        let mut out = Vec::new();
        for pair in cuts.windows(2) {
            let (&a, &b) = match pair {
                [a, b] => (a, b),
                _ => continue,
            };
            let rate_pct = self
                .speed
                .iter()
                .filter(|p| p.at_ms <= a)
                .max_by_key(|p| p.at_ms)
                .map_or(100, |p| p.rate_pct.max(1));
            let mut start = a;
            while start < b {
                let end = start.saturating_add(max_ms).min(b);
                let src_start = self
                    .src_in_ms
                    .saturating_add(ramp::source_offset(&self.speed, start))
                    .min(self.src_out_ms.saturating_sub(1));
                let src_end = self
                    .src_in_ms
                    .saturating_add(ramp::source_offset(&self.speed, end))
                    .clamp(
                        src_start.saturating_add(1),
                        self.src_out_ms.max(src_start.saturating_add(1)),
                    );
                out.push(Piece {
                    out_start_ms: self.at_ms.saturating_add(start),
                    out_end_ms: self.at_ms.saturating_add(end),
                    src_start_ms: src_start,
                    src_end_ms: src_end,
                    rate_pct,
                });
                start = end;
            }
        }
        out
    }

    /// The largest zoom the layer reaches (1 without transform keys): how big its
    /// source has to be decoded so a zoom-in is scaled up once, not twice.
    pub fn max_zoom(&self) -> f32 {
        self.transform
            .iter()
            .map(|k| k.value.zoom)
            .filter(|z| z.is_finite())
            .fold(1.0f32, f32::max)
            .clamp(1.0, *crate::advanced::ZOOM.end())
    }
}

impl Composition {
    /// The output time of frame `i` at `fps`, or `None` past the end.
    pub fn frame_out_ms(&self, i: u64, fps: u32) -> Option<u64> {
        if fps == 0 {
            return None;
        }
        let ms = i.checked_mul(1000)?.checked_div(u64::from(fps))?;
        (ms < self.duration_ms).then_some(ms)
    }

    /// How many output frames an export at `fps` has.
    pub fn frame_count(&self, fps: u32) -> u64 {
        if fps == 0 {
            return 0;
        }
        self.duration_ms
            .saturating_mul(u64::from(fps))
            .saturating_add(999)
            .checked_div(1000)
            .unwrap_or(0)
    }

    /// The text cues shown at `out_ms`, in layer order.
    pub fn texts_at(&self, out_ms: u64) -> impl Iterator<Item = &TextLayer> {
        self.texts
            .iter()
            .filter(move |t| out_ms >= t.start_ms && out_ms < t.end_ms)
    }

    /// The most pictures on screen at once (video and image layers; text is drawn on
    /// top separately): what a renderer needs in simultaneous inputs.
    pub fn max_overlap(&self) -> usize {
        let mut events: Vec<(u64, i32)> = Vec::new();
        for l in self
            .layers
            .iter()
            .filter(|l| matches!(l.kind, LayerKind::Video | LayerKind::Image))
        {
            events.push((l.at_ms, 1));
            events.push((l.end_ms, -1));
        }
        // Ends before starts at the same time: a cut is not an overlap.
        events.sort_unstable();
        let (mut now, mut most) = (0i32, 0i32);
        for (_, d) in events {
            now = now.saturating_add(d);
            most = most.max(now);
        }
        usize::try_from(most).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advanced::{PlacedClip, Track, Transition};
    use crate::plan::SourceEvent;
    use crate::project::Piece;

    fn layer(id: u32, kind: LayerKind, at_ms: u64, end_ms: u64) -> Layer {
        Layer {
            id,
            track: 0,
            kind,
            source: Source::Main,
            src_in_ms: 0,
            src_out_ms: end_ms.saturating_sub(at_ms),
            at_ms,
            end_ms,
            z: 0,
            muted: false,
            hidden: false,
            transform: Vec::new(),
            opacity: Vec::new(),
            volume: Vec::new(),
            speed: Vec::new(),
        }
    }

    fn tf(zoom: f32, x: f32, y: f32) -> Transform {
        Transform { zoom, x, y }
    }

    #[test]
    fn fit_in_never_upscales_and_gives_even_sides() {
        assert_eq!(fit_in(1920, 1080, 1280, 720), (1280, 720));
        assert_eq!(fit_in(640, 360, 1920, 1080), (640, 360), "no upscaling");
        assert_eq!(fit_in(1001, 501, 4000, 4000), (1000, 500), "even sides");
        assert_eq!(
            fit_in(3840, 2160, 1080, 1920),
            (1080, 608),
            "607.5 rounds up"
        );
        assert_eq!(fit_in(0, 0, 0, 0), (2, 2));
        assert_eq!(fit_in(1, 1000, 1080, 1080), (2, 1000));
        for (w, h) in [(1, 1), (3, 7), (1919, 1081), (4097, 33), (u32::MAX, 3)] {
            for (mw, mh) in [(2, 2), (1280, 720), (1081, 1921), (u32::MAX, u32::MAX)] {
                let (fw, fh) = fit_in(w, h, mw, mh);
                assert!(fw >= 2 && fh >= 2, "{w}x{h} in {mw}x{mh}");
                assert!(fw % 2 == 0 && fh % 2 == 0, "{w}x{h} in {mw}x{mh}");
                assert!(fw <= w.max(2) && fh <= h.max(2), "{w}x{h} upscaled");
                assert!(
                    fw <= mw.max(2) && fh <= mh.max(2),
                    "{w}x{h} overflows {mw}x{mh}"
                );
            }
        }
    }

    #[test]
    fn place_centres_zooms_and_shifts() {
        let p = place(Transform::default(), 1280, 720, 1920, 1080);
        assert_eq!(
            p,
            Placement {
                x: 320,
                y: 180,
                width: 1280,
                height: 720
            }
        );
        let z = place(tf(2.0, 0.0, 0.0), 1920, 1080, 1920, 1080);
        assert_eq!((z.x, z.y, z.width, z.height), (-960, -540, 3840, 2160));
        let s = place(tf(1.0, 0.25, -0.5), 1920, 1080, 1920, 1080);
        assert_eq!((s.x, s.y), (480, -540), "a fraction of the canvas");
        let r = place(tf(1.5, 0.0, 0.0), 101, 51, 101, 51);
        assert_eq!((r.width, r.height), (152, 77), "151.5 and 76.5 round up");
    }

    #[test]
    fn place_rounds_halves_up_like_javascript() {
        // (1920 - 1921) / 2 = -0.5: floor(v + 0.5) = 0 (Rust's round says -1).
        assert_eq!(place(Transform::default(), 1921, 1080, 1920, 1080).x, 0);
        assert_eq!(place(Transform::default(), 1920, 1080, 1921, 1080).x, 1);
        // (1080 - 1083) / 2 = -1.5 -> -1 (Rust's round says -2).
        assert_eq!(place(Transform::default(), 1920, 1083, 1920, 1080).y, -1);
        assert_eq!(round_half_up(-2.5), -2.0);
        assert_eq!(round_half_up(2.5), 3.0);
    }

    #[test]
    fn place_survives_hostile_transforms() {
        let centred = place(Transform::default(), 1280, 720, 1920, 1080);
        let nan = tf(f32::NAN, f32::INFINITY, f32::NAN);
        assert_eq!(place(nan, 1280, 720, 1920, 1080), centred);
        let inf = tf(f32::INFINITY, 0.0, 0.0);
        assert_eq!(place(inf, 1280, 720, 1920, 1080), centred);
        let big = place(tf(100.0, 0.0, 0.0), 10, 10, 10, 10);
        assert_eq!(big.width, 80, "zoom capped at 8");
        let small = place(tf(0.0, 0.0, 0.0), 10, 10, 10, 10);
        assert_eq!(small.width, 1, "zoom at least 0.1");
        let far = place(tf(1.0, 5.0, -5.0), 10, 10, 10, 10);
        assert_eq!((far.x, far.y), (10, -10), "shift capped at 1");
        let huge = place(tf(8.0, 1.0, 1.0), u32::MAX, u32::MAX, u32::MAX, u32::MAX);
        assert_eq!(
            (huge.width, huge.x),
            (u32::MAX, i32::MIN),
            "clamped, no overflow"
        );
    }

    #[test]
    fn pieces_follow_speed_points_and_the_length_cap() {
        let mut l = layer(1, LayerKind::Video, 500, 3_500);
        l.src_in_ms = 100;
        l.src_out_ms = 5_100;
        l.speed = vec![
            SpeedPoint {
                at_ms: 0,
                rate_pct: 100,
            },
            SpeedPoint {
                at_ms: 1_000,
                rate_pct: 200,
            },
            SpeedPoint {
                at_ms: 9_000,
                rate_pct: 50,
            },
        ];
        let p = l.pieces(60_000);
        assert_eq!(
            p,
            vec![
                super::Piece {
                    out_start_ms: 500,
                    out_end_ms: 1_500,
                    src_start_ms: 100,
                    src_end_ms: 1_100,
                    rate_pct: 100
                },
                super::Piece {
                    out_start_ms: 1_500,
                    out_end_ms: 3_500,
                    src_start_ms: 1_100,
                    src_end_ms: 5_100,
                    rate_pct: 200
                },
            ]
        );
        let capped = l.pieces(700);
        assert_eq!(capped.len(), 5, "1000 = 700+300, 2000 = 700+700+600");
        assert_eq!(capped.first().map(|p| p.out_start_ms), Some(l.at_ms));
        assert_eq!(capped.last().map(|p| p.out_end_ms), Some(l.end_ms));
        for w in capped.windows(2) {
            assert_eq!(w[0].out_end_ms, w[1].out_start_ms);
            assert_eq!(w[0].src_end_ms, w[1].src_start_ms);
        }
        for piece in &capped {
            assert!(piece.out_end_ms - piece.out_start_ms <= 700);
            assert_eq!(l.source_ms(piece.out_start_ms), Some(piece.src_start_ms));
        }
        assert!(layer(2, LayerKind::Video, 10, 10).pieces(1_000).is_empty());
        assert_eq!(
            layer(3, LayerKind::Video, 0, 3).pieces(0).len(),
            3,
            "0 = 1 ms"
        );
        let mut late = layer(4, LayerKind::Video, 0, 1_000);
        late.speed = vec![SpeedPoint {
            at_ms: 400,
            rate_pct: 50,
        }];
        let rates: Vec<u32> = late.pieces(60_000).iter().map(|p| p.rate_pct).collect();
        assert_eq!(rates, vec![100, 50], "100 % before the first point");
    }

    #[test]
    fn max_zoom_is_the_largest_finite_key() {
        let mut l = layer(1, LayerKind::Video, 0, 1_000);
        assert_eq!(l.max_zoom(), 1.0);
        l.transform = [0.5, 3.0, f32::NAN]
            .iter()
            .map(|&zoom| TransformKey {
                value: tf(zoom, 0.0, 0.0),
                ..TransformKey::default()
            })
            .collect();
        assert_eq!(l.max_zoom(), 3.0);
        l.transform.push(TransformKey {
            value: tf(100.0, 0.0, 0.0),
            ..TransformKey::default()
        });
        assert_eq!(l.max_zoom(), 8.0);
    }

    #[test]
    fn frames_texts_and_overlap() {
        let mut c = Composition {
            duration_ms: 1_000,
            ..Composition::default()
        };
        assert_eq!(c.frame_count(60), 60);
        assert_eq!(c.frame_out_ms(59, 60), Some(983));
        assert_eq!(c.frame_out_ms(60, 60), None);
        assert_eq!((c.frame_count(0), c.frame_out_ms(0, 0)), (0, None));
        c.duration_ms = 1_001;
        assert_eq!(c.frame_count(60), 61);
        for fps in [1, 24, 30, 60, 144, 240] {
            let some = (0..c.frame_count(fps) + 2)
                .filter(|&i| c.frame_out_ms(i, fps).is_some())
                .count() as u64;
            assert_eq!(some, c.frame_count(fps), "{fps} fps");
        }
        assert_eq!(c.frame_out_ms(u64::MAX, 60), None);

        c.layers = vec![
            layer(1, LayerKind::Video, 0, 1_000),
            layer(2, LayerKind::Video, 1_000, 2_000),
            layer(3, LayerKind::Music, 0, 2_000),
        ];
        assert_eq!(
            c.max_overlap(),
            1,
            "a cut is not an overlap; music has no picture"
        );
        c.layers.push(layer(4, LayerKind::Image, 900, 1_100));
        c.texts = vec![
            TextLayer {
                id: 1,
                start_ms: 950,
                end_ms: 1_500,
                ..TextLayer::default()
            },
            TextLayer {
                id: 2,
                start_ms: 1_500,
                end_ms: 1_600,
                ..TextLayer::default()
            },
        ];
        assert_eq!(c.max_overlap(), 2, "text is not counted");
        let ids: Vec<u32> = c.texts_at(1_500).map(|t| t.id).collect();
        assert_eq!(ids, vec![2]);
        assert_eq!(c.texts_at(949).count(), 0);
    }

    #[test]
    fn fitted_sizes_follow_the_layer_kind() {
        let canvas = (1280, 720);
        assert_eq!(
            fitted(LayerKind::Video, true, (1920, 1080), 1920, canvas),
            canvas
        );
        // Another video fits inside, up or down.
        assert_eq!(
            fitted(LayerKind::Video, false, (640, 360), 1920, canvas),
            (1280, 720)
        );
        assert_eq!(
            fitted(LayerKind::Video, false, (1080, 1920), 1920, canvas),
            (404, 720)
        );
        // A 512 px picture on a 1920-wide crop is 512/1920 of the canvas at any size.
        assert_eq!(
            fitted(LayerKind::Image, false, (512, 512), 1920, canvas),
            (340, 340)
        );
        assert_eq!(
            fitted(LayerKind::Image, false, (512, 512), 1920, (1920, 1080)),
            (512, 512)
        );
        // ... and never more than the canvas.
        assert_eq!(
            fitted(LayerKind::Image, false, (8000, 100), 1920, canvas),
            (1280, 16)
        );
        assert_eq!(fitted(LayerKind::Image, false, (0, 0), 0, (0, 0)), (2, 2));
    }

    #[test]
    fn visible_crops_to_the_canvas() {
        let full = Placement {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let r = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        assert_eq!(
            visible(full, 1920, 1080, 1920, 1080),
            Some((r(0, 0, 1920, 1080), r(0, 0, 1920, 1080)))
        );
        // Zoom 2 about the centre: the middle half of the source fills the canvas.
        let z = place(tf(2.0, 0.0, 0.0), 1920, 1080, 1920, 1080);
        assert_eq!(
            visible(z, 1920, 1080, 1920, 1080),
            Some((r(480, 270, 960, 540), r(0, 0, 1920, 1080)))
        );
        // The same with a source decoded at half size.
        assert_eq!(
            visible(z, 960, 540, 1920, 1080),
            Some((r(240, 135, 480, 270), r(0, 0, 1920, 1080)))
        );
        // Shifted half off the right edge.
        let s = place(tf(1.0, 0.5, 0.0), 1920, 1080, 1920, 1080);
        assert_eq!(
            visible(s, 1920, 1080, 1920, 1080),
            Some((r(0, 0, 960, 1080), r(960, 0, 960, 1080)))
        );
        // A sliver keeps at least one source pixel.
        let thin = Placement {
            x: 1919,
            y: 0,
            width: 7680,
            height: 1080,
        };
        let (src, dst) = visible(thin, 10, 10, 1920, 1080).unwrap();
        assert_eq!((src.width, dst), (1, r(1919, 0, 1, 1080)));
        // Off the canvas, or empty.
        let off = Placement { x: 1920, ..full };
        assert_eq!(visible(off, 1920, 1080, 1920, 1080), None);
        let left = Placement { x: -1920, ..full };
        assert_eq!(visible(left, 1920, 1080, 1920, 1080), None);
        assert_eq!(visible(Placement::default(), 10, 10, 10, 10), None);
        assert_eq!(visible(full, 0, 10, 10, 10), None);
        let huge = place(tf(8.0, 1.0, 1.0), u32::MAX, u32::MAX, u32::MAX, u32::MAX);
        let (src, dst) = visible(huge, 10, 10, 1920, 1080).unwrap();
        assert!(dst.x + dst.width <= 1920 && dst.y + dst.height <= 1080);
        assert!(src.width >= 1 && src.x + src.width <= 10);
    }

    #[test]
    fn colour_is_one_affine_matrix() {
        let id = Colour::default().matrix();
        assert_eq!(id[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(id[2], [0.0, 0.0, 1.0, 0.0]);
        assert!(Colour::default().is_identity());
        let apply = |m: [[f32; 4]; 3], px: [f32; 3]| -> Vec<f32> {
            m.iter()
                .map(|row| row[0] * px[0] + row[1] * px[1] + row[2] * px[2] + row[3])
                .collect()
        };
        let grey = Colour {
            saturation: 0.0,
            ..Colour::default()
        };
        let g = apply(grey.matrix(), [255.0, 0.0, 0.0]);
        assert!(g.iter().all(|v| (v - 0.2126 * 255.0).abs() < 1e-3), "{g:?}");
        let bright = Colour {
            brightness: 0.5,
            ..Colour::default()
        };
        assert_eq!(
            apply(bright.matrix(), [10.0, 20.0, 30.0]),
            vec![137.5, 147.5, 157.5]
        );
        let flat = Colour {
            contrast: 0.0,
            ..Colour::default()
        };
        assert_eq!(
            apply(flat.matrix(), [0.0, 99.0, 255.0]),
            vec![128.0, 128.0, 128.0]
        );
        let junk = Colour {
            brightness: f32::NAN,
            contrast: 9.0,
            saturation: f32::NEG_INFINITY,
        };
        assert_eq!(
            junk.sanitized(),
            Colour {
                brightness: 0.0,
                contrast: 2.0,
                saturation: 1.0
            }
        );
    }

    #[test]
    fn composition_keeps_the_crop() {
        let mut p = Project::new(10_000);
        p.format.kind = crate::format::FormatKind::Tall;
        let c = compose(&p, &source());
        assert_eq!(c.crop, p.format.crop_rect(1920, 1080));
        assert!(c.crop.width < c.crop.height);
        assert_eq!(c.source, source());
    }

    fn source() -> SourceInfo {
        SourceInfo {
            width: 1920,
            height: 1080,
            duration_ms: 10_000,
            fps: Some(60.0),
            bookmarks_ms: vec![500, 7_000],
            events: Vec::new(),
        }
    }

    #[test]
    fn output_ms_inverts_source_ms() {
        let l = Layer {
            id: 1,
            track: 1,
            kind: LayerKind::Video,
            source: Source::Main,
            src_in_ms: 1_000,
            src_out_ms: 5_000,
            at_ms: 2_000,
            end_ms: 2_000 + ramp::output_length(&ramp_points(), 4_000),
            z: 1,
            muted: false,
            hidden: false,
            transform: Vec::new(),
            opacity: Vec::new(),
            volume: Vec::new(),
            speed: ramp_points(),
        };
        for out in (l.at_ms..l.end_ms).step_by(7) {
            let src = l.source_ms(out).unwrap();
            let back = l.output_ms(src).unwrap();
            assert!(back <= out, "{out} → {src} → {back}");
            assert_eq!(l.source_ms(back), Some(src), "{out}");
        }
        assert_eq!(l.output_ms(1_000), Some(2_000));
        assert_eq!(l.output_ms(999), None);
        assert_eq!(l.output_ms(5_000), None);
    }

    fn ramp_points() -> Vec<SpeedPoint> {
        vec![
            SpeedPoint {
                at_ms: 0,
                rate_pct: 50,
            },
            SpeedPoint {
                at_ms: 1_000,
                rate_pct: 300,
            },
        ]
    }

    #[test]
    fn chapters_of_every_clip_become_markers() {
        let mut main = source();
        main.events = vec![SourceEvent {
            at_ms: 7_000,
            title: "Ace".into(),
        }];
        let other = SourceInfo {
            bookmarks_ms: vec![1_000, 3_000],
            events: vec![SourceEvent {
                at_ms: 2_500,
                title: "Clutch".into(),
            }],
            ..source()
        };
        let mut p = Project::new(10_000);
        p.pieces = vec![Piece {
            start_ms: 0,
            end_ms: 8_000,
            speed: Speed::Normal,
        }];
        let clip = |id: u32, at_ms: u64| PlacedClip {
            id,
            source: Source::Library {
                key: "G/b.mp4".into(),
            },
            in_ms: 2_000,
            out_ms: 4_000,
            at_ms,
            ..PlacedClip::default()
        };
        p.advanced.tracks = vec![Track {
            id: 1,
            kind: TrackKind::Video,
            name: "Montage".into(),
            muted: false,
            hidden: false,
            main: false,
            clips: vec![clip(1, 8_000), clip(2, 10_000)],
        }];
        p.advanced.markers = vec![Marker {
            at_ms: 9_000,
            title: "Mine".into(),
            kind: MarkerKind::User,
        }];
        let c = compose_with(&p, &main, &[("G/b.mp4".into(), other.clone())]);
        let got: Vec<(u64, MarkerKind, &str)> = c
            .markers
            .iter()
            .map(|m| (m.at_ms, m.kind, m.title.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (500, MarkerKind::Bookmark, "Bookmark 1"),
                (7_000, MarkerKind::Event, "Ace"),
                (8_500, MarkerKind::Event, "Clutch"),
                (9_000, MarkerKind::Bookmark, "Bookmark 2"),
                (9_000, MarkerKind::User, "Mine"),
                (10_500, MarkerKind::Event, "Clutch"),
                (11_000, MarkerKind::Bookmark, "Bookmark 3"),
            ],
            "b's 1000 is before its in point; 3000 plays at 8000 + 1000"
        );
        // Without the clip's info its chapters are unknown, the rest stays.
        assert_eq!(compose(&p, &main).markers.len(), 3);
    }

    #[test]
    fn normal_mode_is_one_video_layer_per_segment() {
        let mut p = Project::new(10_000);
        p.pieces = vec![
            Piece {
                start_ms: 0,
                end_ms: 4_000,
                speed: Speed::Double,
            },
            Piece {
                start_ms: 6_000,
                end_ms: 8_000,
                speed: Speed::Normal,
            },
        ];
        let c = compose(&p, &source());
        assert_eq!(c.duration_ms, 4_000);
        assert_eq!(c.layers.len(), 2);
        let a = &c.layers[0];
        assert_eq!(
            (a.at_ms, a.end_ms, a.src_in_ms, a.src_out_ms),
            (0, 2_000, 0, 4_000)
        );
        assert_eq!(a.source_ms(1_000), Some(2_000));
        assert_eq!(a.source_ms(2_000), None);
        assert_eq!(c.layers[1].source_ms(2_500), Some(6_500));
        assert_eq!(c.visible_at(2_500).len(), 1);
        assert_eq!(c.markers.len(), 2, "500 → 250 and 7000 → 3000");
        assert_eq!(c.markers[0].at_ms, 250);
        assert_eq!(c.markers[1].at_ms, 3_000);
        assert!(!c.has_advanced_layers());
    }

    #[test]
    fn advanced_tracks_add_layers_with_ramps_and_transitions() {
        let mut p = Project::new(10_000);
        p.advanced.tracks = vec![Track {
            id: 7,
            kind: TrackKind::Video,
            name: "B-roll".into(),
            muted: false,
            hidden: false,
            main: false,
            clips: vec![
                PlacedClip {
                    id: 1,
                    source: Source::Library {
                        key: "b.mp4".into(),
                    },
                    in_ms: 0,
                    out_ms: 2_000,
                    at_ms: 1_000,
                    speed: vec![SpeedPoint {
                        at_ms: 0,
                        rate_pct: 200,
                    }],
                    ..PlacedClip::default()
                },
                PlacedClip {
                    id: 2,
                    source: Source::Image {
                        path: "s.png".into(),
                    },
                    in_ms: 0,
                    out_ms: 3_000,
                    at_ms: 2_000,
                    transform: vec![TransformKey {
                        at_ms: 0,
                        value: Transform {
                            zoom: 2.0,
                            x: 0.5,
                            y: 0.0,
                        },
                        ease: Ease::Hold,
                    }],
                    ..PlacedClip::default()
                },
            ],
        }];
        p.advanced.transitions = vec![Transition {
            track: 7,
            after_clip: 1,
            kind: TransitionKind::Crossfade,
            ms: 500,
        }];
        let c = compose(&p, &source());
        assert_eq!(c.layers.len(), 3);
        let b = &c.layers[1];
        assert_eq!((b.at_ms, b.end_ms), (1_000, 2_000), "2 s at 2x");
        assert_eq!(b.source_ms(1_500), Some(1_000));
        assert_eq!(b.z, 1);
        let s = &c.layers[2];
        assert_eq!(s.kind, LayerKind::Image);
        assert_eq!(
            (s.at_ms, s.end_ms),
            (1_500, 4_500),
            "starts 500 ms early for the crossfade"
        );
        assert_eq!(s.opacity_at(1_500), 0.0);
        assert_eq!(s.opacity_at(1_750), 0.5);
        assert_eq!(s.opacity_at(3_000), 1.0);
        assert_eq!(s.transform_at(2_000).zoom, 2.0);
        assert_eq!(c.duration_ms, 10_000);
        assert!(c.has_advanced_layers());
        let top: Vec<u32> = c.visible_at(1_600).iter().map(|l| l.id).collect();
        assert_eq!(top, vec![1, 2, 3]);
    }

    #[test]
    fn an_arranged_main_track_replaces_the_normal_cut() {
        // Normal mode cut 0–10 s; V1 arranged: the clip's 6–8 s at 1 s, then its 0–2 s
        // at 5 s (reordered, with gaps).
        let mut p = Project::new(10_000);
        let piece = |id, in_ms, out_ms, at_ms| PlacedClip {
            id,
            source: Source::Main,
            in_ms,
            out_ms,
            at_ms,
            ..PlacedClip::default()
        };
        p.advanced.tracks = vec![Track {
            id: 4,
            kind: TrackKind::Video,
            main: true,
            clips: vec![piece(1, 6_000, 8_000, 1_000), piece(2, 0, 2_000, 5_000)],
            ..Track::default()
        }];
        let c = compose(&p, &source());
        let spans: Vec<(u64, u64, u64, u32)> = c
            .layers
            .iter()
            .map(|l| (l.src_in_ms, l.at_ms, l.end_ms, l.z))
            .collect();
        assert_eq!(spans, vec![(6_000, 1_000, 3_000, 0), (0, 5_000, 7_000, 0)]);
        assert_eq!(c.duration_ms, 7_000);
        assert!(c.visible_at(4_000).is_empty(), "a gap: nothing drawn");
        // The bookmark at source 7 s plays at 2 s, the one at 0.5 s at 5.5 s.
        let at: Vec<u64> = c.markers.iter().map(|m| m.at_ms).collect();
        assert_eq!(at, vec![2_000, 5_500]);

        // Repair: one main track, of the main clip only; an empty one still counts.
        let mut a = p.advanced.clone();
        a.tracks[0].clips[1].source = Source::Library { key: "x".into() };
        a.tracks.push(Track {
            id: 5,
            kind: TrackKind::Music,
            main: true,
            ..Track::default()
        });
        a.repair();
        assert_eq!(a.tracks[0].clips.len(), 1);
        assert!(!a.tracks[1].main);
        a.tracks[0].clips.clear();
        assert!(!a.is_empty());
    }

    #[test]
    fn a_hidden_track_is_heard_not_seen() {
        let mut p = Project::new(10_000);
        p.advanced.tracks = vec![Track {
            id: 1,
            kind: TrackKind::Video,
            name: String::new(),
            muted: false,
            hidden: true,
            main: false,
            clips: vec![PlacedClip {
                id: 1,
                source: Source::Main,
                in_ms: 0,
                out_ms: 2_000,
                at_ms: 1_000,
                ..PlacedClip::default()
            }],
        }];
        let c = compose(&p, &source());
        assert!(c.layers[1].hidden);
        let seen: Vec<u32> = c.visible_at(1_500).iter().map(|l| l.id).collect();
        assert_eq!(seen, vec![1], "only Main is drawn");
        let heard: Vec<u32> = c.audible_at(1_500).iter().map(|l| l.id).collect();
        assert_eq!(heard, vec![1, 2], "Main and the hidden clip's sound");
        p.advanced.tracks[0].muted = true;
        let heard: Vec<u32> = compose(&p, &source())
            .audible_at(1_500)
            .iter()
            .map(|l| l.id)
            .collect();
        assert_eq!(heard, vec![1], "hidden and muted: nothing from it");
        // Older projects have no `hidden`: shown.
        let old: Track = serde_json::from_str(r#"{"id":1,"kind":"video","clips":[]}"#).unwrap();
        assert!(!old.hidden);
    }

    #[test]
    fn dip_goes_through_black() {
        let mut p = Project::new(10_000);
        p.advanced.tracks = vec![Track {
            id: 1,
            kind: TrackKind::Video,
            name: String::new(),
            muted: false,
            hidden: false,
            main: false,
            clips: vec![
                PlacedClip {
                    id: 1,
                    source: Source::Main,
                    in_ms: 0,
                    out_ms: 2_000,
                    at_ms: 0,
                    ..PlacedClip::default()
                },
                PlacedClip {
                    id: 2,
                    source: Source::Main,
                    in_ms: 5_000,
                    out_ms: 7_000,
                    at_ms: 2_000,
                    ..PlacedClip::default()
                },
            ],
        }];
        p.advanced.transitions = vec![Transition {
            track: 1,
            after_clip: 1,
            kind: TransitionKind::Dip,
            ms: 400,
        }];
        let c = compose(&p, &source());
        assert_eq!(c.layers.len(), 4);
        let (a, b, black) = (&c.layers[1], &c.layers[2], &c.layers[3]);
        assert_eq!((b.at_ms, b.end_ms), (2_000, 4_000), "no overlap for a dip");
        // The clips stay opaque: the black over them goes 0 → 1 at the cut → 0, so
        // Main below never shows through.
        assert_eq!((a.opacity_at(1_800), b.opacity_at(2_000)), (1.0, 1.0));
        assert_eq!(black.kind, LayerKind::Black);
        assert_eq!((black.at_ms, black.end_ms, black.z), (1_600, 2_400, 1));
        let ramp: Vec<f32> = [1_600, 1_800, 2_000, 2_200, 2_399]
            .iter()
            .map(|&t| black.opacity_at(t))
            .collect();
        assert_eq!(&ramp[..4], &[0.0, 0.5, 1.0, 0.5]);
        assert!(ramp[4] < 0.01);
        let order: Vec<u32> = c.visible_at(2_000).iter().map(|l| l.id).collect();
        assert_eq!(order, vec![1, 3, 4], "over the clip, Main below");

        // A gap between the clips stays black; the next clip comes back from black.
        p.advanced.tracks[0].clips[1].at_ms = 3_000;
        let black = compose(&p, &source()).layers[3].clone();
        assert_eq!((black.at_ms, black.end_ms), (1_600, 3_400));
        assert_eq!(black.opacity_at(2_500), 1.0);

        // On a sound track a dip still fades the clips (it has no picture).
        p.advanced.tracks[0].kind = TrackKind::Audio;
        for c in &mut p.advanced.tracks[0].clips {
            c.source = Source::File {
                path: "s.wav".into(),
            };
        }
        p.advanced.tracks[0].clips[1].at_ms = 2_000;
        let c = compose(&p, &source());
        assert_eq!(c.layers.len(), 3);
        assert_eq!(c.layers[1].opacity_at(1_800), 0.5);
        assert_eq!(c.layers[2].opacity_at(2_000), 0.0);
    }

    #[test]
    fn a_first_clip_crossfades_in_from_below() {
        // Main 0–10 s; a track's first clip at 10 s fades in over Main's last 500 ms.
        let mut p = Project::new(10_000);
        let clip = |id, at_ms| PlacedClip {
            id,
            source: Source::Main,
            in_ms: 0,
            out_ms: 2_000,
            at_ms,
            ..PlacedClip::default()
        };
        p.advanced.tracks = vec![Track {
            id: 1,
            kind: TrackKind::Video,
            name: String::new(),
            muted: false,
            hidden: false,
            main: false,
            clips: vec![clip(1, 10_000), clip(2, 12_000)],
        }];
        p.advanced.transitions = vec![Transition {
            track: 1,
            after_clip: FROM_BELOW,
            kind: TransitionKind::Crossfade,
            ms: 500,
        }];
        let c = compose(&p, &source());
        let first = &c.layers[1];
        assert_eq!(
            (first.at_ms, first.end_ms),
            (9_500, 11_500),
            "pulled under Main"
        );
        assert_eq!(first.opacity_at(9_500), 0.0);
        assert_eq!(first.opacity_at(9_750), 0.5);
        assert_eq!(first.opacity_at(10_000), 1.0);
        let second = &c.layers[2];
        assert_eq!(second.at_ms, 12_000, "only the first clip comes from below");
        assert!(second.opacity.is_empty());
    }

    #[test]
    fn muted_tracks_and_long_music_are_handled() {
        let mut p = Project::new(10_000);
        p.advanced.tracks = vec![Track {
            id: 1,
            kind: TrackKind::Music,
            name: String::new(),
            muted: true,
            hidden: false,
            main: false,
            clips: vec![PlacedClip {
                id: 1,
                source: Source::File {
                    path: "m.mp3".into(),
                },
                in_ms: 0,
                out_ms: 60_000,
                at_ms: 0,
                ..PlacedClip::default()
            }],
        }];
        let c = compose(&p, &source());
        assert_eq!(c.layers.len(), 2);
        assert_eq!(c.layers[1].kind, LayerKind::Music);
        assert_eq!(c.layers[1].volume_at(5), 0.0);
        assert!(c.audible_at(5).iter().all(|l| l.kind != LayerKind::Music));
        assert_eq!(c.duration_ms, 60_000, "music outlasts the clip");
    }
}
