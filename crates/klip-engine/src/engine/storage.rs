//! Storage management (ADR 0014): keep the clip folder under the size cap by moving the
//! oldest unstarred clips to the Recycle Bin / Trash (or deleting them, a setting), and
//! warn when the clip drive runs low.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use klip_control::CommandError;
use klip_library::{GIB, Library};
use klip_settings::{OverCap, Settings};
use serde_json::{Value, json};

use super::{Inner, lock};
use crate::config;
use crate::events::Event;

/// At most one low-disk warning per this long.
const LOW_DISK_EVERY: Duration = Duration::from_secs(30 * 60);

impl Inner {
    fn library(settings: &Settings) -> Result<Library, CommandError> {
        config::clip_base(settings)
            .map(Library::new)
            .map_err(|e| CommandError::new("failed", e.to_string()))
    }

    /// Bring the clip folder under the cap. `protect`: never removed (the clip just
    /// saved).
    pub(super) fn cleanup(&self, protect: &[PathBuf]) -> Result<Value, CommandError> {
        let settings = self.settings();
        let lib = Self::library(&settings)?;
        let cap = u64::from(settings.storage.cap_gb).saturating_mul(GIB);
        let _guard = lock(&self.library_lock);
        let files = lib.scan();
        let mut meta = lib.load_meta();
        let doomed = klip_library::over_cap(&files, &meta, cap, protect);
        if doomed.is_empty() {
            return Ok(json!({ "removed": 0, "bytes": 0, "failed": [] }));
        }
        let paths: Vec<PathBuf> = doomed.iter().map(|f| f.path.clone()).collect();
        let to_trash = settings.storage.over_cap == OverCap::Trash;
        let results: Vec<Result<(), String>> = if to_trash {
            klip_platform::move_to_trash(&paths)
                .into_iter()
                .map(|r| r.map_err(|e| e.to_string()))
                .collect()
        } else {
            paths
                .iter()
                .map(|p| std::fs::remove_file(p).map_err(|e| e.to_string()))
                .collect()
        };
        let mut gone = Vec::new();
        let mut bytes = 0u64;
        let mut failed = Vec::new();
        for (f, r) in doomed.iter().zip(results) {
            match r {
                Ok(()) => {
                    gone.push(f.key.clone());
                    bytes = bytes.saturating_add(f.bytes);
                }
                Err(e) => failed.push(json!({ "clip": f.key, "error": e })),
            }
        }
        if klip_library::forget(&mut meta, &gone)
            && let Err(e) = lib.save_meta(&meta)
        {
            tracing::warn!(error = %e, "could not save the clip metadata");
        }
        let report = json!({
            "removed": gone.len(),
            "bytes": bytes,
            "to_trash": to_trash,
            "failed": failed,
        });
        tracing::info!(removed = gone.len(), bytes, "clip folder over its cap");
        self.emit(&Event::StorageCleaned {
            report: report.clone(),
        });
        Ok(report)
    }

    /// Run [`Inner::cleanup`] on a job thread (one at a time).
    pub(super) fn cleanup_in_background(&self, protect: Vec<PathBuf>) {
        if self.cleaning.swap(true, Ordering::AcqRel) {
            return;
        }
        let me = self.me.clone();
        let spawned = std::thread::Builder::new()
            .name("klip-storage".into())
            .spawn(move || {
                if let Some(inner) = me.upgrade() {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if let Err(e) = inner.cleanup(&protect) {
                            tracing::warn!(error = %e.message, "storage clean-up");
                        }
                        inner.check_disk(false);
                    }));
                    if r.is_err() {
                        tracing::error!("storage clean-up panicked");
                    }
                    inner.cleaning.store(false, Ordering::Release);
                }
            });
        if spawned.is_err() {
            self.cleaning.store(false, Ordering::Release);
        }
    }

    /// Free space on the clip drive; warns (at most every 30 min unless `force`) when it
    /// is below the setting.
    pub(super) fn check_disk(&self, force: bool) -> Option<(u64, bool)> {
        let settings = self.settings();
        let base = config::clip_base(&settings).ok()?;
        let free = free_space(&base)?;
        let threshold = u64::from(settings.storage.low_disk_gb).saturating_mul(GIB);
        let low = threshold > 0 && free < threshold;
        if low {
            let mut last = lock(&self.low_disk_warned);
            if force || last.is_none_or(|t| t.elapsed() >= LOW_DISK_EVERY) {
                *last = Some(Instant::now());
                drop(last);
                // Rounded down to whole GB; the folder isn't named (it may hold the
                // user's name).
                let gb = free.checked_div(GIB).unwrap_or(0);
                self.emit(&Event::Warning {
                    message: format!("Low disk space: {gb} GB free on the clip drive"),
                });
            }
        }
        Some((free, low))
    }

    pub(super) fn storage_status(&self) -> Result<Value, CommandError> {
        let settings = self.settings();
        let lib = Self::library(&settings)?;
        let (files, meta) = {
            let _guard = lock(&self.library_lock);
            (lib.scan(), lib.load_meta())
        };
        let usage = klip_library::usage(&files, &meta);
        let disk = self.check_disk(false);
        Ok(json!({
            "folder": lib.root().to_string_lossy(),
            "cap_bytes": u64::from(settings.storage.cap_gb).saturating_mul(GIB),
            "over_cap": settings.storage.over_cap,
            "usage": usage,
            "free_bytes": disk.map(|d| d.0),
            "low_disk": disk.is_some_and(|d| d.1),
        }))
    }

    pub(super) fn star(&self, args: &Value) -> Result<Value, CommandError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| CommandError::bad_args("path must be a string"))?;
        let starred = args
            .get("starred")
            .and_then(Value::as_bool)
            .ok_or_else(|| CommandError::bad_args("starred must be true or false"))?;
        let settings = self.settings();
        let lib = Self::library(&settings)?;
        let _guard = lock(&self.library_lock);
        let p = Path::new(path);
        if !p.is_file() {
            return Err(CommandError::bad_args("no such clip"));
        }
        lib.set_starred(p, starred)
            .map_err(|e| CommandError::bad_args(e.to_string()))?;
        self.emit(&Event::LibraryChanged {
            reason: "starred".into(),
        });
        Ok(json!({ "starred": starred }))
    }
}

fn free_space(dir: &Path) -> Option<u64> {
    // The folder may not exist yet: ask about the nearest existing parent.
    let mut d = Some(dir);
    while let Some(p) = d {
        if p.exists() {
            return klip_platform::free_disk_space(p).ok();
        }
        d = p.parent();
    }
    None
}
