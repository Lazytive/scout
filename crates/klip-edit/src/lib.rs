//! Klip's editor model (ADR 0017), pure: the project file (non-destructive edits of one
//! clip), the timeline math shared with the front end (cuts and speed: output time ↔
//! source time), the 16:9 / 9:16 format crop, text layer presets and templates.
//!
//! Nothing here touches media or GStreamer; `klip-export` renders a project, the app
//! previews it. Every input is treated as hostile: a project file never fails to load
//! (bad parts are repaired and reported), all math is checked, and every list is capped.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

pub mod advanced;
pub mod compose;
mod file;
pub mod format;
pub mod plan;
mod project;
pub mod template;
pub mod text;
pub mod timeline;

pub use advanced::{
    Advanced, Keyframe, PlacedClip, Source, SpeedPoint, Track, TrackKind, Transform,
};
pub use compose::{Composition, Layer, LayerKind, compose, compose_with};
pub use file::{LoadStatus, Loaded, load, save};
pub use format::{Format, FormatKind, Rect};
pub use plan::{Render, SourceEvent, SourceInfo};
pub use project::{
    Issue, MAX_EXTRA_KEYS, MAX_FILE_BYTES, MAX_PIECES, MAX_SOURCE_MS, MAX_TEXT_LAYERS, MAX_VOLUME,
    Mode, Piece, Project, SCHEMA_VERSION, Speed, Volumes,
};
pub use text::render::{
    RenderError, TextBitmap, TextMeasure, cache_key, measure, render, render_layer,
};
pub use text::{MAX_TEXT_CHARS, Position, TextLayer, TextStyle};
pub use timeline::{EditError, Segment};
