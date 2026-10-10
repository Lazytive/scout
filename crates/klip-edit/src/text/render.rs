//! Text rasterisation (ADR 0017): one text layer → an RGBA bitmap at the output size,
//! drawn with the bundled Inter font so the preview and the export show the same pixels.
//!
//! Layout: the text is cleaned ([`clean_text`]), upper-cased if the style says so (and
//! cut to [`MAX_TEXT_CHARS`] again: `ß` → `SS`), and wrapped at spaces inside the
//! frame's side margins (runs of spaces collapse; a word wider than the line breaks
//! between characters). More than [`MAX_TEXT_LINES`] lines, a line still too wide or a
//! block taller than the frame shrink the font in 10 % steps down to [`MIN_PX`];
//! whatever still doesn't fit is cut after the last line. Effects, back to front: a rounded pill behind each line, a drop
//! shadow (offset copy of the glyph coverage, box-blurred twice), an outline (the
//! coverage dilated by a disc) and the fill. Glyph outlines come from `skrifa`
//! (unhinted), coverage from `ab_glyph_rasterizer`; the rest is integer math or float
//! math on the same inputs, so one input gives the same bytes on every machine (the
//! tests keep golden hashes).
//!
//! Hostile input: any string, any style values (clamped), any frame size up to
//! [`MAX_FRAME_SIDE`]; buffers cover the drawn area clamped to the frame.

use std::sync::OnceLock;

use ab_glyph_rasterizer::{Rasterizer, point};
use skrifa::charmap::Charmap;
use skrifa::metrics::GlyphMetrics;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::prelude::{FontRef, GlyphId, LocationRef, MetadataProvider, Size};

use super::{
    DEFAULT_PRESET, MAX_TEXT_CHARS, MAX_TEXT_LINES, Position, TextLayer, TextStyle, clean_text,
    preset,
};

/// The bundled font family (OFL 1.1, see ATTRIBUTION.md).
pub const FONT_FAMILY: &str = "Inter";
/// Its release.
pub const FONT_VERSION: &str = "4.1";
/// Bumped whenever rendering changes, so cached bitmaps ([`cache_key`]) are redone.
pub const RENDER_VERSION: u32 = 1;
/// Longest frame side accepted.
pub const MAX_FRAME_SIDE: u32 = 4096;
/// Smallest font size the auto-shrink goes down to, in pixels.
pub const MIN_PX: f32 = 4.0;

/// Side margin the text wraps inside, as a fraction of the frame width.
const MARGIN_X: f32 = 0.05;
/// Top and bottom margin, as a fraction of the frame height.
const MARGIN_Y: f32 = 0.06;
/// Line height as a multiple of the font size.
const LINE_SPACING: f32 = 1.2;
/// Largest font size as a fraction of the frame height (a style asking for more is clamped).
const MAX_SIZE: f32 = 0.5;
/// Largest outline width as a fraction of the font size.
const MAX_OUTLINE: f32 = 0.3;
/// Each shrink step multiplies the font size by this.
const SHRINK: f32 = 0.9;
/// Enough steps to go from a 4096 px size to [`MIN_PX`] (66); a backstop only.
const MAX_SHRINK_STEPS: u32 = 80;
/// Pill padding left and right, as a fraction of the font size.
const PILL_PAD_X: f32 = 0.35;
/// Gap between the pills of two lines, as a fraction of the font size.
const PILL_GAP: f32 = 0.04;
/// Shadow blur radius as a fraction of the font size.
const SHADOW_BLUR: f32 = 0.03;
/// Largest outline radius, shadow offset and blur radius in pixels (bounded kernels:
/// the outline splats a (2r+1)² disc from every edge pixel).
const MAX_RADIUS: f32 = 32.0;
/// Most path commands kept for one glyph (Inter's biggest glyphs have a few hundred).
const MAX_GLYPH_CMDS: usize = 8192;

static BOLD_TTF: &[u8] = include_bytes!("../../../../assets/fonts/Inter-Bold.ttf");
static EXTRA_BOLD_TTF: &[u8] = include_bytes!("../../../../assets/fonts/Inter-ExtraBold.ttf");

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("the bundled font could not be read")]
    Font,
    #[error("unknown text preset {0:?}")]
    Preset(String),
    #[error("frame {0}x{1} cannot be rendered (sides must be 1..={MAX_FRAME_SIDE})")]
    Frame(u32, u32),
    #[error("not enough memory for a {0}x{1} bitmap")]
    Memory(u32, u32),
    #[error("png encoding failed: {0}")]
    Png(String),
}

/// A rendered layer: straight-alpha RGBA rows, cropped to the drawn area, placed at
/// (`x`, `y`) in a frame of `frame_width` × `frame_height`. Never empty (at least 1×1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextBitmap {
    pub frame_width: u32,
    pub frame_height: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, straight (not premultiplied) alpha.
    pub rgba: Vec<u8>,
}

impl TextBitmap {
    fn blank(frame_width: u32, frame_height: u32) -> Self {
        TextBitmap {
            frame_width,
            frame_height,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        }
    }

    /// No pixel has any alpha.
    pub fn is_blank(&self) -> bool {
        self.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0)
    }

    /// The pixel at (`x`, `y`) of the bitmap (not the frame).
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize)
            .checked_mul(self.width as usize)?
            .checked_add(x as usize)?;
        self.rgba.as_chunks::<4>().0.get(i).copied()
    }

    /// The bitmap as an RGBA PNG.
    pub fn png(&self) -> Result<Vec<u8>, RenderError> {
        let err = |e: png::EncodingError| RenderError::Png(e.to_string());
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(err)?;
        w.write_image_data(&self.rgba).map_err(err)?;
        w.finish().map_err(err)?;
        Ok(out)
    }
}

/// What [`measure`] reports about a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct TextMeasure {
    /// Font size in pixels after auto-shrink (ascent to descent).
    pub font_px: f32,
    /// Line height in pixels.
    pub line_height: u32,
    /// The lines as laid out (after wrapping and cutting).
    pub lines: Vec<String>,
    /// Frame y of the text block's top edge.
    pub top: u32,
    /// The font had to shrink to fit.
    pub shrunk: bool,
    /// Lines were cut even at [`MIN_PX`].
    pub truncated: bool,
}

/// Lay out and draw `text` in `style` at `position` for a `width` × `height` frame.
/// `text` may be anything (it's cleaned first); an empty text gives a blank 1×1 bitmap.
pub fn render(
    text: &str,
    style: &TextStyle,
    position: Position,
    width: u32,
    height: u32,
) -> Result<TextBitmap, RenderError> {
    let frame = Frame::new(width, height)?;
    let face = face(style.weight)?;
    let layout = Layout::new(face, text, style, position, frame);
    raster(face, &layout, style, frame)
}

/// [`render`] for a project layer (an unknown preset renders as [`DEFAULT_PRESET`]).
pub fn render_layer(layer: &TextLayer, width: u32, height: u32) -> Result<TextBitmap, RenderError> {
    let style = preset(&layer.preset)
        .or_else(|| preset(DEFAULT_PRESET))
        .ok_or_else(|| RenderError::Preset(layer.preset.clone()))?;
    render(&layer.text, style, layer.position, width, height)
}

/// The layout [`render`] would use, without drawing.
pub fn measure(
    text: &str,
    style: &TextStyle,
    position: Position,
    width: u32,
    height: u32,
) -> Result<TextMeasure, RenderError> {
    let frame = Frame::new(width, height)?;
    let face = face(style.weight)?;
    let l = Layout::new(face, text, style, position, frame);
    Ok(TextMeasure {
        font_px: l.px,
        line_height: l.line_height as u32,
        lines: l.lines.iter().map(|l| l.text.clone()).collect(),
        top: l.top as u32,
        shrunk: l.shrunk,
        truncated: l.truncated,
    })
}

/// A stable name for the bitmap of (`text`, `style`, `position`, frame size): the same
/// on every machine and every run, different whenever any input or the renderer changes.
/// 16 hex digits (FNV-1a 64), safe as a file name.
pub fn cache_key(
    text: &str,
    style: &TextStyle,
    position: Position,
    width: u32,
    height: u32,
) -> String {
    let style_json = serde_json::to_string(style).unwrap_or_default();
    let s = format!(
        "klip-text\n{RENDER_VERSION}\n{FONT_FAMILY} {FONT_VERSION}\n{width}x{height}\n{position:?}\n{style_json}\n{}",
        clean_text(text)
    );
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// One bundled weight, parsed once.
struct Face {
    font: FontRef<'static>,
    /// Font units per em.
    upem: f32,
    /// Ascent minus descent in font units: what a text size of `px` pixels maps to.
    height_units: f32,
    ascent_units: f32,
}

impl Face {
    fn load(bytes: &'static [u8]) -> Option<Face> {
        let font = FontRef::new(bytes).ok()?;
        let m = font.metrics(Size::unscaled(), LocationRef::default());
        let upem = f32::from(m.units_per_em);
        let height_units = m.ascent - m.descent;
        (upem > 0.0 && height_units > 0.0 && m.ascent.is_finite()).then_some(Face {
            font,
            upem,
            height_units,
            ascent_units: m.ascent,
        })
    }

    /// The font at a text size of `px` pixels (ascent to descent).
    fn at(&self, px: f32) -> Scaled<'_> {
        let size = Size::new(px * self.upem / self.height_units);
        Scaled {
            face: self,
            size,
            metrics: self.font.glyph_metrics(size, LocationRef::default()),
            charmap: self.font.charmap(),
        }
    }
}

struct Scaled<'a> {
    face: &'a Face,
    size: Size,
    metrics: GlyphMetrics<'static>,
    charmap: Charmap<'static>,
}

impl Scaled<'_> {
    fn glyph(&self, ch: char) -> GlyphId {
        self.charmap.map(ch).unwrap_or(GlyphId::NOTDEF)
    }

    fn advance(&self, id: GlyphId) -> f32 {
        let a = self.metrics.advance_width(id).unwrap_or(0.0);
        if a.is_finite() { a.max(0.0) } else { 0.0 }
    }

    /// The outline of `id` with its origin at (`x`, `baseline`), in frame pixels.
    fn path(&self, id: GlyphId, x: f32, baseline: f32) -> Option<GlyphPath> {
        let outline = self.face.font.outline_glyphs().get(id)?;
        let mut pen = PathPen::new(x, baseline);
        outline
            .draw(
                DrawSettings::unhinted(self.size, LocationRef::default()),
                &mut pen,
            )
            .ok()?;
        pen.finish()
    }
}

fn face(weight: u16) -> Result<&'static Face, RenderError> {
    static BOLD: OnceLock<Option<Face>> = OnceLock::new();
    static EXTRA_BOLD: OnceLock<Option<Face>> = OnceLock::new();
    let (cell, bytes) = if weight >= 800 {
        (&EXTRA_BOLD, EXTRA_BOLD_TTF)
    } else {
        (&BOLD, BOLD_TTF)
    };
    cell.get_or_init(|| Face::load(bytes))
        .as_ref()
        .ok_or(RenderError::Font)
}

#[derive(Clone, Copy)]
struct Frame {
    w: u32,
    h: u32,
}

impl Frame {
    fn new(w: u32, h: u32) -> Result<Self, RenderError> {
        if w == 0 || h == 0 || w > MAX_FRAME_SIDE || h > MAX_FRAME_SIDE {
            return Err(RenderError::Frame(w, h));
        }
        Ok(Frame { w, h })
    }
}

/// Characters that take no space and have no glyph of their own (joiners, marks,
/// bidi controls, variation selectors). Skipped instead of drawn as a box.
fn drawable(ch: char) -> bool {
    !(ch.is_control()
        || matches!(
            ch,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{1F3FB}'..='\u{1F3FF}'
                | '\u{E0100}'..='\u{E01EF}'
        ))
}

#[derive(Clone, Copy)]
struct Placed {
    id: GlyphId,
    /// Left edge of the glyph's advance, from the line start.
    x: f32,
}

#[derive(Default)]
struct Line {
    text: String,
    glyphs: Vec<Placed>,
    width: f32,
}

impl Line {
    fn push(&mut self, ch: char, font: &Scaled<'_>) {
        let id = font.glyph(ch);
        let x = self.width;
        self.width = x + font.advance(id);
        self.glyphs.push(Placed { id, x });
        self.text.push(ch);
    }

    fn push_str(&mut self, s: &str, font: &Scaled<'_>) {
        for ch in s.chars() {
            self.push(ch, font);
        }
    }
}

fn width_of(s: &str, font: &Scaled<'_>) -> f32 {
    let mut l = Line::default();
    l.push_str(s, font);
    l.width
}

/// Greedy word wrap of `text` (already cleaned) at `max_width`; lines never start or
/// end with a space. A word wider than the line breaks between characters.
fn wrap(face: &Face, text: &str, px: f32, max_width: f32) -> Vec<Line> {
    let font = face.at(px);
    let mut lines: Vec<Line> = Vec::new();
    for para in text.split('\n') {
        let mut line = Line::default();
        let words = para.split(' ').filter(|w| !w.is_empty());
        for word in words {
            let word: String = word.chars().filter(|c| drawable(*c)).collect();
            if word.is_empty() {
                continue;
            }
            let word_width = width_of(&word, &font);
            if !line.text.is_empty() {
                let mut with_space = Line::default();
                with_space.push_str(&line.text, &font);
                with_space.push(' ', &font);
                with_space.push_str(&word, &font);
                if with_space.width <= max_width {
                    line = with_space;
                    continue;
                }
                lines.push(std::mem::take(&mut line));
            }
            if word_width <= max_width {
                line.push_str(&word, &font);
                continue;
            }
            for ch in word.chars() {
                if !line.text.is_empty() {
                    let mut longer = Line::default();
                    longer.push_str(&line.text, &font);
                    longer.push(ch, &font);
                    if longer.width <= max_width {
                        line = longer;
                        continue;
                    }
                    lines.push(std::mem::take(&mut line));
                }
                line.push(ch, &font);
            }
        }
        lines.push(line);
    }
    lines
}

fn line_height_for(px: f32) -> f32 {
    (px * LINE_SPACING).round().max(1.0)
}

struct Layout {
    px: f32,
    line_height: f32,
    ascent: f32,
    /// Frame y of the block's top edge (whole pixels, ≥ 0).
    top: f32,
    lines: Vec<Line>,
    /// Frame x of each line's left edge (whole pixels).
    line_x: Vec<f32>,
    shrunk: bool,
    truncated: bool,
}

impl Layout {
    fn new(face: &Face, text: &str, style: &TextStyle, position: Position, frame: Frame) -> Self {
        let text = clean_text(text);
        let text = if style.uppercase {
            text.to_uppercase().chars().take(MAX_TEXT_CHARS).collect()
        } else {
            text
        };
        let fw = frame.w as f32;
        let fh = frame.h as f32;
        let nominal = (clamp_frac(style.size, MAX_SIZE) * fh)
            .round()
            .clamp(1.0, fh);
        let max_width = (fw * (1.0 - 2.0 * MARGIN_X)).max(1.0);
        let fits = |lines: &[Line], px: f32| {
            lines.len() <= MAX_TEXT_LINES
                && lines.iter().all(|l| l.width <= max_width)
                && line_height_for(px) * lines.len() as f32 <= fh
        };

        let mut px = nominal;
        let mut shrunk = false;
        let mut lines = wrap(face, &text, px, max_width);
        let mut steps = 0;
        while !fits(&lines, px) && px > MIN_PX && steps < MAX_SHRINK_STEPS {
            px = (px * SHRINK).max(MIN_PX);
            shrunk = true;
            steps = steps.saturating_add(1);
            lines = wrap(face, &text, px, max_width);
        }
        let truncated = lines.len() > MAX_TEXT_LINES;
        lines.truncate(MAX_TEXT_LINES);
        if lines.iter().all(|l| l.glyphs.is_empty()) {
            lines.clear();
        }

        let line_height = line_height_for(px);
        let block_h = line_height * lines.len() as f32;
        let margin_y = (fh * MARGIN_Y).round();
        let top = match position {
            Position::Top => margin_y,
            Position::Middle => ((fh - block_h) / 2.0).round(),
            Position::Bottom => fh - margin_y - block_h,
        }
        .max(0.0);
        let line_x = lines
            .iter()
            .map(|l| ((fw - l.width) / 2.0).round())
            .collect();
        Layout {
            px,
            line_height,
            ascent: face.ascent_units * px / face.height_units,
            top,
            lines,
            line_x,
            shrunk,
            truncated,
        }
    }

    /// Baseline y (frame pixels) of line `i`.
    fn baseline(&self, i: usize) -> f32 {
        (self.top + self.line_height * i as f32 + (self.line_height - self.px) / 2.0 + self.ascent)
            .round()
    }

    /// The pill behind line `i`: left, top, right, bottom in frame pixels.
    fn pill(&self, i: usize) -> Option<[f32; 4]> {
        let line = self.lines.get(i).filter(|l| !l.glyphs.is_empty())?;
        let x = *self.line_x.get(i)?;
        let pad = self.px * PILL_PAD_X;
        let gap = self.px * PILL_GAP;
        let top = self.top + self.line_height * i as f32;
        Some([
            x - pad,
            top + gap,
            x + line.width + pad,
            top + self.line_height - gap,
        ])
    }

    /// Every glyph's outline in frame pixels.
    fn paths(&self, face: &Face) -> Vec<GlyphPath> {
        let font = face.at(self.px);
        let mut out = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            let x0 = self.line_x.get(i).copied().unwrap_or(0.0);
            let y = self.baseline(i);
            for g in &line.glyphs {
                if let Some(p) = font.path(g.id, x0 + g.x, y) {
                    out.push(p);
                }
            }
        }
        out
    }
}

#[derive(Clone, Copy)]
enum Cmd {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

/// A glyph outline in frame pixels (y down) and the whole-pixel box around it.
struct GlyphPath {
    cmds: Vec<Cmd>,
    bx: Box,
}

/// Collects a glyph's path, moving its origin to (`ox`, `oy`) and flipping y.
struct PathPen {
    ox: f32,
    oy: f32,
    cmds: Vec<Cmd>,
    min: (f32, f32),
    max: (f32, f32),
}

impl PathPen {
    fn new(ox: f32, oy: f32) -> Self {
        PathPen {
            ox,
            oy,
            cmds: Vec::new(),
            min: (f32::INFINITY, f32::INFINITY),
            max: (f32::NEG_INFINITY, f32::NEG_INFINITY),
        }
    }

    fn p(&mut self, x: f32, y: f32) -> (f32, f32) {
        let x = self.ox + x;
        let y = self.oy - y;
        self.min = (self.min.0.min(x), self.min.1.min(y));
        self.max = (self.max.0.max(x), self.max.1.max(y));
        (x, y)
    }

    fn push(&mut self, c: Cmd) {
        if self.cmds.len() < MAX_GLYPH_CMDS {
            self.cmds.push(c);
        }
    }

    fn finish(self) -> Option<GlyphPath> {
        let all = [self.min.0, self.min.1, self.max.0, self.max.1];
        if self.cmds.is_empty() || all.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let bx = Box::from_f32(self.min.0, self.min.1, self.max.0, self.max.1);
        (!bx.empty()).then_some(GlyphPath {
            cmds: self.cmds,
            bx,
        })
    }
}

impl OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        self.push(Cmd::Move(x, y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        self.push(Cmd::Line(x, y));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (cx0, cy0) = self.p(cx0, cy0);
        let (x, y) = self.p(x, y);
        self.push(Cmd::Quad(cx0, cy0, x, y));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (cx0, cy0) = self.p(cx0, cy0);
        let (cx1, cy1) = self.p(cx1, cy1);
        let (x, y) = self.p(x, y);
        self.push(Cmd::Cubic(cx0, cy0, cx1, cy1, x, y));
    }

    fn close(&mut self) {
        self.push(Cmd::Close);
    }
}

/// Draws one glyph's coverage into `cov` (max blend).
fn draw_glyph(g: &GlyphPath, canvas: &Canvas, cov: &mut [u8]) {
    let Some((w, h)) = g.bx.size() else { return };
    let outside = g.bx.x1 <= canvas.bx.x0
        || g.bx.y1 <= canvas.bx.y0
        || g.bx.x0 >= canvas.bx.x1
        || g.bx.y0 >= canvas.bx.y1;
    if outside || w > MAX_FRAME_SIDE as usize || h > MAX_FRAME_SIDE as usize {
        return;
    }
    let (ox, oy) = (g.bx.x0 as f32, g.bx.y0 as f32);
    let pt = |x: f32, y: f32| point(x - ox, y - oy);
    let mut r = Rasterizer::new(w, h);
    let mut start: Option<(f32, f32)> = None;
    let mut cur = (0.0, 0.0);
    // Closes the current contour (fonts leave the last segment implicit).
    let close = |r: &mut Rasterizer, start: Option<(f32, f32)>, cur: (f32, f32)| {
        if let Some(s) = start
            && s != cur
        {
            r.draw_line(pt(cur.0, cur.1), pt(s.0, s.1));
        }
    };
    for c in &g.cmds {
        match *c {
            Cmd::Move(x, y) => {
                close(&mut r, start, cur);
                start = Some((x, y));
                cur = (x, y);
            }
            Cmd::Line(x, y) => {
                r.draw_line(pt(cur.0, cur.1), pt(x, y));
                cur = (x, y);
            }
            Cmd::Quad(cx, cy, x, y) => {
                r.draw_quad(pt(cur.0, cur.1), pt(cx, cy), pt(x, y));
                cur = (x, y);
            }
            Cmd::Cubic(cx0, cy0, cx1, cy1, x, y) => {
                r.draw_cubic(pt(cur.0, cur.1), pt(cx0, cy0), pt(cx1, cy1), pt(x, y));
                cur = (x, y);
            }
            Cmd::Close => {
                close(&mut r, start, cur);
                if let Some(s) = start {
                    cur = s;
                }
            }
        }
    }
    close(&mut r, start, cur);
    r.for_each_pixel_2d(|x, y, c| {
        let fx = g.bx.x0.saturating_add(i64::from(x));
        let fy = g.bx.y0.saturating_add(i64::from(y));
        if let Some(p) = canvas.at(fx, fy).and_then(|i| cov.get_mut(i)) {
            *p = (*p).max(to_u8(c));
        }
    });
}

/// A whole-pixel rectangle in frame coordinates, `x0..x1` × `y0..y1`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Box {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl Box {
    fn empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    /// Width and height, if not empty.
    fn size(&self) -> Option<(usize, usize)> {
        if self.empty() {
            return None;
        }
        let w = usize::try_from(self.x1.checked_sub(self.x0)?).ok()?;
        let h = usize::try_from(self.y1.checked_sub(self.y0)?).ok()?;
        Some((w, h))
    }

    fn union(&mut self, other: Box) {
        if other.empty() {
            return;
        }
        if self.empty() {
            *self = other;
            return;
        }
        self.x0 = self.x0.min(other.x0);
        self.y0 = self.y0.min(other.y0);
        self.x1 = self.x1.max(other.x1);
        self.y1 = self.y1.max(other.y1);
    }

    fn from_f32(l: f32, t: f32, r: f32, b: f32) -> Box {
        Box {
            x0: l.floor() as i64,
            y0: t.floor() as i64,
            x1: r.ceil() as i64,
            y1: b.ceil() as i64,
        }
    }
}

/// The drawn area: its frame rectangle and size (both sides ≥ 1).
struct Canvas {
    bx: Box,
    w: usize,
    h: usize,
}

impl Canvas {
    fn new(bx: Box) -> Option<Self> {
        let (w, h) = bx.size()?;
        Some(Canvas { bx, w, h })
    }

    fn len(&self) -> Result<usize, RenderError> {
        self.w
            .checked_mul(self.h)
            .ok_or(RenderError::Memory(self.w as u32, self.h as u32))
    }

    /// A zeroed single-channel buffer.
    fn mask(&self) -> Result<Vec<u8>, RenderError> {
        Ok(vec![0; self.len()?])
    }

    /// A zeroed premultiplied RGBA buffer.
    fn pixels(&self) -> Result<Vec<[u8; 4]>, RenderError> {
        Ok(vec![[0; 4]; self.len()?])
    }

    /// Index of frame pixel (`x`, `y`) in a buffer, if inside.
    fn at(&self, x: i64, y: i64) -> Option<usize> {
        if x < self.bx.x0 || y < self.bx.y0 || x >= self.bx.x1 || y >= self.bx.y1 {
            return None;
        }
        let lx = usize::try_from(x.checked_sub(self.bx.x0)?).ok()?;
        let ly = usize::try_from(y.checked_sub(self.bx.y0)?).ok()?;
        ly.checked_mul(self.w)?.checked_add(lx)
    }

    /// Index of buffer pixel (`x`, `y`), if inside.
    fn index(&self, x: usize, y: usize) -> Option<usize> {
        if x >= self.w || y >= self.h {
            return None;
        }
        y.checked_mul(self.w)?.checked_add(x)
    }
}

fn clamp_frac(v: f32, max: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, max)
    } else {
        0.0
    }
}

/// `a * b / 255` rounded, for `a` and `b` in `0..=255`.
#[allow(clippy::arithmetic_side_effects)] // 255 * 255 + 127 fits in a u32; the divisor is a constant.
fn mul255(a: u32, b: u32) -> u32 {
    ((a & 0xFF) * (b & 0xFF) + 127) / 255
}

/// Coverage 0..=1 → 0..=255.
fn to_u8(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn raster(
    face: &Face,
    layout: &Layout,
    style: &TextStyle,
    frame: Frame,
) -> Result<TextBitmap, RenderError> {
    let paths = layout.paths(face);
    let outline_r = style.outline.map_or(0.0, |(_, w)| {
        (clamp_frac(w, MAX_OUTLINE) * layout.px).min(MAX_RADIUS)
    });
    let shadow = style.shadow.map(|(c, off)| {
        let off = if off.is_finite() {
            off.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let d = (off * layout.px).round().clamp(-MAX_RADIUS, MAX_RADIUS) as i64;
        let blur = (layout.px * SHADOW_BLUR).round().clamp(1.0, MAX_RADIUS) as usize;
        (c, d, blur)
    });

    // The drawn area: glyph boxes and pills, padded for the outline and the shadow.
    let mut area = Box::default();
    for p in &paths {
        area.union(p.bx);
    }
    let pills: Vec<[f32; 4]> = match style.background {
        Some(_) => (0..layout.lines.len())
            .filter_map(|i| layout.pill(i))
            .collect(),
        None => Vec::new(),
    };
    for p in &pills {
        area.union(Box::from_f32(p[0], p[1], p[2], p[3]));
    }
    if area.empty() {
        return Ok(TextBitmap::blank(frame.w, frame.h));
    }
    let pad = (outline_r.ceil() as i64)
        .saturating_add(shadow.map_or(0, |(_, d, blur)| {
            d.abs().saturating_add((blur as i64).saturating_mul(2))
        }))
        .saturating_add(1);
    let bx = Box {
        x0: area.x0.saturating_sub(pad).max(0),
        y0: area.y0.saturating_sub(pad).max(0),
        x1: area.x1.saturating_add(pad).min(i64::from(frame.w)),
        y1: area.y1.saturating_add(pad).min(i64::from(frame.h)),
    };
    let Some(canvas) = Canvas::new(bx) else {
        return Ok(TextBitmap::blank(frame.w, frame.h));
    };

    // Glyph coverage.
    let mut cov = canvas.mask()?;
    for p in &paths {
        draw_glyph(p, &canvas, &mut cov);
    }

    let outline = (outline_r > 0.0).then(|| dilate(&cov, &canvas, outline_r));
    let shadow_mask = match shadow {
        Some((_, d, blur)) => {
            let mut m = canvas.mask()?;
            let base = outline.as_deref().unwrap_or(&cov);
            for (y, row) in base.chunks_exact(canvas.w).enumerate() {
                for (x, &v) in row.iter().enumerate() {
                    if v == 0 {
                        continue;
                    }
                    let fx = canvas.bx.x0.saturating_add(x as i64).saturating_add(d);
                    let fy = canvas.bx.y0.saturating_add(y as i64).saturating_add(d);
                    if let Some(p) = canvas.at(fx, fy).and_then(|j| m.get_mut(j)) {
                        *p = v;
                    }
                }
            }
            box_blur(&mut m, &canvas, blur)?;
            box_blur(&mut m, &canvas, blur)?;
            Some(m)
        }
        None => None,
    };

    // Compose, premultiplied.
    let mut rgba = canvas.pixels()?;
    if let Some(bg) = style.background {
        for p in &pills {
            draw_pill(&mut rgba, &canvas, *p, bg);
        }
    }
    if let (Some(m), Some((c, _, _))) = (&shadow_mask, shadow) {
        blend_mask(&mut rgba, m, c);
    }
    if let (Some(m), Some((c, _))) = (&outline, style.outline) {
        blend_mask(&mut rgba, m, c);
    }
    blend_mask(&mut rgba, &cov, style.fill);

    // Straight alpha for the PNG and the compositor.
    for p in &mut rgba {
        let a = p[3];
        for c in p.iter_mut().take(3) {
            *c = unpremultiply(*c, a);
        }
    }
    Ok(TextBitmap {
        frame_width: frame.w,
        frame_height: frame.h,
        x: bx.x0 as u32,
        y: bx.y0 as u32,
        width: canvas.w as u32,
        height: canvas.h as u32,
        rgba: rgba.into_flattened(),
    })
}

fn unpremultiply(c: u8, a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    let a = u32::from(a);
    u32::from(c)
        .saturating_mul(255)
        .saturating_add(a / 2)
        .checked_div(a)
        .unwrap_or(0)
        .min(255) as u8
}

/// `src` dilated by a disc of radius `r` (antialiased edge). Only edge pixels splat:
/// a fully covered pixel whose four neighbours are fully covered adds nothing beyond
/// what they add (for r ≥ 1 their discs cover its disc).
fn dilate(src: &[u8], canvas: &Canvas, r: f32) -> Vec<u8> {
    let rr = r.ceil().clamp(0.0, MAX_RADIUS) as usize;
    let k = rr.saturating_mul(2).saturating_add(1);
    let mut weights = vec![0u8; k.saturating_mul(k)];
    for (ky, row) in weights.chunks_exact_mut(k).enumerate() {
        for (kx, w) in row.iter_mut().enumerate() {
            let dx = kx as f32 - rr as f32;
            let dy = ky as f32 - rr as f32;
            *w = to_u8(r + 0.5 - (dx * dx + dy * dy).sqrt());
        }
    }
    let mut out = src.to_vec();
    let full = |x: usize, y: usize| canvas.index(x, y).and_then(|i| src.get(i)) == Some(&255);
    for (y, row) in src.chunks_exact(canvas.w).enumerate() {
        for (x, &v) in row.iter().enumerate() {
            if v == 0 {
                continue;
            }
            if v == 255
                && r >= 1.0
                && x > 0
                && y > 0
                && full(x.wrapping_sub(1), y)
                && full(x.saturating_add(1), y)
                && full(x, y.wrapping_sub(1))
                && full(x, y.saturating_add(1))
            {
                continue;
            }
            for (ky, krow) in weights.chunks_exact(k).enumerate() {
                let Some(yy) = y.checked_add(ky).and_then(|v| v.checked_sub(rr)) else {
                    continue;
                };
                for (kx, &wgt) in krow.iter().enumerate() {
                    if wgt == 0 {
                        continue;
                    }
                    let Some(xx) = x.checked_add(kx).and_then(|v| v.checked_sub(rr)) else {
                        continue;
                    };
                    let val = mul255(u32::from(v), u32::from(wgt)) as u8;
                    if let Some(p) = canvas.index(xx, yy).and_then(|j| out.get_mut(j)) {
                        *p = (*p).max(val);
                    }
                }
            }
        }
    }
    out
}

/// One horizontal and one vertical box blur of radius `r` (outside the buffer counts as 0).
fn box_blur(buf: &mut [u8], canvas: &Canvas, r: usize) -> Result<(), RenderError> {
    let (w, h) = (canvas.w, canvas.h);
    let n = (r.saturating_mul(2).saturating_add(1) as u32).max(1);
    let mut tmp = canvas.mask()?;
    // Horizontal: rows of `buf` → `tmp`.
    for (row_in, row_out) in buf.chunks_exact(w).zip(tmp.chunks_exact_mut(w)) {
        // The window for x is x-r..=x+r; start with 0..r in it, then slide.
        let mut sum: u32 = row_in.iter().take(r).map(|&v| u32::from(v)).sum();
        for (x, o) in row_out.iter_mut().enumerate() {
            if let Some(&v) = x.checked_add(r).and_then(|i| row_in.get(i)) {
                sum = sum.saturating_add(u32::from(v));
            }
            if let Some(&v) = x
                .checked_sub(r.saturating_add(1))
                .and_then(|i| row_in.get(i))
            {
                sum = sum.saturating_sub(u32::from(v));
            }
            *o = sum.checked_div(n).unwrap_or(0) as u8;
        }
    }
    // Vertical: columns of `tmp` → `buf`.
    for x in 0..w {
        let mut sum: u32 = (0..r.min(h))
            .filter_map(|y| canvas.index(x, y).and_then(|i| tmp.get(i)))
            .map(|&v| u32::from(v))
            .sum();
        for y in 0..h {
            if let Some(&v) = y
                .checked_add(r)
                .and_then(|yy| canvas.index(x, yy))
                .and_then(|i| tmp.get(i))
            {
                sum = sum.saturating_add(u32::from(v));
            }
            if let Some(&v) = y
                .checked_sub(r.saturating_add(1))
                .and_then(|yy| canvas.index(x, yy))
                .and_then(|i| tmp.get(i))
            {
                sum = sum.saturating_sub(u32::from(v));
            }
            if let Some(o) = canvas.index(x, y).and_then(|i| buf.get_mut(i)) {
                *o = sum.checked_div(n).unwrap_or(0) as u8;
            }
        }
    }
    Ok(())
}

/// `colour` with coverage `cov` (0..=255) over a premultiplied pixel.
fn over(p: &mut [u8; 4], colour: [u8; 4], cov: u8) {
    if cov == 0 || colour[3] == 0 {
        return;
    }
    let sa = mul255(u32::from(colour[3]), u32::from(cov));
    let inv = 255u32.saturating_sub(sa);
    for (i, c) in p.iter_mut().enumerate() {
        let s = if i == 3 {
            sa
        } else {
            mul255(u32::from(colour.get(i).copied().unwrap_or(0)), sa)
        };
        *c = s.saturating_add(mul255(u32::from(*c), inv)).min(255) as u8;
    }
}

fn blend_mask(rgba: &mut [[u8; 4]], mask: &[u8], colour: [u8; 4]) {
    for (p, &m) in rgba.iter_mut().zip(mask) {
        over(p, colour, m);
    }
}

/// A rounded rectangle (radius = half its height) with an antialiased edge.
fn draw_pill(rgba: &mut [[u8; 4]], canvas: &Canvas, [l, t, r, b]: [f32; 4], colour: [u8; 4]) {
    if !(l.is_finite() && t.is_finite() && r.is_finite() && b.is_finite()) || r <= l || b <= t {
        return;
    }
    let radius = ((b - t) / 2.0).min((r - l) / 2.0);
    let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
    let (hx, hy) = ((r - l) / 2.0 - radius, (b - t) / 2.0 - radius);
    let x0 = (l.floor() as i64).max(canvas.bx.x0);
    let x1 = (r.ceil() as i64).min(canvas.bx.x1);
    let y0 = (t.floor() as i64).max(canvas.bx.y0);
    let y1 = (b.ceil() as i64).min(canvas.bx.y1);
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = ((x as f32 + 0.5 - cx).abs() - hx).max(0.0);
            let dy = ((y as f32 + 0.5 - cy).abs() - hy).max(0.0);
            let d = (dx * dx + dy * dy).sqrt() - radius;
            if let Some(p) = canvas.at(x, y).and_then(|i| rgba.get_mut(i)) {
                over(p, colour, to_u8(0.5 - d));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::PRESETS;
    use super::*;

    fn bold() -> &'static TextStyle {
        preset("bold").unwrap()
    }

    #[test]
    fn fonts_load() {
        assert!(face(700).is_ok());
        assert!(face(800).is_ok());
        assert!(face(0).is_ok());
        let f = face(800).unwrap();
        assert!(f.upem > 0.0 && f.ascent_units > 0.0 && f.height_units > f.ascent_units);
    }

    #[test]
    fn empty_text_is_blank() {
        let b = render("", bold(), Position::Middle, 640, 360).unwrap();
        assert!(b.is_blank());
        assert_eq!((b.width, b.height), (1, 1));
        let b = render(" \n\t ", bold(), Position::Middle, 640, 360).unwrap();
        assert!(b.is_blank());
    }

    #[test]
    fn bad_frames_are_errors() {
        assert_eq!(
            render("x", bold(), Position::Top, 0, 10),
            Err(RenderError::Frame(0, 10))
        );
        assert_eq!(
            render("x", bold(), Position::Top, 10, 0),
            Err(RenderError::Frame(10, 0))
        );
        assert!(render("x", bold(), Position::Top, MAX_FRAME_SIDE + 1, 10).is_err());
    }

    #[test]
    fn wraps_shrinks_and_cuts() {
        let m = measure("one two", bold(), Position::Middle, 1920, 1080).unwrap();
        assert_eq!(m.lines, vec!["one two"]);
        assert!(!m.shrunk && !m.truncated);
        let long = "word ".repeat(40);
        let m = measure(&long, bold(), Position::Middle, 1920, 1080).unwrap();
        assert!(m.lines.len() <= MAX_TEXT_LINES);
        assert!(m.shrunk, "{m:?}");
        assert!(m.font_px < 0.09 * 1080.0);
        for l in &m.lines {
            assert!(!l.starts_with(' ') && !l.ends_with(' '), "{l:?}");
        }
        let endless = "x".repeat(200);
        let m = measure(&endless, bold(), Position::Middle, 64, 64).unwrap();
        assert_eq!(m.font_px, MIN_PX);
        assert!(m.truncated);
        assert_eq!(m.lines.len(), MAX_TEXT_LINES);
    }

    #[test]
    fn wrapping_keeps_every_character() {
        let text = "supercalifragilisticexpialidocious and a few more words to wrap";
        let m = measure(text, bold(), Position::Middle, 300, 300).unwrap();
        let joined: String = m.lines.join("");
        let expected: String = text.chars().filter(|c| *c != ' ').collect();
        let got: String = joined.chars().filter(|c| *c != ' ').collect();
        assert!(expected.starts_with(&got) && !got.is_empty(), "{m:?}");
    }

    #[test]
    fn wraps_at_spaces_without_shrinking() {
        let text = "the quick brown fox jumps over the lazy dog";
        let m = measure(text, bold(), Position::Middle, 640, 360).unwrap();
        assert!(!m.shrunk && !m.truncated, "{m:?}");
        assert_eq!(m.lines.len(), 2, "{m:?}");
        assert_eq!(m.lines.join(" "), text);
    }

    #[test]
    fn empty_lines_get_no_pill_and_blank_text_draws_nothing() {
        let pill = preset("pill").unwrap();
        let with_gap = render("gg\n\nwp", pill, Position::Top, 640, 360).unwrap();
        let no_gap = render("gg\nwp", pill, Position::Top, 640, 360).unwrap();
        // Same ink, just further apart: not wider, and no third pill's worth of alpha.
        assert_eq!(with_gap.width, no_gap.width);
        let alpha = |b: &TextBitmap| {
            b.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| u64::from(p[3]))
                .sum::<u64>()
        };
        assert!(
            alpha(&with_gap) <= alpha(&no_gap) + 255,
            "{} vs {}",
            alpha(&with_gap),
            alpha(&no_gap)
        );
        assert!(
            render("\u{200B}\u{FE0F}", pill, Position::Middle, 640, 360)
                .unwrap()
                .is_blank()
        );
        assert!(
            measure("\u{200B}", pill, Position::Middle, 640, 360)
                .unwrap()
                .lines
                .is_empty()
        );
    }

    #[test]
    fn uppercase_never_exceeds_the_character_cap() {
        let m = measure(
            &"ß".repeat(300),
            preset("title").unwrap(),
            Position::Top,
            4096,
            4096,
        )
        .unwrap();
        assert!(
            m.lines.iter().map(|l| l.chars().count()).sum::<usize>() <= MAX_TEXT_CHARS,
            "{m:?}"
        );
    }

    #[test]
    fn effects_are_really_drawn() {
        let mut style = *bold();
        style.fill = [255, 255, 255, 255];
        style.outline = Some(([255, 0, 0, 255], 0.12));
        style.shadow = Some(([0, 0, 255, 255], 0.1));
        let b = render("I", &style, Position::Middle, 640, 360).unwrap();
        let px = b.rgba.as_chunks::<4>().0;
        assert!(px.contains(&[255, 255, 255, 255]), "fill");
        assert!(px.contains(&[255, 0, 0, 255]), "outline");
        assert!(
            px.iter().any(|p| p[2] == 255 && p[0] == 0 && p[3] > 0),
            "shadow"
        );
        style.shadow = None;
        let no_shadow = render("I", &style, Position::Middle, 640, 360).unwrap();
        assert!(no_shadow.width < b.width || no_shadow.height < b.height);
        let pill = render("I", preset("pill").unwrap(), Position::Bottom, 640, 360).unwrap();
        assert!(
            pill.rgba.as_chunks::<4>().0.contains(&[0, 0, 0, 178]),
            "pill"
        );
    }

    #[test]
    fn hostile_style_values_are_clamped() {
        let mut bad = *bold();
        for size in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 1.0, 0.0] {
            bad.size = size;
            bad.outline = Some(([0, 0, 0, 255], size));
            bad.shadow = Some(([0, 0, 0, 255], size));
            let b = render("Wg", &bad, Position::Middle, 1920, 1080).unwrap();
            assert!(b.x + b.width <= 1920 && b.y + b.height <= 1080, "{size}");
            let m = measure("Wg", &bad, Position::Middle, 1920, 1080).unwrap();
            assert!(
                m.font_px >= 1.0 && m.font_px <= MAX_SIZE * 1080.0 + 0.5,
                "{size} {m:?}"
            );
        }
        // A tall block has to fit the frame too.
        bad.size = 0.5;
        bad.outline = None;
        bad.shadow = None;
        let m = measure("a\nb\nc", &bad, Position::Middle, 400, 400).unwrap();
        assert!(m.shrunk && f64::from(m.line_height) * 3.0 <= 400.0, "{m:?}");
    }

    #[test]
    fn box_blur_is_a_true_sliding_window() {
        let canvas = Canvas::new(Box {
            x0: 0,
            y0: 0,
            x1: 8,
            y1: 1,
        })
        .unwrap();
        let mut row = vec![255u8, 255, 255, 0, 0, 0, 0, 0];
        box_blur(&mut row, &canvas, 1).unwrap();
        // Horizontal pass [170, 255, 170, 85, 0..], then the vertical pass over one row
        // divides by 3 again. (The unprimed window gave [56, 85, 56, 28] / [28, 56, 28, 0].)
        assert_eq!(row, vec![56, 85, 56, 28, 0, 0, 0, 0]);
        let canvas = Canvas::new(Box {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 4,
        })
        .unwrap();
        let mut col = vec![255u8, 0, 0, 0];
        box_blur(&mut col, &canvas, 1).unwrap();
        assert_eq!(col, vec![28, 28, 0, 0]);
    }

    #[test]
    fn uppercase_presets_render_uppercase() {
        let m = measure(
            "gg wp",
            preset("caption").unwrap(),
            Position::Bottom,
            640,
            360,
        )
        .unwrap();
        assert_eq!(m.lines, vec!["GG WP"]);
    }

    #[test]
    fn positions_stack_top_to_bottom() {
        let tops: Vec<u32> = [Position::Top, Position::Middle, Position::Bottom]
            .iter()
            .map(|p| measure("hi", bold(), *p, 640, 360).unwrap().top)
            .collect();
        assert!(tops[0] < tops[1] && tops[1] < tops[2], "{tops:?}");
    }

    #[test]
    fn missing_glyphs_draw_a_box_and_joiners_are_skipped() {
        let b = render("🎮", bold(), Position::Middle, 640, 360).unwrap();
        assert!(!b.is_blank());
        let m = measure("a\u{200D}b\u{FE0F}", bold(), Position::Middle, 640, 360).unwrap();
        assert_eq!(m.lines, vec!["ab"]);
    }

    #[test]
    fn text_sits_where_the_layout_says() {
        // A one-line "I" in the middle: its ink is inside the line box and centred.
        let m = measure("I", bold(), Position::Middle, 640, 360).unwrap();
        let b = render("I", bold(), Position::Middle, 640, 360).unwrap();
        assert!(
            b.y >= m.top.saturating_sub(16) && b.y + b.height <= m.top + m.line_height + 16,
            "{m:?} {b:?}"
        );
        let centre = f64::from(b.x) + f64::from(b.width) / 2.0;
        assert!((centre - 320.0).abs() < 4.0, "{centre}");
    }

    #[test]
    fn bitmap_is_inside_the_frame_and_png_round_trips() {
        for s in PRESETS {
            let b = render("Clutch 1v4\nnice", s, s.position, 1280, 720).unwrap();
            assert!(b.x + b.width <= 1280 && b.y + b.height <= 720, "{s:?}");
            assert_eq!(b.rgba.len(), (b.width * b.height * 4) as usize);
            assert!(!b.is_blank());
            let png = b.png().unwrap();
            let mut d = png::Decoder::new(std::io::Cursor::new(png.as_slice()))
                .read_info()
                .unwrap();
            assert_eq!((d.info().width, d.info().height), (b.width, b.height));
            let mut buf = vec![0; d.output_buffer_size().unwrap()];
            let info = d.next_frame(&mut buf).unwrap();
            assert_eq!(info.color_type, png::ColorType::Rgba);
            assert_eq!(&buf[..info.buffer_size()], &b.rgba[..]);
        }
    }

    #[test]
    fn cache_keys_are_stable_and_distinct() {
        let k = cache_key("gg", bold(), Position::Middle, 1920, 1080);
        // Changing the recipe (RENDER_VERSION, font, style fields) must be deliberate.
        assert_eq!(RENDER_VERSION, 1);
        assert_eq!(k, "d64c926611ec5eee");
        assert_eq!(k, cache_key("gg", bold(), Position::Middle, 1920, 1080));
        assert_eq!(k, cache_key("  gg  ", bold(), Position::Middle, 1920, 1080));
        assert_ne!(k, cache_key("gg!", bold(), Position::Middle, 1920, 1080));
        assert_ne!(k, cache_key("gg", bold(), Position::Top, 1920, 1080));
        assert_ne!(k, cache_key("gg", bold(), Position::Middle, 1080, 1920));
        assert_ne!(
            k,
            cache_key("gg", preset("pill").unwrap(), Position::Middle, 1920, 1080)
        );
    }

    #[test]
    fn over_is_bounded() {
        let mut p = [0u8, 0, 0, 0];
        over(&mut p, [255, 255, 255, 255], 255);
        assert_eq!(p, [255, 255, 255, 255]);
        over(&mut p, [0, 0, 0, 255], 128);
        assert_eq!(p[3], 255);
        let mut q = [0u8, 0, 0, 0];
        over(&mut q, [255, 0, 0, 128], 255);
        assert_eq!(q, [128, 0, 0, 128]);
    }
}
