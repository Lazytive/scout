//! NVENC and Media Foundation in one process (ADR 0009).
//!
//! Measured on the dev machine (GStreamer 1.28.6, NVIDIA driver 32.0.16.1742): once a
//! GStreamer NVENC session has existed in a process, starting Media Foundation (any MF
//! element, e.g. `mfaacenc`, which records our audio) makes every later NVENC session
//! fail with `NV_ENC_ERR_INVALID_VERSION`. If Media Foundation started first, NVENC keeps
//! working; `klip_probe::init` does that. Separately, a session on NVIDIA's H.264 MFT
//! (`mfh264enc` here) breaks later NVENC sessions even then, which is why D3D12 ranks
//! before hardware MFTs and probes run in child processes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use gstreamer::prelude::*;
use klip_probe::{EncoderChoice, allowed};

fn choice(element: &str) -> EncoderChoice {
    let a = allowed(element).unwrap();
    EncoderChoice {
        element: element.into(),
        codec: a.codec,
        family: a.family,
        vendor: a.vendor,
        description: String::new(),
        rank: 0,
        gpu: None,
        device_index: 0,
    }
}

/// Run 100 buffers of test audio through `mfaacenc`.
fn run_mfaacenc() -> bool {
    let p = gstreamer::parse::launch(
        "audiotestsrc num-buffers=100 ! audio/x-raw,format=S16LE,rate=48000,channels=2 ! \
         mfaacenc ! fakesink",
    )
    .unwrap();
    p.set_state(gstreamer::State::Playing).unwrap();
    let msg = p.bus().unwrap().timed_pop_filtered(
        gstreamer::ClockTime::from_seconds(10),
        &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
    );
    let _ = p.set_state(gstreamer::State::Null);
    matches!(msg.map(|m| m.type_()), Some(gstreamer::MessageType::Eos))
}

fn nvenc_ok() -> bool {
    let r = klip_probe::probe(&choice("nvd3d11h264enc"), Duration::from_secs(10));
    if let Some(e) = &r.error {
        println!("nvd3d11h264enc: {e}");
    }
    r.ok
}

#[test]
#[ignore = "hardware: needs an NVIDIA GPU and Media Foundation; run via `cargo xtask e2e`"]
fn nvenc_survives_media_foundation_in_between() {
    klip_probe::init().unwrap();
    if gstreamer::ElementFactory::find("nvd3d11h264enc").is_none()
        || gstreamer::ElementFactory::find("mfaacenc").is_none()
    {
        println!("skipped: needs nvd3d11h264enc and mfaacenc");
        return;
    }
    assert!(nvenc_ok(), "first NVENC session");
    assert!(run_mfaacenc(), "mfaacenc");
    assert!(nvenc_ok(), "NVENC after Media Foundation started");
    assert!(run_mfaacenc(), "mfaacenc again");
    assert!(nvenc_ok(), "NVENC again");
}

/// Reproduce the driver/GStreamer behaviour without Klip's warm-up:
/// `KLIP_DIAG_SEQ=nvd3d11h264enc,mfaacenc,nvd3d11h264enc cargo test -p klip-probe
/// --test nvenc_media_foundation diag_ -- --ignored --nocapture`
#[test]
#[ignore = "diagnostic: needs an NVIDIA GPU"]
fn diag_sequence_without_warm_up() {
    gstreamer::init().unwrap();
    let seq = std::env::var("KLIP_DIAG_SEQ")
        .unwrap_or_else(|_| "nvd3d11h264enc,mfaacenc,nvd3d11h264enc".into());
    for element in seq.split(',') {
        let ok = if element == "mfaacenc" {
            run_mfaacenc()
        } else {
            klip_probe::probe(&choice(element), Duration::from_secs(10)).ok
        };
        println!("{element}: ok={ok}");
    }
}
