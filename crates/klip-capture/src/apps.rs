//! Choosing which applications' audio to record.
//!
//! WASAPI captures either one process tree or everything except one process tree per
//! stream. This module turns "system audio without music players" and "these apps" into
//! process ids, given the process list and the processes that have audio sessions. Pure
//! logic, so it's tested without any audio hardware.

use klip_platform::{AudioSession, ProcessInfo};

/// Music and media players left out of system audio by default, so shared clips don't
/// carry someone else's music (executable names, compared case-insensitively).
pub const DEFAULT_EXCLUDED_APPS: &[&str] = &[
    "Spotify.exe",
    "AppleMusic.exe",
    "iTunes.exe",
    "Music.UI.exe",
    "Microsoft.Media.Player.exe",
    "wmplayer.exe",
    "TIDAL.exe",
    "Deezer.exe",
    "Amazon Music.exe",
    "Qobuz.exe",
    "Pandora.exe",
    "SoundCloud.exe",
    "YouTube Music.exe",
    "YouTube Music Desktop App.exe",
    "Cider.exe",
    "Plexamp.exe",
    "foobar2000.exe",
    "MusicBee.exe",
    "AIMP.exe",
    "Winamp.exe",
    "MediaMonkey.exe",
    "vlc.exe",
];

/// What "system audio" records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemPlan {
    /// The whole output mix: no excluded app is running.
    Whole,
    /// Everything except one process tree.
    Except { pid: u32, exe: String },
    /// Several excluded apps play audio, and a stream can only leave out one process
    /// tree, so each other app with an audio session is recorded instead. Apps that start
    /// playing later aren't included until the next recording.
    Apps {
        pids: Vec<u32>,
        excluded: Vec<String>,
    },
}

/// Parent lookups over a process snapshot.
struct Tree<'a> {
    procs: &'a [ProcessInfo],
}

/// Longest parent chain followed (guards against pid reuse loops).
const MAX_DEPTH: usize = 64;

impl Tree<'_> {
    fn get(&self, pid: u32) -> Option<&ProcessInfo> {
        self.procs.iter().find(|p| p.pid == pid)
    }

    /// `pid` and its ancestors, nearest first.
    fn lineage(&self, pid: u32) -> Vec<&ProcessInfo> {
        let mut out: Vec<&ProcessInfo> = Vec::new();
        let mut cur = self.get(pid);
        while let Some(p) = cur {
            if out.len() >= MAX_DEPTH || out.iter().any(|q| q.pid == p.pid) {
                break;
            }
            out.push(p);
            cur = if p.parent_pid == p.pid || p.parent_pid == 0 {
                None
            } else {
                self.get(p.parent_pid)
            };
        }
        out
    }

    fn is_within(&self, pid: u32, root: u32) -> bool {
        self.lineage(pid).iter().any(|p| p.pid == root)
    }
}

fn matches_any(exe: &str, names: &[String]) -> bool {
    names.iter().any(|n| n.eq_ignore_ascii_case(exe))
}

/// Top-most running processes whose executable is one of `names`: their parent isn't one
/// of `names`. Browsers and Electron apps run many same-named processes; capturing each
/// tree once avoids recording the same audio twice.
pub fn app_roots(procs: &[ProcessInfo], names: &[String]) -> Vec<(u32, String)> {
    let tree = Tree { procs };
    let mut roots: Vec<(u32, String)> = procs
        .iter()
        .filter(|p| matches_any(&p.exe, names))
        .filter(|p| {
            p.parent_pid == p.pid
                || tree
                    .get(p.parent_pid)
                    .is_none_or(|parent| !matches_any(&parent.exe, names))
        })
        .map(|p| (p.pid, p.exe.clone()))
        .collect();
    roots.sort_unstable();
    roots
}

/// Drop pids that have an ancestor in the list (its tree already includes them).
fn without_nested(tree: &Tree<'_>, pids: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = pids
        .iter()
        .copied()
        .filter(|&pid| {
            !tree
                .lineage(pid)
                .iter()
                .skip(1)
                .any(|anc| pids.contains(&anc.pid))
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Plan "system audio without `excluded` apps".
pub fn plan_system(
    procs: &[ProcessInfo],
    sessions: &[AudioSession],
    excluded: &[String],
) -> SystemPlan {
    let tree = Tree { procs };
    let roots = app_roots(procs, excluded);
    let has_session = |root: u32| sessions.iter().any(|s| tree.is_within(s.pid, root));
    let excluded_sounding: Vec<&(u32, String)> =
        roots.iter().filter(|(pid, _)| has_session(*pid)).collect();
    let except = |r: &(u32, String)| SystemPlan::Except {
        pid: r.0,
        exe: r.1.clone(),
    };
    match (roots.as_slice(), excluded_sounding.as_slice()) {
        ([], _) => SystemPlan::Whole,
        ([one], _) => except(one),
        // Several are running but at most one plays right now: leave that one (or the
        // first) out. The others are idle; if one starts playing later it is recorded.
        (_, [one]) => except(one),
        ([first, ..], []) => except(first),
        (_, sounding) => {
            let in_excluded = |pid: u32| roots.iter().any(|(r, _)| tree.is_within(pid, *r));
            let keep: Vec<u32> = sessions
                .iter()
                .map(|s| s.pid)
                .filter(|&pid| !in_excluded(pid))
                .collect();
            let pids = without_nested(&tree, &keep);
            match sounding.first() {
                Some(first) if pids.is_empty() => except(first),
                _ => SystemPlan::Apps {
                    pids,
                    excluded: roots.iter().map(|(_, e)| e.clone()).collect(),
                },
            }
        }
    }
}

/// Track label for a set of app names: `Discord`, `Discord + Chrome`, … (no `.exe`).
pub fn label_for(names: &[String]) -> String {
    let parts: Vec<&str> = names
        .iter()
        .map(|n| {
            n.get(..n.len().saturating_sub(4))
                .filter(|_| n.to_ascii_lowercase().ends_with(".exe"))
                .unwrap_or(n)
        })
        .collect();
    let label = parts.join(" + ");
    if label.chars().count() > 60 {
        format!("{} apps", names.len())
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, exe: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: parent,
            exe: exe.into(),
        }
    }

    fn s(pid: u32) -> AudioSession {
        AudioSession { pid, active: true }
    }

    fn defaults() -> Vec<String> {
        DEFAULT_EXCLUDED_APPS
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn procs() -> Vec<ProcessInfo> {
        vec![
            p(4, 0, "System"),
            p(100, 4, "explorer.exe"),
            // Spotify: main process plus a child that renders audio.
            p(200, 100, "Spotify.exe"),
            p(201, 200, "Spotify.exe"),
            // Discord: same shape.
            p(300, 100, "Discord.exe"),
            p(301, 300, "Discord.exe"),
            p(400, 100, "game.exe"),
            p(500, 100, "vlc.exe"),
            // A loop in the parent chain (pid reuse) must not hang.
            p(600, 601, "a.exe"),
            p(601, 600, "b.exe"),
        ]
    }

    #[test]
    fn roots_skip_same_named_children() {
        let r = app_roots(&procs(), &["discord.EXE".to_string()]);
        assert_eq!(r, [(300, "Discord.exe".to_string())]);
        assert!(app_roots(&procs(), &["nope.exe".to_string()]).is_empty());
    }

    #[test]
    fn whole_system_when_no_music_app_runs() {
        let procs: Vec<ProcessInfo> = procs()
            .into_iter()
            .filter(|p| p.exe != "Spotify.exe" && p.exe != "vlc.exe")
            .collect();
        assert_eq!(
            plan_system(&procs, &[s(301), s(400)], &defaults()),
            SystemPlan::Whole
        );
    }

    #[test]
    fn one_music_app_is_left_out_as_a_tree() {
        let procs: Vec<ProcessInfo> = procs().into_iter().filter(|p| p.exe != "vlc.exe").collect();
        assert_eq!(
            plan_system(&procs, &[s(201), s(301), s(400)], &defaults()),
            SystemPlan::Except {
                pid: 200,
                exe: "Spotify.exe".into()
            }
        );
    }

    #[test]
    fn two_running_but_one_playing_leaves_out_the_playing_one() {
        assert_eq!(
            plan_system(&procs(), &[s(400), s(501)], &defaults()),
            SystemPlan::Except {
                pid: 200,
                exe: "Spotify.exe".into()
            },
            "neither plays: leave out the first"
        );
        assert_eq!(
            plan_system(&procs(), &[s(400), s(500)], &defaults()),
            SystemPlan::Except {
                pid: 500,
                exe: "vlc.exe".into()
            }
        );
    }

    #[test]
    fn two_playing_music_apps_switch_to_per_app_capture() {
        let plan = plan_system(
            &procs(),
            &[s(201), s(500), s(300), s(301), s(400), s(600)],
            &defaults(),
        );
        assert_eq!(
            plan,
            SystemPlan::Apps {
                // 301 is inside 300's tree; the 600/601 loop is handled.
                pids: vec![300, 400, 600],
                excluded: vec!["Spotify.exe".into(), "vlc.exe".into()],
            }
        );
        // Only music playing: nothing else to keep, leave out the first.
        assert!(matches!(
            plan_system(&procs(), &[s(201), s(500)], &defaults()),
            SystemPlan::Except { pid: 200, .. }
        ));
    }

    #[test]
    fn labels() {
        assert_eq!(label_for(&["Discord.exe".into()]), "Discord");
        assert_eq!(
            label_for(&["Discord.exe".into(), "chrome".into()]),
            "Discord + chrome"
        );
        assert_eq!(label_for(&vec!["x".repeat(40); 2]), "2 apps");
        assert_eq!(label_for(&["é.exe".into()]), "é");
    }
}
