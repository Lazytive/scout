//! Hardware-test harness: barcoded frames pushed in real time through a real encoder
//! into a ring sink.

#![allow(
    dead_code,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use klip_capture::klip_probe::EncoderChoice;
use klip_capture::{CaptureSession, CaptureStats, EncodeConfig};
use klip_media::{Codec, NANOS_PER_SEC};
use klip_recorder::RingSink;
use klip_testkit::barcode;

pub const W: usize = 640;
pub const H: usize = 360;
pub const FPS: u64 = 60;
pub const FRAME_NS: u64 = NANOS_PER_SEC as u64 / FPS;

pub fn vendors() -> Vec<klip_platform::GpuVendor> {
    klip_platform::list_gpus()
        .unwrap()
        .into_iter()
        .filter(|g| !g.software)
        .map(|g| g.vendor)
        .collect()
}

/// The encoder the recorder would pick for H.264 here.
pub fn best_h264() -> EncoderChoice {
    klip_capture::klip_probe::init().unwrap();
    klip_capture::klip_probe::best(Codec::H264, &vendors(), None, Duration::from_secs(10)).unwrap()
}

pub struct Harness {
    pub encoder: EncoderChoice,
    pub sink: Arc<RingSink>,
    pub stats: Arc<CaptureStats>,
    appsrc: gst_app::AppSrc,
    session: CaptureSession,
    t0: Instant,
}

impl Harness {
    /// appsrc (BGRA, W x H at FPS, pts = frame index / FPS) → encode chain → `sink`.
    pub fn start(encoder: EncoderChoice, sink: Arc<RingSink>) -> Harness {
        println!("encoder: {} ({})", encoder.element, encoder.description);
        let pipeline = gst::Pipeline::new();
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "BGRA")
            .field("width", W as i32)
            .field("height", H as i32)
            .field("framerate", gst::Fraction::new(FPS as i32, 1))
            .build();
        let appsrc = gst_app::AppSrc::builder()
            .caps(&caps)
            .format(gst::Format::Time)
            // Not blocking: a failed pipeline stops draining the queue, and a blocked push
            // would hang the test instead of failing it.
            .block(false)
            .max_bytes((W * H * 4 * 8) as u64)
            .build();
        // The encode chain takes D3D11 textures on Windows, system memory on Linux.
        let upload = gst::ElementFactory::make(if cfg!(windows) {
            "d3d11upload"
        } else {
            "identity"
        })
        .build()
        .unwrap();
        pipeline.add_many([appsrc.upcast_ref(), &upload]).unwrap();
        appsrc.link(&upload).unwrap();
        let cfg = EncodeConfig {
            encoder: encoder.clone(),
            bitrate_kbps: 8_000,
            keyframe_interval: FPS as u32,
            max_bitrate_kbps: None,
            leaky_queue: true,
        };
        let chain =
            klip_capture::build_encode_chain(&pipeline, &cfg, W as u32, H as u32, FPS as u32)
                .unwrap();
        upload.link(&chain.input).unwrap();
        let stats = Arc::new(CaptureStats::default());
        chain.attach(0, sink.clone(), stats.clone(), klip_capture::Timing::AsIs);
        let session = CaptureSession::run(
            pipeline,
            sink.clone(),
            stats.clone(),
            (W as u32, H as u32),
            None,
        )
        .unwrap();
        Harness {
            encoder,
            sink,
            stats,
            appsrc,
            session,
            t0: Instant::now(),
        }
    }

    /// Push frame `i` at its real-time slot (a flood would make the leaky queue in front
    /// of the encoder drop frames, like a capture never would).
    pub fn push(&self, i: u32) {
        let due = self.t0 + Duration::from_nanos(u64::from(i) * FRAME_NS);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let mut buf =
            gst::Buffer::from_mut_slice(barcode::render_bgra(i, W, H, i.is_multiple_of(60)));
        {
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_nseconds(u64::from(i) * FRAME_NS));
            b.set_duration(gst::ClockTime::from_nseconds(FRAME_NS));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.appsrc.current_level_bytes() >= self.appsrc.max_bytes() {
            assert_eq!(
                self.session
                    .stats()
                    .errors
                    .load(std::sync::atomic::Ordering::Relaxed),
                0,
                "{}: pipeline error: {:?}",
                self.encoder.element,
                self.sink.last_error()
            );
            assert!(
                Instant::now() < deadline,
                "{}: encoder stalled",
                self.encoder.element
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        self.appsrc.push_buffer(buf).unwrap();
    }

    /// End the stream, wait until the ring holds `frames` video packets, stop.
    pub fn finish(self, frames: u32) {
        self.appsrc.end_of_stream().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.sink.stats().packets[0] < frames as usize {
            assert!(
                Instant::now() < deadline,
                "encoder produced {} of {frames}",
                self.sink.stats().packets[0]
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        self.session.stop();
        assert_eq!(
            self.stats
                .encoder_queue_drops
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "{}: frames dropped in front of the encoder",
            self.encoder.element
        );
    }
}
