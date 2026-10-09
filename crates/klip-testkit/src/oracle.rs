//! ffmpeg/ffprobe as **external test oracles** (AGENTS.md: never linked or shipped).
//! They're run as child processes; their output is parsed defensively.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

/// Errors from running an oracle.
#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    #[error("{0} is not installed or not on PATH")]
    Missing(&'static str),
    #[error("{tool} failed: {stderr}")]
    Failed { tool: &'static str, stderr: String },
    #[error("could not parse {0} output: {1}")]
    Parse(&'static str, String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// True if `tool -version` runs.
pub fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// One stream as reported by ffprobe.
#[derive(Clone, Debug, Deserialize, Default)]
pub struct StreamInfo {
    pub index: u32,
    pub codec_type: Option<String>,
    pub codec_name: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub r_frame_rate: Option<String>,
    pub avg_frame_rate: Option<String>,
    pub nb_frames: Option<String>,
    pub duration: Option<String>,
    pub sample_rate: Option<String>,
    pub channels: Option<u32>,
    pub pix_fmt: Option<String>,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub start_time: Option<String>,
    pub bit_rate: Option<String>,
    /// e.g. `{"default": 1, ...}`.
    #[serde(default)]
    pub disposition: std::collections::HashMap<String, i64>,
    /// e.g. `handler_name`.
    #[serde(default)]
    pub tags: std::collections::HashMap<String, String>,
}

impl StreamInfo {
    pub fn duration_secs(&self) -> Option<f64> {
        self.duration.as_deref()?.parse().ok()
    }

    pub fn frames(&self) -> Option<u64> {
        self.nb_frames.as_deref()?.parse().ok()
    }

    /// `avg_frame_rate` as a float.
    pub fn avg_fps(&self) -> Option<f64> {
        parse_rate(self.avg_frame_rate.as_deref()?)
    }
}

/// Parse an ffprobe rate such as `60/1` or `30000/1001`.
pub fn parse_rate(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/')?;
    let n: f64 = n.parse().ok()?;
    let d: f64 = d.parse().ok()?;
    (d != 0.0).then(|| n / d)
}

#[derive(Deserialize)]
struct Streams {
    streams: Vec<StreamInfo>,
}

fn run(tool: &'static str, args: &[&str]) -> Result<Vec<u8>, OracleError> {
    let out = Command::new(tool).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            OracleError::Missing(tool)
        } else {
            OracleError::Io(e)
        }
    })?;
    if !out.status.success() {
        return Err(OracleError::Failed {
            tool,
            stderr: String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(2000)
                .collect(),
        });
    }
    Ok(out.stdout)
}

fn path_str(path: &Path) -> Result<&str, OracleError> {
    path.to_str()
        .ok_or_else(|| OracleError::Parse("path", "not UTF-8".into()))
}

/// All streams in a file.
pub fn probe_streams(path: &Path) -> Result<Vec<StreamInfo>, OracleError> {
    let out = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-show_streams",
            "-of",
            "json",
            path_str(path)?,
        ],
    )?;
    let s: Streams =
        serde_json::from_slice(&out).map_err(|e| OracleError::Parse("ffprobe", e.to_string()))?;
    Ok(s.streams)
}

#[derive(Debug, Deserialize)]
struct Chapters {
    #[serde(default)]
    chapters: Vec<ChapterEntry>,
}

#[derive(Debug, Deserialize)]
struct ChapterEntry {
    start_time: String,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

/// Chapters as ffprobe reports them: (start in seconds, title).
pub fn chapters(path: &Path) -> Result<Vec<(f64, String)>, OracleError> {
    let out = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-show_chapters",
            "-of",
            "json",
            path_str(path)?,
        ],
    )?;
    let c: Chapters =
        serde_json::from_slice(&out).map_err(|e| OracleError::Parse("ffprobe", e.to_string()))?;
    c.chapters
        .into_iter()
        .map(|ch| {
            let start = ch
                .start_time
                .parse::<f64>()
                .map_err(|e| OracleError::Parse("ffprobe", e.to_string()))?;
            Ok((start, ch.tags.get("title").cloned().unwrap_or_default()))
        })
        .collect()
}

/// Presentation times (seconds) of every frame of the first video stream, in decode order.
pub fn video_frame_times(path: &Path) -> Result<Vec<f64>, OracleError> {
    frame_times(path, "v:0")
}

/// Presentation times (seconds) of every frame of the stream `selector` (`v:0`, `a:1`,
/// …), in decode order, edit lists applied.
pub fn frame_times(path: &Path, selector: &str) -> Result<Vec<f64>, OracleError> {
    let out = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            selector,
            "-show_entries",
            "frame=pts_time",
            "-of",
            "csv=p=0",
            path_str(path)?,
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.trim_end_matches(',')
                .parse::<f64>()
                .map_err(|e| OracleError::Parse("ffprobe frame times", format!("{l:?}: {e}")))
        })
        .collect()
}

/// Decode audio stream `index` (0 = first audio stream) to mono S16 at `rate` Hz. The
/// first sample is at the stream's first frame time (see [`frame_times`]).
pub fn decode_audio_mono(path: &Path, index: usize, rate: u32) -> Result<Vec<i16>, OracleError> {
    let map = format!("0:a:{index}");
    let rate = rate.to_string();
    let out = run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-nostdin",
            "-i",
            path_str(path)?,
            "-map",
            &map,
            "-ac",
            "1",
            "-ar",
            &rate,
            "-f",
            "s16le",
            "-",
        ],
    )?;
    Ok(out
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect())
}

/// Decode the frame at `at_s` seconds of the first video stream as `width`x`height`
/// 8-bit gray (luma) with ffmpeg.
pub fn decode_gray_frame(
    path: &Path,
    at_s: f64,
    width: usize,
    height: usize,
) -> Result<Vec<u8>, OracleError> {
    let scale = format!("scale={width}:{height}:flags=area");
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-ss",
            &format!("{at_s:.3}"),
            "-i",
        ])
        .arg(path_str(path)?)
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-vf",
            &scale,
            "-pix_fmt",
            "gray",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                OracleError::Missing("ffmpeg")
            } else {
                OracleError::Io(e)
            }
        })?;
    if !out.status.success() || out.stdout.len() != width.saturating_mul(height) {
        return Err(OracleError::Failed {
            tool: "ffmpeg",
            stderr: String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(2000)
                .collect(),
        });
    }
    Ok(out.stdout)
}

/// Decode the first video stream to `width`x`height` 8-bit gray with ffmpeg (no frame
/// rate conversion) and read the barcode of every frame.
pub fn decode_counters(
    path: &Path,
    width: usize,
    height: usize,
) -> Result<Vec<Option<u32>>, OracleError> {
    let scale = format!("scale={width}:{height}:flags=area");
    let mut child = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-i",
            path_str(path)?,
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-vf",
            &scale,
            "-pix_fmt",
            "gray",
            "-f",
            "rawvideo",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                OracleError::Missing("ffmpeg")
            } else {
                OracleError::Io(e)
            }
        })?;
    let frame_len = width.saturating_mul(height);
    let mut counters = Vec::new();
    let mut buf = vec![0u8; frame_len];
    if let Some(mut stdout) = child.stdout.take() {
        loop {
            match stdout.read_exact(&mut buf) {
                Ok(()) => counters.push(crate::barcode::read_gray(&buf, width, height)),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }
    }
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(OracleError::Failed {
            tool: "ffmpeg",
            stderr: stderr.chars().take(2000).collect(),
        });
    }
    Ok(counters)
}

/// Fourcc of each top-level box (pure Rust, no oracle). Used to check faststart order.
pub fn top_level_boxes(path: &Path) -> Result<Vec<String>, OracleError> {
    let data = std::fs::read(path)?;
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(hdr) = data.get(pos..pos.saturating_add(8)) {
        let (size_bytes, kind) = hdr.split_at(4);
        let mut b4 = [0u8; 4];
        b4.copy_from_slice(size_bytes);
        let size32 = u32::from_be_bytes(b4);
        out.push(String::from_utf8_lossy(kind).into_owned());
        let size = if size32 == 1 {
            let large = data
                .get(pos.saturating_add(8)..pos.saturating_add(16))
                .ok_or_else(|| OracleError::Parse("mp4", "truncated largesize".into()))?;
            let mut b = [0u8; 8];
            b.copy_from_slice(large);
            usize::try_from(u64::from_be_bytes(b)).unwrap_or(usize::MAX)
        } else if size32 == 0 {
            data.len().saturating_sub(pos)
        } else {
            size32 as usize
        };
        if size < 8 {
            return Err(OracleError::Parse(
                "mp4",
                format!("box size {size} at {pos}"),
            ));
        }
        pos = pos.saturating_add(size);
    }
    if pos != data.len() {
        return Err(OracleError::Parse(
            "mp4",
            "trailing bytes or truncated box".into(),
        ));
    }
    Ok(out)
}
