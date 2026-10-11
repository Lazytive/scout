//! Custom theme commands (M10, ADR 0019): import → list → export round trips, the
//! editor's saves, deleting the theme in use, and hostile files. No recording: the replay
//! buffer and the control channel are off.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use klip_engine::{Engine, EngineOptions, Event};
use klip_settings::theme_file;
use serde_json::{Value, json};

const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 1, 2,
];
const JPG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 16, 7];

fn quiet_settings(dir: &Path) -> PathBuf {
    let path = dir.join("settings.json");
    let s = json!({
        "app": {"control_channel": false},
        "clips": {"folder": dir.join("clips")},
        "hotkeys": [],
        "games": {"buffer": "off"},
    });
    std::fs::write(&path, s.to_string()).unwrap();
    path
}

fn start(path: PathBuf) -> (Engine, Receiver<Event>) {
    let (tx, rx) = std::sync::mpsc::sync_channel(klip_engine::events::QUEUE);
    let mut opts = EngineOptions::new(path);
    opts.events = Some(tx);
    opts.poll = Duration::from_millis(100);
    (Engine::start(opts).unwrap(), rx)
}

fn next(rx: &Receiver<Event>, name: &str) -> Value {
    loop {
        let e = rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("no {name} event"))
            .to_json();
        if e["event"] == name {
            return e;
        }
    }
}

fn shared_file(dir: &Path, name: &str, picture: Option<&[u8]>) -> PathBuf {
    let mut t = theme_file::CustomTheme {
        name: name.into(),
        base: "graphite".into(),
        radius: 9,
        font: Some("Inter".into()),
        ..Default::default()
    };
    t.colors.insert("accent".into(), "#e0601f".into());
    let pic = picture.map(|b| theme_file::Picture::from_bytes(b.to_vec()).unwrap());
    if pic.is_some() {
        t.backdrop = Some(theme_file::Backdrop {
            dim: 40,
            opacity: 75,
        });
    }
    let path = dir.join(format!("{name}.klip-theme"));
    std::fs::write(
        &path,
        serde_json::to_vec(&t.to_export(pic.as_ref())).unwrap(),
    )
    .unwrap();
    path
}

fn code(r: Result<Value, klip_control::CommandError>) -> &'static str {
    r.unwrap_err().code
}

#[test]
fn import_list_export_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, rx) = start(quiet_settings(dir.path()));
    next(&rx, "ready");

    let l = engine.handle("theme.list", &Value::Null).unwrap();
    assert_eq!(l["themes"], json!([]));
    assert_eq!(
        Path::new(l["dir"].as_str().unwrap()),
        klip_engine::themes_dir(&dir.path().join("settings.json"))
    );

    let src = shared_file(dir.path(), "Sun Set", Some(PNG));
    let imp = engine
        .handle("theme.import", &json!({ "path": src }))
        .unwrap();
    assert_eq!(imp["id"], "sun-set");
    assert_eq!(imp["issues"], json!([]));
    assert_eq!(next(&rx, "themes_changed")["id"], "sun-set");
    let t = &imp["theme"];
    assert_eq!(t["name"], "Sun Set");
    assert_eq!(t["base"], "graphite");
    assert_eq!(t["colors"]["accent"], "#e0601f");
    let pic = Path::new(t["background"]["path"].as_str().unwrap());
    assert_eq!(std::fs::read(pic).unwrap(), PNG);
    assert_eq!(pic.file_name().unwrap(), "sun-set.png");
    assert!(t["background"]["version"].as_u64().unwrap() > 0);
    assert_eq!(t["background"]["opacity"], 75);

    // The same file again gets its own id.
    let again = engine
        .handle("theme.import", &json!({ "path": src }))
        .unwrap();
    assert_eq!(again["id"], "sun-set-2");
    let l = engine.handle("theme.list", &Value::Null).unwrap();
    assert_eq!(l["themes"].as_array().unwrap().len(), 2);
    assert_eq!(l["issues"], json!([]));

    // Export = the original file's content.
    let out = dir.path().join("out.klip-theme");
    engine
        .handle("theme.export", &json!({ "id": "sun-set", "path": out }))
        .unwrap();
    let a = theme_file::parse_export(&std::fs::read(&out).unwrap()).unwrap();
    let b = theme_file::parse_export(&std::fs::read(&src).unwrap()).unwrap();
    assert!(a.issues.is_empty());
    assert_eq!(a.theme, b.theme);
    assert_eq!(a.picture, b.picture);
}

#[test]
fn editor_saves_create_update_and_swap_pictures() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, rx) = start(quiet_settings(dir.path()));
    next(&rx, "ready");
    let themes = klip_engine::themes_dir(&dir.path().join("settings.json"));

    let ui = json!({ "name": "Mine", "base": "light", "colors": { "panel": "#fafafa" },
        "radius": 2, "font": null, "background": null });
    let s = engine
        .handle("theme.save", &json!({ "theme": ui }))
        .unwrap();
    assert_eq!(s["id"], "mine");
    assert_eq!(s["theme"]["background"], Value::Null);

    // A picture from a file the user picked.
    let jpg = dir.path().join("pic.jpg");
    std::fs::write(&jpg, JPG).unwrap();
    let s = engine
        .handle(
            "theme.save",
            &json!({ "id": "mine", "theme": ui, "picture": { "file": jpg } }),
        )
        .unwrap();
    assert_eq!(s["theme"]["background"]["dim"], theme_file::DIM_DEFAULT);
    assert!(themes.join("mine.jpg").is_file());

    // No `picture`: kept (with the edited backdrop); a PNG replaces the JPEG.
    let mut ui2 = ui.clone();
    ui2["background"] = json!({ "dim": 10, "opacity": 90 });
    let s = engine
        .handle("theme.save", &json!({ "id": "mine", "theme": ui2 }))
        .unwrap();
    assert_eq!(s["theme"]["background"]["dim"], 10);
    assert!(themes.join("mine.jpg").is_file());
    let png = dir.path().join("pic.png");
    std::fs::write(&png, PNG).unwrap();
    engine
        .handle(
            "theme.save",
            &json!({ "id": "mine", "theme": ui2, "picture": { "file": png } }),
        )
        .unwrap();
    assert!(themes.join("mine.png").is_file());
    assert!(!themes.join("mine.jpg").exists());

    // `picture: null` removes it.
    let s = engine
        .handle(
            "theme.save",
            &json!({ "id": "mine", "theme": ui2, "picture": null }),
        )
        .unwrap();
    assert_eq!(s["theme"]["background"], Value::Null);
    assert!(!themes.join("mine.png").exists());

    // Strict: a bad colour, an unknown id, a non-picture file.
    let mut bad = ui.clone();
    bad["colors"]["panel"] = json!("red");
    assert_eq!(
        code(engine.handle("theme.save", &json!({ "theme": bad }))),
        "bad_args"
    );
    assert_eq!(
        code(engine.handle("theme.save", &json!({ "id": "nope", "theme": ui }))),
        "bad_args"
    );
    let txt = dir.path().join("x.png");
    std::fs::write(&txt, b"not a picture").unwrap();
    assert_eq!(
        code(engine.handle(
            "theme.save",
            &json!({ "id": "mine", "theme": ui, "picture": { "file": txt } })
        )),
        "bad_args"
    );
}

#[test]
fn deleting_the_theme_in_use_follows_the_system() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, rx) = start(quiet_settings(dir.path()));
    next(&rx, "ready");
    let src = shared_file(dir.path(), "Neon", Some(PNG));
    let id = engine
        .handle("theme.import", &json!({ "path": src }))
        .unwrap()["id"]
        .clone();
    engine
        .handle(
            "settings.set",
            &json!({ "appearance": { "theme": "custom", "custom_theme": id } }),
        )
        .unwrap();
    let d = engine.handle("theme.delete", &json!({ "id": id })).unwrap();
    assert_eq!(d["deleted"], true);
    let s = engine.handle("settings.get", &Value::Null).unwrap();
    assert_eq!(s["settings"]["appearance"]["theme"], "system");
    assert_eq!(s["settings"]["appearance"]["custom_theme"], id);
    let themes = klip_engine::themes_dir(&dir.path().join("settings.json"));
    assert_eq!(std::fs::read_dir(&themes).unwrap().count(), 0);
    // Deleting it again is fine; a bad id isn't.
    assert_eq!(
        engine.handle("theme.delete", &json!({ "id": id })).unwrap()["deleted"],
        false
    );
    assert_eq!(
        code(engine.handle("theme.delete", &json!({ "id": "../settings" }))),
        "bad_args"
    );
}

#[test]
fn hostile_files_are_refused_or_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, rx) = start(quiet_settings(dir.path()));
    next(&rx, "ready");
    let themes = klip_engine::themes_dir(&dir.path().join("settings.json"));

    for (name, bytes) in [
        ("garbage", b"\x00\x01binary".to_vec()),
        ("newer", br#"{"klip_theme": 7, "name": "x"}"#.to_vec()),
        ("huge", vec![b' '; theme_file::MAX_EXPORT_BYTES + 1]),
    ] {
        let p = dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        assert_eq!(
            code(engine.handle("theme.import", &json!({ "path": p }))),
            "bad_args",
            "{name}"
        );
    }
    assert_eq!(
        code(engine.handle("theme.import", &json!({ "path": "relative.klip-theme" }))),
        "bad_args"
    );
    assert_eq!(
        code(engine.handle("theme.export", &json!({ "id": "x", "path": "rel" }))),
        "bad_args"
    );

    // Repairs come back as issues.
    let p = dir.path().join("odd.klip-theme");
    std::fs::write(
        &p,
        json!({ "klip_theme": 1, "name": "Odd", "colors": { "panel": "nope" }, "radius": 50 })
            .to_string(),
    )
    .unwrap();
    let imp = engine
        .handle("theme.import", &json!({ "path": p }))
        .unwrap();
    let paths: Vec<&str> = imp["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["colors.panel", "radius"]);

    // Files edited behind Klip's back: a picture outside the theme, an oversized file,
    // a name that isn't an id. None of them crash the list.
    std::fs::write(
        themes.join("sneaky.json"),
        json!({ "klip_theme": 1, "name": "S", "background": { "file": "odd.png" } }).to_string(),
    )
    .unwrap();
    std::fs::write(
        themes.join("big.json"),
        vec![b' '; theme_file::MAX_STORED_BYTES + 1],
    )
    .unwrap();
    std::fs::write(themes.join("Not An Id.json"), b"{}").unwrap();
    let l = engine.handle("theme.list", &Value::Null).unwrap();
    let ids: Vec<&str> = l["themes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["odd", "sneaky"]);
    assert_eq!(l["themes"][1]["background"], Value::Null);
    let issue_ids: Vec<&str> = l["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(issue_ids, ["big", "sneaky"]);
}
