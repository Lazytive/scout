//! What the overlay needs from the engine: a thumbnail of each saved clip (made after the
//! save, so the pop-up appears at once and the picture follows) and a preview command.
//! The thumbnail is also the clip's library poster; its hover-scrub strip is queued for
//! the library's thumbnail thread (made once the game is over). The poster is decoded by
//! a one-shot media worker process (ADR 0016), so no decoder stays in the engine.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use klip_control::CommandError;
use klip_library::{Library, ThumbKind};
use serde_json::{Value, json};

use super::{Inner, lock};
use crate::config;
use crate::events::Event;
use crate::media_worker::{MediaWorker, ThumbJob};

/// How far before the clip's end the thumbnail is taken (the moment of the hotkey).
const THUMB_BACK: Duration = crate::media_worker::POSTER_BACK;
/// A poster job, including the worker's start.
const POSTER_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest poster read back from the worker.
const MAX_POSTER: u64 = 8 * 1024 * 1024;

/// Unique names for posters on their way from the worker.
static POSTER_SEQ: AtomicU64 = AtomicU64::new(0);

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

    /// The clip's poster JPEG, decoded by a one-shot media worker (it exits right after,
    /// taking the decoder with it), or here without one (tests).
    fn poster_jpeg(&self, clip: &Path) -> Result<Vec<u8>, String> {
        let Some(exe) = &self.media_worker else {
            return klip_capture::thumbnail::jpeg_near_end(
                clip,
                THUMB_BACK,
                klip_library::POSTER_WIDTH,
            )
            .map_err(|e| e.to_string());
        };
        let out = std::env::temp_dir().join(format!(
            "klip-poster-{}-{}.jpg",
            std::process::id(),
            POSTER_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let job = ThumbJob {
            id: 1,
            clip: clip.to_path_buf(),
            poster: Some(out.clone()),
            poster_width: klip_library::POSTER_WIDTH,
            strip: None,
            tiles: 0,
            tile_width: 0,
        };
        let r = MediaWorker::isolated(exe.clone())
            .run(&job, POSTER_TIMEOUT)
            .and_then(|()| {
                let mut jpeg = Vec::new();
                std::fs::File::open(&out)
                    .and_then(|f| f.take(MAX_POSTER).read_to_end(&mut jpeg))
                    .map_err(|e| format!("reading the poster: {e}"))?;
                Ok(jpeg)
            });
        let _ = std::fs::remove_file(&out);
        r
    }

    fn make_thumbnail(&self, clip: &Path) {
        let jpeg = match self.poster_jpeg(clip) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "thumbnail");
                return;
            }
        };
        let settings = self.settings();
        let lib = config::clip_base(&settings).ok().map(Library::new);
        let key = lib.as_ref().and_then(|l| l.key(clip).ok());
        let cached: Option<PathBuf> = lib
            .as_ref()
            .zip(key.as_deref())
            .and_then(|(l, k)| l.thumb_path(k, ThumbKind::Poster));
        let mut written = false;
        if let Some(p) = &cached {
            let _guard = lock(&self.library_lock);
            match write_atomic(p, &jpeg) {
                Ok(()) => written = true,
                Err(e) => tracing::warn!(error = %e, "could not cache a thumbnail"),
            }
        }
        if let (Some(lib), Some(key)) = (&lib, key) {
            if written {
                self.emit(&Event::ThumbsReady {
                    key: key.clone(),
                    poster: Some(super::library::now_ms().max(1)),
                    strip: None,
                    failed: false,
                });
            }
            self.queue_new_clip(lib.root(), key);
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
