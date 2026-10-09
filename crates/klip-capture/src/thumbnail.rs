//! Still pictures of clips: a poster (the overlay's pop-up and the library card: the
//! keyframe at or before a point near the end, as a small JPEG) and a hover-scrub strip
//! for the library (key frames spread over the clip, stacked into one JPEG). GStreamer
//! picks the decoder. Library clips can be any file: the app runs these in a child
//! process (klip-engine's media worker, ADR 0015), which calls
//! [`prefer_software_decoders`] first: for a few key frames a software decoder is faster
//! than starting a GPU decoder and leaves the GPU to the game (ADR 0016). Only
//! GStreamer itself is initialised here, not the encoders' Media Foundation warm-up
//! (`klip_probe::init`, ~0.7 s), which decoding doesn't need.

use std::path::Path;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::VideoFrameExt as _;

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

/// Rank this process's hardware video decoders (element class
/// `Codec/Decoder/Video/Hardware`) below software ones, so `decodebin` picks a software
/// decoder where one is installed (`avdec_h264`, `avdec_h265`, `dav1ddec`, `openh264dec`)
/// and a hardware one otherwise. For the media worker only: it decodes a few key frames
/// per clip, where starting a GPU decoder costs more than decoding (measured on the dev
/// machine: a 1080p poster 71 ms in software vs 336 ms with D3D11 in a fresh process).
/// Returns how many decoders were ranked down.
pub fn prefer_software_decoders() -> Result<usize, CaptureError> {
    gst::init().map_err(gst_err)?;
    let mut n: usize = 0;
    for f in gst::Registry::get().features(gst::ElementFactory::static_type()) {
        let Ok(factory) = f.clone().downcast::<gst::ElementFactory>() else {
            continue;
        };
        let klass = factory
            .metadata(gst::ELEMENT_METADATA_KLASS)
            .unwrap_or_default();
        let hardware_video_decoder = ["Decoder", "Video", "Hardware"]
            .iter()
            .all(|k| klass.contains(k));
        if hardware_video_decoder && f.rank() > gst::Rank::MARGINAL {
            f.set_rank(gst::Rank::MARGINAL);
            n = n.saturating_add(1);
        }
    }
    Ok(n)
}

/// A JPEG of the clip at `back` before its end (snapped to the keyframe at or before
/// it), at most `max_width` pixels wide (height keeps the aspect ratio).
pub fn jpeg_near_end(path: &Path, back: Duration, max_width: u32) -> Result<Vec<u8>, CaptureError> {
    gst::init().map_err(gst_err)?;
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

/// Most frames in a strip.
pub const MAX_STRIP_TILES: u32 = 64;
/// Largest strip (raw RGB) assembled in memory.
const MAX_STRIP_BYTES: usize = 64 << 20;
/// JPEG quality of strips (they are shown small and change on every mouse move).
const STRIP_QUALITY: &str = "75";

/// A hover-scrub strip: `tiles` frames, each `tile_width` x `tile_height`, stacked top to
/// bottom in one JPEG.
#[derive(Clone, Debug)]
pub struct Strip {
    pub jpeg: Vec<u8>,
    pub tiles: u32,
    pub tile_width: u32,
    pub tile_height: u32,
}

fn step_timeout() -> gst::ClockTime {
    gst::ClockTime::from_nseconds(u64::try_from(STEP.as_nanos()).unwrap_or(u64::MAX))
}

/// `tiles` frames spread evenly over the clip (the key frame nearest the middle of each
/// slice), `tile_width` pixels wide, as one JPEG with the frames stacked top to bottom.
pub fn strip(path: &Path, tiles: u32, tile_width: u32) -> Result<Strip, CaptureError> {
    gst::init().map_err(gst_err)?;
    let tiles = tiles.clamp(1, MAX_STRIP_TILES);
    let width = tile_width.clamp(16, 1920) & !1;
    let pipeline = gst::Pipeline::new();
    let src = make("filesrc")?;
    props::require(&src, "location", &path.to_string_lossy()).map_err(CaptureError::Gst)?;
    let decode = make("decodebin")?;
    let convert = make("videoconvert")?;
    let scale = make("videoscale")?;
    let caps = capsfilter(&format!(
        "video/x-raw,format=RGB,width={width},pixel-aspect-ratio=1/1"
    ))?;
    let sink = make("appsink")?;
    props::set_str(&sink, "sync", "false");
    pipeline
        .add_many([&src, &decode, &convert, &scale, &caps, &sink])
        .map_err(gst_err)?;
    src.link(&decode).map_err(gst_err)?;
    gst::Element::link_many([&convert, &scale, &caps, &sink]).map_err(gst_err)?;
    let convert_weak = convert.downgrade();
    decode.connect_pad_added(move |_, pad| {
        let is_video = pad
            .current_caps()
            .and_then(|c| c.structure(0).map(|s| s.name().starts_with("video/")))
            .unwrap_or(false);
        if let Some(convert) = convert_weak.upgrade()
            && is_video
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
    let duration = pipeline
        .query_duration::<gst::ClockTime>()
        .map(gst::ClockTime::nseconds)
        .filter(|d| *d > 0)
        .ok_or_else(|| CaptureError::Gst("the clip has no known duration".into()))?;
    let appsink = sink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CaptureError::Gst("appsink is not an AppSink".into()))?;
    let mut rgb: Vec<u8> = Vec::new();
    let mut size = None;
    for i in 0..tiles {
        // The middle of slice i of `tiles`.
        let at = u128::from(duration)
            .saturating_mul(u128::from(i).saturating_mul(2).saturating_add(1))
            .checked_div(u128::from(tiles).saturating_mul(2))
            .and_then(|t| u64::try_from(t).ok())
            .unwrap_or(0);
        pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_NEAREST,
                gst::ClockTime::from_nseconds(at),
            )
            .map_err(gst_err)?;
        wait_async(pipeline)?;
        let sample = appsink
            .try_pull_preroll(Some(step_timeout()))
            .ok_or_else(|| CaptureError::Gst("no picture in the clip".into()))?;
        let (w, h) = append_rgb_rows(&sample, &mut rgb, tiles)?;
        match size {
            None => size = Some((w, h)),
            Some(s) if s == (w, h) => {}
            Some(_) => return Err(CaptureError::Gst("the picture size changed".into())),
        }
    }
    let (w, h) = size.ok_or_else(|| CaptureError::Gst("no picture in the clip".into()))?;
    let total_h = h
        .checked_mul(tiles)
        .ok_or_else(|| CaptureError::Gst("strip too tall".into()))?;
    let jpeg = encode_rgb_jpeg(&rgb, w, total_h, STRIP_QUALITY)?;
    Ok(Strip {
        jpeg,
        tiles,
        tile_width: w,
        tile_height: h,
    })
}

fn bad(what: &str) -> CaptureError {
    CaptureError::Gst(what.to_string())
}

/// Append a sample's RGB rows (without stride padding) to `out`; returns its size. Refuses
/// pictures that would make a strip of `tiles` frames bigger than [`MAX_STRIP_BYTES`].
fn append_rgb_rows(
    sample: &gst::Sample,
    out: &mut Vec<u8>,
    tiles: u32,
) -> Result<(u32, u32), CaptureError> {
    let caps = sample.caps().ok_or_else(|| bad("picture without caps"))?;
    let info = gst_video::VideoInfo::from_caps(caps).map_err(gst_err)?;
    let buffer = sample.buffer().ok_or_else(|| bad("empty picture"))?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|_| bad("unreadable picture"))?;
    let (w, h) = (info.width(), info.height());
    let to_usize = |v: u32| usize::try_from(v).map_err(gst_err);
    let row = to_usize(w)?
        .checked_mul(3)
        .ok_or_else(|| bad("picture too wide"))?;
    let tile_bytes = row
        .checked_mul(to_usize(h)?)
        .ok_or_else(|| bad("picture too big"))?;
    if tile_bytes.saturating_mul(to_usize(tiles)?) > MAX_STRIP_BYTES {
        return Err(bad("picture too big for a strip"));
    }
    let stride = frame
        .plane_stride()
        .first()
        .and_then(|s| usize::try_from(*s).ok())
        .ok_or_else(|| bad("bad stride"))?;
    let data = frame.plane_data(0).map_err(gst_err)?;
    out.reserve(tile_bytes);
    for y in 0..to_usize(h)? {
        let start = y.checked_mul(stride).ok_or_else(|| bad("bad stride"))?;
        let end = start.checked_add(row).ok_or_else(|| bad("bad stride"))?;
        let line = data.get(start..end).ok_or_else(|| bad("short picture"))?;
        out.extend_from_slice(line);
    }
    Ok((w, h))
}

/// Encode tightly packed RGB as a JPEG.
fn encode_rgb_jpeg(
    rgb: &[u8],
    width: u32,
    height: u32,
    quality: &str,
) -> Result<Vec<u8>, CaptureError> {
    let info = gst_video::VideoInfo::builder(gst_video::VideoFormat::Rgb, width, height)
        .fps(gst::Fraction::new(0, 1))
        .build()
        .map_err(gst_err)?;
    let row = usize::try_from(width)
        .map_err(gst_err)?
        .checked_mul(3)
        .ok_or_else(|| bad("picture too wide"))?;
    let stride = info
        .stride()
        .first()
        .and_then(|s| usize::try_from(*s).ok())
        .ok_or_else(|| bad("bad stride"))?;
    let mut padded = vec![0u8; info.size()];
    for (y, line) in rgb.chunks_exact(row).enumerate() {
        let start = y.checked_mul(stride).ok_or_else(|| bad("bad stride"))?;
        let end = start.checked_add(row).ok_or_else(|| bad("bad stride"))?;
        padded
            .get_mut(start..end)
            .ok_or_else(|| bad("short picture"))?
            .copy_from_slice(line);
    }
    let pipeline = gst::Pipeline::new();
    let src = make("appsrc")?;
    let jpeg = make("jpegenc")?;
    props::set_str(&jpeg, "quality", quality);
    let sink = make("appsink")?;
    props::set_str(&sink, "sync", "false");
    pipeline.add_many([&src, &jpeg, &sink]).map_err(gst_err)?;
    gst::Element::link_many([&src, &jpeg, &sink]).map_err(gst_err)?;
    let appsrc = src
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| bad("appsrc is not an AppSrc"))?;
    appsrc.set_caps(Some(&info.to_caps().map_err(gst_err)?));
    appsrc.set_format(gst::Format::Time);
    let appsink = sink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| bad("appsink is not an AppSink"))?;
    let running = Running(pipeline);
    running.0.set_state(gst::State::Playing).map_err(gst_err)?;
    let mut buffer = gst::Buffer::from_mut_slice(padded);
    if let Some(b) = buffer.get_mut() {
        b.set_pts(gst::ClockTime::ZERO);
    }
    appsrc.push_buffer(buffer).map_err(gst_err)?;
    appsrc.end_of_stream().map_err(gst_err)?;
    let sample = appsink
        .try_pull_sample(Some(step_timeout()))
        .ok_or_else(|| bad("JPEG encoder gave nothing"))?;
    let buffer = sample.buffer().ok_or_else(|| bad("empty JPEG"))?;
    let map = buffer.map_readable().map_err(gst_err)?;
    Ok(map.as_slice().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_video_decoders_rank_below_software() {
        prefer_software_decoders().unwrap();
        for f in gst::Registry::get().features(gst::ElementFactory::static_type()) {
            let factory = f.clone().downcast::<gst::ElementFactory>().unwrap();
            let klass = factory
                .metadata(gst::ELEMENT_METADATA_KLASS)
                .unwrap_or_default();
            if klass.contains("Decoder") && klass.contains("Video") && klass.contains("Hardware") {
                assert!(f.rank() <= gst::Rank::MARGINAL, "{}", f.name());
            }
        }
        // Still there as a fallback where no software decoder is installed.
        if let Some(d3d11) = gst::ElementFactory::find("d3d11h264dec") {
            assert_eq!(d3d11.rank(), gst::Rank::MARGINAL);
        }
        // Idempotent.
        assert_eq!(prefer_software_decoders().unwrap(), 0);
    }
}
