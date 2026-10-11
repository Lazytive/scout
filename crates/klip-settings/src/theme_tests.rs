use proptest::prelude::*;
use serde_json::{Value, json};

use super::*;

const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
const JPG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 16];
const WEBP: &[u8] = b"RIFF\x10\0\0\0WEBPVP8 ";

fn sample() -> CustomTheme {
    let mut t = CustomTheme {
        name: "Sunset".into(),
        base: "light".into(),
        radius: 10,
        font: Some("Noto Sans".into()),
        backdrop: Some(Backdrop {
            dim: 30,
            opacity: 70,
        }),
        ..CustomTheme::default()
    };
    t.colors.insert("accent".into(), "#e0601f".into());
    t.colors.insert("panel-raised".into(), "#fafafa".into());
    t
}

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

fn paths(issues: &[Issue]) -> Vec<&str> {
    issues.iter().map(|i| i.path.as_str()).collect()
}

#[test]
fn export_round_trips_with_the_picture() {
    let t = sample();
    let pic = Picture::from_bytes(PNG.to_vec()).unwrap();
    let file = t.to_export(Some(&pic));
    assert!(
        file["background"]["image"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    let back = parse_export(&bytes(&file)).unwrap();
    assert!(back.issues.is_empty(), "{:?}", back.issues);
    assert_eq!(back.theme, t);
    assert_eq!(back.picture, Some(pic));
}

#[test]
fn stored_round_trips_and_names_the_picture() {
    let t = sample();
    let back = parse_stored(&bytes(&t.to_stored(Some("sunset.png")))).unwrap();
    assert!(back.issues.is_empty(), "{:?}", back.issues);
    assert_eq!(back.theme, t);
    assert_eq!(back.picture_file.as_deref(), Some("sunset.png"));
    // No picture file: no backdrop either.
    let plain = parse_stored(&bytes(&t.to_stored(None))).unwrap();
    assert_eq!(plain.theme.backdrop, None);
    assert_eq!(plain.picture_file, None);
}

#[test]
fn the_ui_form_has_the_id_and_picture_path() {
    let ui = sample().to_ui("sunset", Some("C:/x/themes/sunset.png"));
    assert_eq!(ui["id"], "sunset");
    assert_eq!(ui["background"]["path"], "C:/x/themes/sunset.png");
    assert_eq!(ui["colors"]["accent"], "#e0601f");
    assert!(ui.get("scout_theme").is_none());
    assert_eq!(
        CustomTheme::default().to_ui("a", None)["background"],
        Value::Null
    );
}

#[test]
fn wrong_fields_are_repaired_one_by_one() {
    let file = json!({
        "scout_theme": 1,
        "name": "  \u{7}Neon\n  ",
        "base": "purple",
        "colors": {
            "accent": "#ABC",
            "panel": "red",
            "hover": "#ffffff",
            "--evil": "#000000",
            "text": 12,
        },
        "radius": 99,
        "font": "Inter\"; background: url(http://x)",
        "background": { "image": format!("data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(PNG)), "dim": 95, "opacity": 10 },
    });
    let r = parse_export(&bytes(&file)).unwrap();
    assert_eq!(r.theme.name, "Neon");
    assert_eq!(r.theme.base, "dark");
    assert_eq!(r.theme.colors.len(), 1);
    assert_eq!(r.theme.colors["accent"], "#aabbcc");
    assert_eq!(r.theme.radius, RADIUS_MAX);
    assert_eq!(r.theme.font, None);
    assert_eq!(
        r.theme.backdrop,
        Some(Backdrop {
            dim: DIM_MAX,
            opacity: OPACITY_MIN
        })
    );
    let p = paths(&r.issues);
    for want in [
        "base",
        "colors.panel",
        "colors.hover",
        "colors.--evil",
        "colors.text",
        "radius",
        "font",
        "background.dim",
        "background.opacity",
    ] {
        assert!(p.contains(&want), "{want} missing in {p:?}");
    }
    assert!(!p.contains(&"name"), "{p:?}");
}

#[test]
fn files_from_before_the_rename_still_import() {
    let r = parse_export(br##"{"klip_theme": 1, "name": "Old", "colors": {"panel": "#101010"}}"##)
        .unwrap();
    assert!(r.issues.is_empty(), "{:?}", r.issues);
    assert_eq!(r.theme.name, "Old");
    // Written back with the new key.
    let v = r.theme.to_export(None);
    assert_eq!(v[FORMAT_KEY], 1);
    assert!(v.get(LEGACY_FORMAT_KEY).is_none());
    assert_eq!(
        parse_export(br#"{"klip_theme": 9}"#).unwrap_err(),
        ThemeError::Newer(9)
    );
}

#[test]
fn a_name_that_is_only_spaces_gets_the_default() {
    let r = parse_export(&bytes(&json!({ "scout_theme": 1, "name": " \t " }))).unwrap();
    assert_eq!(r.theme.name, "Custom theme");
    assert_eq!(paths(&r.issues), ["name"]);
}

#[test]
fn non_theme_files_are_refused() {
    assert_eq!(parse_export(b"\xff\xfe").unwrap_err(), ThemeError::NotJson);
    assert_eq!(parse_export(b"[1]").unwrap_err(), ThemeError::NotTheme);
    assert_eq!(
        parse_export(b"{\"name\":\"x\"}").unwrap_err(),
        ThemeError::NotTheme
    );
    assert_eq!(
        parse_export(b"{\"scout_theme\":0}").unwrap_err(),
        ThemeError::NotTheme
    );
    assert_eq!(
        parse_export(b"{\"scout_theme\":\"1\"}").unwrap_err(),
        ThemeError::NotTheme
    );
    assert_eq!(
        parse_export(b"{\"scout_theme\":2}").unwrap_err(),
        ThemeError::Newer(2)
    );
    let big = vec![b' '; MAX_EXPORT_BYTES + 1];
    assert_eq!(parse_export(&big).unwrap_err(), ThemeError::TooLarge(12));
    let big = vec![b' '; MAX_STORED_BYTES + 1];
    assert!(matches!(parse_stored(&big), Err(ThemeError::TooLarge(_))));
}

#[test]
fn deep_nesting_is_refused_not_overflowed() {
    let mut s = String::from("{\"scout_theme\":1,\"colors\":");
    s.push_str(&"[".repeat(100_000));
    assert_eq!(parse_export(s.as_bytes()).unwrap_err(), ThemeError::NotJson);
}

#[test]
fn pictures_are_checked_by_their_bytes() {
    assert_eq!(Picture::from_bytes(PNG.to_vec()).unwrap().ext, "png");
    assert_eq!(Picture::from_bytes(JPG.to_vec()).unwrap().ext, "jpg");
    assert_eq!(Picture::from_bytes(WEBP.to_vec()).unwrap().ext, "webp");
    assert_eq!(
        Picture::from_bytes(b"GIF89a".to_vec()).unwrap_err(),
        ThemeError::NotPicture
    );
    assert_eq!(
        Picture::from_bytes(b"RIFF\0\0\0\0WAVE".to_vec()).unwrap_err(),
        ThemeError::NotPicture
    );
    let mut huge = PNG.to_vec();
    huge.resize(MAX_PICTURE_BYTES + 1, 0);
    assert_eq!(
        Picture::from_bytes(huge).unwrap_err(),
        ThemeError::PictureTooLarge
    );

    // The URI's type is ignored: the bytes decide (a JPEG labelled PNG is a JPEG).
    let enc = base64::engine::general_purpose::STANDARD.encode(JPG);
    let file = json!({ "scout_theme": 1, "name": "x",
        "background": { "image": format!("data:image/png;base64,{enc}") } });
    let r = parse_export(&bytes(&file)).unwrap();
    assert_eq!(r.picture.unwrap().ext, "jpg");
    assert_eq!(r.theme.backdrop, Some(Backdrop::default()));
}

#[test]
fn a_bad_picture_drops_only_the_background() {
    for image in [
        json!("https://example.com/a.png"),
        json!("data:image/png,rawtext"),
        json!("data:image/png;base64,!!!!"),
        json!(format!(
            "data:image/gif;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(b"GIF89a")
        )),
        json!(42),
    ] {
        let file = json!({ "scout_theme": 1, "name": "x", "colors": { "panel": "#101010" },
            "background": { "image": image } });
        let r = parse_export(&bytes(&file)).unwrap();
        assert_eq!(r.picture, None);
        assert_eq!(r.theme.backdrop, None);
        assert_eq!(r.theme.colors["panel"], "#101010");
        assert_eq!(paths(&r.issues), ["background.image"]);
    }
}

#[test]
fn stored_picture_names_cannot_leave_the_folder() {
    for f in [
        "../x.png",
        "a/b.png",
        "C:\\x.png",
        "x.gif",
        "x",
        ".png",
        "-x.png",
        "X.png",
    ] {
        let file = json!({ "scout_theme": 1, "name": "x", "background": { "file": f } });
        let r = parse_stored(&bytes(&file)).unwrap();
        assert_eq!(r.picture_file, None, "{f}");
        assert_eq!(r.theme.backdrop, None, "{f}");
    }
}

#[test]
fn ids_and_slugs() {
    assert_eq!(slug("My Theme!"), "my-theme");
    assert_eq!(slug("  --Ünïcode  Neon 2  "), "n-code-neon-2");
    assert_eq!(slug("日本"), "theme");
    assert_eq!(slug(&"a".repeat(200)).len(), 32);
    for ok in ["dark-2", "a", "x1"] {
        assert!(valid_id(ok), "{ok}");
    }
    for bad in ["", "-a", "A", "a b", "a.b", "../a", &"a".repeat(41)] {
        assert!(!valid_id(bad), "{bad}");
    }
}

#[test]
fn colours_and_fonts() {
    assert_eq!(normalise_colour("#ABCDEF").as_deref(), Some("#abcdef"));
    assert_eq!(normalise_colour(" #f0a ").as_deref(), Some("#ff00aa"));
    for bad in ["abcdef", "#abcd", "#ggg", "#abcdef00", "", "#"] {
        assert_eq!(normalise_colour(bad), None, "{bad}");
    }
    for ok in [
        "Inter",
        "Segoe UI Variable",
        "Noto_Sans-2.0",
        "源ノ角ゴシック",
    ] {
        assert!(valid_font(ok), "{ok}");
    }
    for bad in [
        "",
        "a\"b",
        "a;b",
        "a{b",
        "a\\b",
        "url(x)",
        "a,b",
        &"a".repeat(65),
    ] {
        assert!(!valid_font(bad), "{bad}");
    }
}

#[test]
fn ui_edits_are_repaired_like_files() {
    let (t, issues) = from_value(&json!({ "name": "Mine", "colors": { "panel": "#000" },
        "radius": 3, "background": { "dim": 10, "opacity": 100 } }));
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(t.colors["panel"], "#000000");
    assert_eq!(t.radius, 3);
    assert_eq!(
        t.backdrop,
        Some(Backdrop {
            dim: 10,
            opacity: 100
        })
    );
    let (t, issues) = from_value(&json!("nope"));
    assert_eq!(t.name, "Custom theme");
    assert!(!issues.is_empty());
}

#[test]
fn many_problems_are_summarised() {
    let colors: serde_json::Map<String, Value> = (0..300)
        .map(|i| (format!("c{i}"), json!("#000000")))
        .collect();
    let r = parse_export(&bytes(
        &json!({ "scout_theme": 1, "name": "x", "colors": colors }),
    ))
    .unwrap();
    assert_eq!(r.issues.len(), 33);
    assert!(r.issues.last().unwrap().message.contains("more problems"));
}

fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        (0u64..120).prop_map(|n| json!(n)),
        "\\PC{0,24}".prop_map(Value::String),
        "#[0-9a-fA-F]{3,6}".prop_map(Value::String),
        Just(json!("data:image/png;base64,iVBORw0KGgoAAAAN")),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        let key = prop_oneof![
            Just("scout_theme"),
            Just("name"),
            Just("base"),
            Just("colors"),
            Just("radius"),
            Just("font"),
            Just("background"),
            Just("image"),
            Just("file"),
            Just("dim"),
            Just("opacity"),
            Just("panel"),
            Just("accent"),
            Just("text-muted"),
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
    fn any_file_parses_or_is_refused_and_output_is_stable(mut v in arb_json()) {
        if let Value::Object(m) = &mut v {
            m.insert("scout_theme".into(), json!(1));
        }
        if let Ok(r) = parse_export(&bytes(&v)) {
            let again = parse_export(&bytes(&r.theme.to_export(r.picture.as_ref()))).unwrap();
            prop_assert!(again.issues.is_empty(), "{:?}", again.issues);
            prop_assert_eq!(&again.theme, &r.theme);
            let stored = parse_stored(&bytes(&r.theme.to_stored(Some("t.png")))).unwrap();
            prop_assert!(stored.issues.is_empty(), "{:?}", stored.issues);
        }
        let (t, _) = from_value(&v);
        let (back, issues) = from_value(&t.to_ui("t", None));
        prop_assert!(issues.is_empty(), "{:?}", issues);
        prop_assert_eq!(back, CustomTheme { backdrop: None, ..t });
    }
}
