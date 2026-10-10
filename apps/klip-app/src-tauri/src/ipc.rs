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

/// What the editor's preview plays for a clip (`editor.proxy`), made readable through
/// the asset protocol (`convertFileSrc` in the page): only this one file, and only
/// what the engine named (a clip in the folder or its proxy), never a path the page
/// chose. The previously allowed file is forbidden again.
#[tauri::command]
pub async fn editor_media(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<Value, Value> {
    let h = state.handler.clone();
    let media = tauri::async_runtime::spawn_blocking(move || {
        h.handle(
            klip_control::commands::EDITOR_PROXY,
            &json!({ "path": path }),
        )
        .map_err(|e| json!({ "code": e.code, "message": e.message }))
    })
    .await
    .map_err(|e| json!({ "code": "internal", "message": e.to_string() }))??;
    if let Some(file) = media.get("path").and_then(Value::as_str) {
        let file = std::path::PathBuf::from(file);
        let scope = app.asset_protocol_scope();
        let previous = {
            let mut m = state
                .media
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            m.replace(file.clone())
        };
        if let Some(old) = previous.filter(|old| *old != file) {
            let _ = scope.forbid_file(&old);
        }
        scope
            .allow_file(&file)
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
