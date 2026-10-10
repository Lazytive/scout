//! The editor preview's sound (M9.5): a clip's audio tracks copied one per file
//! (`qtdemux` → `mp4mux`, no re-encode), so the preview can play Game and Microphone
//! at their own volumes (a `<video>` plays only the first track). Each file is written
//! as `<n>.m4a.part` and renamed when the whole split is done; a failed or timed-out
//! split leaves no `.m4a` behind.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

use crate::ExportError;

/// Most audio tracks split (Klip writes 1–4).
pub const MAX_AUDIO_TRACKS: u32 = 16;

fn gst_err(e: impl std::fmt::Display) -> ExportError {
    ExportError::Gst(e.to_string())
}

fn make(factory: &str) -> Result<gst::Element, ExportError> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| ExportError::Gst(format!("{factory} is not installed")))
}

/// The file track `n` goes to.
pub fn track_file(out_dir: &Path, n: u32) -> PathBuf {
    out_dir.join(format!("{n}.m4a"))
}

fn part_of(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Shut the pipeline down however we leave.
struct Running(gst::Pipeline);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

/// Copy the first `tracks` audio tracks of `input` into `out_dir/<n>.m4a` (n from 0).
/// `timeout` bounds the whole run. Returns the files, in track order.
pub fn split_audio(
    input: &Path,
    out_dir: &Path,
    tracks: u32,
    timeout: Duration,
) -> Result<Vec<PathBuf>, ExportError> {
    if tracks == 0 || tracks > MAX_AUDIO_TRACKS {
        return Err(ExportError::Plan(format!(
            "a clip has 1…{MAX_AUDIO_TRACKS} audio tracks to split"
        )));
    }
    gst::init().map_err(gst_err)?;
    std::fs::create_dir_all(out_dir)?;
    let files: Vec<PathBuf> = (0..tracks).map(|n| track_file(out_dir, n)).collect();
    let parts: Vec<PathBuf> = files.iter().map(|f| part_of(f)).collect();
    let r = run(input, &parts, timeout);
    if let Err(e) = r {
        for p in &parts {
            let _ = std::fs::remove_file(p);
        }
        return Err(e);
    }
    for part in &parts {
        let len = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if len == 0 {
            for p in &parts {
                let _ = std::fs::remove_file(p);
            }
            return Err(ExportError::Source(
                "an audio track of the clip came out empty".into(),
            ));
        }
    }
    for (part, file) in parts.iter().zip(&files) {
        let _ = std::fs::remove_file(file);
        std::fs::rename(part, file)?;
    }
    Ok(files)
}

fn run(input: &Path, parts: &[PathBuf], timeout: Duration) -> Result<(), ExportError> {
    let pipeline = gst::Pipeline::new();
    let guard = Running(pipeline.clone());
    let src = make("filesrc")?;
    src.set_property("location", input.to_string_lossy().as_ref());
    let demux = make("qtdemux")?;
    pipeline.add_many([&src, &demux]).map_err(gst_err)?;
    src.link(&demux).map_err(gst_err)?;
    // One queue → mp4mux → filesink per track; the demuxer's `audio_<n>` pad goes to
    // queue n when it appears. Anything else (video) goes to a fakesink.
    let mut queues: Vec<gst::Element> = Vec::with_capacity(parts.len());
    for part in parts {
        let queue = make("queue")?;
        let mux = make("mp4mux")?;
        let sink = make("filesink")?;
        sink.set_property("location", part.to_string_lossy().as_ref());
        sink.set_property("sync", false);
        pipeline.add_many([&queue, &mux, &sink]).map_err(gst_err)?;
        let mux_pad = mux
            .request_pad_simple("audio_%u")
            .ok_or_else(|| ExportError::Gst("mp4mux has no audio pad".into()))?;
        queue
            .static_pad("src")
            .ok_or_else(|| ExportError::Gst("queue without a src pad".into()))?
            .link(&mux_pad)
            .map_err(gst_err)?;
        mux.link(&sink).map_err(gst_err)?;
        queues.push(queue);
    }
    let rest = make("fakesink")?;
    rest.set_property("sync", false);
    let rest_queue = make("queue")?;
    pipeline.add_many([&rest_queue, &rest]).map_err(gst_err)?;
    rest_queue.link(&rest).map_err(gst_err)?;
    let wanted = queues.clone();
    let video_queue = rest_queue.clone();
    let found: Arc<Vec<AtomicBool>> =
        Arc::new(parts.iter().map(|_| AtomicBool::new(false)).collect());
    let seen = Arc::clone(&found);
    demux.connect_pad_added(move |_, pad| {
        let name = pad.name();
        let (target, n) = if let Some(n) = name.strip_prefix("audio_") {
            let n = n.parse::<usize>().ok();
            (n.and_then(|n| wanted.get(n)), n)
        } else if name.starts_with("video_") {
            (Some(&video_queue), None)
        } else {
            (None, None)
        };
        if let Some(sink) = target.and_then(|q| q.static_pad("sink"))
            && !sink.is_linked()
            && pad.link(&sink).is_ok()
            && let Some(flag) = n.and_then(|n| seen.get(n))
        {
            flag.store(true, Ordering::Relaxed);
        }
    });
    // A track the file doesn't have (or no video): end that branch instead of waiting
    // forever for it.
    let mut ends = queues.clone();
    ends.push(rest_queue.clone());
    demux.connect_no_more_pads(move |_| {
        for q in &ends {
            if let Some(sink) = q.static_pad("sink")
                && !sink.is_linked()
            {
                let _ = sink.send_event(gst::event::Eos::new());
            }
        }
    });
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| ExportError::Source("the clip's audio cannot be read".into()))?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| ExportError::Gst("pipeline without a bus".into()))?;
    let start = Instant::now();
    loop {
        let left = timeout.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Err(ExportError::Source("splitting the audio timed out".into()));
        }
        let wait =
            gst::ClockTime::from_nseconds(u64::try_from(left.as_nanos()).unwrap_or(u64::MAX));
        match bus.timed_pop_filtered(
            Some(wait),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) {
            Some(msg) => match msg.view() {
                gst::MessageView::Error(e) => {
                    return Err(ExportError::Source(format!(
                        "the clip's audio cannot be read: {}",
                        e.error()
                    )));
                }
                _ => break,
            },
            None => continue,
        }
    }
    drop(guard);
    if found.iter().any(|f| !f.load(Ordering::Relaxed)) {
        return Err(ExportError::Source(
            "the clip has fewer audio tracks than expected".into(),
        ));
    }
    Ok(())
}
