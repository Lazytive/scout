//! Sharing commands (M11, ADR 0021). `share.discord {path}`: the clip on the clipboard
//! for Discord, as it is when Discord takes it (fits the upload limit, H.264 in an MP4,
//! no edits), else a Discord-preset export (edits applied, under the limit) whose result
//! the export thread copies when it's done.

use std::path::Path;

use klip_control::CommandError;
use klip_edit::Project;
use klip_export::Preset;
use klip_library::ClipInfo;
use serde_json::{Value, json};

use super::Inner;
use super::editor::source_of;
use super::export::ExportKind;
use super::library::{recording_in_progress, str_arg};

/// Why a clip can't go to Discord as it is (`None`: it can).
pub(super) fn discord_reason(
    path: &Path,
    bytes: u64,
    limit_bytes: u64,
    info: &ClipInfo,
    edited: bool,
) -> Option<String> {
    let mp4 = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
    let codec = info.codec.as_deref().unwrap_or("");
    if bytes > limit_bytes {
        Some(format!(
            "it's over Discord's {} MB",
            limit_bytes.checked_div(1_000_000).unwrap_or(0)
        ))
    } else if edited {
        Some("it has edits".into())
    } else if !mp4 {
        Some("it isn't an MP4".into())
    } else if !codec.eq_ignore_ascii_case("H.264") {
        Some(if codec.is_empty() {
            "Discord may not play it".into()
        } else {
            format!("Discord may not play {codec}")
        })
    } else {
        None
    }
}

impl Inner {
    pub(super) fn share_discord(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        if recording_in_progress(&path) {
            return Err(CommandError::new(
                "busy",
                "that recording is still being written",
            ));
        }
        let preset = Preset::Discord;
        if let Some(id) = self.export_running_for(&key, preset.id()) {
            return Ok(json!({ "action": "export", "id": id, "running": true,
                "reason": "a Discord copy is already being made" }));
        }
        let info = self.clip_info(&lib, &path, &key)?;
        let source = source_of(&info);
        let edit = self.load_project(&lib, &key, &source)?;
        let edited = edit.status != "new" && edit.project != Project::new(source.duration_ms);
        let bytes = std::fs::metadata(&path)
            .map_err(|e| CommandError::new("failed", e.to_string()))?
            .len();
        let limit = u64::from(self.settings().export.discord_limit_mb).saturating_mul(1_000_000);
        match discord_reason(&path, bytes, limit, &info, edited) {
            None => {
                klip_platform::copy_files(&[path])
                    .map_err(|e| CommandError::new("failed", e.to_string()))?;
                Ok(json!({ "action": "copied" }))
            }
            Some(reason) => {
                let (id, out) =
                    self.start_export(ExportKind::Preset(preset), &lib, path, key, &info, true)?;
                Ok(json!({
                    "action": "export",
                    "id": id,
                    "out": out.to_string_lossy(),
                    "reason": reason,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(codec: &str) -> ClipInfo {
        ClipInfo {
            duration_ms: Some(10_000),
            created_unix: 0,
            codec: Some(codec.into()),
            width: 1920,
            height: 1080,
            fps: Some(60.0),
            audio_tracks: 1,
            bookmarks_ms: Vec::new(),
            events: Vec::new(),
        }
    }

    #[test]
    fn discord_takes_small_unedited_h264_mp4s_as_they_are() {
        let p = Path::new("C:/Videos/Scout/Apex/Scout_1.mp4");
        let limit = 10_000_000;
        assert_eq!(discord_reason(p, limit, limit, &info("H.264"), false), None);
        assert_eq!(
            discord_reason(p, limit + 1, limit, &info("H.264"), false).as_deref(),
            Some("it's over Discord's 10 MB")
        );
        assert_eq!(
            discord_reason(p, 5, limit, &info("H.264"), true).as_deref(),
            Some("it has edits")
        );
        assert_eq!(
            discord_reason(p, 5, limit, &info("HEVC"), false).as_deref(),
            Some("Discord may not play HEVC")
        );
        assert_eq!(
            discord_reason(p, 5, limit, &info(""), false).as_deref(),
            Some("Discord may not play it")
        );
        let mkv = Path::new("C:/Videos/Scout/x.mkv");
        assert_eq!(
            discord_reason(mkv, 5, limit, &info("H.264"), false).as_deref(),
            Some("it isn't an MP4")
        );
    }
}
