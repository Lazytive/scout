//! Machine-readable frame counter drawn on every test frame.
//!
//! A band near the top of the frame holds [`CELLS`] square cells on black: cell 0 is
//! always white and cell 1 always black (references for an adaptive threshold), then 32
//! data bits MSB first: a 24-bit frame counter followed by its CRC-8. Positions are
//! fractions of the frame size, so the code survives scaling, and cells are inset by
//! [`Layout::margin`] so chroma subsampling and scaling blur don't reach the sample area.

/// Number of cells: 2 references + 32 data bits.
pub const CELLS: u32 = 34;

/// Counter values wrap at 2^24.
pub const COUNTER_MASK: u32 = 0x00FF_FFFF;

/// Band geometry as fractions of the frame size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
    /// Inset of the lit square inside each cell, as a fraction of the cell size.
    pub margin: f32,
}

/// The layout used by the test game and the synthetic renderer.
pub const LAYOUT: Layout = Layout {
    x0: 0.02,
    x1: 0.98,
    y0: 0.02,
    y1: 0.14,
    margin: 0.18,
};

/// CRC-8 with polynomial 0x07 (CRC-8/SMBUS).
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// 32-bit code for a counter: `counter[23:0] << 8 | crc8(counter bytes)`.
pub fn encode(counter: u32) -> u32 {
    let c = counter & COUNTER_MASK;
    let b = c.to_be_bytes();
    let crc = crc8(b.get(1..4).unwrap_or(&[0, 0, 0]));
    (c << 8) | u32::from(crc)
}

/// Counter from a 32-bit code, if the CRC matches.
pub fn decode(code: u32) -> Option<u32> {
    let counter = code >> 8;
    (encode(counter) == code).then_some(counter)
}

/// Whether cell `i` is lit (white) for `code`.
pub fn cell_lit(code: u32, i: u32) -> bool {
    match i {
        0 => true,
        1 => false,
        2..=33 => {
            let bit = 33u32.saturating_sub(i);
            (code >> bit) & 1 == 1
        }
        _ => false,
    }
}

fn mean(frame: &[u8], stride: usize, x0: usize, x1: usize, y0: usize, y1: usize) -> Option<u32> {
    let mut sum = 0u64;
    let mut n = 0u64;
    for y in y0..y1 {
        let row = frame.get(y.checked_mul(stride)?..)?;
        for x in x0..x1 {
            sum = sum.checked_add(u64::from(*row.get(x)?))?;
            n = n.checked_add(1)?;
        }
    }
    u32::try_from(sum.checked_div(n)?).ok()
}

/// Read the counter from an 8-bit grayscale frame. `None` if the code is unreadable or
/// the CRC fails.
pub fn read_gray(frame: &[u8], width: usize, height: usize) -> Option<u32> {
    if width < 64 || height < 32 || frame.len() < width.checked_mul(height)? {
        return None;
    }
    let w = width as f32;
    let h = height as f32;
    let cell_w = (LAYOUT.x1 - LAYOUT.x0) * w / CELLS as f32;
    let band_h = (LAYOUT.y1 - LAYOUT.y0) * h;
    // Sample the central half of the lit square.
    let yc = (LAYOUT.y0 + LAYOUT.y1) * 0.5 * h;
    let ys = (band_h * 0.2).max(1.0);
    let y0 = (yc - ys).max(0.0) as usize;
    let y1 = ((yc + ys) as usize).min(height);
    let mut levels = [0u32; CELLS as usize];
    for (i, level) in levels.iter_mut().enumerate() {
        let xc = LAYOUT.x0 * w + (i as f32 + 0.5) * cell_w;
        let xs = (cell_w * 0.2).max(1.0);
        let x0 = (xc - xs).max(0.0) as usize;
        let x1 = ((xc + xs) as usize).min(width);
        *level = mean(frame, width, x0, x1, y0, y1)?;
    }
    let white = *levels.first()?;
    let black = *levels.get(1)?;
    if white < black.checked_add(60)? {
        return None;
    }
    let threshold = (white + black) / 2;
    let mut code = 0u32;
    for level in levels.get(2..)? {
        code = (code << 1) | u32::from(*level > threshold);
    }
    decode(code)
}

/// Render a test frame in BGRA (for synthetic sources). Same layout as the test game:
/// dark background with a scrolling checker and a moving bar, the barcode band on
/// black, and a full white flash (below the band) when `flash` is set.
pub fn render_bgra(counter: u32, width: usize, height: usize, flash: bool) -> Vec<u8> {
    let mut out = vec![0u8; width.saturating_mul(height).saturating_mul(4)];
    let code = encode(counter);
    let w = width as f32;
    let h = height as f32;
    let band_y0 = (LAYOUT.y0 * h) as usize;
    let band_y1 = (LAYOUT.y1 * h) as usize;
    let cell_w = (LAYOUT.x1 - LAYOUT.x0) * w / CELLS as f32;
    let band_h = (LAYOUT.y1 - LAYOUT.y0) * h;
    let bar_x = ((counter % 120) as f32 / 120.0 * w) as usize;
    let checker_shift = (counter as usize).wrapping_mul(4);
    for (y, row) in out
        .chunks_exact_mut(width.saturating_mul(4).max(4))
        .enumerate()
    {
        let in_band = y >= band_y0.saturating_sub(2) && y < band_y1.saturating_add(2);
        for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let v: u8 = if in_band {
                let fx = x as f32 - LAYOUT.x0 * w;
                let fy = y as f32 - LAYOUT.y0 * h;
                if fx < 0.0 || fy < 0.0 || fy >= band_h {
                    0
                } else {
                    let cell = (fx / cell_w) as u32;
                    let lx = fx / cell_w - cell as f32;
                    let ly = fy / band_h;
                    let inside = (LAYOUT.margin..=1.0 - LAYOUT.margin).contains(&lx)
                        && (LAYOUT.margin..=1.0 - LAYOUT.margin).contains(&ly);
                    if cell < CELLS && inside && cell_lit(code, cell) {
                        255
                    } else {
                        0
                    }
                }
            } else if flash {
                255
            } else if x.abs_diff(bar_x) < width / 50 {
                220
            } else if ((x.wrapping_add(checker_shift) / 32) + (y / 32)) % 2 == 0 {
                60
            } else {
                90
            };
            px.copy_from_slice(&[v, v, v, 255]);
        }
    }
    out
}

/// Convert BGRA to 8-bit gray (Rec. 709 luma, full range). Test helper.
pub fn bgra_to_gray(bgra: &[u8]) -> Vec<u8> {
    bgra.as_chunks::<4>()
        .0
        .iter()
        .map(|[b, g, r, _]| {
            let y = 0.0722 * f32::from(*b) + 0.7152 * f32::from(*g) + 0.2126 * f32::from(*r);
            y.round().clamp(0.0, 255.0) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_and_crc_catches_bit_flips() {
        for c in [0, 1, 59, 60, 12345, COUNTER_MASK] {
            let code = encode(c);
            assert_eq!(decode(code), Some(c));
            for bit in 0..32 {
                assert_eq!(decode(code ^ (1 << bit)), None, "counter {c} bit {bit}");
            }
        }
    }

    #[test]
    fn rendered_frames_read_back_at_several_sizes() {
        for (w, h) in [(1280, 720), (640, 360), (1920, 1080), (854, 480)] {
            for c in [0u32, 1, 2, 255, 256, 4095, 99_999] {
                for flash in [false, true] {
                    let gray = bgra_to_gray(&render_bgra(c, w, h, flash));
                    assert_eq!(
                        read_gray(&gray, w, h),
                        Some(c),
                        "{w}x{h} c={c} flash={flash}"
                    );
                }
            }
        }
    }

    #[test]
    fn garbage_frames_are_unreadable() {
        assert_eq!(read_gray(&[0u8; 640 * 360], 640, 360), None);
        assert_eq!(read_gray(&[255u8; 640 * 360], 640, 360), None);
        assert_eq!(read_gray(&[1, 2, 3], 640, 360), None);
        assert_eq!(read_gray(&[], 0, 0), None);
    }
}
