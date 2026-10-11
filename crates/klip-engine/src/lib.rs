//! Klip's engine (ADR 0014): everything the app does that isn't UI. It owns the settings,
//! watches for games, runs the recorder while one plays, binds hotkeys, reports events
//! and executes every command. The desktop app and `klip-cli daemon` are front ends.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

pub mod config;
mod engine;
pub mod events;
pub mod games;
pub mod media_worker;
mod worker;

pub use engine::{
    Engine, EngineOptions, Isolation, POLL, RETRY, default_settings_path, themes_dir,
};
pub use events::Event;
pub use worker::{RECORDER_FLAG, worker_main};

/// Errors from starting the engine or a recording.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Config(String),
    #[error("recorder: {0}")]
    Recorder(#[from] klip_recorder::RecorderError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}
