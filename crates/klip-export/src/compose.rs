//! Pixel and sample work done on the CPU between decoding and encoding: text bitmaps
//! blended onto BGRA frames, audio tracks mixed with the editor's volumes.

use klip_edit::TextBitmap;

/// `bitmap` (straight-alpha RGBA, placed at its own `x`/`y`) over a BGRA frame of
/// `width` × `height` with `stride` bytes per row. Rows and columns outside the frame
/// are skipped; nothing can panic.
pub fn blend_rgba_over_bgra(
    frame: &mut [u8],
    width: u32,
    height: u32,
    stride: usize,
    bitmap: &TextBitmap,
) {
    let bw = bitmap.width as usize;
    let rows = bitmap.rgba.chunks_exact(bw.saturating_mul(4));
    for (by, row) in rows.enumerate() {
        let Some(y) = (bitmap.y as usize).checked_add(by) else {
            break;
        };
        if y >= height as usize {
            break;
        }
        let Some(dst_row) = y.checked_mul(stride).and_then(|o| frame.get_mut(o..)) else {
            break;
        };
        for (bx, px) in row.as_chunks::<4>().0.iter().enumerate() {
            let a = u32::from(px[3]);
            if a == 0 {
                continue;
            }
            let Some(x) = (bitmap.x as usize).checked_add(bx) else {
                break;
            };
            if x >= width as usize {
                break;
            }
            let Some(d) = x
                .checked_mul(4)
                .and_then(|o| dst_row.get_mut(o..o.saturating_add(4)))
            else {
                break;
            };
            let Some(d) = <&mut [u8; 4]>::try_from(d).ok() else {
                break;
            };
            let inv = 255u32.saturating_sub(a);
            // Frame is BGRA, bitmap is RGBA.
            d[0] = lerp(px[2], d[0], a, inv);
            d[1] = lerp(px[1], d[1], a, inv);
            d[2] = lerp(px[0], d[2], a, inv);
            d[3] = 255;
        }
    }
}

fn lerp(src: u8, dst: u8, a: u32, inv: u32) -> u8 {
    let v = u32::from(src)
        .saturating_mul(a)
        .saturating_add(u32::from(dst).saturating_mul(inv))
        .saturating_add(127)
        .checked_div(255)
        .unwrap_or(0);
    u8::try_from(v.min(255)).unwrap_or(255)
}

/// One decoded audio track and the slider that applies to it.
pub struct MixInput<'a> {
    pub samples: &'a [i16],
    /// 0 = muted, 1 = as recorded.
    pub volume: f32,
}

/// Sum of the inputs, each scaled by its volume, clamped to 16 bits. The result is as
/// long as the longest input (shorter ones count as silence past their end).
pub fn mix(inputs: &[MixInput<'_>]) -> Vec<i16> {
    let len = inputs.iter().map(|i| i.samples.len()).max().unwrap_or(0);
    let mut out = vec![0i16; len];
    for input in inputs {
        let gain = if input.volume.is_finite() {
            input.volume.clamp(0.0, 4.0)
        } else {
            1.0
        };
        if gain == 0.0 {
            continue;
        }
        for (o, &s) in out.iter_mut().zip(input.samples) {
            let v = f32::from(*o) + f32::from(s) * gain;
            *o = v.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_is_bounded_and_clipped() {
        let mut frame = vec![0u8; 4 * 4 * 4];
        let bitmap = TextBitmap {
            frame_width: 4,
            frame_height: 4,
            x: 2,
            y: 3,
            width: 4,
            height: 4,
            rgba: [255, 0, 0, 255].repeat(16),
        };
        blend_rgba_over_bgra(&mut frame, 4, 4, 16, &bitmap);
        // Only (2,3) and (3,3) are inside: red → BGRA 0,0,255,255.
        assert_eq!(
            &frame[3 * 16 + 8..3 * 16 + 16],
            &[0, 0, 255, 255, 0, 0, 255, 255]
        );
        assert!(frame[..3 * 16].iter().all(|b| *b == 0));
        // Half alpha over grey.
        let mut frame = vec![100u8; 4];
        let bitmap = TextBitmap {
            frame_width: 1,
            frame_height: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 128],
        };
        blend_rgba_over_bgra(&mut frame, 1, 1, 4, &bitmap);
        assert_eq!(frame, vec![50, 50, 50, 255]);
        // A bitmap wider than its own data never panics.
        let bad = TextBitmap {
            frame_width: 1,
            frame_height: 1,
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
            rgba: vec![1, 2, 3, 4],
        };
        blend_rgba_over_bgra(&mut frame, 1, 1, 4, &bad);
    }

    #[test]
    fn mix_scales_sums_and_clamps() {
        let a = [1000i16, -1000, 30000];
        let b = [1000i16, 1000];
        let out = mix(&[
            MixInput {
                samples: &a,
                volume: 0.5,
            },
            MixInput {
                samples: &b,
                volume: 2.0,
            },
        ]);
        assert_eq!(out, vec![2500, 1500, 15000]);
        let loud = mix(&[MixInput {
            samples: &a,
            volume: 4.0,
        }]);
        assert_eq!(loud[2], i16::MAX);
        assert!(mix(&[]).is_empty());
        let nan = mix(&[MixInput {
            samples: &b,
            volume: f32::NAN,
        }]);
        assert_eq!(nan, vec![1000, 1000]);
    }
}
