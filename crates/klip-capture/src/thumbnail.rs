//! Still pictures of saved clips (the overlay's pop-up now, library thumbnails in M7):
//! decode the keyframe at or before a point near the end of a clip and encode it as a
//! small JPEG. GStreamer picks the decoder (hardware decoders rank first), so nothing
//! here decodes video in software when the GPU can.

use std::path::Path;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::{CaptureError, capsfilter, gst_err, make, props};

/// How long to wait for each pipeline step.
const STEP: Duration = Duration::from_secs(5);

/// Shut the pipeline down however we leave.
struct Running(gst::Pipeline);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

fn wait_async(pipeline: &gst::Pipeline) -> Result<(), CaptureError> {
    let bus = pipeline
        .bus()
        .ok_or_else(|| CaptureError::Gst("pipeline without a bus".into()))?;
    let timeout = gst::ClockTime::from_nseconds(u64::try_from(STEP.as_nanos()).unwrap_or(u64::MAX));
    match bus.timed_pop_filtered(
        Some(timeout),
        &[gst::MessageType::AsyncDone, gst::MessageType::Error],
    ) {
        Some(msg) => match msg.view() {
            gst::MessageView::Error(e) => Err(CaptureError::Gst(e.error().to_string())),
            _ => Ok(()),
        },
        None => Err(CaptureError::Gst("timed out reading the clip".into())),
    }
}

/// A JPEG of the clip at `back` before its end (snapped to the keyframe at or before
/// it), at most `max_width` pixels wide (height keeps the aspect ratio).
pub fn jpeg_near_end(path: &Path, back: Duration, max_width: u32) -> Result<Vec<u8>, CaptureError> {
    klip_probe::init().map_err(gst_err)?;
    let pipeline = gst::Pipeline::new();
    let src = make("filesrc")?;
    props::require(&src, "location", &path.to_string_lossy()).map_err(CaptureError::Gst)?;
    let decode = make("decodebin")?;
    let convert = make("videoconvert")?;
    let scale = make("videoscale")?;
    let width = max_width.clamp(16, 1920) & !1;
    let caps = capsfilter(&format!(
        "video/x-raw,format=I420,width={width},pixel-aspect-ratio=1/1"
    ))?;
    let jpeg = make("jpegenc")?;
    props::set_str(&jpeg, "quality", "82");
    let sink = make("appsink")?;
    props::set_str(&sink, "sync", "false");
    pipeline
        .add_many([&src, &decode, &convert, &scale, &caps, &jpeg, &sink])
        .map_err(gst_err)?;
    src.link(&decode).map_err(gst_err)?;
    gst::Element::link_many([&convert, &scale, &caps, &jpeg, &sink]).map_err(gst_err)?;
    let convert_weak = convert.downgrade();
    decode.connect_pad_added(move |_, pad| {
        let is_video = pad
            .current_caps()
            .and_then(|c| c.structure(0).map(|s| s.name().starts_with("video/")))
            .unwrap_or(false);
        if !is_video {
            return;
        }
        if let Some(convert) = convert_weak.upgrade()
            && let Some(sinkpad) = convert.static_pad("sink")
            && !sinkpad.is_linked()
        {
            let _ = pad.link(&sinkpad);
        }
    });
    let running = Running(pipeline);
    let pipeline = &running.0;
    pipeline.set_state(gst::State::Paused).map_err(gst_err)?;
    wait_async(pipeline)?;
    if let Some(duration) = pipeline.query_duration::<gst::ClockTime>() {
        let back = gst::ClockTime::from_nseconds(u64::try_from(back.as_nanos()).unwrap_or(0));
        let at = duration.saturating_sub(back);
        pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_BEFORE,
                at,
            )
            .map_err(gst_err)?;
        wait_async(pipeline)?;
    }
    let appsink = sink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CaptureError::Gst("appsink is not an AppSink".into()))?;
    let timeout = gst::ClockTime::from_nseconds(u64::try_from(STEP.as_nanos()).unwrap_or(u64::MAX));
    let sample = appsink
        .try_pull_preroll(Some(timeout))
        .ok_or_else(|| CaptureError::Gst("no picture in the clip".into()))?;
    let buffer = sample
        .buffer()
        .ok_or_else(|| CaptureError::Gst("empty picture".into()))?;
    let map = buffer.map_readable().map_err(gst_err)?;
    Ok(map.as_slice().to_vec())
}
