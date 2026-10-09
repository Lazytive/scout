//! Full-session recording: everything captured goes to a fragmented MP4 on disk while the
//! replay buffer keeps running. A killed recorder leaves a playable file; stopping (or the
//! next start, for a killed one) finalises it into a regular MP4. See ADR 0013.
//!
//! - The ring sink hands every accepted packet to a [`SessionTap`] under the ring lock, so
//!   the session sees exactly what the ring sees, in order; a new session starts with what
//!   the ring holds from its newest video keyframe on.
//! - A writer thread queues packets per track and writes a fragment at each video keyframe
//!   (or every [`MAX_FRAGMENT`] / [`MAX_FRAGMENT_BYTES`]). The newest packet of each track
//!   is held back until its successor gives it an exact duration.
//! - Next to the file being written sits `<file>.recording.json` (written atomically)
//!   holding the bookmarks; it marks the file as unfinished until finalising succeeds.
//! - If a track description changes (encoder restart with new parameters), the file is
//!   finalised and recording continues in a new one.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use klip_media::{NANOS_PER_SEC, Nanos, Packet, TrackInfo, TrackKind, nanos_to_millis};
use klip_mp4::{FragmentPart, FragmentedWriter, Marker, Mp4Error, Mp4Options, Mp4Track, finalize};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{RecorderError, unique_path};

/// Free space kept on the recording drive: recording stops before going below it (the
/// final `moov` still has to fit).
pub const DEFAULT_MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Most bytes waiting for the writer before the session gives up (the disk can't keep up).
pub const DEFAULT_MAX_QUEUE_BYTES: u64 = 512 * 1024 * 1024;
/// Longest fragment (when keyframes are rare or video stalls).
pub const MAX_FRAGMENT: Nanos = 4 * NANOS_PER_SEC;
/// Largest fragment payload.
pub const MAX_FRAGMENT_BYTES: u64 = 64 * 1024 * 1024;
/// How often written data is forced to disk (bounds what a power cut loses; a killed
/// process loses at most the fragment being written).
const SYNC_EVERY: Duration = Duration::from_secs(10);
const SPACE_CHECK_EVERY: Duration = Duration::from_secs(5);
/// Audio kept while waiting for the first video keyframe.
const MAX_PREROLL_AUDIO: Nanos = 5 * NANOS_PER_SEC;
/// Most bookmarks per file (a `chpl` box lists at most 255; the rest stay in the log).
pub const MAX_BOOKMARKS: usize = 10_000;
/// Suffix of the sidecar next to a recording that is being written.
pub const SIDECAR_SUFFIX: &str = ".recording.json";
const MAX_SIDECAR_BYTES: u64 = 4 * 1024 * 1024;
/// A recording modified this recently may still be written by another recorder.
pub const RECOVERY_QUIET: Duration = Duration::from_secs(15);

/// Where and how sessions are recorded.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Folder for recordings (created if missing).
    pub dir: PathBuf,
    pub min_free_bytes: u64,
    pub max_queue_bytes: u64,
}

impl SessionConfig {
    pub fn new(dir: PathBuf) -> SessionConfig {
        SessionConfig {
            dir,
            min_free_bytes: DEFAULT_MIN_FREE_BYTES,
            max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        }
    }
}

/// A finished recording file of a session.
#[derive(Clone, Debug, PartialEq)]
pub struct FileReport {
    pub path: PathBuf,
    pub duration: Nanos,
    pub bytes: u64,
    pub bookmarks: usize,
    /// Why finalising failed (the file is then still a playable fragmented MP4 and its
    /// sidecar stays, so the next start retries).
    pub error: Option<String>,
}

impl FileReport {
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path.to_string_lossy(),
            "duration_ms": nanos_to_millis(self.duration),
            "bytes": self.bytes,
            "bookmarks": self.bookmarks,
            "error": self.error,
        })
    }
}

/// What a stopped session left behind.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSummary {
    pub files: Vec<FileReport>,
    /// Why the session ended early, if it did.
    pub error: Option<String>,
}

impl SessionSummary {
    pub fn to_json(&self) -> Value {
        json!({
            "files": self.files.iter().map(FileReport::to_json).collect::<Vec<_>>(),
            "error": self.error,
        })
    }
}

/// A bookmark as stored for the current file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SidecarMarker {
    at_ns: i64,
    title: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Sidecar {
    klip_recording: u32,
    #[serde(default)]
    bookmarks: Vec<SidecarMarker>,
}

enum Msg {
    Info(usize, TrackInfo),
    Packet(usize, Packet),
    /// Finish; packets at or after the time (capture timeline) are left out.
    Stop(Option<Nanos>),
}

/// The session's end of the ring sink: called with the ring locked.
pub(crate) struct SessionTap {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
}

impl SessionTap {
    /// False once the writer has gone (the tap can be dropped).
    pub(crate) fn info(&self, track: usize, info: TrackInfo) -> bool {
        self.tx.send(Msg::Info(track, info)).is_ok()
    }

    /// False once the writer has gone. Over the queue cap, packets are dropped and the
    /// writer ends the session with an error.
    pub(crate) fn packet(&self, track: usize, packet: Packet) -> bool {
        if self.shared.overflow.load(Ordering::Relaxed) {
            return true;
        }
        let len = u64::try_from(packet.data.len()).unwrap_or(u64::MAX);
        let queued = self
            .shared
            .queued
            .fetch_add(len, Ordering::Relaxed)
            .saturating_add(len);
        if queued > self.shared.max_queue {
            self.shared.overflow.store(true, Ordering::Relaxed);
            return true;
        }
        self.tx.send(Msg::Packet(track, packet)).is_ok()
    }
}

struct Shared {
    queued: AtomicU64,
    max_queue: u64,
    overflow: AtomicBool,
    finished: AtomicBool,
    state: Mutex<State>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Default)]
struct State {
    /// Path the next file will get.
    next_path: PathBuf,
    current: Option<CurrentFile>,
    files: Vec<FileReport>,
    /// Bookmarks (capture running time) made before the current file had an origin.
    pending_bookmarks: Vec<Nanos>,
    error: Option<String>,
}

struct CurrentFile {
    path: PathBuf,
    sidecar: PathBuf,
    /// Capture running time of the file's time 0.
    origin: Nanos,
    /// Relative to `origin`.
    bookmarks: Vec<Nanos>,
    bytes: u64,
    fragments: u64,
    /// Written video length.
    duration: Nanos,
}

fn markers(bookmarks: &[Nanos]) -> Vec<Marker> {
    bookmarks
        .iter()
        .enumerate()
        .map(|(i, at)| Marker {
            at: *at,
            title: format!("Bookmark {}", i.saturating_add(1)),
        })
        .collect()
}

impl CurrentFile {
    fn write_sidecar(&self) -> Result<(), std::io::Error> {
        let sidecar = Sidecar {
            klip_recording: 1,
            bookmarks: markers(&self.bookmarks)
                .into_iter()
                .map(|m| SidecarMarker {
                    at_ns: m.at,
                    title: m.title,
                })
                .collect(),
        };
        let text = serde_json::to_vec_pretty(&sidecar).map_err(std::io::Error::other)?;
        write_atomic(&self.sidecar, &text)
    }
}

/// Write `data` to `path` via a temp file and a rename (replacing `path`).
fn write_atomic(path: &Path, data: &[u8]) -> Result<(), std::io::Error> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let res = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_data()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn sidecar_path(mp4: &Path) -> PathBuf {
    let mut s = mp4.as_os_str().to_owned();
    s.push(SIDECAR_SUFFIX);
    PathBuf::from(s)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A running session (owned by the recorder).
pub struct Session {
    shared: Arc<Shared>,
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
    done: Receiver<()>,
    started: Instant,
    started_unix: u64,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

/// How long [`Session::stop`] waits for the writer to finish and finalise.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(120);

impl Session {
    /// Start a writer for ring tracks named `names` (track 0 is video). Returns the session
    /// and the tap to attach to the ring sink.
    pub(crate) fn start(
        cfg: &SessionConfig,
        names: Vec<String>,
    ) -> Result<(Session, SessionTap), RecorderError> {
        fs::create_dir_all(&cfg.dir)?;
        let free = klip_platform::free_disk_space(&cfg.dir).ok();
        if free.is_some_and(|f| f < cfg.min_free_bytes) {
            return Err(RecorderError::Config(format!(
                "not enough free space in {} (need {} MB)",
                cfg.dir.display(),
                cfg.min_free_bytes / (1024 * 1024)
            )));
        }
        let shared = Arc::new(Shared {
            queued: AtomicU64::new(0),
            max_queue: cfg.max_queue_bytes,
            overflow: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            state: Mutex::new(State {
                next_path: unique_path(&cfg.dir, "Klip_Recording")?,
                ..State::default()
            }),
        });
        let (tx, rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let mut writer = Writer::new(cfg.clone(), names, shared.clone(), rx);
        let thread = std::thread::Builder::new()
            .name("klip-session-writer".into())
            .spawn(move || {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.run()));
                if res.is_err() {
                    tracing::error!("session writer panicked");
                    writer.shared.state().error = Some("internal error in the writer".into());
                    writer.finish_file();
                }
                writer.shared.finished.store(true, Ordering::Release);
                let _ = done_tx.send(());
            })?;
        let tap = SessionTap {
            tx: tx.clone(),
            shared: shared.clone(),
        };
        Ok((
            Session {
                shared,
                tx,
                thread: Some(thread),
                done,
                started: Instant::now(),
                started_unix: unix_now(),
            },
            tap,
        ))
    }

    /// The writer has ended on its own (error, disk full); stopping it reports why.
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }

    /// The file being written, or the one the session will write next.
    pub fn path(&self) -> PathBuf {
        let st = self.shared.state();
        st.current
            .as_ref()
            .map(|c| c.path.clone())
            .unwrap_or_else(|| st.next_path.clone())
    }

    /// Mark capture running time `at`. Returns the time within the current file, if a
    /// file is being written (else the bookmark is applied when it starts).
    pub fn bookmark(&self, at: Nanos) -> Result<Option<Nanos>, RecorderError> {
        let mut st = self.shared.state();
        let Some(cur) = st.current.as_mut() else {
            if st.pending_bookmarks.len() < MAX_BOOKMARKS {
                st.pending_bookmarks.push(at);
            }
            return Ok(None);
        };
        if cur.bookmarks.len() >= MAX_BOOKMARKS {
            return Err(RecorderError::Config(
                "too many bookmarks in this file".into(),
            ));
        }
        let rel = at.saturating_sub(cur.origin).max(0);
        cur.bookmarks.push(rel);
        cur.write_sidecar()?;
        Ok(Some(rel))
    }

    pub fn status(&self) -> Value {
        let st = self.shared.state();
        let current = st.current.as_ref().map(|c| {
            json!({
                "path": c.path.to_string_lossy(),
                "duration_ms": nanos_to_millis(c.duration),
                "bytes": c.bytes,
                "fragments": c.fragments,
                "bookmarks": c.bookmarks.len(),
            })
        });
        json!({
            "state": if self.is_finished() {
                "ended"
            } else if current.is_some() {
                "recording"
            } else {
                "starting"
            },
            "path": st.current.as_ref().map(|c| &c.path).unwrap_or(&st.next_path).to_string_lossy(),
            "current": current,
            "files": st.files.iter().map(FileReport::to_json).collect::<Vec<_>>(),
            "started_unix": self.started_unix,
            "elapsed_ms": u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "queued_bytes": self.shared.queued.load(Ordering::Relaxed),
            "error": st.error,
        })
    }

    /// Stop: write what is queued (before `end`, if given), finalise, report. Call after
    /// detaching the tap so no more packets arrive.
    pub fn stop(mut self, end: Option<Nanos>) -> SessionSummary {
        let _ = self.tx.send(Msg::Stop(end));
        let finished = match self.done.recv_timeout(STOP_TIMEOUT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
            Err(RecvTimeoutError::Timeout) => false,
        };
        if finished && let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let mut st = self.shared.state();
        let mut error = st.error.clone();
        if !finished {
            error = Some(
                "the recording is still being written; it is finished in the background".into(),
            );
        }
        SessionSummary {
            files: std::mem::take(&mut st.files),
            error,
        }
    }
}

struct OpenFile {
    w: FragmentedWriter<BufWriter<File>>,
    /// Ring track → file track.
    map: Vec<Option<usize>>,
}

struct Writer {
    cfg: SessionConfig,
    names: Vec<String>,
    shared: Arc<Shared>,
    rx: Receiver<Msg>,
    infos: Vec<Option<TrackInfo>>,
    pending: Vec<VecDeque<Packet>>,
    pending_bytes: u64,
    /// Running time of the current file's origin (its first video keyframe).
    origin: Option<Nanos>,
    file: Option<OpenFile>,
    last_sync: Instant,
    last_space_check: Instant,
    /// Tracks whose packets are dropped because they had no description when the file
    /// started (logged once).
    unmapped_logged: Vec<bool>,
}

#[derive(Debug, thiserror::Error)]
enum WriteError {
    #[error("{0}")]
    Mp4(#[from] Mp4Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Stop(String),
}

impl Writer {
    fn new(
        cfg: SessionConfig,
        names: Vec<String>,
        shared: Arc<Shared>,
        rx: Receiver<Msg>,
    ) -> Writer {
        let n = names.len().max(1);
        Writer {
            cfg,
            names,
            shared,
            rx,
            infos: vec![None; n],
            pending: (0..n).map(|_| VecDeque::new()).collect(),
            pending_bytes: 0,
            origin: None,
            file: None,
            last_sync: Instant::now(),
            last_space_check: Instant::now(),
            unmapped_logged: vec![false; n],
        }
    }

    fn run(&mut self) {
        loop {
            let res = match self.rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Msg::Packet(t, p)) => {
                    let len = u64::try_from(p.data.len()).unwrap_or(0);
                    let _ =
                        self.shared
                            .queued
                            .try_update(Ordering::Relaxed, Ordering::Relaxed, |q| {
                                Some(q.saturating_sub(len))
                            });
                    self.on_packet(t, p)
                }
                Ok(Msg::Info(t, info)) => self.on_info(t, info),
                Ok(Msg::Stop(end)) => {
                    if let Some(end) = end {
                        for q in &mut self.pending {
                            while q.back().is_some_and(|p| p.dts >= end) {
                                q.pop_back();
                            }
                        }
                        self.recount();
                    }
                    break;
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => Ok(()),
            };
            let res = res.and_then(|()| self.housekeeping());
            if let Err(e) = res {
                tracing::error!(error = %e, "session recording stopped");
                self.shared.state().error = Some(e.to_string());
                break;
            }
        }
        self.finish_file();
    }

    fn on_info(&mut self, track: usize, info: TrackInfo) -> Result<(), WriteError> {
        let Some(slot) = self.infos.get_mut(track) else {
            return Ok(());
        };
        if slot.as_ref() == Some(&info) {
            return Ok(());
        }
        let had = slot.replace(info).is_some();
        let mapped = self
            .file
            .as_ref()
            .is_some_and(|f| f.map.get(track).copied().flatten().is_some());
        if mapped {
            // Packets from here on can't go into this file: finish it, start a new one.
            tracing::info!(
                track,
                "track description changed; starting a new recording file"
            );
            self.finish_file();
            let next = unique_path(&self.cfg.dir, "Klip_Recording")
                .map_err(|e| WriteError::Stop(e.to_string()))?;
            self.shared.state().next_path = next;
        } else if had {
            // Not written yet: what's queued for it was for the old description.
            if let Some(q) = self.pending.get_mut(track) {
                q.clear();
            }
            if track == 0 && self.file.is_none() {
                self.origin = None;
            }
        }
        Ok(())
    }

    fn on_packet(&mut self, track: usize, p: Packet) -> Result<(), WriteError> {
        if self.infos.get(track).is_none_or(Option::is_none) {
            return Ok(());
        }
        let len = u64::try_from(p.data.len()).unwrap_or(u64::MAX);
        if track == 0 {
            let key = p.keyframe;
            let dts = p.dts;
            match self.origin {
                None if !key => return Ok(()),
                None => {
                    self.origin = Some(dts);
                    for q in self.pending.iter_mut().skip(1) {
                        q.retain(|a| a.dts >= dts);
                    }
                    self.recount();
                }
                Some(_) => {}
            }
            self.push(0, p, len);
            let queued = self.pending.first().map(VecDeque::len).unwrap_or(0);
            if key && queued > 1 {
                self.flush(Some(dts), false)?;
            }
        } else {
            match self.origin {
                Some(o) if p.dts < o => return Ok(()),
                Some(_) => {}
                None => {
                    // Before the first keyframe: keep only recent audio.
                    let newest = p.dts;
                    if let Some(q) = self.pending.get_mut(track) {
                        while q
                            .front()
                            .is_some_and(|a| a.dts < newest.saturating_sub(MAX_PREROLL_AUDIO))
                        {
                            q.pop_front();
                        }
                    }
                    self.recount();
                }
            }
            self.push(track, p, len);
        }
        if self.pending_bytes > MAX_FRAGMENT_BYTES {
            self.flush(None, false)?;
        }
        Ok(())
    }

    fn push(&mut self, track: usize, p: Packet, len: u64) {
        if let Some(q) = self.pending.get_mut(track) {
            q.push_back(p);
            self.pending_bytes = self.pending_bytes.saturating_add(len);
        }
    }

    fn recount(&mut self) {
        self.pending_bytes = self.pending.iter().flatten().fold(0u64, |acc, p| {
            acc.saturating_add(u64::try_from(p.data.len()).unwrap_or(0))
        });
    }

    fn housekeeping(&mut self) -> Result<(), WriteError> {
        if self.shared.overflow.load(Ordering::Relaxed) {
            return Err(WriteError::Stop(
                "the disk can't keep up with the recording; stopped".into(),
            ));
        }
        // Long fragments when keyframes are rare or video stalls. A lone held-back packet
        // (a track that went quiet) doesn't count.
        let span = {
            let oldest = self
                .pending
                .iter()
                .filter(|q| q.len() > 1)
                .filter_map(|q| q.front().map(|p| p.dts))
                .min();
            let newest = self
                .pending
                .iter()
                .filter_map(|q| q.back().map(|p| p.dts))
                .max();
            match (oldest, newest) {
                (Some(a), Some(b)) => b.saturating_sub(a),
                _ => 0,
            }
        };
        if self.origin.is_some() && span > MAX_FRAGMENT {
            self.flush(None, false)?;
        }
        if let Some(f) = self.file.as_mut()
            && self.last_sync.elapsed() >= SYNC_EVERY
        {
            f.w.get_ref().get_ref().sync_data()?;
            self.last_sync = Instant::now();
        }
        if self.file.is_some() && self.last_space_check.elapsed() >= SPACE_CHECK_EVERY {
            self.last_space_check = Instant::now();
            if klip_platform::free_disk_space(&self.cfg.dir)
                .is_ok_and(|f| f < self.cfg.min_free_bytes)
            {
                return Err(WriteError::Stop(format!(
                    "the recording drive is almost full (less than {} MB free); stopped",
                    self.cfg.min_free_bytes / (1024 * 1024)
                )));
            }
        }
        Ok(())
    }

    /// Open the file for the current origin: sidecar first (so a kill right after leaves
    /// a file recovery knows about), then `ftyp` + init `moov`.
    fn open_file(&mut self, origin: Nanos) -> Result<(), WriteError> {
        let free = klip_platform::free_disk_space(&self.cfg.dir).ok();
        if free.is_some_and(|f| f < self.cfg.min_free_bytes) {
            return Err(WriteError::Stop(
                "the recording drive is almost full; not recording".into(),
            ));
        }
        let mut st = self.shared.state();
        let path = st.next_path.clone();
        let pending: Vec<Nanos> = std::mem::take(&mut st.pending_bookmarks);
        let cur = CurrentFile {
            sidecar: sidecar_path(&path),
            path: path.clone(),
            origin,
            bookmarks: pending
                .iter()
                .map(|at| at.saturating_sub(origin).max(0))
                .collect(),
            bytes: 0,
            fragments: 0,
            duration: 0,
        };
        cur.write_sidecar()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut map = vec![None; self.infos.len()];
        let mut tracks: Vec<Mp4Track<'_>> = Vec::new();
        let mut audio_seen = false;
        for (i, info) in self.infos.iter().enumerate() {
            let Some(info) = info else { continue };
            let mut t = Mp4Track::new(info, &[]);
            if let Some(name) = self.names.get(i).filter(|n| !n.is_empty()) {
                t = t.named(name);
            }
            if info.kind() == TrackKind::Audio {
                t = t.enabled(!audio_seen);
                audio_seen = true;
            }
            if let Some(slot) = map.get_mut(i) {
                *slot = Some(tracks.len());
            }
            tracks.push(t);
        }
        let w = FragmentedWriter::new(
            BufWriter::with_capacity(1 << 20, file),
            &tracks,
            origin,
            &Mp4Options {
                creation_time_unix: unix_now(),
                ..Mp4Options::default()
            },
        )?;
        tracing::info!(path = %path.display(), tracks = tracks.len(), "recording file started");
        st.current = Some(cur);
        drop(st);
        self.file = Some(OpenFile { w, map });
        self.last_sync = Instant::now();
        Ok(())
    }

    /// Write the queued packets before `limit` (all if `None`) as one fragment. Unless
    /// `last`, each track's newest packet stays queued until its successor is known.
    fn flush(&mut self, limit: Option<Nanos>, last: bool) -> Result<(), WriteError> {
        let Some(origin) = self.origin else {
            return Ok(());
        };
        if self.file.is_none() {
            self.open_file(origin)?;
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        let mut taken: Vec<(usize, Vec<Packet>, Option<Nanos>)> = Vec::new();
        for (t, q) in self.pending.iter_mut().enumerate() {
            let Some(ft) = file.map.get(t).copied().flatten() else {
                if !q.is_empty() && !self.unmapped_logged.get(t).copied().unwrap_or(true) {
                    tracing::warn!(
                        track = t,
                        "track started after the recording file; left out"
                    );
                    if let Some(l) = self.unmapped_logged.get_mut(t) {
                        *l = true;
                    }
                }
                q.clear();
                continue;
            };
            let mut k = q
                .iter()
                .take_while(|p| limit.is_none_or(|l| p.dts < l))
                .count();
            if !last && k == q.len() {
                k = k.saturating_sub(1);
            }
            if k == 0 {
                continue;
            }
            let packets: Vec<Packet> = q.drain(..k).collect();
            let next = if last { None } else { q.front().map(|p| p.dts) };
            taken.push((ft, packets, next));
        }
        if taken.is_empty() {
            return Ok(());
        }
        let parts: Vec<FragmentPart<'_>> = taken
            .iter()
            .map(|(ft, packets, next)| FragmentPart {
                track: *ft,
                packets,
                next_dts: *next,
            })
            .collect();
        let info = file.w.write_fragment(&parts)?;
        // Video end relative to the origin.
        let video_end = taken
            .iter()
            .find(|(ft, _, _)| file.map.first().copied().flatten() == Some(*ft))
            .and_then(|(_, p, next)| {
                let last = p.last()?;
                Some(next.unwrap_or(last.dts.saturating_add(last.duration)))
            });
        let (bytes, fragments) = (file.w.bytes_written(), file.w.fragments());
        self.recount();
        let mut st = self.shared.state();
        if let Some(cur) = st.current.as_mut() {
            cur.bytes = bytes;
            cur.fragments = fragments;
            if let Some(end) = video_end {
                cur.duration = end.saturating_sub(origin);
            }
        }
        tracing::trace!(
            bytes = info.bytes,
            samples = info.samples,
            "fragment written"
        );
        Ok(())
    }

    /// Write everything queued, close and finalise the current file (best effort: errors
    /// go to the file's report). Resets for a possible next file.
    fn finish_file(&mut self) {
        if let Err(e) = self.flush(None, true) {
            tracing::error!(error = %e, "writing the last fragment failed");
            let mut st = self.shared.state();
            if st.error.is_none() {
                st.error = Some(e.to_string());
            }
        }
        for q in &mut self.pending {
            q.clear();
        }
        self.pending_bytes = 0;
        self.origin = None;
        let Some(file) = self.file.take() else {
            return;
        };
        let closed = file
            .w
            .into_inner()
            .into_inner()
            .map_err(|e| e.into_error())
            .and_then(|f| f.sync_all());
        let mut st = self.shared.state();
        let Some(cur) = st.current.take() else {
            return;
        };
        let marks = markers(&cur.bookmarks);
        let result = match closed {
            Ok(()) => finalize(&cur.path, &marks).map_err(|e| e.to_string()),
            Err(e) => Err(format!("closing the file: {e}")),
        };
        let error = match result {
            Ok(s) => {
                tracing::info!(
                    path = %cur.path.display(),
                    bytes = s.bytes,
                    fragments = s.fragments,
                    "recording finalised"
                );
                if let Err(e) = fs::remove_file(&cur.sidecar) {
                    tracing::warn!(error = %e, "could not remove the recording sidecar");
                }
                None
            }
            Err(e) => {
                tracing::error!(error = %e, path = %cur.path.display(), "finalising the recording failed");
                Some(e)
            }
        };
        st.files.push(FileReport {
            path: cur.path,
            duration: cur.duration,
            bytes: cur.bytes,
            bookmarks: cur.bookmarks.len(),
            error,
        });
    }
}

/// A leftover recording found by [`recover_recordings`].
#[derive(Clone, Debug, PartialEq)]
pub struct Recovered {
    pub path: PathBuf,
    /// `Ok(true)`: finalised; `Ok(false)`: it held nothing playable and was removed.
    pub result: Result<bool, String>,
}

/// Finalise recordings in `dir` that a killed recorder left unfinished (they have a
/// `.recording.json` sidecar). Files modified in the last few seconds are left alone: another
/// recorder may still be writing them.
pub fn recover_recordings(dir: &Path) -> Vec<Recovered> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten().take(100_000) {
        let sidecar = entry.path();
        let Some(name) = sidecar.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(mp4_name) = name.strip_suffix(SIDECAR_SUFFIX) else {
            continue;
        };
        let mp4 = sidecar.with_file_name(mp4_name);
        let recent = |p: &Path| {
            fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < RECOVERY_QUIET)
        };
        if recent(&mp4) || recent(&sidecar) {
            continue;
        }
        let result = if mp4.exists() {
            match finalize_recording(&mp4) {
                Ok(_) => Ok(true),
                Err(Mp4Error::NoFragments) => fs::remove_file(&mp4)
                    .map(|()| false)
                    .map_err(|e| format!("removing the empty recording: {e}")),
                Err(e) => Err(e.to_string()),
            }
        } else {
            Ok(false)
        };
        if result.is_ok()
            && let Err(e) = fs::remove_file(&sidecar)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %e, "could not remove a recording sidecar");
        }
        out.push(Recovered { path: mp4, result });
    }
    out
}

/// Finalise the recording at `mp4` with the bookmarks from its sidecar (if any), then
/// remove the sidecar.
pub fn finalize_recording(mp4: &Path) -> Result<klip_mp4::FinalizeSummary, Mp4Error> {
    let sidecar = sidecar_path(mp4);
    let summary = finalize(mp4, &read_sidecar(&sidecar))?;
    if let Err(e) = fs::remove_file(&sidecar)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(error = %e, "could not remove a recording sidecar");
    }
    Ok(summary)
}

/// Bookmarks from a sidecar; an unreadable one gives none (the video matters more).
fn read_sidecar(path: &Path) -> Vec<Marker> {
    let Ok(meta) = fs::metadata(path) else {
        return Vec::new();
    };
    if meta.len() > MAX_SIDECAR_BYTES {
        return Vec::new();
    }
    let Ok(text) = fs::read(path) else {
        return Vec::new();
    };
    let Ok(s) = serde_json::from_slice::<Sidecar>(&text) else {
        return Vec::new();
    };
    s.bookmarks
        .into_iter()
        .take(MAX_BOOKMARKS)
        .filter(|m| m.at_ns >= 0)
        .map(|m| Marker {
            at: m.at_ns,
            title: m.title.chars().take(255).collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
