//! One composed frame (`editor.frame`, M9.4): the picture an export encodes at one
//! output time, drawn by the export's own steps ([`crate::render::Drawer`]) at a
//! preview width, as a JPEG. Each visible layer's file is opened and accurately seeked
//! to the source time it shows there, and the frame picked is the one the export's
//! frame loop picks: the largest presentation time at or before that time (the first
//! frame when none is; the file's last frame when the time is past its end).

use std::path::{Path, PathBuf};

use gstreamer as gst;
use klip_edit::Composition;
use klip_edit::advanced::Source as Media;
use klip_edit::compose::{Layer, LayerKind};

use crate::render::{Drawer, native_size, source_config};
use crate::source::{Frame, Source, SourceConfig};
use crate::{
    CUSTOM_KBPS, CustomOutput, Encoders, ExportError, MAX_EXPORT_MS, MAX_SIDE, OutCodec, Preset,
    check_plan, media_files, output_for, probe,
};

/// Narrowest frame asked for.
pub const MIN_FRAME_WIDTH: u32 = 16;
/// JPEG quality: high enough that the preview parity check (PSNR) measures the
/// drawing, not the compression.
const JPEG_QUALITY: &str = "92";
/// How far before a time past the end of a file the last frame is looked for.
const LOOK_BACK_MS: u64 = 2_000;

/// One frame of a composition.
pub struct FrameRequest<'a> {
    /// The main clip (`Source::Main` in the composition).
    pub source: &'a Path,
    pub composition: &'a Composition,
    /// Where the composition's other sources are, as for an export.
    pub media: &'a [(Media, PathBuf)],
    /// Output time; past the end means the last frame.
    pub out_ms: u64,
    /// The picture's width, [`MIN_FRAME_WIDTH`]…[`MAX_SIDE`]; its height follows the
    /// edit's format (as a Custom export of that width).
    pub width: u32,
}

/// A rendered frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameImage {
    pub width: u32,
    pub height: u32,
    /// The output time drawn.
    pub out_ms: u64,
    pub jpeg: Vec<u8>,
}

/// Draw `req`'s frame.
pub fn frame(req: &FrameRequest<'_>) -> Result<FrameImage, ExportError> {
    let comp = req.composition;
    if comp.duration_ms == 0 || comp.duration_ms > MAX_EXPORT_MS {
        return Err(ExportError::Plan("the edit is empty or too long".into()));
    }
    if !(MIN_FRAME_WIDTH..=MAX_SIDE).contains(&req.width) {
        return Err(ExportError::Plan(format!(
            "the frame must be {MIN_FRAME_WIDTH}…{MAX_SIDE} pixels wide"
        )));
    }
    gst::init().map_err(|e| ExportError::Gst(e.to_string()))?;
    let facts = probe(req.source)?;
    let t = req.out_ms.min(comp.duration_ms.saturating_sub(1));
    // Only the files on screen at `t` are read (a clip missing elsewhere in a montage
    // doesn't stop the preview).
    let shown = Composition {
        layers: comp.visible_at(t).into_iter().cloned().collect(),
        texts: Vec::new(),
        markers: Vec::new(),
        ..comp.clone()
    };
    let media = media_files(req.source, &shown, req.media, &facts)?;
    let out = output_for(
        Preset::Custom(CustomOutput {
            width: req.width,
            height: MAX_SIDE,
            fps: 60,
            bitrate_kbps: CUSTOM_KBPS.0,
            codec: OutCodec::H264,
        }),
        comp.crop,
        &facts,
        &Encoders::default(),
        None,
    );
    check_plan(comp, &facts, &out)?;
    let visible = comp.visible_at(t);
    let mut frames: Vec<(u32, Frame, (u32, u32))> = Vec::with_capacity(visible.len());
    for l in &visible {
        let m = media
            .iter()
            .find(|m| m.source == l.source)
            .ok_or_else(|| ExportError::Source("a clip of the edit is missing".into()))?;
        let cfg = source_config(l, m.facts.as_ref(), comp.crop, (out.width, out.height))?;
        let picked = match l.kind {
            LayerKind::Video => layer_frame(l, t, &m.path, cfg)?,
            LayerKind::Image => Source::open(&m.path, cfg)
                .map_err(|e| ExportError::Source(format!("a picture could not be read: {e}")))?
                .next_frame()?,
            LayerKind::Audio | LayerKind::Music => None,
        };
        if let Some(f) = picked {
            let native = native_size(l, m.facts.as_ref(), &f);
            frames.push((l.id, f, native));
        }
    }
    let mut drawer = Drawer::new(comp, &out)?;
    let buffer = drawer.draw(t, &visible, |l| {
        frames
            .iter()
            .find(|(id, _, _)| *id == l.id)
            .map(|(_, f, n)| (f, *n))
    })?;
    let jpeg = bgra_jpeg(&buffer, out.width, out.height)?;
    Ok(FrameImage {
        width: out.width,
        height: out.height,
        out_ms: t,
        jpeg,
    })
}

/// The frame video layer `l` shows at output time `t`.
fn layer_frame(
    l: &Layer,
    t: u64,
    path: &Path,
    cfg: SourceConfig,
) -> Result<Option<Frame>, ExportError> {
    let Some(src_ms) = l.source_ms(t) else {
        return Ok(None);
    };
    let src = Source::open(path, cfg)?;
    for back in [0, LOOK_BACK_MS] {
        src.seek(src_ms.saturating_sub(back), src_ms.saturating_add(1), 1.0)?;
        let mut best: Option<Frame> = None;
        while let Some(f) = src.next_frame()? {
            if f.pts_ms > src_ms {
                // The export shows the first frame of a piece until a frame at or
                // before the time arrives.
                if best.is_none() {
                    best = Some(f);
                }
                break;
            }
            best = Some(f);
        }
        if best.is_some() {
            return Ok(best);
        }
    }
    Ok(None)
}

/// A packed BGRA picture as a JPEG.
fn bgra_jpeg(buffer: &gst::Buffer, width: u32, height: u32) -> Result<Vec<u8>, ExportError> {
    let map = buffer
        .map_readable()
        .map_err(|_| ExportError::Gst("the frame cannot be read".into()))?;
    let bytes = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    let bgra = map
        .as_slice()
        .get(..bytes)
        .ok_or_else(|| ExportError::Gst("the frame is too short".into()))?;
    klip_capture::thumbnail::encode_bgra_jpeg(bgra, width, height, JPEG_QUALITY)
        .map_err(|e| ExportError::Gst(e.to_string()))
}
