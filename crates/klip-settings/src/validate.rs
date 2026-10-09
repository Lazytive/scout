//! Range checks and normalisation. Each fix is reported as an [`Issue`]; normalising
//! (trimming, re-spelling a hotkey, lower-casing a profile key, dropping duplicates) is not.

use std::collections::{BTreeMap, HashSet};
use std::ops::RangeInclusive;
use std::path::Path;

use crate::preset::{BITRATE_RANGE, FPS_RANGE, HEIGHT_RANGE, Quality, WIDTH_RANGE};
use crate::{
    AUDIO_BITRATES, AudioSettings, BUFFER_SECS, CAP_GB_MAX, CLIP_SECS, ClipSettings, GameSettings,
    HotkeyAction, HotkeyBinding, Issue, LOW_DISK_GB_MAX, MAX_HOTKEYS, MAX_LIST, MAX_NAME, MAX_PATH,
    MAX_PROFILES, OVERLAY_SECS, Profile, RecordingSettings, SCHEMA_VERSION, Settings, game_key,
};

pub(crate) fn settings(s: &mut Settings, issues: &mut Vec<Issue>) {
    s.version = SCHEMA_VERSION;
    clips(&mut s.clips, issues);
    recording(&mut s.recording, issues);
    audio(&mut s.audio, "audio", issues);
    hotkeys(&mut s.hotkeys, issues);
    range(
        &mut s.overlay.seconds,
        &OVERLAY_SECS,
        "overlay.seconds",
        issues,
    );
    range(
        &mut s.storage.cap_gb,
        &(0..=CAP_GB_MAX),
        "storage.cap_gb",
        issues,
    );
    range(
        &mut s.storage.low_disk_gb,
        &(0..=LOW_DISK_GB_MAX),
        "storage.low_disk_gb",
        issues,
    );
    games(&mut s.games, issues);
}

/// Shorten untrusted text for messages.
fn short(s: &str) -> String {
    let mut out: String = s.chars().take(40).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    format!("{out:?}")
}

fn range(v: &mut u32, r: &RangeInclusive<u32>, path: &str, issues: &mut Vec<Issue>) {
    if !r.contains(v) {
        let fixed = (*v).clamp(*r.start(), *r.end());
        issues.push(Issue::new(
            path,
            format!("{v} is outside {}–{}; using {fixed}", r.start(), r.end()),
        ));
        *v = fixed;
    }
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// An absolute folder, or `None`.
fn folder(v: &mut Option<String>, path: &str, issues: &mut Vec<Issue>) {
    let Some(s) = v.as_mut() else { return };
    let t = s.trim().to_string();
    if t.is_empty() {
        *v = None;
        return;
    }
    let problem = if t.len() > MAX_PATH {
        Some("is too long")
    } else if has_control(&t) {
        Some("contains control characters")
    } else if !Path::new(&t).is_absolute() {
        Some("is not an absolute path")
    } else {
        None
    };
    match problem {
        // The folder itself isn't repeated: it may contain the user's name.
        Some(p) => {
            issues.push(Issue::new(
                path,
                format!("the folder {p}; using the default"),
            ));
            *v = None;
        }
        None => *s = t,
    }
}

/// Whether `s` can be an executable file name.
fn valid_exe(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME && !has_control(s) && !s.contains(['/', '\\'])
}

/// A free-text name (display name, device id), or `None`.
fn opt_text(v: &mut Option<String>, max: usize, path: &str, issues: &mut Vec<Issue>) {
    let Some(s) = v.as_mut() else { return };
    let t = s.trim().to_string();
    if t.is_empty() {
        *v = None;
    } else if t.len() > max || has_control(&t) {
        issues.push(Issue::new(path, "invalid text; using the default"));
        *v = None;
    } else {
        *s = t;
    }
}

/// A list of executable names: trimmed, valid, unique (ignoring case), capped.
fn exe_list(list: &mut Vec<String>, path: &str, issues: &mut Vec<Issue>) {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(list.len().min(MAX_LIST));
    for (i, name) in list.iter().enumerate() {
        let t = name.trim();
        if t.is_empty() {
            continue;
        }
        if !valid_exe(t) {
            issues.push(Issue::new(
                format!("{path}[{i}]"),
                format!("{} is not a program name; dropped", short(t)),
            ));
            continue;
        }
        if seen.insert(t.to_lowercase()) {
            out.push(t.to_string());
        }
    }
    if out.len() > MAX_LIST {
        issues.push(Issue::new(
            path,
            format!("more than {MAX_LIST} entries; the rest were dropped"),
        ));
        out.truncate(MAX_LIST);
    }
    *list = out;
}

fn clips(c: &mut ClipSettings, issues: &mut Vec<Issue>) {
    folder(&mut c.folder, "clips.folder", issues);
    folder(&mut c.recordings_folder, "clips.recordings_folder", issues);
}

fn quality(q: &mut Quality, path: &str, issues: &mut Vec<Issue>) {
    let c = &mut q.custom;
    range(
        &mut c.max_width,
        &WIDTH_RANGE,
        &format!("{path}.custom.max_width"),
        issues,
    );
    range(
        &mut c.max_height,
        &HEIGHT_RANGE,
        &format!("{path}.custom.max_height"),
        issues,
    );
    // Encoders want even sizes.
    c.max_width &= !1;
    c.max_height &= !1;
    range(
        &mut c.fps,
        &FPS_RANGE,
        &format!("{path}.custom.fps"),
        issues,
    );
    range(
        &mut c.bitrate_kbps,
        &BITRATE_RANGE,
        &format!("{path}.custom.bitrate_kbps"),
        issues,
    );
}

fn recording(r: &mut RecordingSettings, issues: &mut Vec<Issue>) {
    range(
        &mut r.buffer_secs,
        &BUFFER_SECS,
        "recording.buffer_secs",
        issues,
    );
    quality(&mut r.quality, "recording.quality", issues);
    if let Some(e) = r.encoder.as_mut() {
        let t = e.trim().to_string();
        if t.is_empty() {
            r.encoder = None;
        } else if t.len() > 64
            || !t
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            issues.push(Issue::new(
                "recording.encoder",
                format!("{} is not an encoder name; using automatic", short(&t)),
            ));
            r.encoder = None;
        } else {
            *e = t;
        }
    }
}

fn audio(a: &mut AudioSettings, path: &str, issues: &mut Vec<Issue>) {
    exe_list(&mut a.voice_apps, &format!("{path}.voice_apps"), issues);
    exe_list(&mut a.exclude_apps, &format!("{path}.exclude_apps"), issues);
    opt_text(
        &mut a.mic_device,
        MAX_PATH,
        &format!("{path}.mic_device"),
        issues,
    );
    if !AUDIO_BITRATES.contains(&a.bitrate_kbps) {
        let nearest = AUDIO_BITRATES
            .iter()
            .copied()
            .min_by_key(|b| b.abs_diff(a.bitrate_kbps))
            .unwrap_or(160);
        issues.push(Issue::new(
            format!("{path}.bitrate_kbps"),
            format!(
                "{} is not one of {AUDIO_BITRATES:?}; using {nearest}",
                a.bitrate_kbps
            ),
        ));
        a.bitrate_kbps = nearest;
    }
}

fn hotkeys(list: &mut Vec<HotkeyBinding>, issues: &mut Vec<Issue>) {
    if list.len() > MAX_HOTKEYS {
        issues.push(Issue::new(
            "hotkeys",
            format!("more than {MAX_HOTKEYS} bindings; the rest were dropped"),
        ));
        list.truncate(MAX_HOTKEYS);
    }
    let mut taken = HashSet::new();
    for (i, b) in list.iter_mut().enumerate() {
        let path = format!("hotkeys[{i}]");
        if b.action == HotkeyAction::ClipSave {
            range(&mut b.secs, &CLIP_SECS, &format!("{path}.secs"), issues);
        } else {
            b.secs = 0;
        }
        let keys = b.keys.trim();
        if keys.is_empty() {
            b.keys.clear();
            continue;
        }
        match keys.parse::<crate::Hotkey>() {
            Ok(h) => {
                if taken.insert(h) {
                    b.keys = h.to_string();
                } else {
                    issues.push(Issue::new(
                        format!("{path}.keys"),
                        format!("{h} is already used by another action; unbound"),
                    ));
                    b.keys.clear();
                }
            }
            Err(e) => {
                issues.push(Issue::new(
                    format!("{path}.keys"),
                    format!("{}: {e}; unbound", short(keys)),
                ));
                b.keys.clear();
            }
        }
    }
}

fn profile(p: &mut Profile, path: &str, issues: &mut Vec<Issue>) {
    if let Some(b) = p.buffer_secs.as_mut() {
        range(b, &BUFFER_SECS, &format!("{path}.buffer_secs"), issues);
    }
    if let Some(q) = p.quality.as_mut() {
        quality(q, &format!("{path}.quality"), issues);
    }
}

fn games(g: &mut GameSettings, issues: &mut Vec<Issue>) {
    let mut seen = HashSet::new();
    let mut users = Vec::with_capacity(g.user_games.len().min(MAX_LIST));
    for (i, u) in g.user_games.iter_mut().enumerate() {
        let path = format!("games.user_games[{i}]");
        let exe = u.exe.trim();
        if !valid_exe(exe) {
            issues.push(Issue::new(
                format!("{path}.exe"),
                format!("{} is not a program name; dropped", short(exe)),
            ));
            continue;
        }
        u.exe = exe.to_string();
        opt_text(&mut u.name, MAX_NAME, &format!("{path}.name"), issues);
        if seen.insert(game_key(&u.exe)) {
            users.push(u.clone());
        }
    }
    if users.len() > MAX_LIST {
        issues.push(Issue::new(
            "games.user_games",
            format!("more than {MAX_LIST} entries; the rest were dropped"),
        ));
        users.truncate(MAX_LIST);
    }
    g.user_games = users;
    exe_list(&mut g.never, "games.never", issues);

    let old = std::mem::take(&mut g.profiles);
    let mut out = BTreeMap::new();
    for (k, mut p) in old {
        let key = game_key(&k);
        let path = format!("games.profiles.{}", short(&k));
        if !valid_exe(&key) {
            issues.push(Issue::new(path, "not a program name; profile dropped"));
            continue;
        }
        if out.contains_key(&key) {
            issues.push(Issue::new(
                path,
                "duplicate profile (ignoring case); dropped",
            ));
            continue;
        }
        if out.len() >= MAX_PROFILES {
            issues.push(Issue::new(
                "games.profiles",
                format!("more than {MAX_PROFILES} profiles; the rest were dropped"),
            ));
            break;
        }
        profile(&mut p, &path, issues);
        out.insert(key, p);
    }
    g.profiles = out;
}
