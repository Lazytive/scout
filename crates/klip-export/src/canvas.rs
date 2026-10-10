//! One output frame's picture, composed in this process (ADR 0018, M9.3). For each
//! layer, bottom first, only the part that lands on the canvas
//! ([`klip_edit::compose::visible`]) is cropped and scaled by a `GstVideoConverter`:
//! straight into the canvas for opaque video at full opacity, else into a scratch
//! buffer that is blended over the canvas with its alpha × the layer's opacity. Then the
//! colour grade ([`klip_edit::advanced::Colour::matrix`]) runs over the whole frame.
//! Text goes on top afterwards (the caller blends the cue bitmaps).
//!
//! Everything is deterministic: the same frames and numbers give the same pixels, which
//! is what lets the preview and `editor.frame` be compared with it.

use std::collections::HashMap;

use gstreamer as gst;
use gstreamer_video as gst_video;
use klip_edit::Rect;
use klip_edit::compose::{Placement, visible};

use crate::ExportError;
use crate::source::Frame;

/// Converters kept for reuse (a zoom animation makes a new one per frame).
const MAX_CONVERTERS: usize = 24;
/// Threads a converter or a pass over the canvas uses at most (a converter made per
/// frame of a zoom animation starts its own).
const MAX_THREADS: usize = 8;
/// Below this many pixels a pass over the canvas runs on one thread.
const PARALLEL_PIXELS: usize = 1 << 17;

/// One picture to draw.
pub struct Item<'a> {
    pub frame: &'a Frame,
    pub placement: Placement,
    /// 0…1.
    pub opacity: f32,
    /// The frame has no transparency (decoded video): it may be converted straight
    /// into the canvas.
    pub opaque: bool,
}

/// What a converter was made for: input size, source rectangle, destination size and
/// position (`direct`: into the canvas, else into a scratch buffer at 0,0).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Key {
    in_w: u32,
    in_h: u32,
    src: (u32, u32, u32, u32),
    dst: (u32, u32, u32, u32),
    direct: bool,
}

pub struct Canvas {
    width: u32,
    height: u32,
    info: gst_video::VideoInfo,
    converters: HashMap<Key, gst_video::VideoConverter>,
    threads: usize,
}

fn rect_tuple(r: Rect) -> (u32, u32, u32, u32) {
    (r.x, r.y, r.width, r.height)
}

fn bgra(width: u32, height: u32) -> Result<gst_video::VideoInfo, ExportError> {
    gst_video::VideoInfo::builder(gst_video::VideoFormat::Bgra, width, height)
        .fps(gst::Fraction::new(0, 1))
        .build()
        .map_err(|_| ExportError::Plan(format!("bad picture size {width}x{height}")))
}

fn to_i32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// Run `f(row_index, row)` over the `rows` rows of `data` (`stride` bytes apart), on
/// several threads when the area is large.
fn par_rows(
    data: &mut [u8],
    stride: usize,
    rows: usize,
    pixels: usize,
    threads: usize,
    f: impl Fn(usize, &mut [u8]) + Sync,
) {
    let stride = stride.max(1);
    let threads = if pixels >= PARALLEL_PIXELS {
        threads.max(1)
    } else {
        1
    };
    let per = rows.div_ceil(threads).max(1);
    if threads == 1 {
        for (y, row) in data.chunks_mut(stride).take(rows).enumerate() {
            f(y, row);
        }
        return;
    }
    let f = &f;
    std::thread::scope(|s| {
        for (band, chunk) in data
            .chunks_mut(stride.saturating_mul(per))
            .take(threads)
            .enumerate()
        {
            s.spawn(move || {
                let first = band.saturating_mul(per);
                for (i, row) in chunk.chunks_mut(stride).enumerate() {
                    let y = first.saturating_add(i);
                    if y >= rows {
                        break;
                    }
                    f(y, row);
                }
            });
        }
    });
}

/// `(src·a + dst·(255−a) + 127) / 255` per channel, `a` = source alpha × `opacity`.
fn blend_row(dst: &mut [u8], src: &[u8], opacity: u32) {
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        let [sb, sg, sr, sa] = *s;
        let sd = [sb, sg, sr];
        let a = u32::from(sa)
            .saturating_mul(opacity)
            .saturating_add(127)
            .checked_div(255)
            .unwrap_or(0);
        if a == 0 {
            continue;
        }
        let inv = 255u32.saturating_sub(a);
        for (dc, &sc) in d.iter_mut().zip(&sd) {
            let v = u32::from(sc)
                .saturating_mul(a)
                .saturating_add(u32::from(*dc).saturating_mul(inv))
                .saturating_add(127)
                .checked_div(255)
                .unwrap_or(0);
            *dc = u8::try_from(v.min(255)).unwrap_or(255);
        }
    }
}

/// The colour matrix over BGRA pixels (alpha untouched).
fn grade_row(row: &mut [u8], m: &[[f32; 4]; 3]) {
    let [r_row, g_row, b_row] = m;
    let apply = |c: &[f32; 4], r: f32, g: f32, b: f32| -> u8 {
        let v = c[0] * r + c[1] * g + c[2] * b + c[3];
        (v + 0.5).floor().clamp(0.0, 255.0) as u8
    };
    for px in row.as_chunks_mut::<4>().0 {
        {
            let [b, g, r, _] = px;
            let (fr, fg, fb) = (f32::from(*r), f32::from(*g), f32::from(*b));
            let (nr, ng, nb) = (
                apply(r_row, fr, fg, fb),
                apply(g_row, fr, fg, fb),
                apply(b_row, fr, fg, fb),
            );
            (*r, *g, *b) = (nr, ng, nb);
        }
    }
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Result<Canvas, ExportError> {
        Ok(Canvas {
            width,
            height,
            info: bgra(width, height)?,
            converters: HashMap::new(),
            threads: std::thread::available_parallelism()
                .map_or(4, usize::from)
                .min(MAX_THREADS),
        })
    }

    fn converter(
        &mut self,
        key: Key,
        out_info: &gst_video::VideoInfo,
    ) -> Result<&gst_video::VideoConverter, ExportError> {
        if !self.converters.contains_key(&key) {
            if self.converters.len() >= MAX_CONVERTERS {
                self.converters.clear();
            }
            let in_info = bgra(key.in_w, key.in_h)?;
            let mut cfg = gst_video::VideoConverterConfig::new();
            cfg.set_src_x(to_i32(key.src.0));
            cfg.set_src_y(to_i32(key.src.1));
            cfg.set_src_width(Some(to_i32(key.src.2)));
            cfg.set_src_height(Some(to_i32(key.src.3)));
            cfg.set_dest_x(to_i32(key.dst.0));
            cfg.set_dest_y(to_i32(key.dst.1));
            cfg.set_dest_width(Some(to_i32(key.dst.2)));
            cfg.set_dest_height(Some(to_i32(key.dst.3)));
            cfg.set_fill_border(false);
            cfg.set_alpha_mode(gst_video::VideoAlphaMode::Copy);
            cfg.set_threads(u32::try_from(self.threads).unwrap_or(4));
            let conv = gst_video::VideoConverter::new(&in_info, out_info, Some(cfg))
                .map_err(|e| ExportError::Gst(format!("video converter: {e}")))?;
            self.converters.insert(key, conv);
        }
        self.converters
            .get(&key)
            .ok_or_else(|| ExportError::Gst("video converter missing".into()))
    }

    /// Whether `item` alone, as it is, is the whole frame (nothing to compose).
    pub fn is_passthrough(&self, item: &Item<'_>) -> bool {
        item.opaque
            && item.opacity >= 1.0
            && item.placement
                == Placement {
                    x: 0,
                    y: 0,
                    width: self.width,
                    height: self.height,
                }
            && item.frame.info.width() == self.width
            && item.frame.info.height() == self.height
    }

    /// Compose `items` (bottom first) and apply `grade` into a new BGRA frame.
    pub fn compose(
        &mut self,
        items: &[Item<'_>],
        grade: Option<&[[f32; 4]; 3]>,
    ) -> Result<gst::Buffer, ExportError> {
        let (w, h) = (self.width, self.height);
        let size = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        let mut buffer =
            gst::Buffer::with_size(size).map_err(|_| ExportError::Gst("out of memory".into()))?;
        let full = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let plan: Vec<(&Item<'_>, Rect, Rect)> = items
            .iter()
            .filter(|i| i.opacity > 0.0 && i.opacity.is_finite())
            .filter_map(|i| {
                visible(
                    i.placement,
                    i.frame.info.width(),
                    i.frame.info.height(),
                    w,
                    h,
                )
                .map(|(src, dst)| (i, src, dst))
            })
            .collect();
        let covered = plan
            .first()
            .is_some_and(|(i, _, dst)| i.opaque && i.opacity >= 1.0 && *dst == full);
        let out_info = self.info.clone();
        let threads = self.threads;
        {
            let buf = buffer
                .get_mut()
                .ok_or_else(|| ExportError::Gst("canvas buffer is shared".into()))?;
            if !covered {
                let mut map = buf
                    .map_writable()
                    .map_err(|_| ExportError::Gst("canvas cannot be written".into()))?;
                for px in map.as_mut_slice().as_chunks_mut::<4>().0 {
                    *px = [0, 0, 0, 255];
                }
            }
            for (item, src, dst) in &plan {
                self.draw(buf, &out_info, item, *src, *dst)?;
            }
            if let Some(m) = grade {
                let mut map = buf
                    .map_writable()
                    .map_err(|_| ExportError::Gst("canvas cannot be written".into()))?;
                let stride = (w as usize).saturating_mul(4);
                let pixels = (w as usize).saturating_mul(h as usize);
                par_rows(
                    map.as_mut_slice(),
                    stride,
                    h as usize,
                    pixels,
                    threads,
                    |_, row| {
                        grade_row(row, m);
                    },
                );
            }
        }
        Ok(buffer)
    }

    fn draw(
        &mut self,
        canvas: &mut gst::BufferRef,
        out_info: &gst_video::VideoInfo,
        item: &Item<'_>,
        src: Rect,
        dst: Rect,
    ) -> Result<(), ExportError> {
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(
            item.frame.buffer.as_ref(),
            &item.frame.info,
        )
        .map_err(|_| ExportError::Source("a frame could not be read".into()))?;
        let direct = item.opaque && item.opacity >= 1.0;
        let key = Key {
            in_w: item.frame.info.width(),
            in_h: item.frame.info.height(),
            src: rect_tuple(src),
            dst: if direct {
                rect_tuple(dst)
            } else {
                (0, 0, dst.width, dst.height)
            },
            direct,
        };
        if direct {
            let conv = self.converter(key, out_info)?;
            let mut out = gst_video::VideoFrameRef::from_buffer_ref_writable(canvas, out_info)
                .map_err(|_| ExportError::Gst("canvas cannot be written".into()))?;
            conv.frame_ref(&frame, &mut out);
            return Ok(());
        }
        // Into a scratch picture of the destination's size, then blended.
        let scratch_info = bgra(dst.width, dst.height)?;
        let mut scratch = gst::Buffer::with_size(scratch_info.size())
            .map_err(|_| ExportError::Gst("out of memory".into()))?;
        {
            let conv = self.converter(key, &scratch_info)?;
            let sref = scratch
                .get_mut()
                .ok_or_else(|| ExportError::Gst("scratch buffer is shared".into()))?;
            let mut out = gst_video::VideoFrameRef::from_buffer_ref_writable(sref, &scratch_info)
                .map_err(|_| ExportError::Gst("scratch cannot be written".into()))?;
            conv.frame_ref(&frame, &mut out);
        }
        let s_stride =
            usize::try_from(scratch_info.stride().first().copied().unwrap_or(0)).unwrap_or(0);
        let smap = scratch
            .map_readable()
            .map_err(|_| ExportError::Gst("scratch cannot be read".into()))?;
        let sdata = smap.as_slice();
        let mut cmap = canvas
            .map_writable()
            .map_err(|_| ExportError::Gst("canvas cannot be written".into()))?;
        let c_stride = (self.width as usize).saturating_mul(4);
        let x0 = (dst.x as usize).saturating_mul(4);
        let row_bytes = (dst.width as usize).saturating_mul(4);
        let opacity = (item.opacity.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u32;
        let first = (dst.y as usize).saturating_mul(c_stride);
        let rows = dst.height as usize;
        let region = cmap
            .as_mut_slice()
            .get_mut(first..)
            .ok_or_else(|| ExportError::Gst("canvas too small".into()))?;
        let pixels = (dst.width as usize).saturating_mul(rows);
        par_rows(region, c_stride, rows, pixels, self.threads, |y, row| {
            let srow = y.saturating_mul(s_stride);
            if let (Some(d), Some(s)) = (
                row.get_mut(x0..x0.saturating_add(row_bytes)),
                sdata.get(srow..srow.saturating_add(row_bytes)),
            ) {
                blend_row(d, s, opacity);
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Frame {
        gst::init().unwrap();
        let info = bgra(w, h).unwrap();
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&f(x, y));
            }
        }
        Frame {
            pts_ms: 0,
            buffer: gst::Buffer::from_mut_slice(data),
            info,
        }
    }

    fn pixels(b: &gst::Buffer) -> Vec<u8> {
        b.map_readable().unwrap().as_slice().to_vec()
    }

    fn px(data: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = (y.saturating_mul(w).saturating_add(x) as usize).saturating_mul(4);
        data[i..].first_chunk::<4>().copied().unwrap()
    }

    fn full(w: u32, h: u32) -> Placement {
        Placement {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    #[test]
    fn opaque_video_fills_and_crops() {
        // Left half red, right half blue.
        let f = frame(64, 32, |x, _| {
            if x < 32 {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            }
        });
        let mut c = Canvas::new(64, 32).unwrap();
        let item = Item {
            frame: &f,
            placement: full(64, 32),
            opacity: 1.0,
            opaque: true,
        };
        assert!(c.is_passthrough(&item));
        let out = pixels(&c.compose(&[item], None).unwrap());
        assert_eq!(px(&out, 64, 0, 0), [0, 0, 255, 255]);
        assert_eq!(px(&out, 64, 63, 31), [255, 0, 0, 255]);
        // Zoom 2 about the centre: the canvas shows source x 16..48; its left quarter
        // red, right quarter blue, split in the middle.
        let z = klip_edit::compose::place(
            klip_edit::Transform {
                zoom: 2.0,
                x: 0.0,
                y: 0.0,
            },
            64,
            32,
            64,
            32,
        );
        let item = Item {
            frame: &f,
            placement: z,
            opacity: 1.0,
            opaque: true,
        };
        assert!(!c.is_passthrough(&item));
        let out = pixels(&c.compose(&[item], None).unwrap());
        assert_eq!(px(&out, 64, 2, 16), [0, 0, 255, 255]);
        assert_eq!(px(&out, 64, 28, 16), [0, 0, 255, 255]);
        assert_eq!(px(&out, 64, 36, 16), [255, 0, 0, 255]);
        assert_eq!(px(&out, 64, 61, 16), [255, 0, 0, 255]);
    }

    #[test]
    fn a_shifted_layer_leaves_black_and_layers_stack() {
        let red = frame(32, 32, |_, _| [0, 0, 255, 255]);
        let mut c = Canvas::new(32, 32).unwrap();
        let shifted = Placement {
            x: 16,
            ..full(32, 32)
        };
        let out = pixels(
            &c.compose(
                &[Item {
                    frame: &red,
                    placement: shifted,
                    opacity: 1.0,
                    opaque: true,
                }],
                None,
            )
            .unwrap(),
        );
        assert_eq!(px(&out, 32, 8, 8), [0, 0, 0, 255], "black where nothing is");
        assert_eq!(px(&out, 32, 24, 8), [0, 0, 255, 255]);
        // A half-transparent green sticker over red, then a 50 % opaque white layer.
        let green = frame(8, 8, |_, _| [0, 255, 0, 128]);
        let white = frame(32, 32, |_, _| [255, 255, 255, 255]);
        let sticker = Placement {
            x: 4,
            y: 4,
            width: 8,
            height: 8,
        };
        let out = pixels(
            &c.compose(
                &[
                    Item {
                        frame: &red,
                        placement: full(32, 32),
                        opacity: 1.0,
                        opaque: true,
                    },
                    Item {
                        frame: &green,
                        placement: sticker,
                        opacity: 1.0,
                        opaque: false,
                    },
                    Item {
                        frame: &white,
                        placement: full(32, 32),
                        opacity: 0.5,
                        opaque: true,
                    },
                ],
                None,
            )
            .unwrap(),
        );
        // Red under green at alpha 128: (0, 128, 127); then half white.
        assert_eq!(px(&out, 32, 6, 6), [128, 192, 191, 255]);
        assert_eq!(px(&out, 32, 20, 20), [128, 128, 255, 255]);
    }

    #[test]
    fn the_grade_runs_over_every_pixel() {
        let grey = frame(600, 400, |_, _| [100, 100, 100, 255]);
        let mut c = Canvas::new(600, 400).unwrap();
        let m = klip_edit::advanced::Colour {
            brightness: 0.1,
            contrast: 2.0,
            saturation: 1.0,
        }
        .matrix();
        let out = pixels(
            &c.compose(
                &[Item {
                    frame: &grey,
                    placement: full(600, 400),
                    opacity: 1.0,
                    opaque: true,
                }],
                Some(&m),
            )
            .unwrap(),
        );
        // (100 - 128) * 2 + 128 + 25.5 = 97.5 → 98.
        assert!(
            out.as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [98, 98, 98, 255])
        );
    }

    #[test]
    fn nothing_visible_is_black() {
        let red = frame(4, 4, |_, _| [0, 0, 255, 255]);
        let mut c = Canvas::new(16, 16).unwrap();
        let out = pixels(
            &c.compose(
                &[Item {
                    frame: &red,
                    placement: Placement {
                        x: 100,
                        ..full(4, 4)
                    },
                    opacity: 1.0,
                    opaque: true,
                }],
                None,
            )
            .unwrap(),
        );
        assert!(out.as_chunks::<4>().0.iter().all(|p| *p == [0, 0, 0, 255]));
        assert_eq!(pixels(&c.compose(&[], None).unwrap()).len(), 16 * 16 * 4);
    }
}
