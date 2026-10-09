//! Game detection (ADR 0014): which running program is a game, from a snapshot of
//! processes, their executable paths and their windows. Only OS process and window lists
//! are read (no access to any process's memory), so nothing here can upset anti-cheat.
//!
//! A program is a game when it has a big enough visible window (or, where there is no
//! window list, on Linux, simply runs) and either the user listed it or it was installed
//! by a game store into its library folder. Launchers, crash reporters, installers and
//! the like are left out, as is everything on the user's "never" list.

use std::collections::HashMap;

use klip_platform::{PlatformError, ProcessInfo, WindowInfo};
use klip_settings::{GameSettings, game_key};
use serde::Serialize;

/// Windows smaller than this aren't games (launchers, splash screens, tray pop-ups).
pub const MIN_WIDTH: u32 = 640;
pub const MIN_HEIGHT: u32 = 480;
/// Programs the user listed only need a window this big.
pub const MIN_USER_WIDTH: u32 = 320;
pub const MIN_USER_HEIGHT: u32 = 240;

/// Where a detected game was installed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Store {
    Steam,
    Epic,
    Gog,
    Xbox,
    Ea,
    Ubisoft,
    Riot,
    Itch,
    Heroic,
}

/// Why a program counts as a game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "store")]
pub enum Source {
    /// On the user's list.
    User,
    /// In a store's library folder.
    Library(Store),
}

/// A running game.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DetectedGame {
    pub pid: u32,
    /// Executable file name, e.g. `Game.exe`.
    pub exe: String,
    /// Profile key ([`game_key`]).
    pub key: String,
    /// Display name (store folder, user's name, or the executable).
    pub name: String,
    /// Its biggest window (none where there is no window list).
    pub window: Option<u64>,
    pub source: Source,
    /// Whether its window has the keyboard focus.
    pub foreground: bool,
}

/// What the detector looks at.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub processes: Vec<ProcessInfo>,
    /// `None` where the OS gives no window list (Wayland).
    pub windows: Option<Vec<WindowInfo>>,
    /// Executable paths of the processes worth a look (see [`snapshot`]).
    pub paths: HashMap<u32, String>,
    pub foreground: Option<u64>,
}

/// Library folders, as lower-case path fragments with `/` separators; the folder after
/// the fragment is the game's.
const LIBRARIES: &[(&str, Store)] = &[
    ("/steamapps/common/", Store::Steam),
    ("/epic games/", Store::Epic),
    ("/gog galaxy/games/", Store::Gog),
    ("/gog games/", Store::Gog),
    ("/xboxgames/", Store::Xbox),
    ("/ea games/", Store::Ea),
    ("/origin games/", Store::Ea),
    ("/ubisoft game launcher/games/", Store::Ubisoft),
    ("/riot games/", Store::Riot),
    ("/itch/apps/", Store::Itch),
    ("/games/heroic/", Store::Heroic),
];

/// Store sub-folders that aren't games.
const NOT_GAMES: &[&str] = &[
    "steamworks shared",
    "steam controller configs",
    "steamvr",
    "riot client",
    "launcher",
    "directxredist",
    "_commonredist",
];

/// Executables that are never games, lower case.
const NEVER: &[&str] = &[
    "steam.exe",
    "steamwebhelper.exe",
    "steamservice.exe",
    "epicgameslauncher.exe",
    "epicwebhelper.exe",
    "galaxyclient.exe",
    "galaxyclient helper.exe",
    "eadesktop.exe",
    "ealauncher.exe",
    "origin.exe",
    "upc.exe",
    "ubisoftconnect.exe",
    "battle.net.exe",
    "agent.exe",
    "riotclientservices.exe",
    "riotclientux.exe",
    "riotclientuxrender.exe",
    "itch.exe",
    "explorer.exe",
    "discord.exe",
    "obs64.exe",
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "opera.exe",
    "brave.exe",
    "steam",
    "steamwebhelper",
    "heroic",
    "lutris",
    "discord",
    "firefox",
];

/// Name fragments of helper programs that ship inside game folders.
const HELPER_WORDS: &[&str] = &[
    "crashhandler",
    "crash_handler",
    "crashreport",
    "crash_report",
    "crashpad",
    "crashsender",
    "bugreport",
    "errorreport",
    "launcher",
    "setup",
    "install",
    "updater",
    "unins",
    "redist",
    "dxsetup",
    "easyanticheat",
    "battleye",
    "beservice",
    "webhelper",
    "overlay",
    "prereq",
];

fn normalise(path: &str) -> String {
    path.replace('\\', "/")
}

/// Executable file name of a path (either separator).
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The store and game folder of an executable path.
pub fn library_game(path: &str) -> Option<(Store, String)> {
    let norm = normalise(path);
    // ASCII lower-casing keeps byte offsets, so `norm` can be sliced where `lower` matched.
    let lower = norm.to_ascii_lowercase();
    for (frag, store) in LIBRARIES {
        let Some(at) = lower.find(frag) else { continue };
        let start = at.checked_add(frag.len())?;
        let rest = norm.get(start..)?;
        let folder = rest.split('/').next().unwrap_or_default().trim();
        // The executable directly in the library folder isn't inside a game folder.
        if folder.is_empty() || rest.split('/').count() < 2 {
            continue;
        }
        if NOT_GAMES.contains(&folder.to_lowercase().as_str()) {
            return None;
        }
        return Some((*store, folder.to_string()));
    }
    None
}

/// Whether an executable is a known non-game (store client, browser, helper).
pub fn is_helper(exe: &str) -> bool {
    let lower = exe.to_lowercase();
    NEVER.contains(&lower.as_str()) || HELPER_WORDS.iter().any(|w| lower.contains(w))
}

/// A readable name from an executable (`VALORANT-Win64-Shipping.exe` → `VALORANT`).
pub fn name_from_exe(exe: &str) -> String {
    let mut stem = exe;
    for ext in [".exe", ".x86_64", ".x86", ".sh", ".bin"] {
        if stem.len() > ext.len()
            && let Some(cut) = stem.len().checked_sub(ext.len())
            && stem.get(cut..).is_some_and(|e| e.eq_ignore_ascii_case(ext))
        {
            stem = stem.get(..cut).unwrap_or(stem);
            break;
        }
    }
    for suffix in [
        "-Win64-Shipping",
        "-WinGDK-Shipping",
        "-Shipping",
        "_x64",
        "-x64",
    ] {
        if let Some(s) = stem.strip_suffix(suffix)
            && !s.is_empty()
        {
            stem = s;
            break;
        }
    }
    stem.to_string()
}

/// A folder name for a game's clips: no characters Windows or Linux reject, no reserved
/// device names, at most 64 characters; empty input gives `Other`.
pub fn folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut out: String = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(64)
        .collect();
    out = out
        .trim_end_matches(['.', ' '])
        .trim_start_matches('.')
        .to_string();
    let upper = out.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or_default();
    let reserved = matches!(stem, "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.chars().last().is_some_and(|c| c.is_ascii_digit()));
    if out.is_empty() {
        "Other".into()
    } else if reserved {
        format!("{out}_")
    } else {
        out
    }
}

fn big_enough(w: &WindowInfo, min_w: u32, min_h: u32) -> bool {
    !w.minimized && w.client_width >= min_w && w.client_height >= min_h
}

/// Take a snapshot: processes, windows, the foreground window, and executable paths for
/// processes that own a big enough window (all processes where there is no window list).
pub fn snapshot() -> Result<Snapshot, PlatformError> {
    let processes = klip_platform::list_processes()?;
    let windows = match klip_platform::list_windows() {
        Ok(w) => Some(w),
        Err(PlatformError::Unsupported) => None,
        Err(e) => return Err(e),
    };
    let me = std::process::id();
    let wanted: Vec<u32> = match &windows {
        Some(ws) => ws
            .iter()
            .filter(|w| w.pid != me && big_enough(w, MIN_USER_WIDTH, MIN_USER_HEIGHT))
            .map(|w| w.pid)
            .collect(),
        None => processes
            .iter()
            .map(|p| p.pid)
            .filter(|p| *p != me)
            .collect(),
    };
    let mut paths = HashMap::new();
    for pid in wanted {
        if let std::collections::hash_map::Entry::Vacant(e) = paths.entry(pid)
            && let Ok(p) = klip_platform::process_path(pid)
        {
            e.insert(p.to_string_lossy().into_owned());
        }
    }
    Ok(Snapshot {
        processes,
        windows,
        paths,
        foreground: klip_platform::foreground_window(),
    })
}

/// Running games, best first: the one in the foreground, then by window size.
pub fn detect(snap: &Snapshot, settings: &GameSettings) -> Vec<DetectedGame> {
    let me = std::process::id();
    let never: Vec<String> = settings.never.iter().map(|n| game_key(n)).collect();
    let exe_of = |pid: u32| -> Option<String> {
        // The path's file name: on Linux it names the Windows program for Proton games.
        snap.paths
            .get(&pid)
            .map(|p| file_name(p).to_string())
            .or_else(|| {
                snap.processes
                    .iter()
                    .find(|p| p.pid == pid)
                    .map(|p| p.exe.clone())
            })
            .filter(|e| !e.is_empty())
    };
    let classify = |pid: u32| -> Option<(String, String, Source)> {
        let exe = exe_of(pid)?;
        let key = game_key(&exe);
        if never.contains(&key) {
            return None;
        }
        let path = snap.paths.get(&pid);
        let library = path.and_then(|p| library_game(p));
        if let Some(u) = settings.user_games.iter().find(|u| game_key(&u.exe) == key) {
            let name = u
                .name
                .clone()
                .or_else(|| library.as_ref().map(|(_, n)| n.clone()))
                .unwrap_or_else(|| name_from_exe(&exe));
            return Some((exe, name, Source::User));
        }
        if is_helper(&exe) || !settings.detect_stores {
            return None;
        }
        let (store, name) = library?;
        Some((exe, name, Source::Library(store)))
    };

    // pid → (game, biggest window area)
    let mut found: Vec<(DetectedGame, u64)> = Vec::new();
    match &snap.windows {
        Some(windows) => {
            for w in windows.iter().filter(|w| w.pid != me && w.pid != 0) {
                let area = u64::from(w.client_width).saturating_mul(u64::from(w.client_height));
                let foreground = snap.foreground == Some(w.handle);
                if let Some((g, best)) = found.iter_mut().find(|(g, _)| g.pid == w.pid) {
                    if big_enough(w, MIN_USER_WIDTH, MIN_USER_HEIGHT) && area > *best {
                        *best = area;
                        g.window = Some(w.handle);
                    }
                    g.foreground |= foreground;
                    continue;
                }
                let Some((exe, name, source)) = classify(w.pid) else {
                    continue;
                };
                let (min_w, min_h) = match source {
                    Source::User => (MIN_USER_WIDTH, MIN_USER_HEIGHT),
                    Source::Library(_) => (MIN_WIDTH, MIN_HEIGHT),
                };
                if !big_enough(w, min_w, min_h) {
                    continue;
                }
                found.push((
                    DetectedGame {
                        pid: w.pid,
                        key: game_key(&exe),
                        exe,
                        name,
                        window: Some(w.handle),
                        source,
                        foreground,
                    },
                    area,
                ));
            }
        }
        None => {
            for p in snap.processes.iter().filter(|p| p.pid != me) {
                let Some((exe, name, source)) = classify(p.pid) else {
                    continue;
                };
                // A game's child processes (helpers with the same name) count once.
                if found.iter().any(|(g, _)| g.key == game_key(&exe)) {
                    continue;
                }
                found.push((
                    DetectedGame {
                        pid: p.pid,
                        key: game_key(&exe),
                        exe,
                        name,
                        window: None,
                        source,
                        foreground: false,
                    },
                    0,
                ));
            }
        }
    }
    found.sort_by(|(a, area_a), (b, area_b)| {
        b.foreground
            .cmp(&a.foreground)
            .then(area_b.cmp(area_a))
            .then(a.pid.cmp(&b.pid))
    });
    found.into_iter().map(|(g, _)| g).collect()
}

/// A running program the user could add to their game list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub exe: String,
    pub name: String,
    /// Already counted as a game (store folder or the user's list).
    pub detected: bool,
}

/// Programs with a visible window of at least 320x240 (not Klip, not store clients,
/// browsers or helpers), one per executable, detected games first. For the settings
/// page's "add a game" list; window titles are never included.
pub fn candidates(snap: &Snapshot, settings: &GameSettings) -> Vec<Candidate> {
    let me = std::process::id();
    let detected: Vec<String> = detect(snap, settings).into_iter().map(|g| g.key).collect();
    let never: Vec<String> = settings.never.iter().map(|n| game_key(n)).collect();
    let mut out: Vec<Candidate> = Vec::new();
    for w in snap.windows.iter().flatten() {
        if w.pid == me || w.pid == 0 || !big_enough(w, MIN_USER_WIDTH, MIN_USER_HEIGHT) {
            continue;
        }
        let exe = snap
            .paths
            .get(&w.pid)
            .map(|p| file_name(p).to_string())
            .or_else(|| {
                snap.processes
                    .iter()
                    .find(|p| p.pid == w.pid)
                    .map(|p| p.exe.clone())
            })
            .unwrap_or_default();
        let key = game_key(&exe);
        if exe.is_empty() || is_helper(&exe) || never.contains(&key) {
            continue;
        }
        if out.iter().any(|c| game_key(&c.exe) == key) {
            continue;
        }
        let name = snap
            .paths
            .get(&w.pid)
            .and_then(|p| library_game(p))
            .map_or_else(|| name_from_exe(&exe), |(_, n)| n);
        out.push(Candidate {
            detected: detected.contains(&key),
            exe,
            name,
        });
    }
    out.sort_by(|a, b| {
        b.detected
            .cmp(&a.detected)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// Whether a process is still running.
pub fn alive(snap: &Snapshot, pid: u32) -> bool {
    snap.processes.iter().any(|p| p.pid == pid)
}

#[cfg(test)]
mod tests;
