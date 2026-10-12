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

/// The clip folder: the setting, else `Videos/Scout`.
pub fn clip_base(settings: &Settings) -> Result<PathBuf, EngineError> {
    match &settings.clips.folder {
        Some(f) => Ok(PathBuf::from(f)),
        None => dirs::video_dir()
            .or_else(dirs::home_dir)
            .map(|d| d.join(crate::migrate::NAME))
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

/// What tests change about probing (see [`test_overrides`]).
#[derive(Clone, Debug, Default)]
struct Overrides {
    cache: Option<PathBuf>,
    in_process: bool,
}

static OVERRIDES: std::sync::Mutex<Overrides> = std::sync::Mutex::new(Overrides {
    cache: None,
    in_process: false,
});

fn overrides() -> Overrides {
    OVERRIDES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// For tests: remember probe results in `cache` instead of the user's file, and probe
/// in this process (a test binary has no probe worker: every isolated probe would
/// "fail" and be remembered as such). Call before `Engine::start`.
pub fn test_overrides(cache: Option<PathBuf>, in_process: bool) {
    *OVERRIDES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Overrides { cache, in_process };
}

/// Where probe results are remembered between runs.
pub fn encoder_cache_path() -> Option<PathBuf> {
    if let Some(p) = overrides().cache {
        return Some(p);
    }
    dirs::cache_dir().map(|d| d.join(crate::migrate::NAME).join("encoders.json"))
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
/// recorder down and in-process state can't skew the result. `KLIP_PROBE_IN_PROCESS=1`
/// (tests: a test binary has no probe worker, every isolated probe would "fail" and
/// be remembered as such) probes in this process instead.
pub fn prober() -> klip_probe::Prober {
    if overrides().in_process {
        return klip_probe::Prober::InProcess;
    }
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

/// Encoders for an export, per codec: the user's chosen encoder for its own codec,
/// the ranked (probed) list otherwise. H.264 must exist; HEVC and AV1 may be empty
/// (`klip-export` falls back to H.264 for the source codec then).
pub fn export_encoders(
    settings: &Settings,
    cache: &mut ProbeCache,
) -> Result<klip_export::Encoders, EngineError> {
    let requested = settings.recording.encoder.as_deref();
    let vendors = gpu_vendors();
    let mut out = klip_export::Encoders::default();
    for codec in [Codec::H264, Codec::H265, Codec::Av1] {
        let own = requested.filter(|name| {
            klip_probe::candidates(codec, &vendors, None)
                .iter()
                .any(|c| c.element == *name)
        });
        match pick_encoders(codec, own, None, cache) {
            Ok(list) => {
                for c in list {
                    out.push(c);
                }
            }
            Err(e) if codec == Codec::H264 => return Err(e),
            Err(e) => tracing::debug!(codec = codec.name(), error = %e, "no export encoder"),
        }
    }
    Ok(out)
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
        .map(|d| d.join(crate::migrate::NAME).join("screencast-token"))
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

/// Write `token` atomically (temp file, fsync, rename).
pub fn write_portal_token(path: &Path, token: &str) -> std::io::Result<()> {
    crate::atomic::write_atomic(path, token.as_bytes())
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

/// What `game` (or the desktop) is recorded from.
fn video_target(game: Option<&DetectedGame>) -> VideoTarget {
    if cfg!(target_os = "linux") {
        VideoTarget::Portal
    } else {
        match game.and_then(|g| g.window) {
            Some(h) => VideoTarget::Window(h),
            None => VideoTarget::PrimaryMonitor,
        }
    }
}

/// Build the recorder configuration from the settings (with the game's profile).
pub fn plan(
    settings: &Settings,
    game: Option<&DetectedGame>,
    cache: &mut ProbeCache,
) -> Result<Plan, EngineError> {
    let encoders = encoders_for(settings, game, cache)?;
    plan_with(settings, game, encoders)
}

/// The encoders a recording of `game` tries, best first (probing uncached ones).
pub fn encoders_for(
    settings: &Settings,
    game: Option<&DetectedGame>,
    cache: &mut ProbeCache,
) -> Result<Vec<EncoderChoice>, EngineError> {
    let eff = settings.effective(game.map(|g| g.exe.as_str()));
    let target = video_target(game);
    let q = eff.quality.params(target_refresh(target));
    pick_encoders(
        codec(q.codec),
        eff.encoder.as_deref(),
        target_gpu(target),
        cache,
    )
}

/// The installed, allowed encoders named in `names`, in that order (what a recorder
/// process gets from the engine; unknown names are skipped).
pub fn encoders_named(
    settings: &Settings,
    game: Option<&DetectedGame>,
    names: &[String],
) -> Vec<EncoderChoice> {
    let eff = settings.effective(game.map(|g| g.exe.as_str()));
    let target = video_target(game);
    let q = eff.quality.params(target_refresh(target));
    let installed = klip_probe::candidates(codec(q.codec), &gpu_vendors(), target_gpu(target));
    names
        .iter()
        .filter_map(|n| installed.iter().find(|c| &c.element == n).cloned())
        .collect()
}

/// [`plan`] with the encoders already chosen (the first one is used, the rest are
/// fallbacks).
pub fn plan_with(
    settings: &Settings,
    game: Option<&DetectedGame>,
    mut encoders: Vec<EncoderChoice>,
) -> Result<Plan, EngineError> {
    let eff = settings.effective(game.map(|g| g.exe.as_str()));
    let target = video_target(game);
    let q = eff.quality.params(target_refresh(target));
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
                max_bitrate_kbps: None,
                leaky_queue: true,
            },
            encoder_fallbacks: encoders,
            audio,
            session: Some(SessionConfig::new(session_dir)),
        },
        clip_dir,
        fps: q.fps,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use klip_probe::{EncoderFamily, EncoderVendor};

    use super::*;

    fn audio() -> AudioSettings {
        AudioSettings {
            desktop: DesktopAudio::Off,
            voice: false,
            mic: false,
            mic_device: None,
            ..AudioSettings::default()
        }
    }

    #[test]
    fn no_audio_input_is_silence() {
        assert!(audio_config(&audio(), Some(42)).is_none());
    }

    #[test]
    fn microphone_alone_uses_the_chosen_device_and_bitrate() {
        let a = AudioSettings {
            mic: true,
            mic_device: Some("USB mic".into()),
            bitrate_kbps: 160,
            separate_tracks: true,
            ..audio()
        };
        let c = audio_config(&a, None).unwrap();
        assert_eq!(c.inputs.len(), 1);
        assert_eq!(c.inputs[0].label, "Microphone");
        assert!(matches!(
            &c.inputs[0].source,
            AudioSource::Device { device: Some(d) } if d == "USB mic"
        ));
        assert_eq!(c.bitrate, 160_000);
        assert_eq!(c.track_labels(), ["Microphone"]);
    }

    #[test]
    fn game_audio_follows_the_game_process_where_it_can() {
        let a = AudioSettings {
            desktop: DesktopAudio::Game,
            mic: true,
            separate_tracks: true,
            ..audio()
        };
        let c = audio_config(&a, Some(4242)).unwrap();
        let per_app = klip_capture::audio::process_loopback_supported();
        match (&c.inputs[0].source, per_app) {
            (AudioSource::Process { pid: 4242 }, true) => assert_eq!(c.inputs[0].label, "Game"),
            (_, false) => assert_eq!(c.inputs[0].label, "System"),
            (other, _) => panic!("unexpected source {other:?}"),
        }
        assert_eq!(c.inputs[1].label, "Microphone");
        // Two inputs: the Mix first, then each input on its own.
        assert_eq!(c.track_labels().len(), 3);
        assert_eq!(c.track_labels()[0], klip_capture::audio::MIX_LABEL);
        // No game (the desktop) or a pid of 0: all system audio.
        for pid in [None, Some(0)] {
            let c = audio_config(&a, pid).unwrap();
            assert_eq!(c.inputs[0].label, "System", "{pid:?}");
        }
    }

    fn encoder(element: &str) -> EncoderChoice {
        EncoderChoice {
            element: element.into(),
            codec: Codec::H264,
            family: EncoderFamily::Amf,
            vendor: EncoderVendor::Amd,
            description: String::new(),
            rank: 0,
            gpu: None,
            device_index: 0,
        }
    }

    #[test]
    fn a_plan_needs_an_encoder() {
        let s = Settings::default();
        assert!(matches!(
            plan_with(&s, None, vec![]),
            Err(EngineError::Config(_))
        ));
    }

    /// Linux plans open the screen-cast portal, so this runs on Windows.
    #[cfg(windows)]
    #[test]
    fn plan_uses_the_first_encoder_and_keeps_the_rest_as_fallbacks() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Settings::default();
        s.clips.folder = Some(dir.path().to_string_lossy().into_owned());
        s.clips.per_game_folders = true;
        s.recording.buffer_secs = 30;
        s.audio = audio();
        let p = plan_with(&s, None, vec![encoder("a"), encoder("b"), encoder("c")]).unwrap();
        assert_eq!(p.config.encode.encoder.element, "a");
        let rest: Vec<&str> = p
            .config
            .encoder_fallbacks
            .iter()
            .map(|e| e.element.as_str())
            .collect();
        assert_eq!(rest, ["b", "c"]);
        assert_eq!(p.clip_dir, dir.path().join(DESKTOP_FOLDER));
        assert_eq!(p.config.buffer, 30 * 1000 * NANOS_PER_MILLI);
        assert_eq!(p.fps, p.config.video.fps);
        assert_eq!(p.config.encode.keyframe_interval, p.fps);
        assert!(p.config.audio.is_none());
        assert!(p.config.max_buffer_bytes > 0);
        assert!(matches!(p.config.video.target, VideoTarget::PrimaryMonitor));
    }
}
