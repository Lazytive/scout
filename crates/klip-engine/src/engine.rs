//! The engine: settings, the game watcher, the recorder's lifecycle, hotkeys, commands.
//!
//! Threads: `klip-watch` polls for games (every [`POLL`]) and is the only one that
//! starts and stops recorders; hotkey presses run their action on a short-lived job
//! thread (one at a time per hotkey); commands run on the caller's thread (control
//! channel, UI). Locks are held only for short reads and swaps, never across a recorder
//! start, stop or clip save. The recorder runs in a child process (ADR 0016,
//! [`Isolation`]).

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use klip_control::{CommandError, Handler, Server, commands};
use klip_platform::{BoundShortcut, GlobalShortcuts, ShortcutSpec};
use klip_probe::ProbeCache;
use klip_settings::{
    BufferMode, HotkeyAction, HotkeyBinding, Issue, LoadStatus, Settings, SettingsError,
};
use serde_json::{Value, json};

mod editor;
mod export;
mod host;
mod library;
mod overlay;
mod setup;
mod storage;

pub use host::Isolation;
use host::RecorderHandle;

use crate::events::{Bus, Event};
use crate::games::{self, DetectedGame};
use crate::media_worker::MediaWorker;
use crate::{EngineError, config};

/// How often the watcher looks for games.
pub const POLL: Duration = Duration::from_secs(2);
/// Wait before trying again after a recorder failed to start.
pub const RETRY: Duration = Duration::from_secs(30);

/// How to start the engine.
pub struct EngineOptions {
    pub settings_path: PathBuf,
    /// Control-channel discovery file; `None`: the default location.
    pub discovery: Option<PathBuf>,
    pub poll: Duration,
    /// A first subscriber, so it also sees `ready` and what happens during start.
    pub events: Option<SyncSender<Event>>,
    /// Program that hosts the media worker (it calls
    /// `klip_engine::media_worker::worker_main` first thing); `None`: library thumbnails
    /// are made in this process (tests). Default: this executable.
    pub media_worker: Option<PathBuf>,
    /// Where the recorder runs; default [`Isolation::auto`]: a child process when the
    /// executable calls [`crate::worker_main`].
    pub isolation: Isolation,
}

impl EngineOptions {
    pub fn new(settings_path: PathBuf) -> EngineOptions {
        EngineOptions {
            settings_path,
            discovery: None,
            poll: POLL,
            events: None,
            media_worker: std::env::current_exe().ok(),
            isolation: Isolation::auto(),
        }
    }
}

/// `<config dir>/Klip/settings.json`.
pub fn default_settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Klip").join("settings.json"))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn status_name(s: LoadStatus) -> &'static str {
    match s {
        LoadStatus::Missing => "missing",
        LoadStatus::Loaded => "loaded",
        LoadStatus::Repaired => "repaired",
        LoadStatus::Reset => "reset",
    }
}

fn action_id(a: HotkeyAction) -> &'static str {
    match a {
        HotkeyAction::ClipSave => commands::CLIP_SAVE,
        HotkeyAction::SessionToggle => commands::SESSION_TOGGLE,
        HotkeyAction::Bookmark => commands::BOOKMARK_ADD,
        HotkeyAction::QuickMenu => commands::OVERLAY_MENU,
    }
}

fn describe(b: &HotkeyBinding) -> String {
    match b.action {
        HotkeyAction::ClipSave => format!("Save a clip of the last {} s", b.secs),
        HotkeyAction::SessionToggle => "Start or stop recording".into(),
        HotkeyAction::Bookmark => "Bookmark this moment".into(),
        HotkeyAction::QuickMenu => "Open the quick menu".into(),
    }
}

/// The running replay buffer.
struct Current {
    recorder: Arc<RecorderHandle>,
    clip_dir: PathBuf,
}

#[derive(Default)]
struct State {
    /// The game being followed (running).
    game: Option<DetectedGame>,
    /// Its recorder, or the desktop's when `game` is `None` (Always mode).
    current: Option<Current>,
    starting: bool,
    retry_at: Option<Instant>,
    last_error: Option<String>,
    /// Recorder processes of the followed game that ended unexpectedly.
    crashes: u32,
    /// Finalise recordings a dead recorder process left, at this time.
    recover_at: Option<Instant>,
}

pub(crate) struct Inner {
    settings_path: PathBuf,
    isolation: Isolation,
    settings: Mutex<Settings>,
    load_status: LoadStatus,
    load_issues: Vec<Issue>,
    bus: Bus,
    state: Mutex<State>,
    shortcuts: Mutex<Option<GlobalShortcuts>>,
    bindings: Mutex<HashMap<String, HotkeyBinding>>,
    bound: Mutex<Vec<Value>>,
    busy: Mutex<HashSet<String>>,
    cache: Mutex<(ProbeCache, Option<PathBuf>)>,
    /// Serialises read-modify-write of the clip metadata.
    library_lock: Mutex<()>,
    /// The library's info index (lock after `library_lock`, before `thumbs`).
    library: Mutex<library::LibState>,
    /// Clips waiting for thumbnails, and the thread making them.
    thumbs: (Mutex<library::ThumbQueue>, Condvar),
    thumbs_thread: Mutex<Option<JoinHandle<()>>>,
    media_worker: Option<PathBuf>,
    /// Draws `editor.frame` pictures (a worker of its own; stopped when idle).
    frames: Mutex<MediaWorker>,
    /// A storage clean-up job is running.
    cleaning: AtomicBool,
    /// The running export and the last one (lock alone, briefly).
    exports: Mutex<export::ExportState>,
    low_disk_warned: Mutex<Option<Instant>>,
    /// Microphone meter and when it started.
    meter: Mutex<Option<(klip_platform::AudioCapture, Instant)>>,
    /// Hotkeys are unbound (`hotkeys.pause`) since then.
    hotkeys_paused: Mutex<Option<Instant>>,
    wake: (Mutex<bool>, Condvar),
    stop: AtomicBool,
    exit: AtomicBool,
    me: Weak<Inner>,
}

/// The running engine. Dropping it stops everything (the recorder finalises a running
/// recording first).
pub struct Engine {
    inner: Arc<Inner>,
    watcher: Option<JoinHandle<()>>,
    server: Option<Server>,
}

impl Engine {
    /// Load the settings, bind hotkeys, open the control channel (if on) and start
    /// watching for games.
    pub fn start(opts: EngineOptions) -> Result<Engine, EngineError> {
        let loaded = klip_settings::load(&opts.settings_path);
        for i in &loaded.issues {
            tracing::warn!(issue = %i, "settings file");
        }
        if loaded.status != LoadStatus::Loaded
            && let Err(e) = klip_settings::save(&opts.settings_path, &loaded.settings)
        {
            tracing::warn!(error = %e, "could not write the settings file");
        }
        if let Err(e) = klip_probe::init() {
            tracing::warn!(error = %e, "encoder set-up");
        }
        let control = loaded.settings.app.control_channel;
        let inner = Arc::new_cyclic(|me| Inner {
            settings_path: opts.settings_path.clone(),
            isolation: opts.isolation.clone(),
            settings: Mutex::new(loaded.settings),
            load_status: loaded.status,
            load_issues: loaded.issues,
            bus: Bus::default(),
            state: Mutex::new(State::default()),
            shortcuts: Mutex::new(None),
            bindings: Mutex::new(HashMap::new()),
            bound: Mutex::new(Vec::new()),
            busy: Mutex::new(HashSet::new()),
            cache: Mutex::new(config::load_encoder_cache()),
            library_lock: Mutex::new(()),
            library: Mutex::new(library::LibState::default()),
            thumbs: (Mutex::new(library::ThumbQueue::default()), Condvar::new()),
            thumbs_thread: Mutex::new(None),
            media_worker: opts.media_worker.clone(),
            frames: Mutex::new(match &opts.media_worker {
                Some(exe) => MediaWorker::isolated(exe.clone()),
                None => MediaWorker::in_process(),
            }),
            cleaning: AtomicBool::new(false),
            exports: Mutex::new(export::ExportState::default()),
            low_disk_warned: Mutex::new(None),
            meter: Mutex::new(None),
            hotkeys_paused: Mutex::new(None),
            wake: (Mutex::new(false), Condvar::new()),
            stop: AtomicBool::new(false),
            exit: AtomicBool::new(false),
            me: me.clone(),
        });
        if let Some(tx) = opts.events {
            inner.bus.add(tx);
        }
        inner.bind_hotkeys();
        let server = if control {
            let discovery = opts
                .discovery
                .clone()
                .or_else(klip_control::default_discovery_path);
            let handler: Arc<dyn Handler> = inner.clone();
            match Server::start(handler, discovery.as_deref()) {
                Ok(s) => Some(s),
                Err(e) => {
                    let message = format!("control channel: {e}");
                    tracing::warn!("{message}");
                    inner.bus.emit(&Event::Warning { message });
                    None
                }
            }
        } else {
            None
        };
        inner.bus.emit(&Event::Ready {
            version: env!("CARGO_PKG_VERSION"),
            settings: status_name(inner.load_status),
            issues: inner.load_issues.iter().map(ToString::to_string).collect(),
            port: server.as_ref().map(Server::port),
        });
        let w = inner.clone();
        let poll = opts.poll;
        let watcher = std::thread::Builder::new()
            .name("klip-watch".into())
            .spawn(move || w.watch(poll))
            .map_err(EngineError::Io)?;
        Ok(Engine {
            inner,
            watcher: Some(watcher),
            server,
        })
    }

    pub fn subscribe(&self) -> Receiver<Event> {
        self.inner.bus.subscribe()
    }

    /// Run a command (what the UI and the control channel do).
    pub fn handle(&self, cmd: &str, args: &Value) -> Result<Value, CommandError> {
        self.inner.handle(cmd, args)
    }

    /// The command handler, e.g. for another transport.
    pub fn handler(&self) -> Arc<dyn Handler> {
        self.inner.clone()
    }

    /// `recorder.stop` / `app.quit` was asked for: the front end should exit.
    pub fn exit_requested(&self) -> bool {
        self.inner.exit.load(Ordering::Relaxed)
    }

    /// Control channel port, if it is open.
    pub fn port(&self) -> Option<u16> {
        self.server.as_ref().map(Server::port)
    }

    /// Stop watching, stop the recorder (finalising a running recording), unbind hotkeys
    /// and close the control channel.
    pub fn shutdown(mut self) {
        self.stop_all();
    }

    fn stop_all(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        self.inner.wake();
        self.inner.stop_thumbs();
        self.inner.cancel_export();
        if let Some(s) = self.server.take() {
            s.shutdown();
        }
        if let Some(w) = self.watcher.take() {
            let _ = w.join();
        }
        // The watcher stops the recorder on its way out; this covers a panicked one.
        self.inner.stop_recorder("shutdown");
        let old = lock(&self.inner.shortcuts).take();
        drop(old);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop_all();
    }
}

impl Inner {
    fn emit(&self, e: &Event) {
        self.bus.emit(e);
    }

    fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    fn wake(&self) {
        *lock(&self.wake.0) = true;
        self.wake.1.notify_all();
    }

    fn sleep(&self, d: Duration) {
        let mut woken = lock(&self.wake.0);
        if !*woken {
            woken = match self.wake.1.wait_timeout(woken, d) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        *woken = false;
    }

    fn watch(&self, poll: Duration) {
        if catch_unwind(AssertUnwindSafe(|| self.recover())).is_err() {
            tracing::error!("recovering recordings panicked");
        }
        self.cleanup_in_background(Vec::new());
        while !self.stop.load(Ordering::Relaxed) {
            self.meter_watchdog();
            self.hotkeys_watchdog();
            let due = {
                let mut st = lock(&self.state);
                let due = st.recover_at.is_some_and(|t| Instant::now() >= t);
                if due {
                    st.recover_at = None;
                }
                due
            };
            if due && catch_unwind(AssertUnwindSafe(|| self.recover())).is_err() {
                tracing::error!("recovering recordings panicked");
            }
            if catch_unwind(AssertUnwindSafe(|| self.tick())).is_err() {
                tracing::error!("game watcher panicked; carrying on");
            }
            self.sleep(poll);
        }
        self.stop_recorder("shutdown");
    }

    /// Finalise recordings a killed Klip left (recordings folder and its game folders).
    fn recover(&self) {
        let Ok(base) = config::recordings_base(&self.settings()) else {
            return;
        };
        let mut dirs = vec![base.clone()];
        if let Ok(rd) = std::fs::read_dir(&base) {
            dirs.extend(
                rd.flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .take(1000),
            );
        }
        for d in dirs {
            for r in klip_recorder::recover_recordings(&d) {
                let result = match &r.result {
                    Ok(kept) => json!({ "kept": kept }),
                    Err(e) => json!({ "error": e }),
                };
                self.emit(&Event::Recovered {
                    path: r.path.to_string_lossy().into_owned(),
                    result,
                });
            }
        }
    }

    fn may_start(&self) -> bool {
        lock(&self.state)
            .retry_at
            .is_none_or(|t| Instant::now() >= t)
    }

    /// One look at what runs; start or stop the recorder accordingly.
    fn tick(&self) {
        self.reap_crashed_recorder();
        // A frame being drawn holds the lock; it is stopped on a later tick.
        if let Ok(mut frames) = self.frames.try_lock() {
            frames.idle();
        }
        let settings = self.settings();
        let mode = settings.games.buffer;
        if mode == BufferMode::Off {
            self.stop_recorder("replay buffer off");
            self.forget_game();
            return;
        }
        let snap = match games::snapshot() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "could not list processes");
                return;
            }
        };
        let followed = lock(&self.state).game.clone();
        if let Some(g) = followed {
            if games::alive(&snap, g.pid) {
                let has_recorder = lock(&self.state).current.is_some();
                if !has_recorder && self.may_start() {
                    self.start_recorder(&settings, Some(&g));
                }
                return;
            }
            self.stop_recorder("game exited");
            self.forget_game();
        }
        if let Some(g) = games::detect(&snap, &settings.games).into_iter().next() {
            // A desktop recording (Always mode) makes way for the game.
            self.stop_recorder("a game started");
            {
                let mut st = lock(&self.state);
                st.game = Some(g.clone());
                st.retry_at = None;
            }
            self.emit(&Event::GameStarted { game: g.clone() });
            self.start_recorder(&settings, Some(&g));
            return;
        }
        let desktop_running = lock(&self.state).current.is_some();
        match mode {
            BufferMode::Always if !desktop_running && self.may_start() => {
                self.start_recorder(&settings, None);
            }
            BufferMode::Games if desktop_running => self.stop_recorder("no game is running"),
            _ => {}
        }
    }

    /// A recorder process that ended by itself (driver crash, killed): report it, finalise
    /// what it was recording, and restart at once the first time per game (later: after
    /// [`RETRY`]).
    fn reap_crashed_recorder(&self) {
        let Some(rec) = lock(&self.state)
            .current
            .as_ref()
            .map(|c| c.recorder.clone())
        else {
            return;
        };
        // Outside the lock: reaping the process may wait a moment.
        let Some(exit) = rec.ended() else { return };
        let error = format!("the recorder process ended unexpectedly ({exit})");
        tracing::warn!("{error}");
        {
            let mut st = lock(&self.state);
            st.crashes = st.crashes.saturating_add(1);
            st.retry_at = if st.crashes > 1 {
                Instant::now().checked_add(RETRY)
            } else {
                None
            };
            st.last_error = Some(error.clone());
        }
        self.stop_recorder(&error);
    }

    fn forget_game(&self) {
        let gone = {
            let mut st = lock(&self.state);
            st.retry_at = None;
            st.crashes = 0;
            st.game.take()
        };
        if let Some(game) = gone {
            self.emit(&Event::GameStopped { game });
            // Library thumbnails that waited for the game can be made now.
            self.thumbs.1.notify_all();
        }
    }

    fn start_recorder(&self, settings: &Settings, game: Option<&DetectedGame>) {
        lock(&self.state).starting = true;
        let res = (|| -> Result<host::Started, EngineError> {
            let encoders = {
                let mut c = lock(&self.cache);
                let e = config::encoders_for(settings, game, &mut c.0);
                config::save_encoder_cache(&c.0, c.1.as_deref());
                e?
            };
            let started = host::start(&self.isolation, settings, game, encoders)?;
            let mut c = lock(&self.cache);
            for a in &started.attempts {
                match &a.error {
                    None => c.0.record_started(&a.element),
                    Some(e) => c.0.record_start_failure(&a.element, e),
                }
            }
            config::save_encoder_cache(&c.0, c.1.as_deref());
            Ok(started)
        })();
        let name = game.map(|g| g.name.clone());
        let mut st = lock(&self.state);
        st.starting = false;
        match res {
            Ok(host::Started {
                handle,
                clip_dir,
                fps,
                buffer_ms,
                status,
                attempts: _,
            }) => {
                self.check_disk(true);
                st.current = Some(Current {
                    recorder: Arc::new(handle),
                    clip_dir: clip_dir.clone(),
                });
                st.retry_at = None;
                st.last_error = None;
                drop(st);
                self.emit(&Event::RecorderStarted {
                    game: name,
                    encoder: status.get("encoder").cloned().unwrap_or(Value::Null),
                    width: status.get("width").cloned().unwrap_or(Value::Null),
                    height: status.get("height").cloned().unwrap_or(Value::Null),
                    fps,
                    buffer_ms,
                    clip_dir: clip_dir.to_string_lossy().into_owned(),
                    audio: status.get("audio").cloned().unwrap_or(Value::Null),
                });
            }
            Err(e) => {
                let error = e.to_string();
                st.retry_at = Instant::now().checked_add(RETRY);
                st.last_error = Some(error.clone());
                drop(st);
                tracing::warn!(error = %error, "the recorder did not start");
                self.emit(&Event::RecorderFailed {
                    game: name,
                    error,
                    retry_secs: RETRY.as_secs(),
                });
            }
        }
    }

    /// Stop the recorder, finalising a running recording; a recorder process has exited
    /// (and given back its memory) when this returns.
    fn stop_recorder(&self, reason: &str) {
        let cur = lock(&self.state).current.take();
        if let Some(c) = cur {
            let stopped = c.recorder.stop();
            if let Some(summary) = stopped.summary {
                self.emit(&Event::RecordingStopped { summary });
            }
            self.emit(&Event::RecorderStopped {
                reason: reason.into(),
            });
            // A process that died may have left a recording unfinished. Recovery leaves
            // files written in the last moments alone (another recorder might still
            // write them), so it runs once that quiet time has passed.
            if stopped.unclean {
                lock(&self.state).recover_at = Instant::now()
                    .checked_add(klip_recorder::session::RECOVERY_QUIET)
                    .and_then(|t| t.checked_add(Duration::from_secs(1)));
            }
        }
    }

    /// (Re)bind the hotkeys from the settings; reports each one's state.
    fn bind_hotkeys(&self) {
        let list = lock(&self.settings).hotkeys.clone();
        // Unregister the old ones first so the same keys can be bound again.
        let old = lock(&self.shortcuts).take();
        drop(old);
        let mut specs = Vec::new();
        let mut map = HashMap::new();
        let mut counts: HashMap<&'static str, u32> = HashMap::new();
        for b in &list {
            let Some(h) = b.hotkey() else { continue };
            let action = action_id(b.action);
            let n = counts.entry(action).or_insert(0);
            let id = format!("{action}.{n}");
            *n = n.saturating_add(1);
            specs.push(ShortcutSpec {
                id: id.clone(),
                description: describe(b),
                preferred_trigger: Some(h.to_trigger()),
            });
            map.insert(id, b.clone());
        }
        *lock(&self.bindings) = map.clone();
        let bound: Vec<BoundShortcut> = if specs.is_empty() {
            Vec::new()
        } else {
            let me = self.me.clone();
            let on_press = move |id: &str| {
                if let Some(inner) = me.upgrade() {
                    inner.on_hotkey(id);
                }
            };
            match GlobalShortcuts::bind(&specs, Box::new(on_press)) {
                Ok(s) => {
                    let b = s.bound().to_vec();
                    *lock(&self.shortcuts) = Some(s);
                    b
                }
                Err(e) => specs
                    .iter()
                    .map(|s| BoundShortcut {
                        id: s.id.clone(),
                        trigger: s.preferred_trigger.clone().unwrap_or_default(),
                        error: Some(e.to_string()),
                    })
                    .collect(),
            }
        };
        let report: Vec<Value> = bound
            .iter()
            .map(|b| {
                let binding = map.get(&b.id);
                json!({
                    "id": b.id,
                    "action": binding.map(|x| action_id(x.action)),
                    "keys": binding.map(|x| x.keys.clone()),
                    "secs": binding.map(|x| x.secs),
                    "trigger": b.trigger,
                    "error": b.error,
                })
            })
            .collect();
        for b in &bound {
            if let Some(e) = &b.error {
                tracing::warn!(hotkey = %b.trigger, error = %e, "hotkey not active");
            }
        }
        *lock(&self.bound) = report.clone();
        self.emit(&Event::Hotkeys { bound: report });
    }

    /// A hotkey was pressed (on the hotkey thread): run its action on a job thread, unless
    /// the previous press of the same hotkey is still busy.
    fn on_hotkey(&self, id: &str) {
        let Some(b) = lock(&self.bindings).get(id).cloned() else {
            return;
        };
        if !lock(&self.busy).insert(id.to_string()) {
            return;
        }
        let me = self.me.clone();
        let key = id.to_string();
        let spawned = std::thread::Builder::new()
            .name("klip-hotkey-job".into())
            .spawn(move || {
                if let Some(inner) = me.upgrade() {
                    if catch_unwind(AssertUnwindSafe(|| inner.run_binding(&b))).is_err() {
                        tracing::error!("hotkey action panicked");
                    }
                    lock(&inner.busy).remove(&key);
                }
            });
        if spawned.is_err() {
            lock(&self.busy).remove(id);
        }
    }

    fn run_binding(&self, b: &HotkeyBinding) {
        let args = match b.action {
            HotkeyAction::ClipSave => json!({ "last_ms": u64::from(b.secs).saturating_mul(1000) }),
            _ => Value::Null,
        };
        let cmd = action_id(b.action);
        if let Err(e) = self.handle(cmd, &args) {
            self.emit(&Event::ActionFailed {
                action: cmd.into(),
                code: e.code.into(),
                message: e.message,
            });
        }
    }

    fn recorder(&self) -> Result<(Arc<RecorderHandle>, Option<DetectedGame>), CommandError> {
        let st = lock(&self.state);
        match &st.current {
            Some(c) => Ok((c.recorder.clone(), st.game.clone())),
            None => Err(CommandError::new(
                "not_ready",
                if st.starting {
                    "the recorder is starting"
                } else if st.game.is_some() {
                    "the recorder isn't running (see engine.status)"
                } else {
                    "no game is running"
                },
            )),
        }
    }

    fn set_settings(&self, patch: &Value) -> Result<Value, CommandError> {
        let (new, old) = {
            let mut s = lock(&self.settings);
            let new = s.apply_patch(patch).map_err(|e| match e {
                SettingsError::Invalid(i) => CommandError::bad_args(i.to_string()),
                other => CommandError::new("failed", other.to_string()),
            })?;
            klip_settings::save(&self.settings_path, &new)
                .map_err(|e| CommandError::new("failed", format!("saving the settings: {e}")))?;
            let old = std::mem::replace(&mut *s, new.clone());
            (new, old)
        };
        if new.hotkeys != old.hotkeys {
            self.bind_hotkeys();
        }
        if new.storage != old.storage || new.clips != old.clips {
            self.cleanup_in_background(Vec::new());
        }
        let v = new
            .to_value()
            .map_err(|e| CommandError::new("failed", e.to_string()))?;
        self.emit(&Event::SettingsChanged {
            settings: v.clone(),
        });
        self.wake();
        Ok(json!({ "settings": v }))
    }

    fn status(&self) -> Value {
        let settings = self.settings();
        let (game, current, starting, last_error) = {
            let st = lock(&self.state);
            (
                st.game.clone(),
                st.current
                    .as_ref()
                    .map(|c| (c.recorder.clone(), c.clip_dir.clone())),
                st.starting,
                st.last_error.clone(),
            )
        };
        let recorder = current.as_ref().map(|(r, _)| match r.status() {
            Ok((status, session)) => (status, Some(session)),
            Err(e) => (json!({ "error": e.message }), None),
        });
        json!({
            "buffer_mode": settings.games.buffer,
            "game": game,
            "recording": current.is_some(),
            "desktop": current.is_some() && game.is_none(),
            "starting": starting,
            "clip_dir": current.as_ref().map(|(_, d)| d.to_string_lossy()),
            "session": recorder.as_ref().and_then(|(_, s)| *s),
            "recorder": recorder.map(|(r, _)| r),
            "recorder_pid": current.as_ref().and_then(|(r, _)| r.pid()),
            "last_error": last_error,
            "hotkeys": lock(&self.bound).clone(),
            "hotkeys_paused": lock(&self.hotkeys_paused).is_some(),
            "settings_status": status_name(self.load_status),
            "settings_issues": self.load_issues.iter().map(ToString::to_string).collect::<Vec<_>>(),
        })
    }
}

impl Handler for Inner {
    fn handle(&self, cmd: &str, args: &Value) -> Result<Value, CommandError> {
        match cmd {
            commands::PING => Ok(json!({ "pong": true })),
            commands::ENGINE_STATUS => Ok(self.status()),
            commands::RECORDER_STOP | commands::APP_QUIT => {
                self.exit.store(true, Ordering::Relaxed);
                self.wake();
                Ok(json!({ "stopping": true }))
            }
            commands::SETTINGS_GET => {
                let v = self
                    .settings()
                    .to_value()
                    .map_err(|e| CommandError::new("failed", e.to_string()))?;
                Ok(json!({ "settings": v }))
            }
            commands::SETTINGS_SET => self.set_settings(args),
            commands::GAMES_LIST => {
                let settings = self.settings();
                let running = games::snapshot()
                    .map(|s| games::detect(&s, &settings.games))
                    .map_err(|e| CommandError::new("failed", e.to_string()))?;
                Ok(json!({ "running": running, "followed": lock(&self.state).game }))
            }
            commands::RECORDER_STATUS => self.recorder()?.0.handle(cmd, args),
            commands::CLIP_SAVE => {
                let (rec, game) = self.recorder()?;
                let clip = rec.handle(cmd, args)?;
                self.emit(&Event::ClipSaved {
                    game: game.as_ref().map(|g| g.name.clone()),
                    window: game.as_ref().and_then(|g| g.window),
                    clip: clip.clone(),
                });
                let saved: Vec<PathBuf> = clip
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .into_iter()
                    .collect();
                if let Some(p) = saved.first() {
                    self.thumbnail_in_background(p.clone());
                }
                self.cleanup_in_background(saved);
                Ok(clip)
            }
            commands::SESSION_START | commands::SESSION_STOP | commands::SESSION_TOGGLE => {
                let (rec, _) = self.recorder()?;
                let r = rec.handle(cmd, args)?;
                if r.get("recording").and_then(Value::as_bool) == Some(true) {
                    self.emit(&Event::RecordingStarted {
                        path: r
                            .get("path")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    });
                } else {
                    self.emit(&Event::RecordingStopped {
                        summary: r.get("summary").cloned().unwrap_or(Value::Null),
                    });
                }
                Ok(r)
            }
            commands::ENCODERS_LIST => self.encoders_list(args),
            commands::AUDIO_DEVICES => self.audio_devices(),
            commands::AUDIO_METER => self.audio_meter(args),
            commands::RECORDING_ESTIMATE => self.recording_estimate(args),
            commands::HOTKEYS_PAUSE => Ok(self.hotkeys_pause()),
            commands::HOTKEYS_RESUME => Ok(self.hotkeys_resume()),
            commands::GAMES_CANDIDATES => self.games_candidates(),
            commands::STORAGE_STATUS => self.storage_status(),
            commands::STORAGE_CLEANUP => self.cleanup(&[]),
            commands::CLIP_STAR => self.star(args),
            commands::LIBRARY_LIST => self.library_list(args),
            commands::LIBRARY_THUMBS => self.library_thumbs(args),
            commands::LIBRARY_THUMB => self.library_thumb(args),
            commands::CLIP_RENAME => self.clip_rename(args),
            commands::CLIP_TAGS => self.clip_tags(args),
            commands::CLIP_DELETE => self.clip_delete(args),
            commands::CLIP_OPEN => self.clip_open(args, false),
            commands::CLIP_REVEAL => self.clip_open(args, true),
            commands::EDITOR_PROJECT_GET => self.editor_project_get(args),
            commands::EDITOR_PROJECT_SET => self.editor_project_set(args),
            commands::EDITOR_PROXY => self.editor_proxy(args),
            commands::EDITOR_AUDIO => self.editor_audio(args),
            commands::EDITOR_FILES => self.editor_files(args),
            commands::EDITOR_TEXT_RENDER => self.editor_text_render(args),
            commands::EDITOR_FRAME => self.editor_frame(args),
            commands::EDITOR_MONTAGE => self.editor_montage(args),
            commands::EDITOR_HISTORY_LIST => self.editor_history_list(args),
            commands::EDITOR_HISTORY_RESTORE => self.editor_history_restore(args),
            commands::EXPORT_START => self.export_start(args),
            commands::EXPORT_CANCEL => self.export_cancel(args),
            commands::EXPORT_STATUS => self.export_status(args),
            commands::OVERLAY_PREVIEW => self.overlay_preview(),
            commands::OVERLAY_MENU => {
                self.emit(&Event::QuickMenu);
                Ok(json!({ "shown": true }))
            }
            commands::BOOKMARK_ADD => {
                let (rec, _) = self.recorder()?;
                let r = rec.handle(cmd, args)?;
                self.emit(&Event::Bookmark { result: r.clone() });
                Ok(r)
            }
            other => Err(CommandError::unknown(other)),
        }
    }
}
