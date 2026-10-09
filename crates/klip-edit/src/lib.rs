//! Klip's editor model (ADR 0017), pure: the project file (non-destructive edits of one
//! clip), the timeline math shared with the front end (cuts and speed: output time ↔
//! source time), the 16:9 / 9:16 format crop, text layer presets and templates.
//!
//! Nothing here touches media or GStreamer; `klip-export` renders a project, the app
//! previews it. Every input is treated as hostile: a project file never fails to load
//! (bad parts are repaired and reported), all math is checked, and every list is capped.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod file;
pub mod format;
mod project;
pub mod template;
pub mod text;
pub mod timeline;

pub use file::{LoadStatus, Loaded, load, save};
pub use format::{Format, FormatKind, Rect};
pub use project::{
    Issue, MAX_EXTRA_KEYS, MAX_FILE_BYTES, MAX_PIECES, MAX_SOURCE_MS, MAX_TEXT_LAYERS, MAX_VOLUME,
    Mode, Piece, Project, SCHEMA_VERSION, Speed, Volumes,
};
pub use text::{MAX_TEXT_CHARS, Position, TextLayer, TextStyle};
pub use timeline::{EditError, Segment};
