//! Screen casting through the xdg-desktop-portal ScreenCast interface: the OS API for
//! capture on Linux (rule 1). The desktop shows its own dialog where the user picks a
//! monitor or window; with a restore token from an earlier session the portal reuses that
//! choice without asking (persist mode "until revoked"). Frames then come from PipeWire
//! (the caller connects `pipewiresrc` to [`ScreenCast::pipewire_fd`] and a stream's node).
//!
//! Elsewhere [`ScreenCast::start`] returns [`PlatformError::Unsupported`].

use crate::PlatformError;

/// What the portal dialog offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenCastSource {
    Monitor,
    Window,
    /// Monitors and windows.
    Any,
}

/// How to open a screen-cast session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenCastRequest {
    pub source: ScreenCastSource,
    /// Draw the cursor into the frames.
    pub show_cursor: bool,
    /// Token from an earlier session ([`ScreenCast::restore_token`]): restores that
    /// choice without a dialog if the portal still knows it. Tokens are single-use.
    pub restore_token: Option<String>,
}

/// One stream of a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenCastStream {
    /// PipeWire node to connect to.
    pub node_id: u32,
    /// Monitor or window, if the portal says.
    pub kind: Option<ScreenCastSource>,
    /// Position in the compositor's logical coordinates (monitors).
    pub position: Option<(i32, i32)>,
    /// Size in logical pixels (the stream itself may be larger on scaled outputs).
    pub size: Option<(i32, i32)>,
}

/// An open screen-cast session. The streams stay available while it lives; dropping it
/// closes the session (the desktop's "sharing" indicator goes away).
pub struct ScreenCast {
    #[cfg(target_os = "linux")]
    inner: linux::Session,
    streams: Vec<ScreenCastStream>,
    restore_token: Option<String>,
}

impl std::fmt::Debug for ScreenCast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenCast")
            .field("streams", &self.streams)
            .field("restore_token", &self.restore_token.is_some())
            .finish_non_exhaustive()
    }
}

impl ScreenCast {
    /// Open a session: may show the desktop's dialog and block until the user answers.
    /// Cancelling gives [`PlatformError::Cancelled`].
    pub fn start(request: &ScreenCastRequest) -> Result<ScreenCast, PlatformError> {
        #[cfg(target_os = "linux")]
        {
            let (inner, streams, restore_token) = linux::start(request)?;
            Ok(ScreenCast {
                inner,
                streams,
                restore_token,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = request;
            Err(PlatformError::Unsupported)
        }
    }

    pub fn streams(&self) -> &[ScreenCastStream] {
        &self.streams
    }

    /// Token to restore this choice next time (store it; it replaces the one used).
    pub fn restore_token(&self) -> Option<&str> {
        self.restore_token.as_deref()
    }

    /// The desktop ended the session (the user stopped sharing); its streams deliver no
    /// more frames.
    pub fn is_closed(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.closed.load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// The PipeWire remote the streams are on. Valid while `self` lives; consumers such as
    /// `pipewiresrc` duplicate it.
    pub fn pipewire_fd(&self) -> i32 {
        #[cfg(target_os = "linux")]
        {
            self.inner.raw_fd()
        }
        #[cfg(not(target_os = "linux"))]
        {
            -1
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use ashpd::desktop::PersistMode;
    use ashpd::desktop::screencast::{
        CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
        StartCastOptions,
    };
    use ashpd::desktop::{CreateSessionOptions, ResponseError};
    use futures_util::StreamExt;
    use futures_util::future::{Either, select};

    use super::{ScreenCastRequest, ScreenCastSource, ScreenCastStream};
    use crate::PlatformError;

    /// The open session. Its portal objects live on their own thread, which waits for the
    /// desktop to close the session or for this to be dropped (then it closes it).
    pub(super) struct Session {
        fd: OwnedFd,
        /// Set when the portal reports the session closed.
        pub(super) closed: Arc<AtomicBool>,
        stop: Option<futures_channel::oneshot::Sender<()>>,
    }

    impl Session {
        pub(super) fn raw_fd(&self) -> i32 {
            self.fd.as_raw_fd()
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // The portal thread closes the session and ends; not waited for, so a slow
            // bus never blocks the caller.
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
        }
    }

    fn os(e: ashpd::Error) -> PlatformError {
        match e {
            ashpd::Error::Response(ResponseError::Cancelled) => PlatformError::Cancelled,
            other => PlatformError::Os(format!("screen-cast portal: {other}")),
        }
    }

    fn kind(t: SourceType) -> Option<ScreenCastSource> {
        match t {
            SourceType::Monitor => Some(ScreenCastSource::Monitor),
            SourceType::Window => Some(ScreenCastSource::Window),
            _ => None,
        }
    }

    type Opened = (Vec<ScreenCastStream>, Option<String>, OwnedFd);
    type Started = (Session, Vec<ScreenCastStream>, Option<String>);

    /// Open the session, report it on `tx`, then hold it until the desktop closes it
    /// (`closed`) or `stop` fires.
    async fn serve(
        request: &ScreenCastRequest,
        tx: &std::sync::mpsc::Sender<Result<Opened, PlatformError>>,
        stop: futures_channel::oneshot::Receiver<()>,
        closed: &AtomicBool,
    ) -> Result<(), PlatformError> {
        let proxy = Screencast::new().await.map_err(os)?;
        let sources = match request.source {
            ScreenCastSource::Monitor => SourceType::Monitor.into(),
            ScreenCastSource::Window => SourceType::Window.into(),
            ScreenCastSource::Any => SourceType::Monitor | SourceType::Window,
        };
        let available = proxy.available_cursor_modes().await.map_err(os)?;
        let cursor = if request.show_cursor && available.contains(CursorMode::Embedded) {
            CursorMode::Embedded
        } else {
            CursorMode::Hidden
        };
        let session = proxy
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(os)?;
        let options = SelectSourcesOptions::default()
            .set_sources(sources)
            .set_cursor_mode(cursor)
            .set_multiple(false)
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(request.restore_token.as_deref());
        proxy
            .select_sources(&session, options)
            .await
            .map_err(os)?
            .response()
            .map_err(os)?;
        let started = proxy
            .start(&session, None, StartCastOptions::default())
            .await
            .map_err(os)?
            .response()
            .map_err(os)?;
        let streams: Vec<ScreenCastStream> = started
            .streams()
            .iter()
            .map(|s| ScreenCastStream {
                node_id: s.pipe_wire_node_id(),
                kind: s.source_type().and_then(kind),
                position: s.position(),
                size: s.size(),
            })
            .collect();
        if streams.is_empty() {
            return Err(PlatformError::Os(
                "screen-cast portal returned no streams".into(),
            ));
        }
        let token = started.restore_token().map(str::to_string);
        let fd = proxy
            .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
            .await
            .map_err(os)?;
        let signals = session.receive_closed().await;
        if tx.send(Ok((streams, token, fd))).is_err() {
            let _ = session.close().await;
            return Ok(());
        }
        match signals {
            Ok(signals) => {
                let mut signals = std::pin::pin!(signals);
                let next = signals.next();
                match select(std::pin::pin!(next), stop).await {
                    // Closed by the desktop, or the bus connection ended.
                    Either::Left(_) => {
                        closed.store(true, Ordering::Relaxed);
                        return Ok(());
                    }
                    Either::Right(_) => {}
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot watch the screen-cast session");
                let _ = stop.await;
            }
        }
        if let Err(e) = session.close().await {
            tracing::debug!(error = %e, "closing the screen-cast session failed");
        }
        Ok(())
    }

    pub(super) fn start(request: &ScreenCastRequest) -> Result<Started, PlatformError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = futures_channel::oneshot::channel::<()>();
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let request = request.clone();
        std::thread::Builder::new()
            .name("klip-screencast".into())
            .spawn(move || {
                // The portal crate asserts on malformed replies; a hostile or buggy portal
                // must not take the app down.
                let res = catch_unwind(AssertUnwindSafe(|| {
                    pollster::block_on(serve(&request, &tx, stop_rx, &flag))
                }));
                let err = match res {
                    Ok(Ok(())) => return,
                    Ok(Err(e)) => e,
                    Err(_) => PlatformError::Os("screen-cast portal: unexpected reply".into()),
                };
                // After a successful open the receiver may be gone; then this is a no-op.
                let _ = tx.send(Err(err));
            })
            .map_err(|e| PlatformError::Os(format!("screen-cast thread: {e}")))?;
        // Waits for the user's answer in the dialog (no time limit: it is their choice).
        match rx.recv() {
            Ok(Ok((streams, token, fd))) => Ok((
                Session {
                    fd,
                    closed,
                    stop: Some(stop_tx),
                },
                streams,
                token,
            )),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(PlatformError::Os("screen-cast portal thread ended".into())),
        }
    }
}
