//! The window's command. `tauri::command` generates a wrapper next to the function whose
//! argument plumbing contains `unreachable!()`; our code never reaches it (every error is
//! returned as a value), so that lint is allowed in this module only.

#![allow(clippy::unreachable)]

use serde_json::{Value, json};

use crate::AppState;

/// The overlay page closes the quick menu (the game gets its focus back).
#[tauri::command]
pub fn overlay_close(app: tauri::AppHandle) {
    crate::overlay::close(&app);
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
