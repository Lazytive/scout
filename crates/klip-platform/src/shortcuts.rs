//! System-wide shortcuts. Linux: the xdg-desktop-portal GlobalShortcuts interface
//! (Wayland has no other way for an app to see keys while another app has focus); the
//! desktop asks the user to confirm or pick the keys the first time and afterwards
//! remembers them for the app id ([`register_app_id`]). Windows: `RegisterHotKey` on a
//! thread of our own (`hotkeys_win`, ADR 0014); the suggested trigger is used as is.
//!
//! Elsewhere [`GlobalShortcuts::bind`] returns [`PlatformError::Unsupported`].

use crate::PlatformError;

/// App id, used by the portals to remember the user's choices (shortcuts, screen-cast
/// restore tokens); the same as the Tauri identifier (ADR 0020).
pub const APP_ID: &str = "io.github.lazytive.Scout";
/// The id before the rename to Scout; its hidden desktop entry is removed.
#[cfg(target_os = "linux")]
const LEGACY_APP_ID: &str = "dev.klip.Klip";

/// A shortcut to ask the desktop for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutSpec {
    /// Stable id, e.g. the command id `clip.save`.
    pub id: String,
    /// Shown to the user in the desktop's dialog and settings.
    pub description: String,
    /// Suggested keys in the shortcuts spec format, e.g. `ALT+F10`.
    pub preferred_trigger: Option<String>,
}

/// A shortcut the desktop bound, with its keys as the desktop describes them (empty when
/// the user left it unassigned).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundShortcut {
    pub id: String,
    pub trigger: String,
    /// Why it isn't active (Windows: keys taken by another app, no keys, unknown key).
    pub error: Option<String>,
}

/// Tell the portals which app this is (host apps only; sandboxed ones already have an
/// id). Must come before any other portal call of the process, else it is refused. The
/// portal looks the id up as a desktop entry; an installed Scout ships one, and for the
/// CLI a hidden one (`NoDisplay=true`, no menu item) is written to
/// `$XDG_DATA_HOME/applications` if it is missing.
pub fn register_app_id(app_id: &str) -> Result<(), PlatformError> {
    #[cfg(target_os = "linux")]
    {
        let id = ashpd::AppID::try_from(app_id)
            .map_err(|e| PlatformError::Os(format!("app id {app_id:?}: {e}")))?;
        let written = linux::ensure_desktop_entry(app_id)?;
        // A just-written entry may take a moment to be noticed by the portal.
        let mut tries: u32 = if written { 10 } else { 1 };
        loop {
            let id = id.clone();
            let res = std::panic::catch_unwind(|| pollster::block_on(ashpd::register_host_app(id)))
                .unwrap_or_else(|_| Err(ashpd::Error::NoResponse));
            tries = tries.saturating_sub(1);
            match res {
                Ok(()) => return Ok(()),
                Err(_) if tries > 0 => std::thread::sleep(std::time::Duration::from_millis(200)),
                Err(e) => {
                    return Err(PlatformError::Os(format!(
                        "registering with the portals: {e}"
                    )));
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app_id;
        Err(PlatformError::Unsupported)
    }
}

/// Bound shortcuts; activations are delivered until this is dropped.
pub struct GlobalShortcuts {
    bound: Vec<BoundShortcut>,
    #[cfg(target_os = "linux")]
    _session: linux::Session,
    #[cfg(windows)]
    _session: crate::hotkeys_win::Session,
}

impl std::fmt::Debug for GlobalShortcuts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalShortcuts")
            .field("bound", &self.bound)
            .finish_non_exhaustive()
    }
}

impl GlobalShortcuts {
    /// Ask the desktop for `shortcuts` (Linux: it may show a dialog and block until the
    /// user answers). `on_activated` gets the id of each shortcut pressed, on a portal or
    /// hotkey thread; it must be quick. On Windows a shortcut that can't be registered is
    /// returned with its `error` set; the others still work.
    pub fn bind(
        shortcuts: &[ShortcutSpec],
        on_activated: Box<dyn Fn(&str) + Send>,
    ) -> Result<GlobalShortcuts, PlatformError> {
        #[cfg(target_os = "linux")]
        {
            let (session, bound) = linux::bind(shortcuts, on_activated)?;
            Ok(GlobalShortcuts {
                bound,
                _session: session,
            })
        }
        #[cfg(windows)]
        {
            let (session, bound) = crate::hotkeys_win::bind(shortcuts, on_activated)?;
            Ok(GlobalShortcuts {
                bound,
                _session: session,
            })
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = (shortcuts, on_activated);
            Err(PlatformError::Unsupported)
        }
    }

    pub fn bound(&self) -> &[BoundShortcut] {
        &self.bound
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::PathBuf;

    use ashpd::desktop::global_shortcuts::{
        BindShortcutsOptions, GlobalShortcuts as Portal, NewShortcut,
    };
    use ashpd::desktop::{CreateSessionOptions, ResponseError};
    use futures_util::StreamExt;
    use futures_util::future::{Either, select};

    use super::{BoundShortcut, LEGACY_APP_ID, ShortcutSpec};
    use crate::PlatformError;

    /// The portal objects live on their own thread until this is dropped.
    pub(super) struct Session {
        stop: Option<futures_channel::oneshot::Sender<()>>,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
        }
    }

    fn os(e: ashpd::Error) -> PlatformError {
        match e {
            ashpd::Error::Response(ResponseError::Cancelled) => PlatformError::Cancelled,
            other => PlatformError::Os(format!("global shortcuts portal: {other}")),
        }
    }

    /// Write a hidden desktop entry for `app_id` unless an identical one exists (atomic:
    /// temp + rename). The portal only accepts entries whose `Exec` program exists, so it
    /// names this executable (or `true` if its path needs quoting). Returns whether it
    /// wrote one.
    pub(super) fn ensure_desktop_entry(app_id: &str) -> Result<bool, PlatformError> {
        // The id comes from a constant, but keep it a plain file name regardless.
        if app_id.is_empty()
            || !app_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        {
            return Err(PlatformError::Os(format!("bad app id {app_id:?}")));
        }
        let data = std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .ok_or_else(|| PlatformError::Os("no XDG_DATA_HOME or HOME".into()))?;
        let dir = data.join("applications");
        // The entry written before the rename (ADR 0020), when it is our small hidden one.
        let old = dir.join(format!("{LEGACY_APP_ID}.desktop"));
        if std::fs::metadata(&old).is_ok_and(|m| m.len() < 4096)
            && std::fs::read_to_string(&old).is_ok_and(|c| c.contains("NoDisplay=true"))
        {
            let _ = std::fs::remove_file(&old);
        }
        let path = dir.join(format!("{app_id}.desktop"));
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .filter(|p| {
                p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-+".contains(&b))
            })
            .unwrap_or_else(|| "true".into());
        let content = format!(
            "[Desktop Entry]\nType=Application\nName=Scout\nComment=Game clip recorder\n\
             Exec={exe}\nTerminal=true\nNoDisplay=true\n"
        );
        // Small file we wrote ourselves; anything bigger is not ours to compare.
        let same = std::fs::metadata(&path).is_ok_and(|m| m.len() < 4096)
            && std::fs::read_to_string(&path).is_ok_and(|c| c == content);
        if same {
            return Ok(false);
        }
        let io = |e: std::io::Error| PlatformError::Os(format!("desktop entry: {e}"));
        std::fs::create_dir_all(&dir).map_err(io)?;
        let tmp = dir.join(format!(".{app_id}.desktop.tmp"));
        std::fs::write(&tmp, content).map_err(io)?;
        std::fs::rename(&tmp, &path).map_err(io)?;
        Ok(true)
    }

    async fn serve(
        specs: &[ShortcutSpec],
        tx: &std::sync::mpsc::Sender<Result<Vec<BoundShortcut>, PlatformError>>,
        mut stop: futures_channel::oneshot::Receiver<()>,
        on_activated: &dyn Fn(&str),
    ) -> Result<(), PlatformError> {
        let proxy = Portal::new().await.map_err(os)?;
        let session = proxy
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(os)?;
        let wanted: Vec<NewShortcut> = specs
            .iter()
            .map(|s| {
                NewShortcut::new(s.id.as_str(), s.description.as_str())
                    .preferred_trigger(s.preferred_trigger.as_deref())
            })
            .collect();
        let response = proxy
            .bind_shortcuts(&session, &wanted, None, BindShortcutsOptions::default())
            .await
            .map_err(os)?
            .response()
            .map_err(os)?;
        let bound = response
            .shortcuts()
            .iter()
            .map(|s| BoundShortcut {
                id: s.id().to_string(),
                trigger: s.trigger_description().to_string(),
                error: None,
            })
            .collect();
        // The portal sends activations only to the session's owner (this connection).
        let activated = proxy.receive_activated().await.map_err(os)?;
        if tx.send(Ok(bound)).is_err() {
            let _ = session.close().await;
            return Ok(());
        }
        let mut activated = std::pin::pin!(activated);
        // Ends when the bus connection ends or we are asked to stop.
        while let Either::Left((Some(a), rest)) = select(activated.next(), stop).await {
            stop = rest;
            on_activated(a.shortcut_id());
        }
        let _ = session.close().await;
        Ok(())
    }

    pub(super) fn bind(
        specs: &[ShortcutSpec],
        on_activated: Box<dyn Fn(&str) + Send>,
    ) -> Result<(Session, Vec<BoundShortcut>), PlatformError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = futures_channel::oneshot::channel::<()>();
        let specs = specs.to_vec();
        std::thread::Builder::new()
            .name("klip-shortcuts".into())
            .spawn(move || {
                // Portal replies are hostile input; the crate asserts on malformed ones.
                let res = catch_unwind(AssertUnwindSafe(|| {
                    pollster::block_on(serve(&specs, &tx, stop_rx, on_activated.as_ref()))
                }));
                let err = match res {
                    Ok(Ok(())) => return,
                    Ok(Err(e)) => e,
                    Err(_) => PlatformError::Os("global shortcuts portal: unexpected reply".into()),
                };
                let _ = tx.send(Err(err));
            })
            .map_err(|e| PlatformError::Os(format!("shortcuts thread: {e}")))?;
        match rx.recv() {
            Ok(Ok(bound)) => Ok((
                Session {
                    stop: Some(stop_tx),
                },
                bound,
            )),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(PlatformError::Os("shortcuts portal thread ended".into())),
        }
    }
}
