//! The renderer (ADR 0018): a `klip_edit::Composition` → frames and sound for the
//! encoder, in one pass over the output frames. Normal mode is the special case of one
//! track of back-to-back video layers.
//!
//! Every layer plays in pieces (`Layer::pieces`, at most [`PIECE_MAX_MS`]): one flushing
//! seek of its source per piece at the piece's rate, exactly like a Normal-mode segment,
//! so cuts and speed ramps land on the same frames as in the preview. Output frame `i` at
//! time `t` shows, for each visible layer, the decoded frame with the largest pts ≤
//! `layer.source_ms(t)`; pictures are decoded once. Sources are pooled: layers that
//! follow each other in the same file at the same size share one pipeline (Normal mode
//! is one source, as before), and a pipeline is never shared by two layers at once.
//!
//! A piece's audio is taken when the piece ends ([`crate::mixer`]); the mixer hands
//! audio to the encoder up to the start of the earliest piece still playing, so memory
//! is bounded by the longest piece, not by the export.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gstreamer as gst;
use klip_edit::Composition;
use klip_edit::advanced::Source as Media;
use klip_edit::compose::{Layer, LayerKind, Piece, fit_in, fitted, place};
use klip_edit::{Rect, Volumes};

use crate::canvas::{Canvas, Item};
use crate::encode::Encoded;
use crate::mixer::{self, Bus, Mixer};
use crate::source::{Frame, Source, SourceConfig};
use crate::{Cue, ExportError, ExportRequest, Facts, Output, Progress};

/// Longest piece of a layer played by one seek: bounds the audio a source buffers and
/// the audio the mixer holds.
pub const PIECE_MAX_MS: u64 = 60_000;
/// Most layers that may play at once (each one is a decoding pipeline).
pub const MAX_RUNNING: usize = 16;
/// Idle pipelines kept open for a later layer from the same file.
const MAX_IDLE: usize = 4;

/// A file the composition plays and what the probe found (`None`: not an MP4 with
/// video, e.g. music, a sound or a picture).
pub(crate) struct MediaFile {
    pub source: Media,
    pub path: PathBuf,
    pub facts: Option<Facts>,
}

struct Pooled {
    media: usize,
    cfg: SourceConfig,
    source: Source,
    busy: bool,
    /// The last frame its previous user showed: what a piece past the end of the file
    /// (the movie is a few ms longer than its video) keeps showing.
    last: Option<Frame>,
}

#[derive(Default)]
struct Pool {
    slots: Vec<Option<Pooled>>,
}

impl Pool {
    /// A pipeline for `media` read with `cfg` (an idle one if there is one), and the
    /// last frame its previous user showed.
    fn acquire(
        &mut self,
        media: usize,
        path: &Path,
        cfg: SourceConfig,
    ) -> Result<(usize, Option<Frame>), ExportError> {
        let reuse = self.slots.iter().position(|s| {
            s.as_ref()
                .is_some_and(|p| !p.busy && p.media == media && p.cfg == cfg)
        });
        if let Some(i) = reuse
            && let Some(Some(p)) = self.slots.get_mut(i)
        {
            p.busy = true;
            return Ok((i, p.last.take()));
        }
        let idle: Vec<usize> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_ref().is_some_and(|p| !p.busy))
            .map(|(i, _)| i)
            .collect();
        if idle.len() >= MAX_IDLE
            && let Some(slot) = idle.first().and_then(|&i| self.slots.get_mut(i))
        {
            *slot = None;
        }
        let source = Source::open(path, cfg)?;
        let p = Pooled {
            media,
            cfg,
            source,
            busy: true,
            last: None,
        };
        match self.slots.iter().position(Option::is_none) {
            Some(i) => {
                if let Some(slot) = self.slots.get_mut(i) {
                    *slot = Some(p);
                }
                Ok((i, None))
            }
            None => {
                self.slots.push(Some(p));
                Ok((self.slots.len().saturating_sub(1), None))
            }
        }
    }

    fn get(&self, i: usize) -> Result<&Source, ExportError> {
        self.slots
            .get(i)
            .and_then(Option::as_ref)
            .map(|p| &p.source)
            .ok_or_else(|| ExportError::Source("a source went away".into()))
    }

    fn release(&mut self, i: usize, last: Option<Frame>) {
        if let Some(Some(p)) = self.slots.get_mut(i) {
            p.busy = false;
            p.last = last;
        }
    }
}

/// One layer being played.
struct Run {
    layer: usize,
    media: usize,
    kind: LayerKind,
    cfg: SourceConfig,
    pieces: Vec<Piece>,
    next: usize,
    current: Option<Piece>,
    slot: Option<usize>,
    cur: Option<Frame>,
    ahead: Option<Frame>,
    last: Option<Frame>,
    /// A picture, decoded the first time it shows.
    still: Option<Frame>,
    /// Its sound goes into the mix.
    audio: bool,
    done: bool,
}

/// What every step reads.
struct Ctx<'a> {
    layers: &'a [Layer],
    media: &'a [MediaFile],
    main_volumes: Volumes,
}

fn rounded(v: f64) -> u32 {
    (v + 0.5).floor().clamp(2.0, f64::from(u32::MAX)) as u32
}

/// How big a video layer is decoded: its fitted size times the largest zoom it
/// reaches (so a zoom-in is scaled up once, by the canvas), never above the file's
/// own size. The main clip without zoom comes out at exactly the canvas size.
fn decode_size(
    layer: &Layer,
    native: (u32, u32),
    main: bool,
    crop: Rect,
    canvas: (u32, u32),
) -> (u32, u32) {
    let zoom = f64::from(layer.max_zoom());
    if main && zoom <= 1.0 {
        return canvas;
    }
    let (nw, nh) = if main {
        (crop.width, crop.height)
    } else {
        native
    };
    let fit = fitted(LayerKind::Video, main, native, crop.width, canvas);
    fit_in(
        nw,
        nh,
        rounded(f64::from(fit.0) * zoom),
        rounded(f64::from(fit.1) * zoom),
    )
}

/// How a layer's file is read: a video decoded at [`decode_size`] (the main clip
/// cropped to the format), a picture at its own size, sound without video.
pub(crate) fn source_config(
    l: &Layer,
    facts: Option<&Facts>,
    crop: Rect,
    canvas: (u32, u32),
) -> Result<SourceConfig, ExportError> {
    Ok(match l.kind {
        LayerKind::Video => {
            let f = facts.ok_or_else(|| {
                ExportError::Source("a video of the edit could not be read".into())
            })?;
            let main = l.source == Media::Main;
            let (w, h) = decode_size(l, (f.width, f.height), main, crop, canvas);
            SourceConfig {
                src_width: f.width,
                src_height: f.height,
                crop: if main { crop } else { Rect::default() },
                out_width: w,
                out_height: h,
                want_video: true,
            }
        }
        LayerKind::Image => SourceConfig {
            src_width: 0,
            src_height: 0,
            crop: Rect::default(),
            out_width: 0,
            out_height: 0,
            want_video: true,
        },
        LayerKind::Audio | LayerKind::Music => SourceConfig {
            src_width: 0,
            src_height: 0,
            crop: Rect::default(),
            out_width: 2,
            out_height: 2,
            want_video: false,
        },
    })
}

/// A layer's picture size before fitting: a video's whole frame as probed, a
/// picture's decoded size.
pub(crate) fn native_size(l: &Layer, facts: Option<&Facts>, frame: &Frame) -> (u32, u32) {
    match (l.kind, facts) {
        (LayerKind::Video, Some(f)) => (f.width, f.height),
        _ => (frame.info.width(), frame.info.height()),
    }
}

/// One output picture from the visible layers' frames (ADR 0018): each frame placed by
/// its layer's transform, blended with its opacity, the colour grade, then the text
/// cues. The export's frame loop and `editor.frame` both draw with it, so a preview
/// frame is the exported frame.
pub(crate) struct Drawer<'c> {
    comp: &'c Composition,
    out: Output,
    canvas: Canvas,
    grade: Option<[[f32; 4]; 3]>,
    cues: Vec<Cue>,
}

impl<'c> Drawer<'c> {
    pub(crate) fn new(comp: &'c Composition, out: &Output) -> Result<Self, ExportError> {
        let cues: Vec<Cue> = comp
            .texts
            .iter()
            .filter_map(|t| {
                klip_edit::render_layer(t, out.width, out.height)
                    .ok()
                    .map(|bitmap| Cue { id: t.id, bitmap })
            })
            .collect();
        Ok(Drawer {
            comp,
            out: out.clone(),
            canvas: Canvas::new(out.width, out.height)?,
            grade: (!comp.colour.is_identity()).then(|| comp.colour.matrix()),
            cues,
        })
    }

    /// The picture at output time `t`: `frame_of` gives each of the `visible` layers'
    /// current frame and native size; a layer without one is left out.
    pub(crate) fn draw<'f>(
        &mut self,
        t: u64,
        visible: &[&Layer],
        frame_of: impl Fn(&Layer) -> Option<(&'f Frame, (u32, u32))>,
    ) -> Result<gst::Buffer, ExportError> {
        let (width, height) = (self.out.width, self.out.height);
        let mut items: Vec<Item<'f>> = Vec::with_capacity(visible.len());
        for l in visible {
            let Some((frame, native)) = frame_of(l) else {
                continue;
            };
            let fit = fitted(
                l.kind,
                l.source == Media::Main,
                native,
                self.comp.crop.width,
                (width, height),
            );
            items.push(Item {
                frame,
                placement: place(l.transform_at(t), fit.0, fit.1, width, height),
                opacity: l.opacity_at(t),
                opaque: l.kind == LayerKind::Video,
            });
        }
        let active: Vec<&Cue> = self
            .comp
            .texts_at(t)
            .filter_map(|l| self.cues.iter().find(|c| c.id == l.id))
            .collect();
        match items.as_slice() {
            [only] if self.grade.is_none() && self.canvas.is_passthrough(only) => {
                if active.is_empty() {
                    Ok(only.frame.buffer.copy())
                } else {
                    crate::composed(only.frame, &self.out, &active)
                }
            }
            _ => {
                let mut b = self.canvas.compose(&items, self.grade.as_ref())?;
                draw_cues(&mut b, &self.out, &active)?;
                Ok(b)
            }
        }
    }
}

impl Run {
    fn layer<'a>(&self, ctx: &Ctx<'a>) -> Result<&'a Layer, ExportError> {
        ctx.layers
            .get(self.layer)
            .ok_or_else(|| ExportError::Plan("a layer went away".into()))
    }

    fn start_piece(&mut self, pool: &mut Pool, ctx: &Ctx<'_>, p: Piece) -> Result<(), ExportError> {
        let slot = match self.slot {
            Some(s) => s,
            None => {
                let m = ctx
                    .media
                    .get(self.media)
                    .ok_or_else(|| ExportError::Plan("a source is unknown".into()))?;
                let (s, last) = pool.acquire(self.media, &m.path, self.cfg)?;
                self.slot = Some(s);
                if self.last.is_none() {
                    self.last = last;
                }
                s
            }
        };
        let src = pool.get(slot)?;
        src.seek(
            p.src_start_ms,
            p.src_end_ms,
            f64::from(p.rate_pct.max(1)) / 100.0,
        )?;
        self.cur = None;
        self.ahead = if self.kind == LayerKind::Video {
            src.next_frame()?
        } else {
            None
        };
        self.current = Some(p);
        Ok(())
    }

    fn finish_piece(
        &mut self,
        pool: &mut Pool,
        mixer: &mut Mixer,
        ctx: &Ctx<'_>,
    ) -> Result<(), ExportError> {
        let Some(p) = self.current.take() else {
            return Ok(());
        };
        let slot = self
            .slot
            .ok_or_else(|| ExportError::Source("a piece without a source".into()))?;
        let src = pool.get(slot)?;
        if self.kind == LayerKind::Video {
            // Run the piece out so its audio is complete.
            while let Some(f) = src.next_frame()? {
                self.ahead = Some(f);
            }
            if let Some(f) = self.cur.take().or(self.ahead.take()) {
                self.last = Some(f);
            }
        }
        let tracks = src.drain_audio()?;
        if self.audio {
            let layer = self.layer(ctx)?;
            let facts = ctx.media.get(self.media).and_then(|m| m.facts.as_ref());
            let names: Vec<&str> = (0..tracks.len())
                .map(|i| {
                    facts
                        .and_then(|f| f.audio_names.get(i))
                        .map_or("", String::as_str)
                })
                .collect();
            let has_separate = names
                .iter()
                .any(|n| matches!(*n, "Game" | "System" | "Microphone" | "Voice"));
            let vols = if layer.source == Media::Main {
                ctx.main_volumes
            } else {
                Volumes::default()
            };
            let weights: Vec<Option<f32>> = names
                .iter()
                .map(|n| mixer::volume_for(n, has_separate, &vols))
                .collect();
            let voice: Vec<bool> = names.iter().map(|n| mixer::is_voice(n)).collect();
            let start = p.out_start_ms;
            let (mix, speech) = mixer::piece_mix(
                tracks,
                &weights,
                &voice,
                layer.kind == LayerKind::Audio,
                p.out_end_ms.saturating_sub(start),
                |ms| layer.volume_at(start.saturating_add(ms)),
            );
            let bus = if layer.kind == LayerKind::Music {
                Bus::Music
            } else {
                Bus::Main
            };
            mixer.add(bus, start, &mix, &speech);
        }
        if self.next >= self.pieces.len() {
            pool.release(slot, self.last.take());
            self.slot = None;
            self.done = true;
        }
        Ok(())
    }

    /// Start and finish pieces until the one playing at `t` (if any) is running.
    fn advance(
        &mut self,
        t: u64,
        pool: &mut Pool,
        mixer: &mut Mixer,
        ctx: &Ctx<'_>,
    ) -> Result<(), ExportError> {
        if self.done {
            return Ok(());
        }
        if self.kind == LayerKind::Image {
            if t >= self.layer(ctx)?.end_ms {
                self.still = None;
                self.done = true;
            }
            return Ok(());
        }
        loop {
            if let Some(p) = self.current {
                if t < p.out_end_ms {
                    return Ok(());
                }
                self.finish_piece(pool, mixer, ctx)?;
                if self.done {
                    return Ok(());
                }
                continue;
            }
            match self.pieces.get(self.next).copied() {
                Some(p) if p.out_start_ms <= t => {
                    self.next = self.next.saturating_add(1);
                    self.start_piece(pool, ctx, p)?;
                }
                _ => return Ok(()),
            }
        }
    }

    /// Pull frames up to output time `t` (video) or decode the picture once (image).
    fn step(&mut self, t: u64, pool: &Pool, ctx: &Ctx<'_>) -> Result<(), ExportError> {
        match self.kind {
            LayerKind::Video => {
                let Some(src_ms) = self.layer(ctx)?.source_ms(t) else {
                    return Ok(());
                };
                let Some(slot) = self.slot else {
                    return Ok(());
                };
                let src = pool.get(slot)?;
                while self.ahead.as_ref().is_some_and(|n| n.pts_ms <= src_ms) {
                    self.cur = self.ahead.take();
                    self.ahead = src.next_frame()?;
                }
                Ok(())
            }
            LayerKind::Image if self.still.is_none() => {
                let m = ctx
                    .media
                    .get(self.media)
                    .ok_or_else(|| ExportError::Plan("a picture is unknown".into()))?;
                let src = Source::open(&m.path, self.cfg).map_err(|e| {
                    ExportError::Source(format!("a picture could not be read: {e}"))
                })?;
                self.still = src.next_frame()?;
                if self.still.is_none() {
                    return Err(ExportError::Source("a picture has no pixels".into()));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn frame(&self) -> Option<&Frame> {
        match self.kind {
            LayerKind::Video => self
                .cur
                .as_ref()
                .or(self.ahead.as_ref())
                .or(self.last.as_ref()),
            LayerKind::Image => self.still.as_ref(),
            _ => None,
        }
    }
}

/// Blend the active text cues over a packed BGRA frame of the output size.
fn draw_cues(buffer: &mut gst::Buffer, out: &Output, cues: &[&Cue]) -> Result<(), ExportError> {
    if cues.is_empty() {
        return Ok(());
    }
    let b = buffer
        .get_mut()
        .ok_or_else(|| ExportError::Gst("frame buffer is shared".into()))?;
    let mut map = b
        .map_writable()
        .map_err(|_| ExportError::Gst("frame cannot be written".into()))?;
    let row_bytes = (out.width as usize).saturating_mul(4);
    for cue in cues {
        crate::compose::blend_rgba_over_bgra(
            map.as_mut_slice(),
            out.width,
            out.height,
            row_bytes,
            &cue.bitmap,
        );
    }
    Ok(())
}

/// One encode pass over the whole composition.
pub(crate) fn run_pass(
    req: &ExportRequest<'_>,
    media: &[MediaFile],
    out: &Output,
    spool: &Path,
    pass: u32,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<Encoded, ExportError> {
    let comp = req.composition;
    let canvas_size = (out.width, out.height);
    let ctx = Ctx {
        layers: &comp.layers,
        media,
        main_volumes: comp.main_volumes,
    };
    let mut runs: Vec<Run> = Vec::new();
    let mut run_of: HashMap<u32, usize> = HashMap::new();
    for (li, l) in comp.layers.iter().enumerate() {
        // Muted sound never plays (its file isn't even among `media`).
        if l.muted && matches!(l.kind, LayerKind::Audio | LayerKind::Music) {
            continue;
        }
        let mi = media
            .iter()
            .position(|m| m.source == l.source)
            .ok_or_else(|| ExportError::Source("a clip of the edit is missing".into()))?;
        let facts = media.get(mi).and_then(|m| m.facts.as_ref());
        let cfg = source_config(l, facts, comp.crop, canvas_size)?;
        let (pieces, audio) = match l.kind {
            LayerKind::Video => (l.pieces(PIECE_MAX_MS), !l.muted),
            LayerKind::Image => (Vec::new(), false),
            LayerKind::Audio | LayerKind::Music => (l.pieces(PIECE_MAX_MS), true),
        };
        run_of.entry(l.id).or_insert(runs.len());
        runs.push(Run {
            layer: li,
            media: mi,
            kind: l.kind,
            cfg,
            pieces,
            next: 0,
            current: None,
            slot: None,
            cur: None,
            ahead: None,
            last: None,
            still: None,
            audio,
            done: false,
        });
    }
    let mut pool = Pool::default();
    let mut mixer = Mixer::new(&comp.ducking);
    let mut drawer = Drawer::new(comp, out)?;
    let mut encoder = crate::start_encoder(out, req.encoders, spool)?;
    let frames_total = comp.frame_count(out.fps);
    let mut report = |done: u64| {
        progress(Progress {
            pass,
            frames_done: done,
            frames_total,
        });
    };
    report(0);
    let mut i: u64 = 0;
    while let Some(t) = comp.frame_out_ms(i, out.fps) {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        for run in &mut runs {
            run.advance(t, &mut pool, &mut mixer, &ctx)?;
        }
        let safe = runs
            .iter()
            .filter_map(|r| r.current.map(|p| p.out_start_ms))
            .fold(t, u64::min);
        let audio = mixer.flush(safe, false);
        if !audio.is_empty() {
            encoder.push_audio(&audio)?;
        }
        let visible = comp.visible_at(t);
        for l in &visible {
            if let Some(run) = run_of.get(&l.id).and_then(|&r| runs.get_mut(r)) {
                run.step(t, &pool, &ctx)?;
            }
        }
        let buffer = drawer.draw(t, &visible, |l| {
            let run = run_of.get(&l.id).and_then(|&r| runs.get(r))?;
            let frame = run.frame()?;
            let facts = media.get(run.media).and_then(|m| m.facts.as_ref());
            Some((frame, native_size(l, facts, frame)))
        })?;
        encoder.push_frame(buffer, i)?;
        i = i.saturating_add(1);
        if i.is_multiple_of(15) {
            report(i);
        }
    }
    report(i);
    // Whatever is left (pieces after the last frame, the last pieces' audio).
    for run in &mut runs {
        run.advance(u64::MAX, &mut pool, &mut mixer, &ctx)?;
    }
    drop(runs);
    drop(pool);
    let audio = mixer.flush(comp.duration_ms, true);
    if !audio.is_empty() {
        encoder.push_audio(&audio)?;
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
    }
    encoder.finish()
}
