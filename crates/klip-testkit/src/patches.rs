//! Brightness patches for colour/HDR checks. The test game draws a row of flat gray
//! patches (`--patches`, always on with `--hdr`) below the moving content; tests read
//! their level back from decoded clip frames.
//!
//! - SDR mode: each patch is the 8-bit code value in [`SDR_CODES`], written unchanged
//!   into an 8-bit swapchain.
//! - HDR mode (scRGB, FP16 swapchain): each patch emits the luminance in [`HDR_NITS`]
//!   (scRGB 1.0 = 80 nits). The rest of the picture is the SDR picture at the paper-white
//!   level given to the game.

/// Patch row geometry as fractions of the frame (below the barcode band and the
/// checker's top area; the moving bar is not drawn over it).
pub const ROW: Row = Row {
    x0: 0.05,
    x1: 0.95,
    y0: 0.78,
    y1: 0.92,
};

/// SDR code values, darkest first.
pub const SDR_CODES: [u8; 6] = [16, 64, 128, 192, 235, 255];

/// HDR patch luminance in nits, darkest first. 80 nits is scRGB 1.0.
pub const HDR_NITS: [f32; 6] = [80.0, 160.0, 320.0, 640.0, 1000.0, 2000.0];

/// Patch row geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
}

/// Number of patches (same for SDR and HDR).
pub const COUNT: usize = SDR_CODES.len();

/// Mean level of each patch's central area in an 8-bit grayscale frame, darkest patch
/// first. `None` if the frame is too small.
pub fn read_gray(frame: &[u8], width: usize, height: usize) -> Option<[f32; COUNT]> {
    if width < 64 || height < 32 || frame.len() < width.checked_mul(height)? {
        return None;
    }
    let w = width as f32;
    let h = height as f32;
    let pw = (ROW.x1 - ROW.x0) * w / COUNT as f32;
    let ph = (ROW.y1 - ROW.y0) * h;
    let y0 = ((ROW.y0 * h) + ph * 0.25) as usize;
    let y1 = (((ROW.y0 * h) + ph * 0.75) as usize).min(height);
    let mut out = [0f32; COUNT];
    for (i, level) in out.iter_mut().enumerate() {
        let x0 = (ROW.x0 * w + (i as f32 + 0.25) * pw) as usize;
        let x1 = ((ROW.x0 * w + (i as f32 + 0.75) * pw) as usize).min(width);
        let mut sum = 0u64;
        let mut n = 0u64;
        for y in y0..y1 {
            let row = frame.get(y * width..(y + 1) * width)?;
            for v in row.get(x0..x1)? {
                sum += u64::from(*v);
                n += 1;
            }
        }
        if n == 0 {
            return None;
        }
        *level = sum as f32 / n as f32;
    }
    Some(out)
}

/// sRGB transfer function, code value (0..=1) to linear light (0..=1).
pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_flat_patches() {
        let (w, h) = (640usize, 360usize);
        let mut frame = vec![0u8; w * h];
        let pw = (ROW.x1 - ROW.x0) * w as f32 / COUNT as f32;
        for y in (ROW.y0 * h as f32) as usize..(ROW.y1 * h as f32) as usize {
            for x in 0..w {
                let rel = (x as f32 - ROW.x0 * w as f32) / pw;
                if rel >= 0.0 && (rel as usize) < COUNT {
                    frame[y * w + x] = SDR_CODES[rel as usize];
                }
            }
        }
        let got = read_gray(&frame, w, h).unwrap();
        for (g, want) in got.iter().zip(SDR_CODES) {
            assert!((g - f32::from(want)).abs() < 0.01, "{got:?}");
        }
    }

    #[test]
    fn tiny_or_short_frames_are_rejected() {
        assert!(read_gray(&[0; 10], 10, 1).is_none());
        assert!(read_gray(&[0; 100], 640, 360).is_none());
    }

    #[test]
    fn srgb_curve_endpoints() {
        assert!(srgb_to_linear(0.0).abs() < 1e-6);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        assert!((srgb_to_linear(0.5) - 0.214).abs() < 0.001);
    }
}
