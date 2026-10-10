use std::time::{Duration, Instant};

use proptest::prelude::*;
use serde_json::{Value, json};

use crate::hotkey::all_keys;
use crate::*;

fn lenient(v: &Value) -> (Settings, Vec<Issue>) {
    Settings::from_json_lenient(&v.to_string())
}

#[test]
fn defaults_are_valid_and_round_trip() {
    let mut d = Settings::default();
    assert!(d.validate().is_empty());
    assert_eq!(d, Settings::default());
    let (s, issues) = lenient(&d.to_value().unwrap());
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(s, d);
    // A missing or empty object is the defaults too.
    assert_eq!(lenient(&json!({})).0, d);
}

#[test]
fn garbage_gives_defaults_with_an_issue() {
    for text in ["", "{", "[1,2]", "42", "null", "\"x\"", "{\"a\":}"] {
        let (s, issues) = Settings::from_json_lenient(text);
        assert_eq!(s, Settings::default(), "{text}");
        assert_eq!(issues.len(), 1, "{text}: {issues:?}");
        assert_eq!(issues[0].path, "");
    }
}

#[test]
fn a_wrong_field_is_repaired_alone() {
    let mut v = Settings::default().to_value().unwrap();
    v["recording"]["buffer_secs"] = json!("lots");
    v["recording"]["show_cursor"] = json!(false);
    v["appearance"]["theme"] = json!("purple");
    v["storage"]["cap_gb"] = json!(-5);
    v["audio"] = json!(7);
    v["overlay"]["mode"] = json!("full");
    let (s, issues) = lenient(&v);
    let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
    assert!(paths.contains(&"recording.buffer_secs"), "{paths:?}");
    assert!(paths.contains(&"appearance.theme"), "{paths:?}");
    assert!(paths.contains(&"storage.cap_gb"), "{paths:?}");
    assert!(paths.contains(&"audio"), "{paths:?}");
    assert_eq!(issues.len(), 4, "{issues:?}");
    assert_eq!(s.recording.buffer_secs, 120);
    assert!(
        !s.recording.show_cursor,
        "the good field next to it is kept"
    );
    assert_eq!(s.appearance.theme, Theme::System);
    assert_eq!(s.storage.cap_gb, 50);
    assert_eq!(s.audio, AudioSettings::default());
    assert_eq!(s.overlay.mode, OverlayMode::Full);
}

#[test]
fn bad_list_entries_go_one_by_one() {
    let v = json!({
        "hotkeys": [
            {"action": "clip.save", "keys": "ctrl+f5", "secs": 15},
            {"action": "launch.missiles", "keys": "F1"},
            {"action": "bookmark.add", "keys": "ALT+F8"},
            7,
            {"action": "session.toggle", "keys": "Shift+Banana"},
            {"action": "clip.save", "keys": "Control+F5", "secs": 60},
        ],
        "audio": {"voice_apps": ["Discord.exe", 5, "  ", "discord.EXE", "a/b.exe", "Mumble.exe"]},
    });
    let (s, issues) = lenient(&v);
    let keys: Vec<(&str, u32)> = s
        .hotkeys
        .iter()
        .map(|b| (b.keys.as_str(), b.secs))
        .collect();
    assert_eq!(
        keys,
        vec![("Ctrl+F5", 15), ("Alt+F8", 0), ("", 0), ("", 60)],
        "{issues:?}"
    );
    assert_eq!(s.audio.voice_apps, vec!["Discord.exe", "Mumble.exe"]);
    // Removed: hotkeys[1] (action), hotkeys[2] after the shift (the 7), voice_apps[1].
    // Repaired: Banana, the duplicate Ctrl+F5, the path-like app name.
    assert_eq!(issues.len(), 6, "{issues:?}");
}

#[test]
fn values_are_clamped_and_normalised() {
    let v = json!({
        "recording": {
            "buffer_secs": 99999,
            "encoder": "nvh264enc; rm -rf",
            "quality": {"preset": "custom", "custom": {"max_width": 1921, "max_height": 10, "fps": 1000, "bitrate_kbps": 5}}
        },
        "audio": {"bitrate_kbps": 150, "mic_device": "  "},
        "overlay": {"seconds": 0},
        "export": {"discord_limit_mb": 0},
        "clips": {"folder": "relative/path", "recordings_folder": "   "},
        "games": {
            "user_games": [{"exe": " Game.exe ", "name": " My Game "}, {"exe": "game.EXE"}, {"exe": ""}],
            "profiles": {"Game.EXE": {"buffer_secs": 1}, "game.exe": {}, "": {}},
        },
    });
    let (s, issues) = lenient(&v);
    assert_eq!(s.recording.buffer_secs, 1800);
    assert_eq!(s.recording.encoder, None);
    let c = s.recording.quality.custom;
    assert_eq!(
        (c.max_width, c.max_height, c.fps, c.bitrate_kbps),
        (1920, 240, 240, 1000)
    );
    assert_eq!(s.audio.bitrate_kbps, 160);
    assert_eq!(s.audio.mic_device, None);
    assert_eq!(s.overlay.seconds, 1);
    assert_eq!(s.export.discord_limit_mb, 1);
    assert_eq!(s.clips.folder, None);
    assert_eq!(s.clips.recordings_folder, None);
    assert_eq!(s.games.user_games.len(), 1);
    assert_eq!(s.games.user_games[0].exe, "Game.exe");
    assert_eq!(s.games.user_games[0].name.as_deref(), Some("My Game"));
    let keys: Vec<&String> = s.games.profiles.keys().collect();
    assert_eq!(keys, vec!["game.exe"]);
    assert_eq!(s.games.profiles["game.exe"].buffer_secs, Some(5));
    let mut again = s.clone();
    assert!(
        again.validate().is_empty(),
        "validating twice changes nothing"
    );
    assert_eq!(again, s);
    assert!(issues.len() >= 12, "{issues:?}");
    // Folder issues don't repeat the folder (it may hold the user's name).
    let folder = issues.iter().find(|i| i.path == "clips.folder").unwrap();
    assert!(!folder.message.contains("relative"));
}

#[test]
fn newer_files_load_with_a_note() {
    let (s, issues) =
        lenient(&json!({"version": 7, "future": {"thing": 1}, "overlay": {"sound": false}}));
    assert_eq!(s.version, SCHEMA_VERSION);
    assert!(!s.overlay.sound);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].path, "version");
}

#[test]
fn hostile_sizes_stay_cheap() {
    let bad: Vec<Value> = (0..200_000).map(|i| json!({"action": i})).collect();
    let mut profiles = serde_json::Map::new();
    for i in 0..5000 {
        profiles.insert(format!("g{i}.exe"), json!({"buffer_secs": "x"}));
    }
    let v = json!({"hotkeys": bad, "games": {"profiles": profiles}});
    let t = Instant::now();
    let (mut s, issues) = lenient(&v);
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    assert!(!issues.is_empty());
    assert!(s.validate().is_empty());
    assert!(s.games.profiles.len() <= MAX_PROFILES);
}

#[test]
fn patches_are_strict() {
    let cur = Settings::default();
    let s = cur
        .apply_patch(&json!({"recording": {"buffer_secs": 300}, "overlay": {"mode": "off"}}))
        .unwrap();
    assert_eq!(s.recording.buffer_secs, 300);
    assert_eq!(s.overlay.mode, OverlayMode::Off);
    assert_eq!(s.audio, cur.audio);
    let g = cur
        .apply_patch(&json!({"appearance": {"theme": "graphite"}}))
        .unwrap();
    assert_eq!(g.appearance.theme, Theme::Graphite);

    let err = |patch: Value| match cur.apply_patch(&patch) {
        Err(SettingsError::Invalid(i)) => i,
        other => panic!("{patch}: {other:?}"),
    };
    let i = err(json!({"recording": {"bufer_secs": 300}}));
    assert_eq!(i.path, "recording.bufer_secs");
    assert_eq!(i.message, "unknown setting");
    let i = err(json!({"recording": {"buffer_secs": "300"}}));
    assert_eq!(i.path, "recording.buffer_secs");
    let i = err(json!({"recording": {"buffer_secs": 2}}));
    assert_eq!(i.path, "recording.buffer_secs");
    assert_eq!(
        i.message, "2 is outside 5–1800",
        "no 'using …' in a rejection"
    );
    let i = err(
        json!({"hotkeys": [{"action": "bookmark.add", "keys": "Alt+Q"}, {"action": "clip.save", "keys": "alt+q", "secs": 30}]}),
    );
    assert_eq!(i.path, "hotkeys[1].keys");
    let i = err(json!({"hotkeys": [{"action": "bookmark.add", "keys": "Alt+Q", "colour": 1}]}));
    assert_eq!(i.path, "hotkeys[0].colour");
    err(json!([1]));
    err(json!({"clips": {"folder": "not/absolute"}}));

    // `null` resets to the default; `version` can't be changed.
    let changed = cur
        .apply_patch(&json!({"overlay": {"sound": false}}))
        .unwrap();
    let back = changed
        .apply_patch(&json!({"overlay": {"sound": null}, "version": 99}))
        .unwrap();
    assert_eq!(back, cur);

    // Profiles: add, normalise the key, remove.
    let with = cur
        .apply_patch(
            &json!({"games": {"profiles": {"MyGame.exe": {"buffer_secs": 60, "mic": true}}}}),
        )
        .unwrap();
    assert_eq!(with.games.profiles["mygame.exe"].buffer_secs, Some(60));
    let without = with
        .apply_patch(&json!({"games": {"profiles": {"mygame.exe": null}}}))
        .unwrap();
    assert!(without.games.profiles.is_empty());
}

#[test]
fn profiles_override_globals() {
    let s = Settings::default()
        .apply_patch(&json!({
            "audio": {"mic": false},
            "games": {"profiles": {"cs2.exe": {
                "buffer_secs": 45,
                "quality": {"preset": "high"},
                "mic": true,
                "desktop_audio": "system",
                "auto_clip": true,
            }}},
        }))
        .unwrap();
    let e = s.effective(Some("CS2.exe"));
    assert_eq!(e.buffer_secs, 45);
    assert_eq!(e.quality.preset, Preset::High);
    assert!(e.audio.mic);
    assert_eq!(e.audio.desktop, DesktopAudio::System);
    assert!(e.auto_clip);
    let g = s.effective(Some("other.exe"));
    assert_eq!(g.buffer_secs, 120);
    assert_eq!(g.quality.preset, Preset::Balanced);
    assert!(!g.audio.mic);
    assert_eq!(s.effective(None), g);
}

#[test]
fn presets_resolve() {
    let q = |preset| Quality {
        preset,
        custom: CustomQuality::default(),
    };
    let low = q(Preset::Low).params(Some(240));
    assert_eq!(
        (low.max_height, low.fps, low.bitrate_kbps),
        (720, 30, 8_000)
    );
    let bal = q(Preset::Balanced).params(None);
    assert_eq!(
        (bal.max_width, bal.max_height, bal.fps, bal.bitrate_kbps),
        (1920, 1080, 60, 20_000)
    );
    assert_eq!(q(Preset::High).params(Some(240)).fps, 144);
    assert_eq!(q(Preset::High).params(Some(120)).fps, 120);
    assert_eq!(q(Preset::High).params(Some(30)).fps, 60);
    assert_eq!(q(Preset::High).params(None).fps, 60);
    let mut c = q(Preset::Custom);
    c.custom.codec = VideoCodec::Av1;
    c.custom.fps = 90;
    let p = c.params(Some(60));
    assert_eq!((p.fps, p.codec), (90, VideoCodec::Av1));
    for preset in [Preset::Low, Preset::Balanced, Preset::High] {
        assert_eq!(q(preset).params(Some(165)).codec, VideoCodec::H264);
    }
}

#[test]
fn hotkeys_parse_and_print() {
    let h: Hotkey = "alt+shift+f10".parse().unwrap();
    assert_eq!(h.to_string(), "Alt+Shift+F10");
    assert_eq!(h.to_trigger(), "ALT+SHIFT+F10");
    let h: Hotkey = "CTRL+LOGO+a".parse().unwrap();
    assert_eq!(h.to_string(), "Ctrl+Win+A");
    assert_eq!(h.to_trigger(), "CTRL+LOGO+a");
    let h: Hotkey = " Super + KP_Add ".parse().unwrap();
    assert_eq!(h.to_string(), "Win+NumAdd");
    assert_eq!(h.to_trigger(), "LOGO+KP_Add");
    assert_eq!("Prior".parse::<Hotkey>().unwrap().to_string(), "PageUp");
    assert_eq!(
        "ctrl+`".parse::<Hotkey>().unwrap().to_trigger(),
        "CTRL+grave"
    );
    assert!(!"F13".parse::<Hotkey>().unwrap().has_modifier());

    for bad in [
        "", "Ctrl", "Ctrl+Alt", "A+B", "F0", "F25", "F01", "Ctrl++", "Hyper+A", "Num10",
    ] {
        assert!(bad.parse::<Hotkey>().is_err(), "{bad}");
    }
    assert_eq!("x".repeat(65).parse::<Hotkey>(), Err(HotkeyError::TooLong));
}

#[test]
fn every_key_round_trips_with_every_modifier_set() {
    for key in all_keys() {
        for m in 0u8..16 {
            let h = Hotkey {
                ctrl: m & 1 != 0,
                alt: m & 2 != 0,
                shift: m & 4 != 0,
                logo: m & 8 != 0,
                key,
            };
            assert_eq!(h.to_string().parse::<Hotkey>(), Ok(h), "{h}");
            assert_eq!(
                h.to_trigger().parse::<Hotkey>(),
                Ok(h),
                "{}",
                h.to_trigger()
            );
        }
    }
}

#[test]
fn files_load_save_and_keep_bad_copies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Klip").join("settings.json");
    let l = load(&path);
    assert_eq!(l.status, LoadStatus::Missing);
    assert_eq!(l.settings, Settings::default());

    let s = Settings::default()
        .apply_patch(&json!({"recording": {"buffer_secs": 90}}))
        .unwrap();
    save(&path, &s).unwrap();
    let l = load(&path);
    assert_eq!(l.status, LoadStatus::Loaded);
    assert_eq!(l.settings, s);
    assert!(l.backup.is_none());
    assert!(!path.with_extension("json.tmp").exists());

    std::fs::write(&path, "\u{feff}{\"recording\": {\"buffer_secs\": 90}}").unwrap();
    assert_eq!(load(&path).status, LoadStatus::Loaded, "a BOM is fine");

    std::fs::write(&path, "{\"recording\": {\"buffer_secs\": true}}").unwrap();
    let l = load(&path);
    assert_eq!(l.status, LoadStatus::Repaired);
    let bad = l.backup.unwrap();
    assert_eq!(
        std::fs::read_to_string(&bad).unwrap(),
        "{\"recording\": {\"buffer_secs\": true}}"
    );

    std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(load(&path).status, LoadStatus::Reset);

    let big = vec![b' '; usize::try_from(MAX_FILE_BYTES).unwrap() + 10];
    std::fs::write(&path, big).unwrap();
    let l = load(&path);
    assert_eq!(l.status, LoadStatus::Reset);
    assert_eq!(l.settings, Settings::default());
}

fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u32>().prop_map(Value::from),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        "[a-zA-Z0-9 .+_\\-/\\\\]{0,12}".prop_map(Value::from),
        prop_oneof![
            Just("clip.save"),
            Just("Alt+F10"),
            Just("custom"),
            Just("high"),
            Just("trash"),
            Just("C:\\\\Videos"),
            Just("/home/x"),
            Just("game.exe"),
            Just("oled"),
            Just("off"),
        ]
        .prop_map(Value::from),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        let key = prop_oneof![
            Just("version"),
            Just("app"),
            Just("clips"),
            Just("recording"),
            Just("audio"),
            Just("hotkeys"),
            Just("overlay"),
            Just("storage"),
            Just("export"),
            Just("discord_limit_mb"),
            Just("appearance"),
            Just("games"),
            Just("buffer_secs"),
            Just("quality"),
            Just("preset"),
            Just("custom"),
            Just("keys"),
            Just("action"),
            Just("secs"),
            Just("profiles"),
            Just("user_games"),
            Just("exe"),
            Just("folder"),
            Just("voice_apps"),
            Just("mode"),
            Just("theme"),
            Just("fps"),
        ]
        .prop_map(String::from);
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::btree_map(key, inner, 0..8)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn any_json_loads_to_valid_settings(v in arb_json()) {
        let (s, _) = lenient(&v);
        let mut again = s.clone();
        prop_assert!(again.validate().is_empty());
        let (back, issues) = lenient(&s.to_value().unwrap());
        prop_assert!(issues.is_empty(), "{:?}", issues);
        prop_assert_eq!(back, s);
    }

    #[test]
    fn any_patch_is_applied_or_rejected_cleanly(v in arb_json()) {
        let cur = Settings::default();
        if let Ok(mut s) = cur.apply_patch(&v) {
            prop_assert!(s.validate().is_empty());
        }
    }

    #[test]
    fn any_text_hotkey_never_panics(s in "\\PC{0,80}") {
        if let Ok(h) = s.parse::<Hotkey>() {
            prop_assert_eq!(h.to_string().parse::<Hotkey>(), Ok(h));
        }
    }
}
