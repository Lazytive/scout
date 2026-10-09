//! Quality presets and what they resolve to at recording start.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

pub const WIDTH_RANGE: RangeInclusive<u32> = 320..=7680;
pub const HEIGHT_RANGE: RangeInclusive<u32> = 240..=4320;
pub const FPS_RANGE: RangeInclusive<u32> = 10..=240;
/// Video bitrate, kbit/s.
pub const BITRATE_RANGE: RangeInclusive<u32> = 1_000..=200_000;
/// The High preset follows the monitor's refresh rate within these bounds.
pub const HIGH_FPS: RangeInclusive<u32> = 60..=144;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    /// 720p30, 8 Mbit/s.
    Low,
    /// 1080p60, 20 Mbit/s.
    #[default]
    Balanced,
    /// Native up to 1440p at the monitor's refresh (60–144 fps), 40 Mbit/s.
    High,
    /// The numbers in [`CustomQuality`].
    Custom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    #[default]
    H264,
    Hevc,
    Av1,
}

/// Numbers for the Custom preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomQuality {
    /// Largest output size; bigger sources are scaled down keeping the aspect ratio.
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: VideoCodec,
}

impl Default for CustomQuality {
    fn default() -> Self {
        CustomQuality {
            max_width: 1920,
            max_height: 1080,
            fps: 60,
            bitrate_kbps: 20_000,
            codec: VideoCodec::H264,
        }
    }
}

/// A preset plus the Custom numbers (kept when another preset is picked).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Quality {
    pub preset: Preset,
    pub custom: CustomQuality,
}

/// What a recording uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct QualityParams {
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: VideoCodec,
}

impl Quality {
    /// Resolve to numbers. `refresh_hz`: the captured monitor's refresh rate, if known.
    pub fn params(&self, refresh_hz: Option<u32>) -> QualityParams {
        let p = |w, h, fps, kbps| QualityParams {
            max_width: w,
            max_height: h,
            fps,
            bitrate_kbps: kbps,
            codec: VideoCodec::H264,
        };
        match self.preset {
            Preset::Low => p(1280, 720, 30, 8_000),
            Preset::Balanced => p(1920, 1080, 60, 20_000),
            Preset::High => {
                let fps = refresh_hz
                    .unwrap_or(60)
                    .clamp(*HIGH_FPS.start(), *HIGH_FPS.end());
                p(2560, 1440, fps, 40_000)
            }
            Preset::Custom => QualityParams {
                max_width: self.custom.max_width,
                max_height: self.custom.max_height,
                fps: self.custom.fps,
                bitrate_kbps: self.custom.bitrate_kbps,
                codec: self.custom.codec,
            },
        }
    }
}
