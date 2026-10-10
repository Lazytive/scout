//! Output format: the clip as recorded (16:9 or whatever it is) or a 9:16 crop whose
//! horizontal position the user drags.

use serde::{Deserialize, Serialize};

use crate::project::Issue;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatKind {
    /// The source frame as it is.
    #[default]
    Wide,
    /// A 9:16 crop of the full source height.
    Tall,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Format {
    pub kind: FormatKind,
    /// Where the 9:16 window sits: 0 = left edge, 0.5 = centre, 1 = right edge.
    pub crop_x: f32,
}

impl Default for Format {
    fn default() -> Self {
        Format {
            kind: FormatKind::Wide,
            crop_x: 0.5,
        }
    }
}

/// A pixel rectangle inside the source frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

fn even(v: u32) -> u32 {
    v & !1
}

impl Format {
    pub(crate) fn repair(&mut self) -> Option<Issue> {
        let fixed = if self.crop_x.is_nan() {
            0.5
        } else {
            self.crop_x.clamp(0.0, 1.0)
        };
        if fixed.to_bits() != self.crop_x.to_bits() {
            self.crop_x = fixed;
            return Some(Issue::new(
                "format.crop_x",
                format!("out of range; set to {fixed}"),
            ));
        }
        None
    }

    /// The part of a `src_width` x `src_height` frame that is shown (even sizes and
    /// offsets, as encoders want). A zero-sized source gives a zero rect.
    pub fn crop_rect(&self, src_width: u32, src_height: u32) -> Rect {
        let full = Rect {
            x: 0,
            y: 0,
            width: even(src_width),
            height: even(src_height),
        };
        match self.kind {
            FormatKind::Wide => full,
            FormatKind::Tall => {
                let wanted = u64::from(src_height)
                    .saturating_mul(9)
                    .checked_div(16)
                    .unwrap_or(0);
                let width = even(u32::try_from(wanted).unwrap_or(u32::MAX).min(src_width));
                let spare = src_width.saturating_sub(width);
                let x = (f64::from(spare) * f64::from(self.crop_x.clamp(0.0, 1.0))).round();
                let x = if x.is_finite() && x >= 0.0 {
                    u32::try_from(x as u64).unwrap_or(u32::MAX)
                } else {
                    0
                };
                Rect {
                    x: even(x.min(spare)),
                    y: 0,
                    width,
                    height: even(src_height),
                }
            }
        }
    }

    /// The exported frame size: the crop, scaled down to `max_height` if given (aspect
    /// kept, even sizes). Never scales up.
    pub fn output_size(
        &self,
        src_width: u32,
        src_height: u32,
        max_height: Option<u32>,
    ) -> (u32, u32) {
        let r = self.crop_rect(src_width, src_height);
        match max_height {
            Some(max) if max > 0 && r.height > max => {
                let w = u64::from(r.width)
                    .saturating_mul(u64::from(max))
                    .checked_div(u64::from(r.height))
                    .unwrap_or(0);
                (even(u32::try_from(w).unwrap_or(u32::MAX)), even(max))
            }
            _ => (r.width, r.height),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_the_full_frame() {
        let f = Format::default();
        assert_eq!(
            f.crop_rect(1920, 1080),
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(
            f.crop_rect(1921, 1081),
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(f.output_size(2560, 1440, Some(1080)), (1920, 1080));
        assert_eq!(f.output_size(1280, 720, Some(1080)), (1280, 720));
        assert_eq!(f.output_size(1280, 720, None), (1280, 720));
    }

    #[test]
    fn tall_crops_nine_by_sixteen_at_the_chosen_position() {
        let mut f = Format {
            kind: FormatKind::Tall,
            crop_x: 0.5,
        };
        // 1080 * 9 / 16 = 607.5 → 606 (even); spare 1314, centred → 657 → 656.
        assert_eq!(
            f.crop_rect(1920, 1080),
            Rect {
                x: 656,
                y: 0,
                width: 606,
                height: 1080
            }
        );
        f.crop_x = 0.0;
        assert_eq!(f.crop_rect(1920, 1080).x, 0);
        f.crop_x = 1.0;
        assert_eq!(f.crop_rect(1920, 1080).x, 1314);
        // A source narrower than 9:16 is used whole.
        assert_eq!(
            f.crop_rect(500, 1080),
            Rect {
                x: 0,
                y: 0,
                width: 500,
                height: 1080
            }
        );
        assert_eq!(f.output_size(1920, 1080, Some(1920)), (606, 1080));
        assert_eq!(f.output_size(3840, 2160, Some(1920)), (1078, 1920));
        assert_eq!(
            f.crop_rect(0, 0),
            Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0
            }
        );
    }

    #[test]
    fn repair_clamps_crop_x() {
        let mut f = Format {
            kind: FormatKind::Tall,
            crop_x: 7.0,
        };
        assert!(f.repair().is_some());
        assert_eq!(f.crop_x, 1.0);
        f.crop_x = f32::NAN;
        assert!(f.repair().is_some());
        assert_eq!(f.crop_x, 0.5);
        assert!(f.repair().is_none());
    }
}
