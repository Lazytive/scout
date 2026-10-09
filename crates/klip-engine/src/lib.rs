//! Klip's engine (ADR 0014): everything the app does that isn't UI. It owns the settings,
//! watches for games, runs the recorder while one plays, binds hotkeys, reports events
//! and executes every command. The desktop app and `klip-cli daemon` are front ends.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

pub mod games;
