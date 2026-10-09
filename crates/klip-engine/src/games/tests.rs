use std::time::Instant;

use klip_platform::{ProcessInfo, WindowInfo};
use klip_settings::{GameSettings, UserGame};

use super::*;

fn proc(pid: u32, exe: &str) -> ProcessInfo {
    ProcessInfo {
        pid,
        parent_pid: 1,
        exe: exe.into(),
    }
}

fn win(handle: u64, pid: u32, w: u32, h: u32) -> WindowInfo {
    WindowInfo {
        handle,
        pid,
        title: String::new(),
        client_width: w,
        client_height: h,
        client_x: 0,
        client_y: 0,
        minimized: false,
        monitor: 1,
    }
}

type Item<'a> = (u32, &'a str, &'a [(u64, u32, u32)]);

/// (pid, exe path, windows as (handle, w, h)).
fn snap(items: &[Item<'_>], foreground: Option<u64>) -> Snapshot {
    let mut s = Snapshot {
        foreground,
        windows: Some(Vec::new()),
        ..Snapshot::default()
    };
    for (pid, path, wins) in items {
        s.processes.push(proc(*pid, file_name(path)));
        s.paths.insert(*pid, (*path).to_string());
        for (h, w, ht) in *wins {
            s.windows.as_mut().unwrap().push(win(*h, *pid, *w, *ht));
        }
    }
    s
}

fn names(games: &[DetectedGame]) -> Vec<&str> {
    games.iter().map(|g| g.name.as_str()).collect()
}

#[test]
fn store_folders_name_the_game() {
    let cases = [
        (
            r"C:\Program Files (x86)\Steam\steamapps\common\Counter-Strike Global Offensive\game\bin\win64\cs2.exe",
            Store::Steam,
            "Counter-Strike Global Offensive",
        ),
        (
            r"D:\SteamLibrary\steamapps\common\ELDEN RING\Game\eldenring.exe",
            Store::Steam,
            "ELDEN RING",
        ),
        (
            r"C:\Program Files\Epic Games\Fortnite\FortniteGame\Binaries\Win64\FortniteClient-Win64-Shipping.exe",
            Store::Epic,
            "Fortnite",
        ),
        (
            r"C:\XboxGames\Forza Horizon 5\Content\ForzaHorizon5.exe",
            Store::Xbox,
            "Forza Horizon 5",
        ),
        (
            r"C:\Riot Games\VALORANT\live\ShooterGame\Binaries\Win64\VALORANT-Win64-Shipping.exe",
            Store::Riot,
            "VALORANT",
        ),
        (
            r"C:\Program Files (x86)\GOG Galaxy\Games\Cyberpunk 2077\bin\x64\Cyberpunk2077.exe",
            Store::Gog,
            "Cyberpunk 2077",
        ),
        (
            r"C:\Program Files\EA Games\Battlefield 2042\BF2042.exe",
            Store::Ea,
            "Battlefield 2042",
        ),
        (
            r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\Rainbow Six Siege\RainbowSix.exe",
            Store::Ubisoft,
            "Rainbow Six Siege",
        ),
        (
            "/home/me/.local/share/Steam/steamapps/common/Dota 2/game/bin/linuxsteamrt64/dota2",
            Store::Steam,
            "Dota 2",
        ),
        (
            r"Z:\home\me\.local\share\Steam\steamapps\common\Hades\x64\Hades.exe",
            Store::Steam,
            "Hades",
        ),
        (
            "/home/me/Games/Heroic/Hollow Knight/hollow_knight.x86_64",
            Store::Heroic,
            "Hollow Knight",
        ),
        (
            "C:\\Users\\\u{dc}n\u{ef}c\u{f8}d\u{e9}\\AppData\\Roaming\\itch\\apps\\Celeste\\Celeste.exe",
            Store::Itch,
            "Celeste",
        ),
    ];
    for (path, store, name) in cases {
        assert_eq!(
            library_game(path),
            Some((store, name.to_string())),
            "{path}"
        );
    }
    for path in [
        r"C:\Program Files (x86)\Steam\steamapps\common\Steamworks Shared\_CommonRedist\x.exe",
        r"C:\Program Files (x86)\Steam\steamapps\common\SteamVR\bin\win64\vrmonitor.exe",
        r"C:\Riot Games\Riot Client\RiotClientServices.exe",
        r"C:\Program Files (x86)\Steam\steamapps\common\loose.exe",
        r"C:\Windows\explorer.exe",
        r"C:\Program Files\Mozilla Firefox\firefox.exe",
        "",
    ] {
        assert_eq!(library_game(path), None, "{path}");
    }
}

#[test]
fn helpers_and_names() {
    for exe in [
        "UnityCrashHandler64.exe",
        "CrashReportClient.exe",
        "steamwebhelper.exe",
        "EpicGamesLauncher.exe",
        "unins000.exe",
        "EasyAntiCheat_EOS_Setup.exe",
        "BEService_x64.exe",
        "Steam.exe",
    ] {
        assert!(is_helper(exe), "{exe}");
    }
    for exe in [
        "CrashBandicoot.exe",
        "cs2.exe",
        "eldenring.exe",
        "Hades.exe",
    ] {
        assert!(!is_helper(exe), "{exe}");
    }
    assert_eq!(name_from_exe("VALORANT-Win64-Shipping.exe"), "VALORANT");
    assert_eq!(name_from_exe("hollow_knight.x86_64"), "hollow_knight");
    assert_eq!(name_from_exe("game.EXE"), "game");
    assert_eq!(name_from_exe(".exe"), ".exe");
    assert_eq!(name_from_exe("-Shipping.exe"), "-Shipping");
}

#[test]
fn folder_names_are_safe() {
    assert_eq!(folder_name("ELDEN RING"), "ELDEN RING");
    assert_eq!(
        folder_name("Half-Life 2: Episode One"),
        "Half-Life 2 Episode One"
    );
    assert_eq!(folder_name("  a/b\\c?*  "), "a b c");
    assert_eq!(folder_name("..."), "Other");
    assert_eq!(folder_name(""), "Other");
    assert_eq!(folder_name("CON"), "CON_");
    assert_eq!(folder_name("com1.txt"), "com1.txt_");
    assert_eq!(folder_name("Trailing."), "Trailing");
    assert_eq!(folder_name(&"x".repeat(300)).chars().count(), 64);
    assert_eq!(folder_name("tab\there\u{0}"), "tab here");
}

#[test]
fn detects_store_games_with_big_windows() {
    let s = snap(
        &[
            (
                10,
                r"C:\Games\steamapps\common\Game A\a.exe",
                &[(1, 1920, 1080)],
            ),
            (
                11,
                r"C:\Games\steamapps\common\Game B\b.exe",
                &[(2, 800, 600), (3, 2560, 1440)],
            ),
            (
                12,
                r"C:\Games\steamapps\common\Game C\c.exe",
                &[(4, 400, 300)],
            ),
            (
                13,
                r"C:\Games\steamapps\common\Game D\UnityCrashHandler64.exe",
                &[(5, 1920, 1080)],
            ),
            (14, r"C:\Windows\notepad.exe", &[(6, 1920, 1080)]),
            (15, r"C:\Games\steamapps\common\Game E\e.exe", &[]),
        ],
        None,
    );
    let games = detect(&s, &GameSettings::default());
    assert_eq!(
        names(&games),
        vec!["Game B", "Game A"],
        "biggest window first"
    );
    assert_eq!(games[0].window, Some(3));
    assert_eq!(games[0].source, Source::Library(Store::Steam));
    assert_eq!(games[0].key, "b.exe");
}

#[test]
fn foreground_wins_and_user_lists_apply() {
    let s = snap(
        &[
            (
                10,
                r"C:\Games\steamapps\common\Big\big.exe",
                &[(1, 3840, 2160)],
            ),
            (
                11,
                r"C:\Games\steamapps\common\Small\small.exe",
                &[(2, 1280, 720)],
            ),
            (
                12,
                r"C:\dev\target\debug\klip-testgame.exe",
                &[(3, 400, 300)],
            ),
            (
                13,
                r"C:\Games\steamapps\common\Never\never.exe",
                &[(4, 1920, 1080)],
            ),
        ],
        Some(2),
    );
    let settings = GameSettings {
        user_games: vec![UserGame {
            exe: "KLIP-TESTGAME.exe".into(),
            name: None,
        }],
        never: vec!["NEVER.EXE".into()],
        ..GameSettings::default()
    };
    let games = detect(&s, &settings);
    assert_eq!(names(&games), vec!["Small", "Big", "klip-testgame"]);
    assert!(games[0].foreground);
    assert_eq!(games[2].source, Source::User);

    // The user's name wins.
    let settings = GameSettings {
        user_games: vec![UserGame {
            exe: "big.exe".into(),
            name: Some("My Big Game".into()),
        }],
        ..GameSettings::default()
    };
    let games = detect(&s, &settings);
    assert!(names(&games).contains(&"My Big Game"));
}

#[test]
fn minimised_windows_dont_start_detection() {
    let mut s = snap(
        &[(
            10,
            r"C:\Games\steamapps\common\Game\g.exe",
            &[(1, 1920, 1080)],
        )],
        None,
    );
    s.windows.as_mut().unwrap()[0].minimized = true;
    assert!(detect(&s, &GameSettings::default()).is_empty());
    assert!(alive(&s, 10));
    assert!(!alive(&s, 99));
}

#[test]
fn without_a_window_list_processes_count() {
    let mut s = snap(
        &[
            (
                20,
                "/home/me/.local/share/Steam/steamapps/common/Dota 2/game/bin/dota2",
                &[],
            ),
            (
                21,
                "/home/me/.local/share/Steam/steamapps/common/Dota 2/game/bin/dota2",
                &[],
            ),
            (22, "/usr/bin/firefox", &[]),
            (
                23,
                r"Z:\home\me\Steam\steamapps\common\Hades\x64\Hades.exe",
                &[],
            ),
        ],
        None,
    );
    s.windows = None;
    // Proton: the process list says wine64-preloader, the path names the program.
    s.processes[3].exe = "wine64-preloader".into();
    let games = detect(&s, &GameSettings::default());
    assert_eq!(names(&games), vec!["Dota 2", "Hades"]);
    assert_eq!(games[1].exe, "Hades.exe");
    assert_eq!(games[1].window, None);
}

#[test]
fn candidates_list_programs_with_windows() {
    let s = snap(
        &[
            (
                10,
                "C:/Games/steamapps/common/Big/big.exe",
                &[(1, 1920, 1080)],
            ),
            (11, "C:/Tools/editor.exe", &[(2, 1200, 800), (3, 900, 700)]),
            (12, "C:/Tools/tiny.exe", &[(4, 200, 100)]),
            (
                13,
                "C:/Program Files/Mozilla Firefox/firefox.exe",
                &[(5, 1600, 900)],
            ),
            (14, "C:/Tools/hidden.exe", &[(6, 1600, 900)]),
        ],
        None,
    );
    let settings = GameSettings {
        never: vec!["hidden.exe".into()],
        ..GameSettings::default()
    };
    let c = candidates(&s, &settings);
    let got: Vec<(&str, &str, bool)> = c
        .iter()
        .map(|c| (c.exe.as_str(), c.name.as_str(), c.detected))
        .collect();
    assert_eq!(
        got,
        vec![("big.exe", "Big", true), ("editor.exe", "editor", false)]
    );
}

/// Cost of one detection poll on this machine (it runs every 2 s).
#[test]
fn a_poll_is_cheap() {
    let settings = GameSettings::default();
    let mut times = Vec::new();
    for _ in 0..9 {
        let t = Instant::now();
        let s = snapshot().unwrap();
        let _ = detect(&s, &settings);
        times.push(t.elapsed());
    }
    times.sort();
    let median = times[times.len() / 2];
    for g in detect(&snapshot().unwrap(), &settings) {
        eprintln!("running game: {} ({}, {:?})", g.name, g.exe, g.source);
    }
    eprintln!(
        "detection poll: median {median:?}, max {:?}",
        times[times.len() - 1]
    );
    assert!(median.as_millis() < 100, "{median:?}");
}
