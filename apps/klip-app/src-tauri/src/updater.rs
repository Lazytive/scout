//! The update check and the updater (ADR 0022). The check is Scout's own: one GET of the
//! newest release's `latest.json` on GitHub (Tauri's updater manifest), compared with the
//! version running. On Windows the signed installer is then fetched, verified and run by
//! `tauri-plugin-updater`; elsewhere the notice sends the user to the release page. The
//! engine never sees any of it: the window's `update.*` commands are answered here. The
//! result lives in memory; nothing is written but the download the plugin verifies.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use klip_control::Handler;
use serde::Serialize;
use serde_json::{Value, json};
// `Manager` only for the Windows install path (`try_state` in `on_before_exit`).
#[cfg(windows)]
use tauri::Manager as _;
use tauri::{AppHandle, Emitter as _, Runtime};

use crate::AppState;

/// Tauri's updater manifest of the newest release: GitHub's fixed redirect to the
/// newest release's asset (no API, no token, no rate limit).
pub const MANIFEST_URL: &str =
    "https://github.com/Lazytive/scout/releases/latest/download/latest.json";
/// Where "Get it on GitHub" goes.
pub const RELEASES_URL: &str = "https://github.com/Lazytive/scout/releases/latest";
/// The first automatic check waits for the app to settle; then one a day.
pub const FIRST_CHECK_AFTER: Duration = Duration::from_secs(30);
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// While a recording is being written the automatic check looks again later instead.
pub const RETRY_WHILE_RECORDING: Duration = Duration::from_secs(10 * 60);
/// A manifest is a few hundred bytes; anything bigger is not ours.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// Release notes are shown as they are, so they stay short on screen.
const MAX_NOTES_CHARS: usize = 4000;
const TIMEOUT: Duration = Duration::from_secs(10);

/// Where the check reads the manifest: `KLIP_UPDATE_URL` (tests: a loopback server, or
/// `off` for no check at all), else GitHub.
pub fn manifest_url() -> Option<String> {
    manifest_url_from(std::env::var("KLIP_UPDATE_URL").ok().as_deref())
}

/// `manifest_url` for a given `KLIP_UPDATE_URL`.
pub fn manifest_url_from(var: Option<&str>) -> Option<String> {
    match var.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("off") => None,
        Some(v) if !v.is_empty() => Some(v.to_string()),
        _ => Some(MANIFEST_URL.to_string()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
// `Downloading` and `Installing` are set only where the installer runs (Windows); the
// UI knows every state on every platform, so the enum is the same everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
pub enum State {
    /// No check yet.
    Idle,
    Checking,
    UpToDate,
    /// A newer release exists (`version`, `notes`, `date`).
    Available,
    /// The installer is being fetched (`progress`).
    Downloading,
    /// The installer was verified and started; the app is about to exit.
    Installing,
    /// The last check or install failed (`error`).
    Failed,
}

/// What the UI shows: the answer to `update.status` and the `update_status` event.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub state: State,
    /// The version running.
    pub current: String,
    /// The newest release when it is newer than `current`: its version, notes and date.
    pub version: Option<String>,
    pub notes: Option<String>,
    pub date: Option<String>,
    /// The release page.
    pub url: String,
    /// Download progress, 0–100.
    pub progress: Option<u8>,
    /// What went wrong, after a failed check or install.
    pub error: Option<String>,
    /// When the last check ended, in milliseconds since the Unix epoch.
    pub checked_ms: Option<u64>,
    /// This PC can install the update itself (Windows, and the release has a Windows
    /// installer); else the UI says "Get it on GitHub".
    pub installs: bool,
}

/// What a manifest says, as far as the check needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: semver::Version,
    pub notes: Option<String>,
    pub date: Option<String>,
    /// The manifest names a Windows installer over HTTPS.
    pub windows: bool,
}

fn cap(s: &str, chars: usize) -> String {
    s.chars().take(chars).collect()
}

/// Read a manifest. Hostile input: the size, the shape and the version are all checked,
/// and nothing of it but a short version text ends up in an error message.
pub fn parse_manifest(text: &str) -> Result<Release, String> {
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err("the update manifest is too big".into());
    }
    let v: Value =
        serde_json::from_str(text).map_err(|_| "the update manifest is not JSON".to_string())?;
    let version = v
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| "the update manifest names no version".to_string())?;
    let version = semver::Version::parse(version.trim().trim_start_matches('v')).map_err(|_| {
        format!(
            "the update manifest's version is not one: {}",
            cap(version, 40)
        )
    })?;
    let notes = v
        .get("notes")
        .and_then(Value::as_str)
        .map(|n| cap(n.trim(), MAX_NOTES_CHARS))
        .filter(|n| !n.is_empty());
    let date = v
        .get("pub_date")
        .and_then(Value::as_str)
        .map(|d| cap(d.trim(), 40))
        .filter(|d| !d.is_empty());
    let windows = v
        .get("platforms")
        .and_then(|p| p.get("windows-x86_64"))
        .and_then(|w| w.get("url"))
        .and_then(Value::as_str)
        .is_some_and(|u| u.starts_with("https://"));
    Ok(Release {
        version,
        notes,
        date,
        windows,
    })
}

/// A newer version. A pre-release counts only against a pre-release: 0.2.0-beta never
/// replaces 0.1.0, but replaces 0.2.0-alpha, and 0.2.0 replaces both.
pub fn is_newer(current: &semver::Version, remote: &semver::Version) -> bool {
    remote > current && (remote.pre.is_empty() || !current.pre.is_empty())
}

fn describe(e: &reqwest::Error) -> String {
    if let Some(status) = e.status() {
        return format!("GitHub answered {}", status.as_u16());
    }
    if e.is_timeout() {
        return "GitHub did not answer in time".into();
    }
    if e.is_connect() {
        return "no connection to GitHub".into();
    }
    if e.is_redirect() {
        return "GitHub's redirect to the manifest failed".into();
    }
    "the update check failed".into()
}

/// GET the manifest with the updater plugin's HTTP client (`reqwest`, blocking: the OS's
/// own TLS and certificate store, schannel on Windows and OpenSSL on Linux), 10 s in
/// all, at most 64 KiB of body. The request carries nothing but
/// `User-Agent: Scout/<version>`.
pub fn fetch(url: &str) -> Result<String, String> {
    use std::io::Read as _;

    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("Scout/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| describe(&e))?;
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| describe(&e))?;
    if response
        .content_length()
        .is_some_and(|n| n > MAX_MANIFEST_BYTES)
    {
        return Err("the update manifest is too big".into());
    }
    let mut text = String::new();
    response
        .take(MAX_MANIFEST_BYTES.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|_| "the update manifest could not be read".to_string())?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err("the update manifest is too big".into());
    }
    Ok(text)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The check's state, shared by the window's commands and the automatic check.
pub struct Updater {
    status: Mutex<Status>,
}

impl Updater {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(Status {
                state: State::Idle,
                current: env!("CARGO_PKG_VERSION").into(),
                version: None,
                notes: None,
                date: None,
                url: RELEASES_URL.into(),
                progress: None,
                error: None,
                checked_ms: None,
                installs: cfg!(windows),
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The status as the window sees it.
    pub fn status(&self) -> Value {
        serde_json::to_value(&*self.lock()).unwrap_or(Value::Null)
    }

    /// Change the status; what comes back is the `update_status` event to send.
    pub fn set(&self, f: impl FnOnce(&mut Status)) -> Value {
        let mut s = self.lock();
        f(&mut s);
        let mut v = serde_json::to_value(&*s).unwrap_or(Value::Null);
        if let Value::Object(m) = &mut v {
            m.insert("event".into(), json!("update_status"));
        }
        v
    }

    /// One check, blocking (run it off the UI thread). `url` is `None` when checks are
    /// off for this run. `emit` gets the `update_status` events; the result is the
    /// status afterwards, or why the check failed.
    pub fn check(&self, url: Option<&str>, emit: &dyn Fn(Value)) -> Result<Value, String> {
        let Some(url) = url else {
            let why = "update checks are switched off in this run".to_string();
            emit(self.set(|s| {
                s.state = State::Failed;
                s.error = Some(why.clone());
            }));
            return Err(why);
        };
        {
            let s = self.lock();
            // One check at a time, and none over an install.
            if matches!(
                s.state,
                State::Checking | State::Downloading | State::Installing
            ) {
                return Ok(serde_json::to_value(&*s).unwrap_or(Value::Null));
            }
        }
        emit(self.set(|s| {
            s.state = State::Checking;
            s.error = None;
        }));
        let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .map_err(|e| format!("this build's version is not one: {e}"));
        // A panic inside the HTTP client (a misconfigured TLS feature, say) ends as a
        // failed check, never as a check stuck at "checking".
        let result = current.and_then(|current| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(url)))
                .unwrap_or_else(|_| Err("the update check failed unexpectedly".into()))
                .and_then(|text| parse_manifest(&text))
                .map(|release| (current, release))
        });
        let checked = now_ms();
        match result {
            Ok((current, release)) => {
                let newer = is_newer(&current, &release.version);
                emit(self.set(|s| {
                    s.checked_ms = Some(checked);
                    s.installs = cfg!(windows) && release.windows;
                    if newer {
                        s.state = State::Available;
                        s.version = Some(release.version.to_string());
                        s.notes = release.notes;
                        s.date = release.date;
                    } else {
                        s.state = State::UpToDate;
                        s.version = None;
                        s.notes = None;
                        s.date = None;
                    }
                }));
                Ok(self.status())
            }
            Err(why) => {
                emit(self.set(|s| {
                    s.state = State::Failed;
                    s.error = Some(why.clone());
                    s.checked_ms = Some(checked);
                }));
                Err(why)
            }
        }
    }
}

/// The automatic check, on its own thread: 30 s after the start, then daily, only while
/// `app.update_check` is on and no recording is being written. Failures stay quiet
/// (being offline is normal); the status says what happened.
pub fn automatic<R: Runtime>(app: &AppHandle<R>, handler: &Arc<dyn Handler>, updater: &Updater) {
    let Some(url) = manifest_url() else {
        return;
    };
    std::thread::sleep(FIRST_CHECK_AFTER);
    loop {
        if recording(handler) {
            std::thread::sleep(RETRY_WHILE_RECORDING);
            continue;
        }
        if crate::setting(handler, "app", "update_check", true) {
            let emit = |v: Value| {
                let _ = app.emit("klip://event", v);
            };
            if let Err(why) = updater.check(Some(&url), &emit) {
                tracing::info!(why, "automatic update check");
            }
        }
        std::thread::sleep(CHECK_EVERY);
    }
}

/// A full-session recording is being written.
fn recording(handler: &Arc<dyn Handler>) -> bool {
    handler
        .handle(klip_control::commands::ENGINE_STATUS, &Value::Null)
        .ok()
        .and_then(|s| s.get("session")?.as_bool())
        .unwrap_or(false)
}

fn err(code: &str, message: impl Into<String>) -> Value {
    json!({ "code": code, "message": message.into() })
}

/// The window's `update.<action>` commands (the `klip` IPC hands them here).
pub async fn command<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    action: &str,
    _args: &Value,
) -> Result<Value, Value> {
    let updater = state.updater.clone();
    match action {
        "status" => Ok(updater.status()),
        "check" => {
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let emit = |v: Value| {
                    let _ = app.emit("klip://event", v);
                };
                updater.check(manifest_url().as_deref(), &emit)
            })
            .await
            .map_err(|e| err("internal", e.to_string()))?
            .map_err(|why| err("update_failed", why))
        }
        "install" => {
            if recording(&state.handler) {
                return Err(err(
                    "busy",
                    "a recording is being written: stop it before installing",
                ));
            }
            install(app, updater)
        }
        "open" => klip_platform::open_file(Path::new(RELEASES_URL))
            .map(|()| Value::Null)
            .map_err(|e| err("failed", e.to_string())),
        _ => Err(err(
            "unknown_command",
            format!("update.{}", cap(action, 40)),
        )),
    }
}

/// Windows: fetch the installer the manifest names, verify its signature and run it;
/// the app exits for it once the engine is shut down (a running recording is
/// finalised). Returns at once; `update_status` events say how it goes.
#[cfg(windows)]
fn install<R: Runtime>(app: &AppHandle<R>, updater: Arc<Updater>) -> Result<Value, Value> {
    {
        let s = updater.lock();
        if !matches!(s.state, State::Available | State::Failed) {
            return Err(err("busy", "no update is waiting to be installed"));
        }
        if !s.installs {
            return Err(err(
                "not_supported",
                "this release has no Windows installer",
            ));
        }
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let emit = {
            let app = app.clone();
            move |v: Value| {
                let _ = app.emit("klip://event", v);
            }
        };
        emit(updater.set(|s| {
            s.state = State::Downloading;
            s.progress = Some(0);
            s.error = None;
        }));
        let result = download_and_install(&app, &updater, &emit).await;
        if let Err(why) = result {
            tracing::warn!(why, "update install");
            emit(updater.set(|s| {
                s.state = State::Available;
                s.progress = None;
                s.error = Some(why);
            }));
        }
    });
    Ok(json!({ "started": true }))
}

#[cfg(windows)]
async fn download_and_install<R: Runtime>(
    app: &AppHandle<R>,
    updater: &Arc<Updater>,
    emit: &(impl Fn(Value) + Clone + Send + 'static),
) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt as _;

    let before_exit = {
        let app = app.clone();
        move || {
            // The recorder stops and a running recording is finalised before the
            // installer takes over (main.rs does the same on a normal exit).
            if let Some(state) = app.try_state::<AppState>()
                && let Some(engine) = state
                    .engine
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
            {
                engine.shutdown();
            }
        }
    };
    let plugin = app
        .updater_builder()
        .timeout(Duration::from_secs(60))
        .on_before_exit(before_exit)
        .build()
        .map_err(|e| cap(&e.to_string(), 200))?;
    let update = plugin
        .check()
        .await
        .map_err(|e| cap(&e.to_string(), 200))?
        .ok_or_else(|| "GitHub has no newer installer for this PC".to_string())?;
    let mut got: u64 = 0;
    let mut last: u8 = 0;
    let progress = {
        let updater = updater.clone();
        let emit = emit.clone();
        move |chunk: usize, total: Option<u64>| {
            got = got.saturating_add(u64::try_from(chunk).unwrap_or(u64::MAX));
            let Some(total) = total.filter(|t| *t > 0) else {
                return;
            };
            let pct = got
                .saturating_mul(100)
                .checked_div(total)
                .and_then(|p| u8::try_from(p).ok())
                .unwrap_or(100)
                .min(100);
            if pct != last {
                last = pct;
                emit(updater.set(|s| s.progress = Some(pct)));
            }
        }
    };
    let done = {
        let updater = updater.clone();
        let emit = emit.clone();
        move || {
            emit(updater.set(|s| {
                s.state = State::Installing;
                s.progress = Some(100);
            }));
        }
    };
    update
        .download_and_install(progress, done)
        .await
        .map_err(|e| cap(&e.to_string(), 200))
}

/// Not Windows: nothing to install here; the UI sends the user to the release page.
#[cfg(not(windows))]
fn install<R: Runtime>(_app: &AppHandle<R>, _updater: Arc<Updater>) -> Result<Value, Value> {
    Err(err(
        "not_supported",
        "Scout can't install updates on this system yet: get it on GitHub",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    #[test]
    fn a_manifest_gives_the_version_notes_date_and_whether_windows_has_an_installer() {
        let r = parse_manifest(
            r#"{"version":"v0.2.0","notes":"  Fixes.  ","pub_date":"2026-11-01T10:00:00Z",
                "platforms":{"windows-x86_64":{"url":"https://github.com/x/y.exe","signature":"s"}}}"#,
        )
        .unwrap();
        assert_eq!(r.version, v("0.2.0"));
        assert_eq!(r.notes.as_deref(), Some("Fixes."));
        assert_eq!(r.date.as_deref(), Some("2026-11-01T10:00:00Z"));
        assert!(r.windows);

        let plain = parse_manifest(r#"{"version":"1.0.0"}"#).unwrap();
        assert_eq!(plain.version, v("1.0.0"));
        assert_eq!(plain.notes, None);
        assert!(!plain.windows);
        let http = parse_manifest(
            r#"{"version":"1.0.0","notes":"","platforms":{"windows-x86_64":{"url":"http://x/y.exe"}}}"#,
        )
        .unwrap();
        assert!(!http.windows, "only HTTPS installers count");
        assert_eq!(http.notes, None, "blank notes are none");
    }

    #[test]
    fn hostile_manifests_are_refused_with_short_messages() {
        assert_eq!(
            parse_manifest("nope").unwrap_err(),
            "the update manifest is not JSON"
        );
        assert_eq!(
            parse_manifest("[1]").unwrap_err(),
            "the update manifest names no version"
        );
        assert_eq!(
            parse_manifest(r#"{"version":5}"#).unwrap_err(),
            "the update manifest names no version"
        );
        let bad = parse_manifest(&format!(r#"{{"version":"{}"}}"#, "x".repeat(500))).unwrap_err();
        assert!(bad.len() < 100, "{bad}");
        let big = format!(r#"{{"version":"1.0.0","notes":"{}"}}"#, "n".repeat(70_000));
        assert_eq!(
            parse_manifest(&big).unwrap_err(),
            "the update manifest is too big"
        );
        let long = format!(r#"{{"version":"1.0.0","notes":"{}"}}"#, "n".repeat(10_000));
        assert_eq!(
            parse_manifest(&long)
                .unwrap()
                .notes
                .unwrap()
                .chars()
                .count(),
            MAX_NOTES_CHARS
        );
    }

    #[test]
    fn newer_means_newer_and_pre_releases_only_replace_pre_releases() {
        assert!(is_newer(&v("0.1.0"), &v("0.2.0")));
        assert!(is_newer(&v("0.1.0"), &v("0.1.1")));
        assert!(!is_newer(&v("0.2.0"), &v("0.2.0")));
        assert!(!is_newer(&v("0.2.0"), &v("0.1.9")));
        assert!(
            !is_newer(&v("0.1.0"), &v("0.2.0-beta.1")),
            "a beta never replaces a release"
        );
        assert!(is_newer(&v("0.2.0-alpha"), &v("0.2.0-beta.1")));
        assert!(is_newer(&v("0.2.0-beta.1"), &v("0.2.0")));
        assert!(!is_newer(&v("0.2.0-beta.1"), &v("0.1.0")));
    }

    #[test]
    fn the_manifest_url_comes_from_the_environment_or_github() {
        assert_eq!(manifest_url_from(None).as_deref(), Some(MANIFEST_URL));
        assert_eq!(manifest_url_from(Some("")).as_deref(), Some(MANIFEST_URL));
        assert_eq!(manifest_url_from(Some("off")), None);
        assert_eq!(manifest_url_from(Some(" OFF ")), None);
        assert_eq!(
            manifest_url_from(Some(" http://127.0.0.1:1/latest.json ")).as_deref(),
            Some("http://127.0.0.1:1/latest.json")
        );
    }

    /// A loopback server that answers one request with `status` and `body`, as GitHub
    /// would after its redirect. Returns the URL to ask.
    fn serve(status: &'static str, body: String) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&request);
            assert!(head.starts_with("GET /latest.json HTTP/1.1"), "{head}");
            assert!(
                head.contains("user-agent: Scout/") || head.contains("User-Agent: Scout/"),
                "{head}"
            );
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes());
        });
        format!("http://127.0.0.1:{port}/latest.json")
    }

    #[test]
    fn fetch_reads_a_manifest_over_a_real_socket_and_refuses_errors_and_big_bodies() {
        let url = serve("200 OK", r#"{"version":"9.9.9"}"#.into());
        assert_eq!(fetch(&url).unwrap(), r#"{"version":"9.9.9"}"#);
        let url = serve("404 Not Found", "gone".into());
        assert_eq!(fetch(&url).unwrap_err(), "GitHub answered 404");
        let url = serve("200 OK", "x".repeat(70_000));
        assert_eq!(fetch(&url).unwrap_err(), "the update manifest is too big");
        assert_eq!(
            fetch("http://127.0.0.1:1/latest.json").unwrap_err(),
            "no connection to GitHub"
        );
    }

    fn states(events: &Mutex<Vec<Value>>) -> Vec<String> {
        events
            .lock()
            .unwrap()
            .iter()
            .map(|e| {
                assert_eq!(e["event"], "update_status");
                e["state"].as_str().unwrap().to_string()
            })
            .collect()
    }

    #[test]
    fn a_check_tells_the_window_what_it_found() {
        let updater = Updater::new();
        let events = Mutex::new(Vec::new());
        let emit = |v: Value| events.lock().unwrap().push(v);

        let url = serve("200 OK", r#"{"version":"99.0.0","notes":"Big.","platforms":{"windows-x86_64":{"url":"https://x/y.exe","signature":"s"}}}"#.into());
        let status = updater.check(Some(&url), &emit).unwrap();
        assert_eq!(status["state"], "available");
        assert_eq!(status["version"], "99.0.0");
        assert_eq!(status["notes"], "Big.");
        assert_eq!(status["current"], env!("CARGO_PKG_VERSION"));
        assert_eq!(status["installs"], cfg!(windows));
        assert!(status["checked_ms"].as_u64().unwrap() > 0);
        assert_eq!(states(&events), ["checking", "available"]);

        events.lock().unwrap().clear();
        let url = serve("200 OK", r#"{"version":"0.0.0"}"#.into());
        let status = updater.check(Some(&url), &emit).unwrap();
        assert_eq!(status["state"], "up_to_date");
        assert_eq!(
            status["version"],
            Value::Null,
            "an older release is forgotten"
        );
        assert_eq!(states(&events), ["checking", "up_to_date"]);

        events.lock().unwrap().clear();
        let url = serve("500 Oops", "".into());
        assert_eq!(
            updater.check(Some(&url), &emit).unwrap_err(),
            "GitHub answered 500"
        );
        assert_eq!(updater.status()["error"], "GitHub answered 500");
        assert_eq!(states(&events), ["checking", "failed"]);

        events.lock().unwrap().clear();
        assert!(
            updater
                .check(None, &emit)
                .unwrap_err()
                .contains("switched off")
        );
        assert_eq!(states(&events), ["failed"]);
    }

    #[test]
    fn no_check_runs_over_a_download() {
        let updater = Updater::new();
        let _ = updater.set(|s| s.state = State::Downloading);
        let events = Mutex::new(Vec::new());
        let emit = |v: Value| events.lock().unwrap().push(v);
        let status = updater
            .check(Some("http://127.0.0.1:1/latest.json"), &emit)
            .unwrap();
        assert_eq!(status["state"], "downloading");
        assert!(events.lock().unwrap().is_empty());
    }
}
