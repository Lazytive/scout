//! From settings to a `RecorderConfig`: folders, quality, encoder choice (with the probe
//! cache), capture target and audio sources. Shared by the engine and `klip-cli record`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use klip_capture::apps::{self, SystemPlan};
use klip_capture::audio::{AudioConfig, AudioInput, AudioSource};
use klip_capture::{CaptureApi, EncodeConfig, VideoConfig, VideoTarget};
use klip_media::{Codec, NANOS_PER_MILLI, Nanos};
use klip_platform::{GpuVendor, ScreenCastSource};
use klip_probe::{EncoderChoice, ProbeCache};
use klip_recorder::{RecorderConfig, SessionConfig, estimate_buffer_bytes};
use klip_settings::{AudioSettings, DesktopAudio, Settings, VideoCodec};

use crate::EngineError;
use crate::games::{DetectedGame, folder_name};

/// Folder name for recordings of the desktop (Always mode, no game).
pub const DESKTOP_FOLDER: &str = "Desktop";

/// The clip folder: the setting, else `Videos/Klip`.
pub fn clip_base(settings: &Settings) -> Result<PathBuf, EngineError> {
    match &settings.clips.folder {
        Some(f) => Ok(PathBuf::from(f)),
        None => dirs::video_dir()
            .or_else(dirs::home_dir)
            .map(|d| d.join("Klip"))
            .ok_or_else(|| EngineError::Config("no Videos folder; choose a clip folder".into())),
    }
}

/// The recordings folder: the setting, else the clip folder.
pub fn recordings_base(settings: &Settings) -> Result<PathBuf, EngineError> {
    match &settings.clips.recordings_folder {
        Some(f) => Ok(PathBuf::from(f)),
        None => clip_base(settings),
    }
}

/// Sub-folder for a game's clips (or the desktop's), if per-game folders are on.
pub fn game_folder(settings: &Settings, base: &Path, game: Option<&DetectedGame>) -> PathBuf {
    if !settings.clips.per_game_folders {
        return base.to_path_buf();
    }
    base.join(game.map_or_else(|| DESKTOP_FOLDER.to_string(), |g| folder_name(&g.name)))
}

pub fn codec(c: VideoCodec) -> Codec {
    match c {
        VideoCodec::H264 => Codec::H264,
        VideoCodec::Hevc => Codec::H265,
        VideoCodec::Av1 => Codec::Av1,
    }
}

/// Vendors of the real (non-software) GPUs.
pub fn gpu_vendors() -> Vec<GpuVendor> {
    match klip_platform::list_gpus() {
        Ok(g) => g
            .into_iter()
            .filter(|a| !a.software)
            .map(|a| a.vendor)
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "could not list GPUs");
            Vec::new()
        }
    }
}

/// Where probe results are remembered between runs.
pub fn encoder_cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("Klip").join("encoders.json"))
}

/// The probe cache for this machine's current setup (empty if none or outdated).
pub fn load_encoder_cache() -> (ProbeCache, Option<PathBuf>) {
    let adapters: Vec<(u32, u32, Option<u64>)> = klip_platform::list_gpus()
        .unwrap_or_default()
        .into_iter()
        .filter(|g| !g.software)
        .map(|g| (g.vendor_id, g.device_id, g.driver_version))
        .collect();
    let fp = klip_probe::fingerprint(&adapters, &klip_probe::installed_encoders());
    let path = encoder_cache_path();
    let cache = match &path {
        Some(p) => ProbeCache::load(p, &fp),
        None => ProbeCache::new(&fp),
    };
    (cache, path)
}

pub fn save_encoder_cache(cache: &ProbeCache, path: Option<&Path>) {
    if let Some(p) = path
        && let Err(e) = cache.save(p)
    {
        tracing::warn!(error = %e, "could not save the encoder cache");
    }
}

/// Vendor of the GPU that shows `target` (encoding there avoids copies between GPUs).
pub fn target_gpu(target: VideoTarget) -> Option<GpuVendor> {
    let monitors = klip_platform::list_monitors().ok()?;
    let handle = target_monitor(target, &monitors)?;
    let luid = monitors.iter().find(|m| m.handle == handle)?.adapter_luid?;
    klip_platform::list_gpus()
        .ok()?
        .into_iter()
        .find(|g| g.luid == luid)
        .map(|g| g.vendor)
}

fn target_monitor(target: VideoTarget, monitors: &[klip_platform::MonitorInfo]) -> Option<u64> {
    match target {
        VideoTarget::PrimaryMonitor => monitors.iter().find(|m| m.primary).map(|m| m.handle),
        VideoTarget::Monitor(h) => Some(h),
        VideoTarget::Window(h) => klip_platform::window_info(h).ok().map(|w| w.monitor),
        VideoTarget::Portal => None,
    }
}

/// Refresh rate of the monitor that shows `target`.
pub fn target_refresh(target: VideoTarget) -> Option<u32> {
    let monitors = klip_platform::list_monitors().ok()?;
    let handle = target_monitor(target, &monitors)?;
    monitors
        .iter()
        .find(|m| m.handle == handle)
        .and_then(|m| m.refresh_hz)
}

/// Probes run in a child copy of this binary, so a misbehaving driver can't take the
/// recorder down and in-process state can't skew the result.
pub fn prober() -> klip_probe::Prober {
    match std::env::current_exe() {
        Ok(exe) => klip_probe::Prober::Isolated(exe),
        Err(_) => klip_probe::Prober::InProcess,
    }
}

/// Encoders to try, best first. A requested encoder is used alone.
pub fn pick_encoders(
    codec: Codec,
    requested: Option<&str>,
    prefer: Option<GpuVendor>,
    cache: &mut ProbeCache,
) -> Result<Vec<EncoderChoice>, EngineError> {
    let vendors = gpu_vendors();
    match requested {
        Some(name) => klip_probe::candidates(codec, &vendors, prefer)
            .into_iter()
            .find(|c| c.element == name)
            .map(|c| vec![c])
            .ok_or_else(|| {
                EngineError::Config(format!(
                    "{name} is not an installed, allowed {} encoder",
                    codec.name()
                ))
            }),
        None => {
            let list = klip_probe::ranked(
                codec,
                &vendors,
                prefer,
                cache,
                &prober(),
                Duration::from_secs(10),
            );
            if list.is_empty() {
                return Err(EngineError::Config(format!(
                    "no working {} encoder found",
                    codec.name()
                )));
            }
            Ok(list)
        }
    }
}

/// Apps left out of system audio.
pub fn excluded_apps(audio: &AudioSettings) -> Vec<String> {
    let mut v: Vec<String> = if audio.default_excludes {
        apps::DEFAULT_EXCLUDED_APPS
            .iter()
            .map(ToString::to_string)
            .collect()
    } else {
        Vec::new()
    };
    v.extend(audio.exclude_apps.iter().cloned());
    v
}

/// System audio without the excluded apps (see `klip_capture::apps::plan_system`).
pub fn system_source(excluded: &[String]) -> AudioSource {
    let whole = AudioSource::System { device: None };
    if excluded.is_empty() || !klip_capture::audio::process_loopback_supported() {
        return whole;
    }
    let procs = match klip_platform::list_processes() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "process list unavailable; recording all system audio");
            return whole;
        }
    };
    let sessions = klip_platform::audio_sessions().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "audio sessions unavailable");
        Vec::new()
    });
    match apps::plan_system(&procs, &sessions, excluded) {
        SystemPlan::Whole => whole,
        SystemPlan::Except { pid, exe } => {
            tracing::info!(app = %exe, "leaving an app out of system audio");
            AudioSource::SystemExcept { pid }
        }
        SystemPlan::Apps { pids, excluded } => {
            tracing::info!(
                left_out = excluded.len(),
                apps = pids.len(),
                "recording the other apps with audio one by one"
            );
            AudioSource::Apps { pids }
        }
    }
}

/// Audio inputs for a recording of `game_pid` (or the desktop), or `None` for silence.
pub fn audio_config(audio: &AudioSettings, game_pid: Option<u32>) -> Option<AudioConfig> {
    let per_app = klip_capture::audio::process_loopback_supported();
    let mut inputs = Vec::new();
    let system = || AudioInput {
        label: "System".into(),
        source: system_source(&excluded_apps(audio)),
    };
    match (audio.desktop, game_pid) {
        (DesktopAudio::Off, _) => {}
        (DesktopAudio::Game, Some(pid)) if per_app && pid != 0 => inputs.push(AudioInput {
            label: "Game".into(),
            source: AudioSource::Process { pid },
        }),
        (DesktopAudio::Game | DesktopAudio::System, _) => inputs.push(system()),
    }
    if audio.voice && per_app && !audio.voice_apps.is_empty() {
        let procs = klip_platform::list_processes().unwrap_or_default();
        let roots = apps::app_roots(&procs, &audio.voice_apps);
        let pids: Vec<u32> = roots.iter().map(|(pid, _)| *pid).collect();
        let source = match pids.as_slice() {
            [] => None,
            [pid] => Some(AudioSource::Process { pid: *pid }),
            _ => Some(AudioSource::Apps { pids }),
        };
        if let Some(source) = source {
            inputs.push(AudioInput {
                label: "Voice".into(),
                source,
            });
        }
    }
    if audio.mic {
        inputs.push(AudioInput {
            label: "Microphone".into(),
            source: AudioSource::Device {
                device: audio.mic_device.clone(),
            },
        });
    }
    if inputs.is_empty() {
        return None;
    }
    Some(AudioConfig {
        inputs,
        bitrate: audio.bitrate_kbps.saturating_mul(1000),
        separate_tracks: audio.separate_tracks,
    })
}

/// Largest restore token accepted from the token file (portals use UUID-sized tokens).
const MAX_PORTAL_TOKEN: u64 = 1024;

/// Default place for the screen-cast restore token.
pub fn portal_token_path() -> Option<PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("Klip").join("screencast-token"))
}

/// The stored restore token, if the file holds a plausible one (it is user-writable, so
/// treat it as hostile: size-capped, printable ASCII only).
pub fn read_portal_token(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_PORTAL_TOKEN {
        return None;
    }
    let token = std::fs::read_to_string(path).ok()?.trim().to_string();
    let ok = !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic());
    ok.then_some(token)
}

/// Write `token` atomically (temp file + rename).
pub fn write_portal_token(path: &Path, token: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, token)?;
    std::fs::rename(&tmp, path)
}

/// Open a screen-cast session: restores the stored choice, or shows the desktop's dialog.
pub fn open_portal(
    source: ScreenCastSource,
    show_cursor: bool,
    token_path: Option<&Path>,
) -> Result<klip_capture::PortalSession, EngineError> {
    let restore_token = token_path.and_then(read_portal_token);
    if restore_token.is_none() {
        tracing::info!("asking the desktop which screen or window to record");
    }
    let cast = klip_platform::ScreenCast::start(&klip_platform::ScreenCastRequest {
        source,
        show_cursor,
        restore_token,
    })
    .map_err(|e| EngineError::Config(format!("screen-cast portal: {e}")))?;
    if let (Some(p), Some(t)) = (token_path, cast.restore_token())
        && let Err(e) = write_portal_token(p, t)
    {
        tracing::warn!(error = %e, "could not store the screen-cast restore token");
    }
    Ok(klip_capture::PortalSession(Arc::new(cast)))
}

/// What a recording of `game` (or the desktop) uses, ready for `Recorder::start`.
pub struct Plan {
    pub config: RecorderConfig,
    pub clip_dir: PathBuf,
    pub fps: u32,
}

/// Build the recorder configuration from the settings (with the game's profile).
pub fn plan(
    settings: &Settings,
    game: Option<&DetectedGame>,
    cache: &mut ProbeCache,
) -> Result<Plan, EngineError> {
    let eff = settings.effective(game.map(|g| g.exe.as_str()));
    let target = if cfg!(target_os = "linux") {
        VideoTarget::Portal
    } else {
        match game.and_then(|g| g.window) {
            Some(h) => VideoTarget::Window(h),
            None => VideoTarget::PrimaryMonitor,
        }
    };
    let q = eff.quality.params(target_refresh(target));
    let codec = codec(q.codec);
    let mut encoders = pick_encoders(codec, eff.encoder.as_deref(), target_gpu(target), cache)?;
    if encoders.is_empty() {
        return Err(EngineError::Config("no encoder".into()));
    }
    let encoder = encoders.remove(0);
    let clip_dir = game_folder(settings, &clip_base(settings)?, game);
    let session_dir = game_folder(settings, &recordings_base(settings)?, game);
    let portal = match target {
        VideoTarget::Portal => Some(open_portal(
            ScreenCastSource::Monitor,
            eff.show_cursor,
            portal_token_path().as_deref(),
        )?),
        _ => None,
    };
    let audio = audio_config(&eff.audio, game.map(|g| g.pid));
    let audio_kbps = audio.as_ref().map_or(0, |a| {
        let tracks = u32::try_from(a.track_labels().len()).unwrap_or(u32::MAX);
        (a.bitrate / 1000).saturating_mul(tracks)
    });
    let buffer: Nanos = i64::from(eff.buffer_secs).saturating_mul(1000 * NANOS_PER_MILLI);
    Ok(Plan {
        config: RecorderConfig {
            buffer,
            max_buffer_bytes: estimate_buffer_bytes(
                buffer,
                q.bitrate_kbps.saturating_add(audio_kbps),
            ),
            clip_dir: clip_dir.clone(),
            video: VideoConfig {
                target,
                fps: q.fps,
                max_width: q.max_width,
                max_height: q.max_height,
                show_cursor: eff.show_cursor,
                api: CaptureApi::Auto,
                output: None,
                portal,
            },
            encode: EncodeConfig {
                encoder,
                bitrate_kbps: q.bitrate_kbps,
                keyframe_interval: q.fps,
            },
            encoder_fallbacks: encoders,
            audio,
            session: Some(SessionConfig::new(session_dir)),
        },
        clip_dir,
        fps: q.fps,
    })
}
