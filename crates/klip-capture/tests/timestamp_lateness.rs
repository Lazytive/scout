//! Diagnostic (hardware): how far behind the shared clock do source buffers arrive?
//! `lateness = running time at arrival - (pts + duration)`. A buffer can't arrive before its
//! last sample was captured, so correct timestamps give a small positive minimum that is
//! the same in every run; a minimum that moves between runs means the source's timestamps
//! carry a per-run offset.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use klip_capture::TimeBase;

fn lateness_ms(src: &gst::Element, tb: &TimeBase, secs: u64) -> Vec<f64> {
    let pipeline = gst::Pipeline::new();
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .unwrap();
    pipeline.add_many([src, &sink]).unwrap();
    src.link(&sink).unwrap();
    let samples = Arc::new(Mutex::new(Vec::new()));
    let s2 = samples.clone();
    let tb2 = tb.clone();
    src.static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(gst::PadProbeData::Buffer(b)) = &info.data
                && let Some(pts) = b.pts()
            {
                let end = pts.nseconds() + b.duration().map_or(0, |d| d.nseconds());
                let now = tb2.now();
                s2.lock().unwrap().push((now as f64 - end as f64) / 1e6);
            }
            gst::PadProbeReturn::Ok
        });
    tb.apply(&pipeline);
    pipeline.set_state(gst::State::Playing).unwrap();
    std::thread::sleep(Duration::from_secs(secs));
    for prop in [
        "actual-latency-time",
        "actual-buffer-time",
        "latency-time",
        "buffer-time",
    ] {
        if src.find_property(prop).is_some() {
            print!("{prop}={} ", src.property::<i64>(prop));
        }
    }
    let mut q = gst::query::Latency::new();
    if src.static_pad("src").unwrap().peer_query(&mut q) || src.query(&mut q) {
        let (live, min, max) = q.result();
        print!("query latency live={live} min={min} max={max:?} ");
    }
    pipeline.set_state(gst::State::Null).unwrap();
    let mut v = samples.lock().unwrap().clone();
    // Skip start-up.
    if v.len() > 20 {
        v.drain(..10);
    }
    v
}

fn summary(v: &mut [f64]) -> String {
    v.sort_by(f64::total_cmp);
    if v.is_empty() {
        return "no buffers".into();
    }
    format!(
        "n {:4} min {:7.2} p50 {:7.2} max {:7.2} ms",
        v.len(),
        v[0],
        v[v.len() / 2],
        v[v.len() - 1]
    )
}

#[test]
#[ignore = "diagnostic: needs audio devices and a desktop; prints, doesn't assert"]
fn diag_source_lateness_across_runs() {
    klip_capture::klip_probe::init().unwrap();
    for method in ["re-timestamp"] {
        for run in 0..2 {
            let tb = TimeBase::new().unwrap();
            // Different start-up phase each run, like a real recorder.
            std::thread::sleep(Duration::from_millis(137 * run));
            let src = gst::ElementFactory::make("wasapi2src").build().unwrap();
            for (k, v) in [
                ("loopback", "true"),
                ("provide-clock", "false"),
                ("low-latency", "true"),
                ("continue-on-error", "true"),
                ("slave-method", method),
            ] {
                let pspec = src.find_property(k).unwrap();
                src.set_property_from_value(
                    k,
                    &gst::glib::Value::deserialize_with_pspec(v, &pspec).unwrap(),
                );
            }
            let mut v = lateness_ms(&src, &tb, 3);
            println!("audio {method:12} run {run}: {}", summary(&mut v));
        }
    }
    for run in 0..3 {
        let tb = TimeBase::new().unwrap();
        std::thread::sleep(Duration::from_millis(137 * run));
        let src = gst::ElementFactory::make("d3d11screencapturesrc")
            .property_from_str("capture-api", "wgc")
            .build()
            .unwrap();
        let mut v = lateness_ms(&src, &tb, 3);
        println!("video monitor   run {run}: {}", summary(&mut v));
    }
}
