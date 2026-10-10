//! The export plan (ADR 0017): everything `klip-export` needs to render a project,
//! computed once from the project and the source clip, with no reference back to the
//! project file. Pure data: it crosses the media-worker boundary as JSON.
//!
//! Times: `segments` and `markers` are output time; frames are addressed through
//! [`Render::frame_source_ms`], which uses the same integer math as the preview
//! (`timeline`), so the export cuts where the preview seeks.

use serde::{Deserialize, Serialize};

use crate::format::Rect;
use crate::project::{MAX_PIECES, MAX_SOURCE_MS, MAX_TEXT_LAYERS, Project, Volumes};
use crate::text::{TextLayer, clean_text, preset};
use crate::timeline::{self, Segment};

/// Most bookmarks carried into an export (MP4 chapters hold 255).
pub const MAX_MARKERS: usize = 255;

/// What the plan needs to know about the source clip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceInfo {
    pub width: u32,
    pub height: u32,
    /// The clip's length, ms (0: unknown; the project's own length is used).
    pub duration_ms: u64,
    /// Frames per second, if known.
    pub fps: Option<f64>,
    /// Bookmarks (chapters), source ms.
    pub bookmarks_ms: Vec<u64>,
    /// Chapters with a title of their own (auto-clip events), source ms.
    pub events: Vec<SourceEvent>,
}

/// A titled chapter of a clip (an auto-clip event).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceEvent {
    pub at_ms: u64,
    pub title: String,
}

/// A bookmark on the output timeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub out_ms: u64,
    pub title: String,
}

/// The plan: source crop, output segments, text cues, volumes, markers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Render {
    pub source: SourceInfo,
    /// The part of the source frame that is kept (the format crop).
    pub crop: Rect,
    /// Output segments back to back (`timeline::segments`).
    pub segments: Vec<Segment>,
    /// Length of the output, ms.
    pub output_ms: u64,
    pub volumes: Volumes,
    /// Text layers with their times clipped to the output (empty texts dropped).
    pub texts: Vec<TextLayer>,
    pub markers: Vec<Marker>,
}

impl Render {
    /// The plan for `project` over `source`. A project whose length disagrees with the
    /// clip is clamped to the clip (the clip is the truth at export time).
    pub fn new(project: &Project, source: &SourceInfo) -> Render {
        let mut p = project.clone();
        if source.duration_ms > 0 && source.duration_ms <= MAX_SOURCE_MS {
            p.source_ms = source.duration_ms;
        }
        p.repair();
        let segments = timeline::segments(&p);
        let output_ms = segments.last().map_or(0, |s| s.out_end_ms);
        let crop = p.format.crop_rect(source.width, source.height);
        let texts = p
            .text
            .iter()
            .filter(|t| !clean_text(&t.text).is_empty() && t.start_ms < t.end_ms)
            .filter(|t| t.start_ms < output_ms)
            .take(MAX_TEXT_LAYERS)
            .map(|t| TextLayer {
                id: t.id,
                preset: preset(&t.preset).map_or_else(
                    || crate::text::DEFAULT_PRESET.to_string(),
                    |s| s.id.to_string(),
                ),
                text: clean_text(&t.text),
                position: t.position,
                start_ms: t.start_ms,
                end_ms: t.end_ms.min(output_ms),
            })
            .collect();
        let markers = source
            .bookmarks_ms
            .iter()
            .filter_map(|&src| timeline::output_time_in(&segments, src))
            .take(MAX_MARKERS)
            .enumerate()
            .map(|(i, out_ms)| Marker {
                out_ms,
                title: format!("Bookmark {}", i.saturating_add(1)),
            })
            .collect();
        Render {
            source: source.clone(),
            crop,
            segments,
            output_ms,
            volumes: p.volumes,
            texts,
            markers,
        }
    }

    /// Whether the plan can be rendered at all: a non-empty crop inside the source,
    /// consistent segments inside the clip, a positive length.
    pub fn validate(&self) -> Result<(), String> {
        if self.source.width == 0 || self.source.height == 0 {
            return Err("the source size is unknown".into());
        }
        if self.crop.width == 0 || self.crop.height == 0 {
            return Err("the crop is empty".into());
        }
        let inside = self
            .crop
            .x
            .checked_add(self.crop.width)
            .is_some_and(|r| r <= self.source.width)
            && self
                .crop
                .y
                .checked_add(self.crop.height)
                .is_some_and(|b| b <= self.source.height);
        if !inside {
            return Err("the crop is outside the source".into());
        }
        if self.segments.is_empty() || self.segments.len() > MAX_PIECES {
            return Err("no segments to render".into());
        }
        let mut out_pos = 0u64;
        for s in &self.segments {
            if s.src_start_ms >= s.src_end_ms
                || s.out_start_ms >= s.out_end_ms
                || s.out_start_ms != out_pos
                || s.src_end_ms > MAX_SOURCE_MS
                || (self.source.duration_ms > 0 && s.src_end_ms > self.source.duration_ms)
            {
                return Err("segments are inconsistent".into());
            }
            out_pos = s.out_end_ms;
        }
        if out_pos != self.output_ms
            || self.output_ms == 0
            || self.output_ms > MAX_SOURCE_MS.saturating_mul(2)
        {
            return Err("the output length is wrong".into());
        }
        if self.texts.len() > MAX_TEXT_LAYERS {
            return Err("too many text layers".into());
        }
        Ok(())
    }

    /// Output time of frame `i` at `fps`, ms (floor). `None` past the end or for a
    /// nonsense rate.
    pub fn frame_out_ms(&self, i: u64, fps: u32) -> Option<u64> {
        if fps == 0 {
            return None;
        }
        let ms = i.checked_mul(1000)?.checked_div(u64::from(fps))?;
        (ms < self.output_ms).then_some(ms)
    }

    /// The source time shown by output frame `i` at `fps`.
    pub fn frame_source_ms(&self, i: u64, fps: u32) -> Option<u64> {
        timeline::source_time_in(&self.segments, self.frame_out_ms(i, fps)?)
    }

    /// How many output frames an export at `fps` has (frame `i` exists while its output
    /// time is before the end).
    pub fn frame_count(&self, fps: u32) -> u64 {
        if fps == 0 {
            return 0;
        }
        // Frames at i*1000/fps < output_ms: i < output_ms*fps/1000, rounded up.
        self.output_ms
            .saturating_mul(u64::from(fps))
            .saturating_add(999)
            .checked_div(1000)
            .unwrap_or(0)
    }

    /// The text layers shown at output time `out_ms`, in layer order.
    pub fn texts_at(&self, out_ms: u64) -> impl Iterator<Item = &TextLayer> {
        self.texts
            .iter()
            .filter(move |t| out_ms >= t.start_ms && out_ms < t.end_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Piece, Speed};
    use crate::text::Position;

    fn source() -> SourceInfo {
        SourceInfo {
            width: 1920,
            height: 1080,
            duration_ms: 20_000,
            fps: Some(60.0),
            bookmarks_ms: vec![1_000, 9_000, 19_500],
            events: Vec::new(),
        }
    }

    #[test]
    fn whole_clip_plan() {
        let p = Project::new(20_000);
        let r = Render::new(&p, &source());
        r.validate().unwrap();
        assert_eq!(r.output_ms, 20_000);
        assert_eq!(r.segments.len(), 1);
        assert_eq!(
            r.crop,
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(r.frame_count(60), 1200);
        assert_eq!(r.frame_source_ms(0, 60), Some(0));
        assert_eq!(r.frame_source_ms(1199, 60), Some(19_983));
        assert_eq!(r.frame_source_ms(1200, 60), None);
        assert_eq!(r.markers.len(), 3);
        assert_eq!(r.markers[1].out_ms, 9_000);
    }

    #[test]
    fn cuts_and_speed_map_frames_and_markers() {
        let mut p = Project::new(20_000);
        p.pieces = vec![
            Piece {
                start_ms: 2_000,
                end_ms: 6_000,
                speed: Speed::Double,
            },
            Piece {
                start_ms: 10_000,
                end_ms: 12_000,
                speed: Speed::Half,
            },
        ];
        let r = Render::new(&p, &source());
        r.validate().unwrap();
        assert_eq!(r.output_ms, 2_000 + 4_000);
        // Frame 30 at 60 fps = 500 ms out = 2x into the first piece = 3000 ms src.
        assert_eq!(r.frame_source_ms(30, 60), Some(3_000));
        // Frame 180 = 3000 ms out = 1000 ms into the half-speed piece = 500 ms src.
        assert_eq!(r.frame_source_ms(180, 60), Some(10_500));
        // Bookmark at 1000 ms is cut; 9000 is cut; 19500 is cut: none survive.
        assert!(r.markers.is_empty());
        let mut src = source();
        src.bookmarks_ms = vec![4_000, 11_000];
        let r = Render::new(&p, &src);
        assert_eq!(
            r.markers.iter().map(|m| m.out_ms).collect::<Vec<_>>(),
            vec![1_000, 4_000]
        );
    }

    #[test]
    fn source_length_wins_over_the_project() {
        let mut p = Project::new(30_000);
        p.pieces = vec![Piece {
            start_ms: 25_000,
            end_ms: 30_000,
            speed: Speed::Normal,
        }];
        let r = Render::new(&p, &source());
        // The piece lies beyond the real clip: repaired away, whole clip instead.
        r.validate().unwrap();
        assert_eq!(r.output_ms, 20_000);
    }

    #[test]
    fn texts_are_cleaned_and_clipped() {
        let mut p = Project::new(20_000);
        p.text = vec![
            TextLayer {
                id: 1,
                preset: "nope".into(),
                text: "  gg \u{0}wp ".into(),
                position: Position::Top,
                start_ms: 19_000,
                end_ms: 40_000,
            },
            TextLayer {
                id: 2,
                preset: "pill".into(),
                text: "   ".into(),
                position: Position::Top,
                start_ms: 0,
                end_ms: 1_000,
            },
            TextLayer {
                id: 3,
                preset: "pill".into(),
                text: "late".into(),
                position: Position::Top,
                start_ms: 25_000,
                end_ms: 26_000,
            },
        ];
        let r = Render::new(&p, &source());
        assert_eq!(r.texts.len(), 1);
        assert_eq!(r.texts[0].text, "gg wp");
        assert_eq!(r.texts[0].preset, "bold");
        assert_eq!(r.texts[0].end_ms, 20_000);
        assert_eq!(r.texts_at(19_500).count(), 1);
        assert_eq!(r.texts_at(18_999).count(), 0);
    }

    #[test]
    fn validate_rejects_nonsense() {
        let mut r = Render::new(&Project::new(20_000), &source());
        r.validate().unwrap();
        r.crop.width = 4000;
        assert!(r.validate().is_err());
        let mut r = Render::new(&Project::new(20_000), &source());
        r.segments[0].out_end_ms = 5;
        assert!(r.validate().is_err());
        let mut r = Render::new(&Project::new(20_000), &source());
        r.output_ms = 0;
        assert!(r.validate().is_err());
        assert!(Render::default().validate().is_err());
    }

    #[test]
    fn json_round_trip() {
        let r = Render::new(&Project::new(20_000), &source());
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Render>(&s).unwrap(), r);
        // Lenient: unknown and missing fields.
        let r2: Render = serde_json::from_str(r#"{"output_ms": 5, "x": 1}"#).unwrap();
        assert_eq!(r2.output_ms, 5);
    }
}
