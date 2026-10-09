//! Test support for Klip (never shipped in the app):
//!
//! - [`barcode`]: the frame-counter code drawn by the test game, a CPU renderer for
//!   synthetic sources, and a reader for decoded frames.
//! - [`oracle`]: ffprobe/ffmpeg run as external processes to check clips.
//! - [`continuity`]: dropped/duplicated frame analysis of counter sequences.
//!
//! Dev tooling, so `arithmetic_side_effects` is allowed here (ADR 0001).

#![allow(clippy::arithmetic_side_effects)]

pub mod barcode;
pub mod continuity;
pub mod oracle;
