//! From a project in either mode to what the renderer draws (ADR 0018): a
//! [`Composition`] of layers on the output timeline. Normal mode projects into it
//! (each output segment of the main clip is a video layer at a constant speed; text
//! and the two volumes ride along), Advanced mode's tracks add theirs; transitions
//! become overlaps and opacity ramps. Pure, integer ms; the renderer (`klip-export`)
//! and the preview's canvas (`src/lib/keyframes.ts`) read the same numbers.

use serde::{Deserialize, Serialize};

use crate::advanced::keyframes::{transform_at, value_at};
use crate::advanced::{
    Colour, Ducking, Ease, Keyframe, Marker, MarkerKind, Source, SpeedPoint, TrackKind, Transform,
    TransformKey, TransitionKind, ramp,
};
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
    pub muted: bool,
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
pub struct Composition {
    pub duration_ms: u64,
    pub layers: Vec<Layer>,
    /// Normal mode's text cues (output time).
    pub texts: Vec<TextLayer>,
    /// The main clip's Game/Mic mix (Normal mode's sliders).
    pub main_volumes: Volumes,
    pub colour: Colour,
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
            .filter(|l| matches!(l.kind, LayerKind::Video | LayerKind::Image) && l.active(out_ms))
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

/// Build the composition of `project` over `source` (the main clip as probed).
pub fn compose(project: &Project, source: &SourceInfo) -> Composition {
    let mut p = project.clone();
    if source.duration_ms > 0 && source.duration_ms <= MAX_SOURCE_MS {
        p.source_ms = source.duration_ms;
    }
    p.repair();
    let segments = timeline::segments(&p);
    let mut layers: Vec<Layer> = Vec::new();
    let mut next_id: u32 = 1;
    // Track 0: the main clip as Normal mode cut it.
    for seg in &segments {
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
    let mut duration = segments.last().map_or(0, |s| s.out_end_ms);
    // Advanced tracks above it, in order.
    for (ti, track) in p.advanced.tracks.iter().enumerate() {
        let z = u32::try_from(ti).unwrap_or(u32::MAX).saturating_add(1);
        // The previous clip of this track: its layer index and its project id.
        let mut previous: Option<(usize, u32)> = None;
        for clip in &track.clips {
            if layers.len() >= MAX_LAYERS {
                break;
            }
            let src_len = clip.out_ms.saturating_sub(clip.in_ms);
            let mut at = clip.at_ms.min(MAX_SOURCE_MS);
            let mut opacity = clip.opacity.clone();
            // A transition after the previous clip of this track: overlap by its
            // length (crossfade) or fade both at the join (dip).
            let transition = previous.and_then(|(_, prev_id)| {
                p.advanced
                    .transitions
                    .iter()
                    .find(|t| t.track == track.id && t.after_clip == prev_id)
            });
            if let (Some((prev_i, _)), Some(tr)) = (previous, transition) {
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
                transform: clip.transform.clone(),
                opacity,
                volume: clip.volume.clone(),
                speed: clip.speed.clone(),
            });
            previous = Some((layers.len().saturating_sub(1), clip.id));
            next_id = next_id.saturating_add(1);
            duration = duration.max(end);
        }
    }
    let mut markers: Vec<Marker> = source
        .bookmarks_ms
        .iter()
        .filter_map(|&src| timeline::output_time_in(&segments, src))
        .map(|at_ms| Marker {
            at_ms,
            title: String::new(),
            kind: MarkerKind::Bookmark,
        })
        .collect();
    markers.extend(
        p.advanced
            .markers
            .iter()
            .filter(|m| m.at_ms < duration)
            .cloned(),
    );
    markers.sort_by_key(|m| m.at_ms);
    markers.truncate(MAX_MARKERS);
    for (i, m) in markers.iter_mut().enumerate() {
        if m.title.is_empty() {
            m.title = format!("Bookmark {}", i.saturating_add(1));
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advanced::{PlacedClip, Track, Transition};
    use crate::project::Piece;

    fn source() -> SourceInfo {
        SourceInfo {
            width: 1920,
            height: 1080,
            duration_ms: 10_000,
            fps: Some(60.0),
            bookmarks_ms: vec![500, 7_000],
        }
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
    fn dip_fades_both_sides() {
        let mut p = Project::new(10_000);
        p.advanced.tracks = vec![Track {
            id: 1,
            kind: TrackKind::Video,
            name: String::new(),
            muted: false,
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
        let (a, b) = (&c.layers[1], &c.layers[2]);
        assert_eq!((b.at_ms, b.end_ms), (2_000, 4_000), "no overlap for a dip");
        assert_eq!(a.opacity_at(1_600), 1.0);
        assert_eq!(a.opacity_at(1_800), 0.5);
        assert_eq!(b.opacity_at(2_000), 0.0);
        assert_eq!(b.opacity_at(2_400), 1.0);
    }

    #[test]
    fn muted_tracks_and_long_music_are_handled() {
        let mut p = Project::new(10_000);
        p.advanced.tracks = vec![Track {
            id: 1,
            kind: TrackKind::Music,
            name: String::new(),
            muted: true,
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
