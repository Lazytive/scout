//! Synthetic audio through the real AAC chain: PCM with beeps at known times → appsrc →
//! `build_aac_chain` (the platform's AAC encoder) → packets → MP4 → ffmpeg decode → onset detection. Checks
//! that each beep lands where its input timestamp says (encoder delay compensated) and
//! measures encode speed. Skips when no AAC encoder or ffmpeg is available.

// Test code: panicking helpers are allowed (AGENTS.md rule 3 applies outside tests).
#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use klip_capture::audio::{self, CHANNELS, SAMPLE_RATE};
use klip_capture::{CaptureSession, CaptureStats, PacketSink, Timing};
use klip_media::{NANOS_PER_SEC, Nanos, Packet, TrackInfo};
use klip_mp4::{Mp4Options, Mp4Track, save_mp4_atomic};
use klip_testkit::{audio as beeps, oracle};

#[derive(Default)]
struct Collect {
    info: Mutex<Option<TrackInfo>>,
    packets: Mutex<Vec<Packet>>,
    errors: Mutex<Vec<String>>,
}

impl PacketSink for Collect {
    fn on_track_info(&self, _track: usize, info: TrackInfo) {
        *self.info.lock().unwrap() = Some(info);
    }
    fn on_packet(&self, _track: usize, packet: Packet) {
        self.packets.lock().unwrap().push(packet);
    }
    fn on_error(&self, message: String) {
        self.errors.lock().unwrap().push(message);
    }
}

const T0: Nanos = 3 * NANOS_PER_SEC;
const TOTAL_S: f64 = 6.0;
const BEEPS_S: [f64; 5] = [0.5, 1.7037, 2.9, 4.1111, 5.3];

#[test]
fn aac_chain_keeps_beeps_on_time() {
    klip_capture::klip_probe::init().unwrap();
    let Some(encoder) = audio::aac_encoder() else {
        eprintln!("skipped: no AAC encoder");
        return;
    };
    if !oracle::available("ffmpeg") {
        eprintln!("skipped: needs ffmpeg");
        return;
    }
    let delay = audio::aac_encoder_delay(encoder);
    println!("encoder {encoder}, compensated delay {delay} ns");
    let pipeline = gst::Pipeline::new();
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("rate", SAMPLE_RATE as i32)
        .field("channels", CHANNELS as i32)
        .build();
    let appsrc = gst_app::AppSrc::builder()
        .caps(&caps)
        .format(gst::Format::Time)
        .block(true)
        .max_bytes(1 << 20)
        .build();
    pipeline.add(appsrc.upcast_ref::<gst::Element>()).unwrap();
    let (first, appsink) = audio::build_aac_chain(&pipeline, audio::DEFAULT_BITRATE).unwrap();
    appsrc.link(&first).unwrap();
    let sink = Arc::new(Collect::default());
    let stats = Arc::new(CaptureStats::default());
    klip_capture::attach_sink_with(
        &appsink,
        0,
        sink.clone(),
        stats.clone(),
        Timing::Shift(-delay),
    );
    let session = CaptureSession::run(pipeline, sink.clone(), stats, (0, 0), None).unwrap();

    let pcm = beeps::render_beeps_s16(SAMPLE_RATE, CHANNELS as u16, TOTAL_S, &BEEPS_S);
    let chunk_frames = (SAMPLE_RATE / 100) as usize; // 10 ms, like a WASAPI period
    let t_encode = Instant::now();
    for (n, chunk) in pcm.chunks(chunk_frames * CHANNELS as usize).enumerate() {
        let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut buf = gst::Buffer::from_mut_slice(bytes);
        {
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_nseconds(
                (T0 + n as i64 * 10_000_000) as u64,
            ));
            b.set_duration(gst::ClockTime::from_mseconds(10));
        }
        appsrc.push_buffer(buf).unwrap();
    }
    appsrc.end_of_stream().unwrap();
    let want = (TOTAL_S * f64::from(SAMPLE_RATE) / 1024.0) as usize;
    let deadline = Instant::now() + Duration::from_secs(20);
    while sink.packets.lock().unwrap().len() < want {
        assert!(
            Instant::now() < deadline,
            "encoder stalled at {} of {want} packets, errors {:?}",
            sink.packets.lock().unwrap().len(),
            sink.errors.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let encode_s = t_encode.elapsed().as_secs_f64();
    session.stop();
    // EOS is reported as "capture stream ended"; anything else is a failure.
    let errors = sink.errors.lock().unwrap().clone();
    assert!(
        errors.iter().all(|e| e == "capture stream ended"),
        "{errors:?}"
    );

    let info = sink.info.lock().unwrap().clone().expect("track info");
    let packets = sink.packets.lock().unwrap().clone();
    let first_pts = packets[0].pts;
    println!(
        "{} AAC packets, first pts {:.4} s, codec_data {:02x?}, encode {:.0}x real time",
        packets.len(),
        first_pts as f64 / 1e9,
        &info.codec_data[..],
        TOTAL_S / encode_s
    );
    for w in packets.windows(2) {
        let step = w[1].pts - w[0].pts;
        assert!(
            (step - 21_333_333).abs() <= 1,
            "packets back to back: {step}"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("beeps.mp4");
    save_mp4_atomic(
        &path,
        &[Mp4Track::new(&info, &packets)],
        &Mp4Options::default(),
    )
    .unwrap();
    let streams = oracle::probe_streams(&path).unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].codec_name.as_deref(), Some("aac"));
    assert_eq!(streams[0].sample_rate.as_deref(), Some("48000"));
    assert_eq!(streams[0].channels, Some(2));

    let start = oracle::frame_times(&path, "a:0").unwrap()[0];
    let decoded = oracle::decode_audio_mono(&path, 0, SAMPLE_RATE).unwrap();
    let found = beeps::onsets(&decoded, SAMPLE_RATE, beeps::ONSET_THRESHOLD, 0.3);
    assert_eq!(found.len(), BEEPS_S.len(), "onsets {found:?}");
    let errors_ms: Vec<f64> = found
        .iter()
        .zip(BEEPS_S)
        .map(|(f, b)| {
            let in_file = f + start;
            let expected = (T0 - first_pts) as f64 / 1e9 + b;
            (in_file - expected) * 1000.0
        })
        .collect();
    let mean = errors_ms.iter().sum::<f64>() / errors_ms.len() as f64;
    println!(
        "beep errors ms {errors_ms:.3?}; mean {mean:+.3} ms = {:+.1} samples left after compensating {} ns",
        mean * 48.0,
        delay
    );
    for e in &errors_ms {
        assert!(
            e.abs() < 1.5,
            "beep off by {e:.3} ms (errors {errors_ms:.3?})"
        );
    }
}
