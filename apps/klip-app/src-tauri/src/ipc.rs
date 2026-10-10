//! The window's command. `tauri::command` generates a wrapper next to the function whose
//! argument plumbing contains `unreachable!()`; our code never reaches it (every error is
//! returned as a value), so that lint is allowed in this module only.

#![allow(clippy::unreachable)]

use serde_json::{Value, json};
use tauri::Manager as _;

use crate::AppState;

/// The overlay page closes the quick menu (the game gets its focus back).
#[tauri::command]
pub fn overlay_close(app: tauri::AppHandle) {
    crate::overlay::close(&app);
}

/// Most files the preview may read at once (a montage's clips, their sound tracks,
/// stickers and music); the oldest allowed file is forbidden again beyond it.
pub(crate) const MAX_MEDIA_FILES: usize = 256;

/// The files an engine reply names for the preview: the clip or proxy it plays
/// (`editor.proxy`), its sound tracks (`editor.audio`), the project's checked pictures
/// and sounds (`editor.files`).
fn named_files(what: &str, reply: &Value) -> Vec<std::path::PathBuf> {
    let paths: Vec<&str> = match what {
        "proxy" => reply
            .get("path")
            .and_then(Value::as_str)
            .into_iter()
            .collect(),
        "audio" => reply
            .get("tracks")
            .and_then(Value::as_array)
            .map(|t| t.iter().filter_map(|t| t.get("path")?.as_str()).collect())
            .unwrap_or_default(),
        _ => reply
            .get("files")
            .and_then(Value::as_array)
            .map(|f| {
                f.iter()
                    .filter(|f| f.get("ok").and_then(Value::as_bool) == Some(true))
                    .filter_map(|f| f.get("path")?.as_str())
                    .collect()
            })
            .unwrap_or_default(),
    };
    paths
        .into_iter()
        .take(MAX_MEDIA_FILES)
        .map(std::path::PathBuf::from)
        .collect()
}

/// What the editor's preview may play for a clip, made readable through the asset
/// protocol (`convertFileSrc` in the page): `what` is `proxy` (default; the clip or
/// its proxy), `audio` (its sound tracks) or `files` (the project's pictures and
/// sounds). Only files the engine named are allowed, never a path the page chose; the
/// set is bounded and the oldest goes first.
#[tauri::command]
pub async fn editor_media(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
    what: Option<String>,
) -> Result<Value, Value> {
    let what = what.unwrap_or_else(|| "proxy".into());
    let cmd = match what.as_str() {
        "proxy" => klip_control::commands::EDITOR_PROXY,
        "audio" => klip_control::commands::EDITOR_AUDIO,
        "files" => klip_control::commands::EDITOR_FILES,
        _ => {
            return Err(
                json!({ "code": "bad_args", "message": "what must be proxy, audio or files" }),
            );
        }
    };
    let h = state.handler.clone();
    let media = tauri::async_runtime::spawn_blocking(move || {
        h.handle(cmd, &json!({ "path": path }))
            .map_err(|e| json!({ "code": e.code, "message": e.message }))
    })
    .await
    .map_err(|e| json!({ "code": "internal", "message": e.to_string() }))??;
    let scope = app.asset_protocol_scope();
    let files = named_files(&what, &media);
    let dropped: Vec<std::path::PathBuf> = {
        let mut m = state
            .media
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for f in &files {
            m.retain(|old| old != f);
            m.push_back(f.clone());
        }
        let over = m.len().saturating_sub(MAX_MEDIA_FILES);
        m.drain(..over).collect()
    };
    for old in dropped {
        let _ = scope.forbid_file(&old);
    }
    for f in &files {
        scope
            .allow_file(f)
            .map_err(|e| json!({ "code": "failed", "message": e.to_string() }))?;
    }
    Ok(media)
}

/// The window's one command: run an engine command.
#[tauri::command]
pub async fn klip(
    state: tauri::State<'_, AppState>,
    cmd: String,
    args: Value,
) -> Result<Value, Value> {
    let h = state.handler.clone();
    tauri::async_runtime::spawn_blocking(move || {
        h.handle(&cmd, &args)
            .map_err(|e| json!({ "code": e.code, "message": e.message }))
    })
    .await
    .map_err(|e| json!({ "code": "internal", "message": e.to_string() }))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_the_engine_named_and_checked_are_allowed() {
        let proxy = json!({ "status": "original", "path": "C:/clips/a.mp4" });
        assert_eq!(
            named_files("proxy", &proxy),
            [std::path::PathBuf::from("C:/clips/a.mp4")]
        );
        assert!(named_files("proxy", &json!({ "status": "making", "id": 3 })).is_empty());
        let audio = json!({ "tracks": [{ "path": "C:/a/0.m4a" }, { "name": "x" }, { "path": 5 }] });
        assert_eq!(
            named_files("audio", &audio),
            [std::path::PathBuf::from("C:/a/0.m4a")]
        );
        let files = json!({ "files": [
            { "path": "C:/s.png", "ok": true },
            { "path": "C:/tool.exe", "ok": false },
            { "path": "C:/x.wav" },
        ] });
        assert_eq!(
            named_files("files", &files),
            [std::path::PathBuf::from("C:/s.png")]
        );
        let many = json!({ "tracks": (0..1000).map(|i| json!({ "path": format!("C:/{i}.m4a") })).collect::<Vec<_>>() });
        assert_eq!(named_files("audio", &many).len(), MAX_MEDIA_FILES);
    }
}
