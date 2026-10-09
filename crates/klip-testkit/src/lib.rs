//! Test support for Klip (never shipped in the app):
//!
//! - [`barcode`]: the frame-counter code drawn by the test game, a CPU renderer for
//!   synthetic sources, and a reader for decoded frames.
//! - [`oracle`]: ffprobe/ffmpeg run as external processes to check clips.
//! - [`continuity`]: dropped/duplicated frame analysis of counter sequences.
//! - [`audio`]: test beeps and an onset detector.
//! - [`sync`]: A/V sync of a clip against the test game's frame and beep logs.
//! - [`patches`]: brightness patches for colour and HDR tone-mapping checks.
//!
//! Dev tooling, so `arithmetic_side_effects` is allowed here (ADR 0001).

#![allow(clippy::arithmetic_side_effects)]

pub mod audio;
pub mod barcode;
pub mod continuity;
pub mod oracle;
pub mod patches;
pub mod sync;
