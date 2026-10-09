//! What the overlay needs from the engine: a thumbnail of each saved clip (made after the
//! save, so the pop-up appears at once and the picture follows) and a preview command.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use klip_control::CommandError;
use klip_library::{Library, META_DIR};
use serde_json::{Value, json};

use super::{Inner, lock};
use crate::config;
use crate::events::Event;

/// Thumbnail width in pixels.
pub const THUMB_WIDTH: u32 = 320;
/// How far before the clip's end the thumbnail is taken (the moment of the hotkey).
const THUMB_BACK: Duration = Duration::from_millis(500);

/// `<clips>/.klip/thumbs/<key with / as ~>.jpg`.
pub fn thumb_path(lib: &Library, clip: &Path) -> Option<PathBuf> {
    let key = lib.key(clip).ok()?;
    Some(
        lib.root()
            .join(META_DIR)
            .join("thumbs")
            .join(format!("{}.jpg", key.replace('/', "~"))),
    )
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("jpg.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

impl Inner {
    /// Make the clip's thumbnail on a job thread; emits `thumbnail` when done.
    pub(super) fn thumbnail_in_background(&self, clip: PathBuf) {
        let me = self.me.clone();
        let spawned = std::thread::Builder::new()
            .name("klip-thumbnail".into())
            .spawn(move || {
                let Some(inner) = me.upgrade() else { return };
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    inner.make_thumbnail(&clip);
                }));
                if r.is_err() {
                    tracing::error!("thumbnail job panicked");
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "no thread for a thumbnail");
        }
    }

    fn make_thumbnail(&self, clip: &Path) {
        let jpeg = match klip_capture::thumbnail::jpeg_near_end(clip, THUMB_BACK, THUMB_WIDTH) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "thumbnail");
                return;
            }
        };
        let settings = self.settings();
        let cached = config::clip_base(&settings)
            .ok()
            .map(Library::new)
            .and_then(|lib| thumb_path(&lib, clip));
        if let Some(p) = &cached {
            let _guard = lock(&self.library_lock);
            if let Err(e) = write_atomic(p, &jpeg) {
                tracing::warn!(error = %e, "could not cache a thumbnail");
            }
        }
        let data_url = format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&jpeg)
        );
        self.emit(&Event::Thumbnail {
            clip: clip.to_string_lossy().into_owned(),
            path: cached.map(|p| p.to_string_lossy().into_owned()),
            data_url,
        });
    }

    /// Show the overlay as if a clip had been saved (settings page, tests).
    pub(super) fn overlay_preview(&self) -> Result<Value, CommandError> {
        let settings = self.settings();
        let secs = settings
            .hotkeys
            .iter()
            .find(|h| h.action == klip_settings::HotkeyAction::ClipSave)
            .map_or(30, |h| h.secs);
        let game = lock(&self.state).game.as_ref().map(|g| g.name.clone());
        self.emit(&Event::OverlayPreview {
            game: game.clone(),
            duration_ms: u64::from(secs).saturating_mul(1000),
        });
        Ok(json!({ "shown": true, "game": game }))
    }
}
